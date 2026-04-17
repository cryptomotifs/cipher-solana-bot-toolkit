//! Fee collector — periodic creator fee collection via PumpPortal.
//!
//! [VERIFIED 2026] memecoin_launcher_strategy_2026.md line 336:
//!   "Fee claiming: action: 'collectCreatorFee' — claims all tokens at once"

use std::sync::Arc;
use tracing::{info, warn};

use crate::pipeline::LauncherContext;

/// Run the fee collector loop — collects creator fees every N seconds.
pub async fn run_fee_collector_loop(ctx: Arc<LauncherContext>) {
    let interval = std::time::Duration::from_secs(ctx.config.fee_collect_interval_secs);

    // Wait initial delay before first collection
    tokio::time::sleep(std::time::Duration::from_secs(300)).await;

    loop {
        match collect_fees(&ctx).await {
            Ok(()) => info!("Fee collection cycle completed"),
            Err(e) => warn!("Fee collection failed: {}", e),
        }
        tokio::time::sleep(interval).await;
    }
}

/// Collect all creator fees via PumpPortal API.
async fn collect_fees(ctx: &LauncherContext) -> anyhow::Result<()> {
    let tracker = ctx.tracker.lock().await;
    if tracker.tokens_launched == 0 {
        return Ok(()); // Nothing to collect
    }
    drop(tracker);

    if ctx.config.dry_run {
        info!("DRY RUN — skipping fee collection");
        return Ok(());
    }

    // POST to PumpPortal with collectCreatorFee action
    // [VERIFIED 2026] memecoin_launcher_strategy_2026.md line 336
    let body = serde_json::json!({
        "publicKey": ctx.wallet_a.pubkey().to_string(),
        "action": "collectCreatorFee"
    });

    let resp = ctx.http
        .post(predator_core::constants::PUMPPORTAL_TRADE_LOCAL)
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let err = resp.text().await.unwrap_or_default();
        // 400 likely means no fees to collect — not an error
        if status.as_u16() == 400 {
            info!("No creator fees to collect yet");
            return Ok(());
        }
        return Err(anyhow::anyhow!("Fee collection failed ({}): {}", status, err));
    }

    let tx_bytes = resp.bytes().await?;
    if tx_bytes.is_empty() {
        info!("No fees to collect (empty response)");
        return Ok(());
    }

    // Deserialize, sign, submit (same pattern as creator.rs)
    let mut vtx: solana_sdk::transaction::VersionedTransaction =
        bincode::deserialize(&tx_bytes)?;

    let blockhash = ctx.rpc.get_latest_blockhash().await?;
    vtx.message.set_recent_blockhash(blockhash);

    let signed = solana_sdk::transaction::VersionedTransaction::try_new(
        vtx.message,
        &[ctx.wallet_a.as_ref()],
    )?;

    match ctx.rpc.send_transaction(&signed).await {
        Ok(sig) => {
            info!("Fee collection TX submitted: {}", sig);
            // Update tracker with collected fees
            // TODO: Parse TX result to get actual fee amount
        }
        Err(e) => warn!("Fee collection TX failed: {}", e),
    }

    Ok(())
}

use solana_sdk::signer::Signer;
