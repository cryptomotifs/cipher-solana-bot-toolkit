//! Memecoin Launcher — entry point.
//!
//! Loads config, wallets (A=creator, B=trader), and runs the launch pipeline
//! on a timer with budget controls. Also spawns fee collector and sell monitor.
//!
//! Build: cargo build --release -p predator-launcher
//! Run:   RUST_LOG=info ./target/release/launcher

use anyhow::Result;
// [VERIFIED 2026] rust_solana_patterns_2026.md s1: "Signer trait for .pubkey()"
use solana_sdk::signer::Signer;
use std::sync::Arc;
use tracing::info;

mod config;
mod wallet;
mod budget;
mod tracker;
mod narrative;
mod concept;
mod image_gen;
mod ipfs;
mod creator;
mod first_buyer;
mod sell_monitor;
mod fee_collector;
mod pipeline;

use config::LauncherConfig;

#[tokio::main]
async fn main() -> Result<()> {
    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("=== PREDATOR LAUNCHER v0.1.0 ===");

    // Load .env (strip CRLF — Windows gotcha)
    // [VERIFIED 2026] project_brief.md: ".env has CRLF line endings — always strip \\r"
    if let Err(e) = dotenvy::dotenv() {
        tracing::warn!("No .env file found: {}", e);
    }

    // Load config
    let config = LauncherConfig::load("config.toml")?;
    info!(
        "Config loaded: dry_run={}, max_tokens/day={}, platform={}",
        config.dry_run, config.max_tokens_per_day, config.primary_platform
    );

    if config.dry_run {
        info!("*** DRY RUN MODE — no real transactions will be submitted ***");
    }

    // Load wallets
    let wallet_a = wallet::load_wallet()?;
    info!("Wallet A (creator): {}", wallet_a.pubkey());

    let wallet_b = wallet::load_trader_wallet(&config)?;
    info!("Wallet B (trader):  {}", wallet_b.pubkey());

    // Build shared resources
    let http = Arc::new(reqwest::Client::new());
    let rpc_url = std::env::var("HELIUS_API_KEY")
        .map(|key| format!("https://mainnet.helius-rpc.com/?api-key={}", key))
        .or_else(|_| std::env::var("RPC_ENDPOINTS").map(|e| {
            e.split(',').next().unwrap_or("https://api.mainnet-beta.solana.com").trim().to_string()
        }))
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let rpc = Arc::new(solana_client::nonblocking::rpc_client::RpcClient::new(rpc_url.clone()));
    info!("RPC: {}", &rpc_url[..rpc_url.len().min(60)]);

    // Check balances
    let bal_a = rpc.get_balance(&wallet_a.pubkey()).await.unwrap_or(0);
    let bal_b = rpc.get_balance(&wallet_b.pubkey()).await.unwrap_or(0);
    info!(
        "Balances: A={:.4} SOL, B={:.4} SOL",
        bal_a as f64 / 1e9,
        bal_b as f64 / 1e9
    );

    // Build Jito submitter
    let jito = Arc::new(predator_execution::JitoSubmitter::new());

    // Build Telegram alerter
    let bot_config = predator_core::BotConfig::load("config.toml")?;
    let alerter = predator_dashboard::alerts::TelegramAlerter::new(
        &bot_config.dashboard,
        http.clone(),
    );
    if alerter.is_some() {
        info!("Telegram alerter: ACTIVE");
    }

    // Build tracker
    let tracker = Arc::new(tokio::sync::Mutex::new(
        tracker::LauncherPnL::load(&config.tracker_file)?,
    ));
    info!(
        "Tracker: {} tokens launched so far",
        tracker.lock().await.records.len()
    );

    // Build context
    let ctx = Arc::new(pipeline::LauncherContext {
        config: config.clone(),
        http: http.clone(),
        rpc: rpc.clone(),
        wallet_a: Arc::new(wallet_a),
        wallet_b: Arc::new(wallet_b),
        jito: jito.clone(),
        alerter,
        tracker: tracker.clone(),
    });

    // Spawn launch pipeline loop
    let ctx_pipeline = ctx.clone();
    let pipeline_handle = tokio::spawn(async move {
        pipeline::run_pipeline_loop(ctx_pipeline).await;
    });

    // Spawn fee collector loop
    let ctx_fees = ctx.clone();
    let fee_handle = tokio::spawn(async move {
        fee_collector::run_fee_collector_loop(ctx_fees).await;
    });

    // Spawn sell monitor loop
    let ctx_sell = ctx.clone();
    let sell_handle = tokio::spawn(async move {
        sell_monitor::run_sell_monitor_loop(ctx_sell).await;
    });

    info!(
        "Pipeline started: launch every {}s, fees every {}s",
        config.launch_interval_secs, config.fee_collect_interval_secs
    );
    info!("Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    // Abort tasks
    pipeline_handle.abort();
    fee_handle.abort();
    sell_handle.abort();

    // Save tracker
    tracker.lock().await.save(&config.tracker_file)?;
    info!("Tracker saved. Goodbye.");

    Ok(())
}
