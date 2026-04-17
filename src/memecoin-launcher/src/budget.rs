//! Budget guard — enforces hard limits before any token launch.
//!
//! [VERIFIED 2026] memecoin_launcher_strategy_2026.md s18: kill criteria
//! [VERIFIED 2026] copy_trade_wallet_filtering_2026.md s7: kill switch

use anyhow::{Result, anyhow};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

use crate::config::LauncherConfig;
use crate::tracker::LauncherPnL;

/// Check if a launch is allowed given current budget constraints.
/// Returns Ok(()) if launch is permitted, Err with reason if not.
pub async fn check_can_launch(
    config: &LauncherConfig,
    rpc: &RpcClient,
    wallet_a: &Keypair,
    tracker: &LauncherPnL,
) -> Result<()> {
    // 1. Kill switch: Wallet A balance too low
    let balance = rpc.get_balance(&wallet_a.pubkey()).await
        .map_err(|e| anyhow!("RPC get_balance failed: {}", e))?;

    if balance < config.kill_switch_balance_lamports {
        return Err(anyhow!(
            "KILL SWITCH: balance {:.4} SOL < {:.4} SOL minimum",
            balance as f64 / 1e9,
            config.kill_switch_balance_lamports as f64 / 1e9
        ));
    }

    // 2. Daily SOL spend limit
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let today_spent = tracker.daily_spend(&today);
    let max_daily_lamports = (config.max_sol_per_day * 1e9) as u64;

    if today_spent >= max_daily_lamports {
        return Err(anyhow!(
            "Daily SOL limit reached: {:.4} SOL spent today (max {:.4})",
            today_spent as f64 / 1e9,
            config.max_sol_per_day
        ));
    }

    // 3. Daily token count limit
    let today_count = tracker.tokens_launched_today(&today);
    if today_count >= config.max_tokens_per_day {
        return Err(anyhow!(
            "Daily token limit: {} launched today (max {})",
            today_count, config.max_tokens_per_day
        ));
    }

    // 4. Peak hours check
    let current_hour = chrono::Utc::now().hour();
    if !config.launch_hours_utc.is_empty() && !config.launch_hours_utc.contains(&current_hour) {
        return Err(anyhow!(
            "Off-peak hour: {} UTC (allowed: {:?})",
            current_hour, config.launch_hours_utc
        ));
    }

    // 5. Enough SOL for creation + trader buy + Jito tip + kill reserve
    let estimated_cost = 16_000_000u64 // ~0.016 SOL creation rent
        + config.jito_tip_lamports
        + config.kill_switch_balance_lamports;

    if balance < estimated_cost {
        return Err(anyhow!(
            "Insufficient: {:.4} SOL < {:.4} SOL needed (cost + reserve)",
            balance as f64 / 1e9,
            estimated_cost as f64 / 1e9
        ));
    }

    Ok(())
}

use chrono::Timelike;
