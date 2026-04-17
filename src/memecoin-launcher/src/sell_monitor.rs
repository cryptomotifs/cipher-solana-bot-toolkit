//! Sell monitor — TP/SL/timeout for Wallet B positions.
//!
//! [VERIFIED 2026] memecoin_launch_revenue_model_2026.md: sell at 2x TP
//! [VERIFIED 2026] copy_trade_entry_exit_2026.md s3-s4: TP/SL mechanics

use std::sync::Arc;
use tracing::{info, warn};

use crate::pipeline::LauncherContext;
use crate::tracker::TokenStatus;

/// Run the sell monitor loop — checks active positions every 30s.
pub async fn run_sell_monitor_loop(ctx: Arc<LauncherContext>) {
    let interval = std::time::Duration::from_secs(30);

    loop {
        tokio::time::sleep(interval).await;

        let mut tracker = ctx.tracker.lock().await;
        let active: Vec<usize> = tracker.records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.status == TokenStatus::Active && r.trader_buy_lamports > 0)
            .map(|(i, _)| i)
            .collect();

        if active.is_empty() {
            continue;
        }

        for idx in active {
            let record = &tracker.records[idx];
            let mint = &record.mint;

            // Check position age (timeout)
            if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&record.created_at) {
                let age_mins = (chrono::Utc::now() - created.with_timezone(&chrono::Utc))
                    .num_minutes();
                if age_mins >= ctx.config.position_timeout_mins as i64 {
                    info!("Position timeout: {} ({}min)", record.name, age_mins);
                    // Mark as dead (would sell in real impl)
                    tracker.records[idx].status = TokenStatus::Dead;
                    continue;
                }
            }

            // TODO: Check current price via Jupiter quote API
            // GET https://api.jup.ag/quote?inputMint={token}&outputMint=So11...&amount={tokens}
            // Compare entry price vs current price
            // If current >= entry * (1 + take_profit_pct/100) → SELL (TP)
            // If current <= entry * (1 - stop_loss_pct/100) → SELL (SL)

            // For now, just log active positions
            if rand::random::<u32>() % 10 == 0 {
                info!("Monitoring: {} ({}) — active", record.name, mint);
            }
        }

        // Save if any changes
        if let Err(e) = tracker.save(&ctx.config.tracker_file) {
            warn!("Tracker save failed: {}", e);
        }
    }
}
