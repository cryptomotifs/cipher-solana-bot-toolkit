//! Pipeline orchestrator — runs the full launch cycle on a timer.
//!
//! [VERIFIED 2026] memecoin_launcher_strategy_2026.md s17: implementation plan

use anyhow::Result;
use std::sync::Arc;
use tracing::{info, warn, error};

use crate::config::LauncherConfig;
use crate::tracker::{LauncherPnL, LaunchRecord};

/// Shared context for all launcher modules.
pub struct LauncherContext {
    pub config: LauncherConfig,
    pub http: Arc<reqwest::Client>,
    pub rpc: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    pub wallet_a: Arc<solana_sdk::signature::Keypair>,
    pub wallet_b: Arc<solana_sdk::signature::Keypair>,
    pub jito: Arc<predator_execution::JitoSubmitter>,
    pub alerter: Option<predator_dashboard::alerts::TelegramAlerter>,
    pub tracker: Arc<tokio::sync::Mutex<LauncherPnL>>,
}

/// Run the main pipeline loop.
pub async fn run_pipeline_loop(ctx: Arc<LauncherContext>) {
    let interval = std::time::Duration::from_secs(ctx.config.launch_interval_secs);

    info!(
        "Pipeline loop started: interval={}s, max={}/day, dry_run={}",
        ctx.config.launch_interval_secs,
        ctx.config.max_tokens_per_day,
        ctx.config.dry_run
    );

    loop {
        match run_launch_cycle(&ctx).await {
            Ok(Some(record)) => {
                info!(
                    "Launch cycle SUCCESS: {} ({}) mint={}",
                    record.name, record.symbol, record.mint
                );
            }
            Ok(None) => {
                // Budget/timing guard blocked the launch — normal
            }
            Err(e) => {
                error!("Launch cycle FAILED: {}", e);
                // Send Telegram alert on failure
                if let Some(ref alerter) = ctx.alerter {
                    let _ = alerter.send_alert(
                        predator_dashboard::alerts::AlertType::StrategyError,
                        &format!("Launch failed: {}", e),
                    ).await;
                }
            }
        }

        // Save tracker after each cycle
        if let Ok(tracker) = ctx.tracker.try_lock() {
            if let Err(e) = tracker.save(&ctx.config.tracker_file) {
                warn!("Tracker save failed: {}", e);
            }
        }

        tokio::time::sleep(interval).await;
    }
}

/// Execute one complete launch cycle.
///
/// Pipeline:
/// 1. Budget check
/// 2. Narrative detection
/// 3. Concept generation
/// 4. Image generation
/// 5. IPFS upload
/// 6. Token creation (PumpPortal)
/// 7. First buyer (Wallet B, separate TX)
/// 8. Telegram alert
/// 9. Update tracker
pub async fn run_launch_cycle(ctx: &LauncherContext) -> Result<Option<LaunchRecord>> {
    // 1. Budget check
    {
        let tracker = ctx.tracker.lock().await;
        if let Err(reason) = crate::budget::check_can_launch(
            &ctx.config,
            &ctx.rpc,
            &ctx.wallet_a,
            &tracker,
        ).await {
            info!("Launch skipped: {}", reason);
            return Ok(None);
        }
    }

    info!("=== LAUNCH CYCLE START ===");

    // 2. Narrative detection
    let signals = crate::narrative::detect_narratives(&ctx.http, &ctx.config).await?;
    if signals.is_empty() {
        warn!("No narratives detected — skipping launch");
        return Ok(None);
    }
    let best = &signals[0];
    info!("Best narrative: '{}' (score={}, source={})", best.topic, best.score, best.source);

    // 3. Concept generation
    let concept = {
        let tracker = ctx.tracker.lock().await;
        crate::concept::generate_concept(best, &tracker)?
    };
    info!("Concept: {} ({}) — '{}'", concept.name, concept.symbol, &concept.description[..concept.description.len().min(60)]);

    // 4. Image generation
    let image_path = crate::image_gen::generate_logo(&concept)?;

    // 5. IPFS upload
    let metadata_uri = crate::ipfs::upload_to_pinata(
        &ctx.http,
        &ctx.config,
        &concept,
        &image_path,
    ).await?;
    info!("Metadata URI: {}", metadata_uri);

    // 6. Token creation
    let mut record = crate::creator::create_token(
        &ctx.http,
        &ctx.rpc,
        &ctx.wallet_a,
        &ctx.jito,
        &concept,
        &metadata_uri,
        &ctx.config,
    ).await?;

    // 7. First buyer (Wallet B, separate TX) — non-fatal if it fails
    match crate::first_buyer::buy_separate(
        &ctx.http,
        &ctx.rpc,
        &ctx.wallet_b,
        &ctx.jito,
        &record,
        &ctx.config,
    ).await {
        Ok(sig) => {
            record.buyer_tx = sig;
            record.trader_buy_lamports = (ctx.config.trader_buy_sol * 1e9) as u64;
            info!("Wallet B buy succeeded");
        }
        Err(e) => {
            warn!("Wallet B buy failed (non-fatal): {}", e);
        }
    }

    // 8. Telegram alert
    if let Some(ref alerter) = ctx.alerter {
        let msg = format!(
            "TOKEN LAUNCHED\nName: {}\nSymbol: {}\nMint: {}\nNarrative: {}\nCost: {:.4} SOL\nPlatform: pump.fun",
            record.name, record.symbol, record.mint,
            record.narrative,
            record.creation_cost_lamports as f64 / 1e9
        );
        let _ = alerter.send_alert(
            predator_dashboard::alerts::AlertType::Info,
            &msg,
        ).await;
    }

    // 9. Update tracker
    {
        let mut tracker = ctx.tracker.lock().await;
        tracker.add_record(record.clone());
    }

    // Clean up temp image
    let _ = std::fs::remove_file(&image_path);

    info!("=== LAUNCH CYCLE COMPLETE: {} ({}) ===", record.name, record.symbol);
    Ok(Some(record))
}
