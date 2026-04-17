//! LauncherConfig — settings for the memecoin token launcher.
//!
//! [VERIFIED 2026] memecoin_launcher_strategy_2026.md s17: implementation plan
//! [VERIFIED 2026] memecoin_launch_revenue_model_2026.md s7: SOL requirements
//! [VERIFIED 2026] bot_attraction_token_design_2026.md: optimal launch hours

use serde::Deserialize;

/// Launcher configuration, loaded from `config.toml` [launcher] section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    /// Master enable switch.
    pub enabled: bool,

    /// Dry run mode — runs full pipeline but skips TX submission.
    /// [VERIFIED 2026] memecoin_launcher_strategy_2026.md s17: "Start safe"
    pub dry_run: bool,

    /// Primary launch platform: "pumpfun" (MVP) or "raydium" (future).
    pub primary_platform: String,

    /// Maximum SOL to spend per day across all launches.
    /// [VERIFIED 2026] memecoin_launcher_strategy_2026.md s18: "max 1 SOL/week"
    pub max_sol_per_day: f64,

    /// Maximum tokens to launch per day.
    pub max_tokens_per_day: u32,

    /// Emergency stop: halt if wallet A balance drops below this (lamports).
    /// [VERIFIED 2026] copy_trade_wallet_filtering_2026.md s7: kill switch
    pub kill_switch_balance_lamports: u64,

    /// SOL amount for Wallet B (trader) to buy each launched token.
    /// [VERIFIED 2026] bot_attraction_token_design_2026.md: "0.5-5 SOL range"
    /// We use 0.02 SOL (minimum viable with our capital).
    pub trader_buy_sol: f64,

    /// Path to Wallet B (trader) encrypted keyfile or JSON.
    /// If empty, uses TRADER_WALLET_KEYFILE + TRADER_WALLET_PASSWORD env vars.
    pub trader_wallet_path: String,

    /// Seconds between launch attempts.
    pub launch_interval_secs: u64,

    /// Seconds between fee collection runs.
    pub fee_collect_interval_secs: u64,

    /// Reddit subreddits to monitor for narrative detection.
    /// [VERIFIED 2026] narrative_detection_twitter_2026.md s3: "FREE RSS feeds"
    pub reddit_subreddits: Vec<String>,

    /// Enable PumpPortal WebSocket for trend detection.
    pub pumpfun_ws_enabled: bool,

    /// Pinata JWT for IPFS uploads.
    /// [VERIFIED 2026] pumpfun_token_creation_technical_2026.md s6: Pinata free tier
    pub pinata_jwt: String,

    /// P&L tracker file path.
    pub tracker_file: String,

    /// Jito tip amount for token creation bundles (lamports).
    /// [VERIFIED 2026] pumpfun_token_creation_technical_2026.md s8: "0.01 SOL standard"
    pub jito_tip_lamports: u64,

    /// Only launch during these UTC hours (peak activity).
    /// [VERIFIED 2026] bot_attraction_token_design_2026.md: "2PM-4PM or 8PM-10PM UTC"
    pub launch_hours_utc: Vec<u32>,

    /// Take profit percentage for Wallet B positions.
    /// [VERIFIED 2026] memecoin_launch_revenue_model_2026.md: sell at 2x
    pub take_profit_pct: f64,

    /// Stop loss percentage for Wallet B positions.
    pub stop_loss_pct: f64,

    /// Position timeout in minutes.
    pub position_timeout_mins: u64,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dry_run: true, // START SAFE
            primary_platform: "pumpfun".to_string(),
            max_sol_per_day: 0.10,
            max_tokens_per_day: 2,
            kill_switch_balance_lamports: 200_000_000, // 0.20 SOL
            trader_buy_sol: 0.02,
            trader_wallet_path: String::new(),
            launch_interval_secs: 7200, // 2 hours
            fee_collect_interval_secs: 21600, // 6 hours
            reddit_subreddits: vec![
                "CryptoCurrency".to_string(),
                "solana".to_string(),
                "memecoin".to_string(),
                "SatoshiStreetBets".to_string(),
            ],
            pumpfun_ws_enabled: true,
            pinata_jwt: String::new(),
            tracker_file: "launcher_pnl.json".to_string(),
            jito_tip_lamports: 5_000_000, // 0.005 SOL
            launch_hours_utc: vec![14, 15, 16, 20, 21, 22],
            take_profit_pct: 100.0, // 2x
            stop_loss_pct: 50.0,
            position_timeout_mins: 30,
        }
    }
}

impl LauncherConfig {
    /// Load from config.toml [launcher] section, with .env overrides.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let raw: toml::Value = toml::from_str(&content).unwrap_or(toml::Value::Table(Default::default()));

        let mut config: Self = if let Some(section) = raw.get("launcher") {
            section.clone().try_into().unwrap_or_default()
        } else {
            Self::default()
        };

        // Override from .env
        if let Ok(jwt) = std::env::var("PINATA_JWT") {
            config.pinata_jwt = jwt.trim().trim_end_matches('\r').to_string();
        }
        if let Ok(val) = std::env::var("LAUNCHER_DRY_RUN") {
            config.dry_run = val.trim() != "false";
        }

        Ok(config)
    }
}
