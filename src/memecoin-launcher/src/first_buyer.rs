//! First buyer — Wallet B buys in a SEPARATE transaction for GMGN reputation.
//!
//! [VERIFIED 2026] gmgn_wallet_reputation_flywheel_2026.md:
//!   "DEV Team label auto-applied to bundled buyers — must be SEPARATE tx, SEPARATE block"
//! [VERIFIED 2026] bot_attraction_token_design_2026.md:
//!   "buy in 0.5-5 SOL range for copy bot triggers" (we use 0.02 SOL minimum viable)

use anyhow::{Result, anyhow, Context};
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use tracing::{info, warn};

use crate::config::LauncherConfig;
use crate::tracker::LaunchRecord;

/// Execute a first-buyer purchase with Wallet B, separate from the create TX.
///
/// Critical: This MUST be in a different block than the create TX to avoid
/// GMGN's "DEV Team" / "Bundled Tx" detection labels.
pub async fn buy_separate(
    http: &reqwest::Client,
    rpc: &solana_client::nonblocking::rpc_client::RpcClient,
    wallet_b: &Keypair,
    jito: &predator_execution::JitoSubmitter,
    record: &LaunchRecord,
    config: &LauncherConfig,
) -> Result<String> {
    if config.dry_run {
        info!("DRY RUN — skipping Wallet B buy for {}", record.mint);
        return Ok("dry_run".to_string());
    }

    if config.trader_buy_sol <= 0.0 {
        info!("Trader buy disabled (0 SOL) — skipping");
        return Ok(String::new());
    }

    // Wait 3-8 seconds to ensure we're in a different block
    // [VERIFIED 2026] gmgn_wallet_reputation_flywheel_2026.md: "separate block"
    let delay = rand::random::<u64>() % 6 + 3; // 3-8 seconds
    info!("Waiting {}s before Wallet B buy (anti-bundling delay)...", delay);
    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;

    // POST to PumpPortal — same pattern as copy_executor.rs
    let body = serde_json::json!({
        "publicKey": wallet_b.pubkey().to_string(),
        "action": "buy",
        "mint": record.mint,
        "amount": config.trader_buy_sol,
        "denominatedInSol": "true",
        "slippage": 25,
        "priorityFee": 0.0001,
        "pool": "pump"
    });

    info!(
        "Wallet B buy: {} SOL → {} ({})",
        config.trader_buy_sol, record.name, record.mint
    );

    let resp = http
        .post(predator_core::constants::PUMPPORTAL_TRADE_LOCAL)
        .json(&body)
        .send()
        .await
        .context("PumpPortal buy request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(anyhow!("PumpPortal buy failed ({}): {}", status, err));
    }

    let tx_bytes = resp.bytes().await
        .context("PumpPortal buy response read failed")?;

    if tx_bytes.is_empty() {
        return Err(anyhow!("PumpPortal buy returned empty response"));
    }

    let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
        .context("Failed to deserialize buy TX")?;

    // Sign with Wallet B ONLY (not Wallet A!)
    let recent_blockhash = rpc.get_latest_blockhash().await?;
    versioned_tx.message.set_recent_blockhash(recent_blockhash);

    let signed_tx = VersionedTransaction::try_new(
        versioned_tx.message,
        &[wallet_b],
    )?;

    // Submit via Jito (use different tip account than create TX — random rotation handles this)
    let tip_amount = config.jito_tip_lamports / 2; // Smaller tip for buy
    let tip_tx = predator_execution::JitoSubmitter::create_tip_transaction(
        wallet_b,
        tip_amount,
        recent_blockhash,
    );

    let sig = match jito.submit_bundle(http, &[signed_tx.clone()], &tip_tx).await {
        Ok(id) => {
            info!("Wallet B buy via Jito bundle={}", id);
            signed_tx.signatures.first().cloned().unwrap_or_default()
        }
        Err(e) => {
            warn!("Jito buy bundle failed ({}), raw RPC fallback", e);
            rpc.send_transaction(&signed_tx)
                .await
                .context("Failed to send buy TX (raw RPC)")?
        }
    };

    info!("Wallet B buy submitted: sig={}, token={}", sig, record.mint);

    // Wait for confirmation
    for attempt in 1..=4 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Ok(statuses) = rpc.get_signature_statuses(&[sig]).await {
            if let Some(Some(status)) = statuses.value.first() {
                if status.err.is_none() {
                    info!("Wallet B buy confirmed: {} (attempt {})", sig, attempt);
                    return Ok(sig.to_string());
                }
            }
        }
    }

    warn!("Wallet B buy TX {} not confirmed after 8s", sig);
    Ok(sig.to_string())
}
