//! Token creation via PumpPortal API.
//!
//! Pattern from copy_executor.rs:2199-2305, adapted for "create" action.
//!
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md lines 438-466
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md line 466:
//!   "Must sign with both creator wallet AND mint keypair"

use anyhow::{Result, anyhow, Context};
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use std::sync::Arc;
use tracing::{info, warn};

use crate::concept::TokenConcept;
use crate::config::LauncherConfig;
use crate::tracker::LaunchRecord;

/// Create a token on pump.fun via PumpPortal API.
///
/// Flow:
/// 1. Generate fresh mint keypair
/// 2. POST to PumpPortal with action: "create"
/// 3. Deserialize unsigned VersionedTransaction
/// 4. Sign with BOTH wallet_a AND mint_keypair
/// 5. Submit via Jito bundle (6-region parallel)
/// 6. Confirm TX landed
///
/// Returns a LaunchRecord on success.
pub async fn create_token(
    http: &reqwest::Client,
    rpc: &solana_client::nonblocking::rpc_client::RpcClient,
    wallet_a: &Keypair,
    jito: &predator_execution::JitoSubmitter,
    concept: &TokenConcept,
    metadata_uri: &str,
    config: &LauncherConfig,
) -> Result<LaunchRecord> {
    // 1. Generate fresh mint keypair
    let mint_keypair = Keypair::new();
    let mint_pubkey = mint_keypair.pubkey().to_string();
    info!("Creating token: {} ({}) mint={}", concept.name, concept.symbol, mint_pubkey);

    if config.dry_run {
        info!("DRY RUN — skipping actual PumpPortal create");
        return Ok(LaunchRecord {
            mint: mint_pubkey,
            name: concept.name.clone(),
            symbol: concept.symbol.clone(),
            platform: "pumpfun".to_string(),
            narrative: concept.narrative_category.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            creation_cost_lamports: 0,
            trader_buy_lamports: 0,
            trader_sell_lamports: 0,
            fees_collected_lamports: 0,
            status: crate::tracker::TokenStatus::Active,
            creator_tx: "dry_run".to_string(),
            buyer_tx: String::new(),
        });
    }

    // 2. POST to PumpPortal
    // [VERIFIED 2026] pumpfun_token_creation_technical_2026.md lines 438-459
    let body = serde_json::json!({
        "publicKey": wallet_a.pubkey().to_string(),
        "action": "create",
        "tokenMetadata": {
            "name": concept.name,
            "symbol": concept.symbol,
            "uri": metadata_uri
        },
        "mint": bs58::encode(mint_keypair.to_bytes()).into_string(),
        "denominatedInSol": "true",
        "amount": 0,  // NO dev buy from Wallet A — save SOL
        "slippage": 15,
        "priorityFee": 0.0001,
        "pool": "pump"
    });

    info!("PumpPortal create request: {} ({})", concept.name, concept.symbol);

    let resp = http
        .post(predator_core::constants::PUMPPORTAL_TRADE_LOCAL)
        .json(&body)
        .send()
        .await
        .context("PumpPortal create request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let error_text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("PumpPortal create failed (HTTP {}): {}", status, error_text));
    }

    // 3. Deserialize unsigned VersionedTransaction
    // [VERIFIED 2026] pumpfun_swap_execution_2026.md s15: "Binary serialized VersionedTransaction"
    let tx_bytes = resp.bytes().await
        .context("PumpPortal create response read failed")?;

    if tx_bytes.is_empty() {
        return Err(anyhow!("PumpPortal returned empty response"));
    }

    let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
        .context("Failed to deserialize PumpPortal VersionedTransaction")?;

    // 4. Sign with BOTH wallet_a AND mint_keypair
    // [VERIFIED 2026] pumpfun_token_creation_technical_2026.md line 466
    let recent_blockhash = rpc.get_latest_blockhash().await?;
    versioned_tx.message.set_recent_blockhash(recent_blockhash);

    let signed_tx = VersionedTransaction::try_new(
        versioned_tx.message,
        &[wallet_a, &mint_keypair],
    )?;

    // 5. Submit via Jito bundle
    // [VERIFIED 2026] operational_data_2026.md s7: "Failed bundle cost: $0.00"
    let tip_amount = config.jito_tip_lamports;
    let tip_tx = predator_execution::JitoSubmitter::create_tip_transaction(
        wallet_a,
        tip_amount,
        recent_blockhash,
    );

    let sig = match jito.submit_bundle(http, &[signed_tx.clone()], &tip_tx).await {
        Ok(bundle_id) => {
            info!("Token create via Jito bundle={}", bundle_id);
            signed_tx.signatures.first().cloned().unwrap_or_default()
        }
        Err(e) => {
            warn!("Jito bundle failed ({}), falling back to raw RPC", e);
            rpc.send_transaction(&signed_tx)
                .await
                .context("Failed to send create TX (raw RPC fallback)")?
        }
    };

    info!("Token create TX submitted: sig={}, mint={}", sig, mint_pubkey);

    // 6. Confirm TX landed
    let confirmed = confirm_transaction(rpc, &sig).await;
    if !confirmed {
        return Err(anyhow!("Create TX {} not confirmed on-chain", sig));
    }

    info!("TOKEN CREATED: {} ({}) mint={} sig={}", concept.name, concept.symbol, mint_pubkey, sig);

    // Estimate creation cost (~0.016 SOL rent + tip)
    let estimated_cost = 16_000_000 + tip_amount;

    Ok(LaunchRecord {
        mint: mint_pubkey,
        name: concept.name.clone(),
        symbol: concept.symbol.clone(),
        platform: "pumpfun".to_string(),
        narrative: concept.narrative_category.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        creation_cost_lamports: estimated_cost,
        trader_buy_lamports: 0,
        trader_sell_lamports: 0,
        fees_collected_lamports: 0,
        status: crate::tracker::TokenStatus::Active,
        creator_tx: sig.to_string(),
        buyer_tx: String::new(),
    })
}

/// Poll for TX confirmation (3 attempts, 2s delay).
/// Same pattern as copy_executor.rs:2311-2322.
async fn confirm_transaction(
    rpc: &solana_client::nonblocking::rpc_client::RpcClient,
    sig: &solana_sdk::signature::Signature,
) -> bool {
    for attempt in 1..=5 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        match rpc.get_signature_statuses(&[*sig]).await {
            Ok(statuses) => {
                if let Some(Some(status)) = statuses.value.first() {
                    if status.err.is_none() {
                        info!("TX {} confirmed (attempt {})", sig, attempt);
                        return true;
                    } else {
                        warn!("TX {} failed: {:?}", sig, status.err);
                        return false;
                    }
                }
            }
            Err(e) => warn!("getSignatureStatuses error (attempt {}): {}", attempt, e),
        }
    }
    false
}
