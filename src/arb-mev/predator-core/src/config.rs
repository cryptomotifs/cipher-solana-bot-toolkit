//! BotConfig — deserialized from config.toml with serde defaults.
//!
//! All design decisions are verified against 2026 research:
//!
//! - [VERIFIED 2026] code_structure_patterns_2026.md Section 6: "Layered Config with
//!   Hot Reload. Using confique/serde for typed config." BotConfig struct with nested
//!   strategy configs, serde(default) for optional fields.
//!
//! - [VERIFIED 2026] code_structure_patterns_2026.md Section 4: "Arc<ArcSwap<BotConfig>>
//!   for hot-reloadable config. Lock-free read (nanosecond latency). Reload config from
//!   file, validate, then swap atomically."
//!
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1421-1590: Full config.toml
//!   layout with [bot], [wallet], [rpc], [grpc], [strategies.*], [protocols.*],
//!   [jito], [flash_loan], [scanner], [crash_prediction], [dashboard], [alerts].
//!
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1469-1505: Per-strategy config
//!   blocks with enabled, priority, protocol-specific params.
//!
//! - [VERIFIED 2026] plugins_frontend_backend_2026.md Section 4: Telegram teloxide
//!   integration for alerts with chat_id and token.
//!
//! - [VERIFIED 2026] crash_prediction_2026.md: Crash prediction config with
//!   coinalyze_api_key, scan_intervals per risk level.

use std::path::Path;

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Top-level BotConfig
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1421-1590
// [VERIFIED 2026] code_structure_patterns_2026.md Section 6
// ---------------------------------------------------------------------------

/// Top-level bot configuration, deserialized from `config.toml`.
///
/// Uses `serde(default)` throughout so that missing keys in the TOML file
/// get sensible defaults rather than causing parse errors.
///
/// [VERIFIED 2026] code_structure_patterns_2026.md Section 6:
///   "Layer precedence: Compiled defaults -> Config file -> Env vars -> CLI args"
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BotConfig {
    /// General settings (log level, dry run, etc.).
    pub general: GeneralConfig,

    /// Jupiter API configuration.
    pub jupiter: JupiterConfig,

    /// Jito bundle submission configuration.
    pub jito: JitoConfig,

    /// Scanner settings (intervals, thresholds).
    pub scanner: ScannerConfig,

    /// Per-strategy configuration.
    pub strategies: StrategiesConfig,

    /// Flash loan provider configuration.
    pub flash_loan: FlashLoanConfig,

    /// Crash prediction engine configuration.
    pub crash_prediction: CrashPredictionConfig,

    /// Dashboard and alerting configuration.
    pub dashboard: DashboardConfig,

    /// Yellowstone gRPC (Geyser) connection configuration.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1445: "[grpc] section"
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2: "Yellowstone gRPC sub-50ms"
    pub geyser: GeyserConfig,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            jupiter: JupiterConfig::default(),
            jito: JitoConfig::default(),
            scanner: ScannerConfig::default(),
            strategies: StrategiesConfig::default(),
            flash_loan: FlashLoanConfig::default(),
            crash_prediction: CrashPredictionConfig::default(),
            dashboard: DashboardConfig::default(),
            geyser: GeyserConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// [general]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1427-1435
// ---------------------------------------------------------------------------

/// General bot settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Path to the wallet keypair file.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1434:
    ///   "path = ~/.config/solana/id.json"
    pub wallet_path: String,

    /// Ordered list of RPC endpoints (primary first, fallbacks after).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1437-1443:
    ///   "QuickNode -> Chainstack -> Public -> Alchemy -> Helius"
    pub rpc_endpoints: Vec<String>,

    /// Log level filter (trace, debug, info, warn, error).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1429: "log_level = 'info'"
    pub log_level: String,

    /// Whether to run in dry-run mode (simulate but don't submit).
    pub dry_run: bool,

    /// Health check interval in seconds.
    pub health_check_interval_secs: u64,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            wallet_path: "~/.config/solana/id.json".to_string(),
            rpc_endpoints: Vec::new(),
            log_level: "info".to_string(),
            dry_run: false,
            health_check_interval_secs: 30,
        }
    }
}

// ---------------------------------------------------------------------------
// [jupiter]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 124-126:
//   "JupiterClient: V2 /build endpoint, Metis dual path"
// ---------------------------------------------------------------------------

/// Jupiter API configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JupiterConfig {
    /// Jupiter API key (from jupiter.pro subscription).
    pub api_key: String,

    /// Base URL for the Jupiter API.
    pub base_url: String,

    /// Maximum requests per second to the Jupiter API.
    pub max_rps: u32,

    /// Whether to use the V2 /build endpoint (single call replaces /quote + /swap).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 124:
    ///   "V2 /build endpoint (single call replaces /quote + /swap-instructions)"
    pub use_v2_build: bool,
}

impl Default for JupiterConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.jup.ag".to_string(),
            max_rps: 10,
            use_v2_build: true,
        }
    }
}

// ---------------------------------------------------------------------------
// [jito]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1546-1557
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 166:
//   "JitoSubmitter: 6 regional endpoints (parallel submission)"
// ---------------------------------------------------------------------------

/// Jito bundle submission configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JitoConfig {
    /// Percentage of estimated profit to use as tip (0-100).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1555:
    ///   "tip_percent = 50"
    pub tip_percent: u8,

    /// Minimum tip amount in lamports.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1556:
    ///   "min_tip_lamports = 50000"
    pub min_tip_lamports: u64,

    /// Maximum tip amount in lamports (safety cap).
    pub max_tip_lamports: u64,

    /// Regional block-engine endpoints for parallel submission.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1547-1553:
    ///   "mainnet, amsterdam, frankfurt, ny, tokyo, slc"
    pub regional_endpoints: Vec<String>,
}

impl Default for JitoConfig {
    fn default() -> Self {
        Self {
            tip_percent: 50,
            min_tip_lamports: 50_000,
            max_tip_lamports: 50_000_000,
            regional_endpoints: vec![
                "https://mainnet.block-engine.jito.wtf".to_string(),
                "https://amsterdam.mainnet.block-engine.jito.wtf".to_string(),
                "https://frankfurt.mainnet.block-engine.jito.wtf".to_string(),
                "https://ny.mainnet.block-engine.jito.wtf".to_string(),
                "https://tokyo.mainnet.block-engine.jito.wtf".to_string(),
                "https://slc.mainnet.block-engine.jito.wtf".to_string(),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// [scanner]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1568-1574
// ---------------------------------------------------------------------------

/// Scanner settings for obligation monitoring.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ScannerConfig {
    /// Normal scan interval in seconds (overridden by crash risk level).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1569:
    ///   "interval_secs = 60"
    pub interval_secs: u64,

    /// Health factor threshold for the "hot" tier (requires immediate attention).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1572:
    ///   "hot_threshold = 1.05"
    pub hot_threshold: f64,

    /// Health factor threshold for the "warm" tier (pre-position).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1573:
    ///   "warm_threshold = 1.20"
    pub warm_threshold: f64,

    /// Maximum obligations to scan per batch.
    pub batch_size: usize,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            interval_secs: 60,
            hot_threshold: 1.05,
            warm_threshold: 1.20,
            batch_size: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// [strategies]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1469-1505
// ---------------------------------------------------------------------------

/// Per-strategy configuration container.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StrategiesConfig {
    /// Liquidation strategy configuration.
    pub liquidation: LiquidationStrategyConfig,

    /// Backrun strategy configuration.
    pub backrun: BackrunStrategyConfig,

    /// Flash arb strategy configuration.
    pub flash_arb: FlashArbStrategyConfig,

    /// LST arb strategy configuration.
    pub lst_arb: LstArbStrategyConfig,

    /// Copy trade strategy configuration.
    pub copy_trade: CopyTradeStrategyConfig,

    /// Migration snipe strategy configuration.
    pub migration_snipe: MigrationSnipeStrategyConfig,
}

impl Default for StrategiesConfig {
    fn default() -> Self {
        Self {
            liquidation: LiquidationStrategyConfig::default(),
            backrun: BackrunStrategyConfig::default(),
            flash_arb: FlashArbStrategyConfig::default(),
            lst_arb: LstArbStrategyConfig::default(),
            copy_trade: CopyTradeStrategyConfig::default(),
            migration_snipe: MigrationSnipeStrategyConfig::default(),
        }
    }
}

// -- [strategies.liquidation] -----------------------------------------------

/// Liquidation strategy settings.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1469-1475
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LiquidationStrategyConfig {
    /// Whether this strategy is enabled.
    pub enabled: bool,

    /// Protocols to scan for liquidation opportunities.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1475:
    ///   "protocols = ['save', 'kamino', 'marginfi', 'juplend']"
    pub protocols: Vec<String>,

    /// Minimum estimated profit in USD to attempt a liquidation.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1472:
    ///   "min_profit_usd = 0.50"
    pub min_profit_usd: f64,

    /// Priority level (lower = higher priority).
    pub priority: u8,

    /// Cooldown between liquidation attempts on the same obligation (seconds).
    pub cooldown_secs: u64,

    /// Maximum concurrent liquidation executions.
    pub max_concurrent: u32,
}

impl Default for LiquidationStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            protocols: vec![
                "save".to_string(),
                "kamino".to_string(),
                "marginfi".to_string(),
                "juplend".to_string(),
            ],
            min_profit_usd: 0.50,
            priority: 1,
            cooldown_secs: 60,
            max_concurrent: 3,
        }
    }
}

// -- [strategies.backrun] ---------------------------------------------------

/// Backrun strategy settings.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1477-1482
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BackrunStrategyConfig {
    /// Whether this strategy is enabled.
    pub enabled: bool,

    /// Minimum price spread in basis points to attempt a backrun.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1480:
    ///   "min_spread_bps = 3.0"
    pub min_spread_bps: f64,

    /// Flash loan amount in SOL for backrun capital.
    pub flash_amount_sol: f64,

    /// Minimum detected swap size in USD to trigger a backrun check.
    pub min_swap_size_usd: f64,

    /// Priority level.
    pub priority: u8,
}

impl Default for BackrunStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_spread_bps: 3.0,
            flash_amount_sol: 10.0,
            min_swap_size_usd: 1000.0,
            priority: 2,
        }
    }
}

// -- [strategies.flash_arb] -------------------------------------------------

/// Flash arb strategy settings.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1484-1491
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FlashArbStrategyConfig {
    /// Whether this strategy is enabled.
    pub enabled: bool,

    /// Tokens to scan for circular arb (SOL -> Token -> SOL).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1488:
    ///   "tokens_per_scan = 24"
    pub tokens: Vec<String>,

    /// Number of amounts to test per token.
    pub amounts_per_token: u32,

    /// Scan interval in seconds.
    pub scan_interval_secs: u64,

    /// Priority level.
    pub priority: u8,

    /// Skip flash arb if a liquidation opportunity is pending.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1491:
    ///   "skip_if_liquidation_pending = true"
    pub skip_if_liquidation_pending: bool,
}

impl Default for FlashArbStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tokens: Vec::new(),
            amounts_per_token: 3,
            scan_interval_secs: 30,
            priority: 3,
            skip_if_liquidation_pending: true,
        }
    }
}

// -- [strategies.lst_arb] ---------------------------------------------------

/// LST arb strategy settings.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1493-1497
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LstArbStrategyConfig {
    /// Whether this strategy is enabled.
    pub enabled: bool,

    /// LST mints to monitor for rate deviations.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1496:
    ///   "lsts = ['JitoSOL', 'mSOL', 'jupSOL', 'bSOL', 'INF', 'BNSOL', 'hSOL', 'dSOL']"
    pub lsts: Vec<String>,

    /// Scan interval in seconds.
    pub scan_interval_secs: u64,

    /// Priority level.
    pub priority: u8,
}

impl Default for LstArbStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lsts: vec![
                "JitoSOL".to_string(),
                "mSOL".to_string(),
                "jupSOL".to_string(),
                "bSOL".to_string(),
                "INF".to_string(),
                "BNSOL".to_string(),
                "hSOL".to_string(),
                "dSOL".to_string(),
            ],
            scan_interval_secs: 30,
            priority: 4,
        }
    }
}

// -- [strategies.copy_trade] ------------------------------------------------

/// Copy trade strategy settings.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1498-1501
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CopyTradeStrategyConfig {
    /// Whether this strategy is enabled (default: off).
    pub enabled: bool,

    /// Whale wallet pubkeys to monitor.
    pub wallets: Vec<String>,

    /// Priority level.
    pub priority: u8,
}

impl Default for CopyTradeStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wallets: Vec::new(),
            priority: 5,
        }
    }
}

// -- [strategies.migration_snipe] -------------------------------------------

/// Migration snipe strategy settings.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1502-1505
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MigrationSnipeStrategyConfig {
    /// Whether this strategy is enabled (default: off).
    pub enabled: bool,

    /// Priority level.
    pub priority: u8,
}

impl Default for MigrationSnipeStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            priority: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// [flash_loan]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1563-1567
// ---------------------------------------------------------------------------

/// Flash loan provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FlashLoanConfig {
    /// Ordered list of flash loan providers (cheapest first).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1564:
    ///   "provider_order = ['juplend', 'kamino', 'save']"
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 123:
    ///   "Provider order: JupLend(0%) > Kamino(0.001%) > Save(0.05%)"
    pub provider_order: Vec<String>,

    /// Maximum flash loan nesting depth.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1565:
    ///   "max_nesting_depth = 2"
    pub max_nesting: u8,

    /// How often to refresh per-token capacity from on-chain (seconds).
    pub capacity_refresh_secs: u64,
}

impl Default for FlashLoanConfig {
    fn default() -> Self {
        Self {
            provider_order: vec![
                "juplend".to_string(),
                "kamino".to_string(),
                "save".to_string(),
            ],
            max_nesting: 2,
            capacity_refresh_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// [crash_prediction]
// [VERIFIED 2026] crash_prediction_2026.md: Composite crash risk engine
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1575-1581
// ---------------------------------------------------------------------------

/// Crash prediction engine configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CrashPredictionConfig {
    /// Whether the crash prediction engine is enabled.
    pub enabled: bool,

    /// Coinalyze API key for funding rate / OI data.
    ///
    /// [VERIFIED 2026] crash_prediction_2026.md lines 669-671:
    ///   "Coinalyze client -- fetch funding rate, OI, long/short ratio"
    pub coinalyze_api_key: String,

    /// Scan intervals per risk level (Green, Yellow, Orange, Red) in seconds.
    ///
    /// [VERIFIED 2026] crash_prediction_2026.md lines 421-424:
    ///   "Green -> 60s, Yellow -> 30s, Orange -> 15s, Red -> 5s"
    pub scan_intervals: CrashScanIntervals,

    /// Volatility threshold (percentage move) to trigger elevated risk.
    pub volatility_threshold_pct: f64,

    /// Utilization threshold (percentage) for lending protocol risk.
    pub utilization_threshold_pct: f64,

    /// Fear & Greed index threshold to trigger elevated risk.
    pub fear_greed_threshold: u8,
}

impl Default for CrashPredictionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            coinalyze_api_key: String::new(),
            scan_intervals: CrashScanIntervals::default(),
            volatility_threshold_pct: 5.0,
            utilization_threshold_pct: 85.0,
            fear_greed_threshold: 20,
        }
    }
}

/// Scan intervals (in seconds) for each crash risk level.
///
/// [VERIFIED 2026] crash_prediction_2026.md lines 421-424
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CrashScanIntervals {
    pub green: u64,
    pub yellow: u64,
    pub orange: u64,
    pub red: u64,
}

impl Default for CrashScanIntervals {
    fn default() -> Self {
        Self {
            green: 60,
            yellow: 30,
            orange: 15,
            red: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// [dashboard]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1582-1590
// [VERIFIED 2026] plugins_frontend_backend_2026.md Section 1-2: Dashboard design
// [VERIFIED 2026] plugins_frontend_backend_2026.md Section 4: Telegram integration
// ---------------------------------------------------------------------------

/// Dashboard and alerting configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    /// Whether the dashboard HTTP server is enabled.
    pub enabled: bool,

    /// Port to bind the dashboard HTTP server.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1584:
    ///   "bind = '127.0.0.1:8080'"
    pub port: u16,

    /// Telegram bot token for alerts.
    ///
    /// [VERIFIED 2026] plugins_frontend_backend_2026.md Section 4:
    ///   "teloxide -- the most mature Rust Telegram framework"
    pub telegram_token: String,

    /// Telegram chat ID to send alerts to.
    pub telegram_chat_id: String,

    /// Minimum profit in USD to trigger a Telegram alert.
    pub min_profit_alert_usd: f64,

    /// WebSocket flush interval in milliseconds.
    pub websocket_flush_ms: u64,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8080,
            telegram_token: String::new(),
            telegram_chat_id: String::new(),
            min_profit_alert_usd: 1.0,
            websocket_flush_ms: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// [geyser]
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1445: "[grpc] section"
// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2:
//   "Yellowstone gRPC — Sub-50ms from validator memory"
// [VERIFIED 2026] grpc_optimization_2026.md: Connection and message-size config
// ---------------------------------------------------------------------------

/// Yellowstone gRPC (Geyser) connection configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GeyserConfig {
    /// Whether gRPC streaming is enabled.
    pub enabled: bool,

    /// Yellowstone gRPC endpoint URL.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1445-1453
    pub endpoint: String,

    /// Authentication token (x-token header for QuickNode/Triton/Helius).
    pub token: String,
}

impl Default for GeyserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: String::new(),
            token: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// BotConfig methods
// [VERIFIED 2026] code_structure_patterns_2026.md Section 6:
//   "Hot reload with ArcSwap -- reload config from file, validate, then swap"
//   "Layer precedence: Compiled defaults -> Config file -> Env vars -> CLI args"
// ---------------------------------------------------------------------------

/// Error type for configuration loading and validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file '{path}': {source}")]
    ReadError {
        path: String,
        source: std::io::Error,
    },

    #[error("Failed to parse config TOML: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Config validation failed: {0}")]
    ValidationError(String),
}

impl BotConfig {
    /// Load a BotConfig from a TOML file at the given path.
    ///
    /// Missing keys in the TOML file receive compiled defaults via `serde(default)`.
    ///
    /// [VERIFIED 2026] code_structure_patterns_2026.md Section 6:
    ///   "Layer precedence: Compiled defaults -> Config file -> Env vars -> CLI args"
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
            path: path.display().to_string(),
            source: e,
        })?;
        let config: BotConfig = toml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Parse a BotConfig from a TOML string (useful for tests).
    pub fn from_toml(toml_str: &str) -> Result<Self, ConfigError> {
        let config: BotConfig = toml::from_str(toml_str)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate configuration values for logical consistency.
    ///
    /// Called automatically by `load()`. Can also be called after hot-reload
    /// to reject invalid configs before swapping them in.
    ///
    /// [VERIFIED 2026] code_structure_patterns_2026.md Section 6:
    ///   "Validate BEFORE swapping"
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Scanner thresholds must be positive and ordered
        if self.scanner.hot_threshold <= 0.0 {
            return Err(ConfigError::ValidationError(
                "scanner.hot_threshold must be > 0".to_string(),
            ));
        }
        if self.scanner.warm_threshold <= self.scanner.hot_threshold {
            return Err(ConfigError::ValidationError(format!(
                "scanner.warm_threshold ({}) must be > hot_threshold ({})",
                self.scanner.warm_threshold, self.scanner.hot_threshold
            )));
        }

        // Jito tip bounds must be ordered
        if self.jito.min_tip_lamports > self.jito.max_tip_lamports {
            return Err(ConfigError::ValidationError(format!(
                "jito.min_tip_lamports ({}) must be <= max_tip_lamports ({})",
                self.jito.min_tip_lamports, self.jito.max_tip_lamports
            )));
        }

        // Tip percent must be 0-100
        if self.jito.tip_percent > 100 {
            return Err(ConfigError::ValidationError(format!(
                "jito.tip_percent ({}) must be <= 100",
                self.jito.tip_percent
            )));
        }

        // Flash loan nesting must be reasonable
        if self.flash_loan.max_nesting > 4 {
            return Err(ConfigError::ValidationError(format!(
                "flash_loan.max_nesting ({}) must be <= 4",
                self.flash_loan.max_nesting
            )));
        }

        // Liquidation min_profit must be non-negative
        if self.strategies.liquidation.min_profit_usd < 0.0 {
            return Err(ConfigError::ValidationError(
                "strategies.liquidation.min_profit_usd must be >= 0".to_string(),
            ));
        }

        // Dashboard port must be valid
        if self.dashboard.enabled && self.dashboard.port == 0 {
            return Err(ConfigError::ValidationError(
                "dashboard.port must be > 0 when dashboard is enabled".to_string(),
            ));
        }

        Ok(())
    }

    /// Check if any strategy is enabled.
    pub fn has_enabled_strategies(&self) -> bool {
        self.strategies.liquidation.enabled
            || self.strategies.backrun.enabled
            || self.strategies.flash_arb.enabled
            || self.strategies.lst_arb.enabled
            || self.strategies.copy_trade.enabled
            || self.strategies.migration_snipe.enabled
    }

    /// Override config fields from environment variables.
    ///
    /// This implements the env-var layer of the config precedence chain:
    ///   Compiled defaults -> Config file -> **Env vars** -> CLI args
    ///
    /// [VERIFIED 2026] code_structure_patterns_2026.md Section 6:
    ///   "Layer precedence: Compiled defaults -> Config file -> Env vars -> CLI args"
    pub fn apply_env(&mut self) {
        // --- RPC endpoints: prefer RPC_ENDPOINTS over QUICKNODE_URL ---
        // [VERIFIED 2026 port_verification_2026.md CHECK 8: QuickNode confirmed as RPC provider]
        // RPC_ENDPOINTS has the current working endpoint; QUICKNODE_URL may be stale
        if let Ok(urls) = std::env::var("RPC_ENDPOINTS") {
            let endpoints: Vec<String> = urls
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !endpoints.is_empty() {
                self.general.rpc_endpoints = endpoints;
            }
        }
        // Fallback to QUICKNODE_URL if RPC_ENDPOINTS not set
        if self.general.rpc_endpoints.is_empty() {
            if let Ok(urls) = std::env::var("QUICKNODE_URL") {
                let endpoints: Vec<String> = urls
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !endpoints.is_empty() {
                    self.general.rpc_endpoints = endpoints;
                }
            }
        }

        // --- Jupiter API key ---
        if let Ok(key) = std::env::var("JUPITER_API_KEY") {
            if !key.is_empty() {
                self.jupiter.api_key = key;
            }
        }

        // --- Yellowstone gRPC ---
        if let Ok(endpoint) = std::env::var("YELLOWSTONE_GRPC_ENDPOINT") {
            if !endpoint.is_empty() {
                self.geyser.endpoint = endpoint;
            }
        }
        if let Ok(token) = std::env::var("YELLOWSTONE_GRPC_TOKEN") {
            if !token.is_empty() {
                self.geyser.token = token;
            }
        }

        // --- Telegram alerts ---
        if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
            if !token.is_empty() {
                self.dashboard.telegram_token = token;
            }
        }
        if let Ok(chat_id) = std::env::var("TELEGRAM_CHAT_ID") {
            if !chat_id.is_empty() {
                self.dashboard.telegram_chat_id = chat_id;
            }
        }

        // --- Wallet path override ---
        if let Ok(path) = std::env::var("WALLET_KEYPAIR_PATH") {
            if !path.is_empty() {
                self.general.wallet_path = path;
            }
        }

        // --- Dry run override ---
        if let Ok(val) = std::env::var("DRY_RUN") {
            match val.to_lowercase().as_str() {
                "true" | "1" | "yes" => self.general.dry_run = true,
                "false" | "0" | "no" => self.general.dry_run = false,
                _ => {}
            }
        }

        // --- Log level override ---
        if let Ok(level) = std::env::var("RUST_LOG") {
            if !level.is_empty() {
                self.general.log_level = level;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = BotConfig::default();
        config.validate().expect("Default config should be valid");
    }

    #[test]
    fn default_config_has_sane_values() {
        let config = BotConfig::default();
        assert_eq!(config.general.log_level, "info");
        assert!(!config.general.dry_run);
        assert_eq!(config.jito.tip_percent, 50);
        assert_eq!(config.jito.min_tip_lamports, 50_000);
        assert_eq!(config.jito.regional_endpoints.len(), 6);
        assert_eq!(config.scanner.hot_threshold, 1.05);
        assert_eq!(config.scanner.warm_threshold, 1.20);
        assert!(config.strategies.liquidation.enabled);
        assert!(!config.strategies.copy_trade.enabled);
        assert!(!config.strategies.migration_snipe.enabled);
        assert_eq!(config.flash_loan.max_nesting, 2);
        assert_eq!(config.dashboard.port, 8080);
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
[general]
log_level = "debug"
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert_eq!(config.general.log_level, "debug");
        // Everything else should have defaults
        assert_eq!(config.jito.tip_percent, 50);
        assert!(config.strategies.liquidation.enabled);
    }

    #[test]
    fn parse_full_strategies() {
        let toml = r#"
[strategies.liquidation]
enabled = true
protocols = ["save", "kamino"]
min_profit_usd = 1.0
priority = 1
cooldown_secs = 30
max_concurrent = 5

[strategies.backrun]
enabled = false
min_spread_bps = 5.0
flash_amount_sol = 20.0

[strategies.flash_arb]
enabled = true
tokens = ["USDC", "USDT", "JitoSOL"]
amounts_per_token = 5

[strategies.lst_arb]
enabled = true
lsts = ["JitoSOL", "mSOL"]

[strategies.copy_trade]
enabled = false
wallets = []

[strategies.migration_snipe]
enabled = false
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert_eq!(config.strategies.liquidation.protocols.len(), 2);
        assert!((config.strategies.liquidation.min_profit_usd - 1.0).abs() < f64::EPSILON);
        assert!(!config.strategies.backrun.enabled);
        assert!((config.strategies.backrun.min_spread_bps - 5.0).abs() < f64::EPSILON);
        assert_eq!(config.strategies.flash_arb.tokens.len(), 3);
        assert_eq!(config.strategies.lst_arb.lsts.len(), 2);
    }

    #[test]
    fn parse_jito_config() {
        let toml = r#"
[jito]
tip_percent = 60
min_tip_lamports = 100000
max_tip_lamports = 10000000
regional_endpoints = [
    "https://mainnet.block-engine.jito.wtf",
    "https://amsterdam.mainnet.block-engine.jito.wtf",
]
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert_eq!(config.jito.tip_percent, 60);
        assert_eq!(config.jito.min_tip_lamports, 100_000);
        assert_eq!(config.jito.regional_endpoints.len(), 2);
    }

    #[test]
    fn parse_flash_loan_config() {
        let toml = r#"
[flash_loan]
provider_order = ["juplend", "kamino", "save"]
max_nesting = 2
capacity_refresh_secs = 120
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert_eq!(config.flash_loan.provider_order.len(), 3);
        assert_eq!(config.flash_loan.provider_order[0], "juplend");
        assert_eq!(config.flash_loan.max_nesting, 2);
    }

    #[test]
    fn parse_crash_prediction() {
        let toml = r#"
[crash_prediction]
enabled = true
coinalyze_api_key = "test-key-123"
volatility_threshold_pct = 3.0
fear_greed_threshold = 15

[crash_prediction.scan_intervals]
green = 60
yellow = 30
orange = 15
red = 5
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert!(config.crash_prediction.enabled);
        assert_eq!(config.crash_prediction.coinalyze_api_key, "test-key-123");
        assert!((config.crash_prediction.volatility_threshold_pct - 3.0).abs() < f64::EPSILON);
        assert_eq!(config.crash_prediction.scan_intervals.red, 5);
    }

    #[test]
    fn parse_dashboard_config() {
        let toml = r#"
[dashboard]
enabled = true
port = 9090
telegram_token = "bot123:ABC"
telegram_chat_id = "-100123456"
min_profit_alert_usd = 2.0
websocket_flush_ms = 200
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert_eq!(config.dashboard.port, 9090);
        assert_eq!(config.dashboard.telegram_token, "bot123:ABC");
        assert_eq!(config.dashboard.telegram_chat_id, "-100123456");
    }

    // -----------------------------------------------------------------------
    // Validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn validation_rejects_bad_hot_threshold() {
        let toml = r#"
[scanner]
hot_threshold = -1.0
warm_threshold = 1.20
"#;
        let result = BotConfig::from_toml(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("hot_threshold"));
    }

    #[test]
    fn validation_rejects_warm_below_hot() {
        let toml = r#"
[scanner]
hot_threshold = 1.20
warm_threshold = 1.05
"#;
        let result = BotConfig::from_toml(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("warm_threshold"));
    }

    #[test]
    fn validation_rejects_inverted_tip_bounds() {
        let toml = r#"
[jito]
min_tip_lamports = 1000000
max_tip_lamports = 1000
"#;
        let result = BotConfig::from_toml(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("min_tip_lamports"));
    }

    #[test]
    fn validation_rejects_tip_percent_over_100() {
        let toml = r#"
[jito]
tip_percent = 150
"#;
        let result = BotConfig::from_toml(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("tip_percent"));
    }

    #[test]
    fn validation_rejects_excessive_nesting() {
        let toml = r#"
[flash_loan]
max_nesting = 10
"#;
        let result = BotConfig::from_toml(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_nesting"));
    }

    #[test]
    fn validation_rejects_zero_dashboard_port() {
        let toml = r#"
[dashboard]
enabled = true
port = 0
"#;
        let result = BotConfig::from_toml(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("port"));
    }

    #[test]
    fn validation_allows_disabled_dashboard_zero_port() {
        let toml = r#"
[dashboard]
enabled = false
port = 0
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert!(!config.dashboard.enabled);
    }

    #[test]
    fn has_enabled_strategies_detects_any() {
        let config = BotConfig::default();
        assert!(config.has_enabled_strategies());
    }

    #[test]
    fn has_enabled_strategies_detects_none() {
        let toml = r#"
[strategies.liquidation]
enabled = false
[strategies.backrun]
enabled = false
[strategies.flash_arb]
enabled = false
[strategies.lst_arb]
enabled = false
[strategies.copy_trade]
enabled = false
[strategies.migration_snipe]
enabled = false
"#;
        let config = BotConfig::from_toml(toml).unwrap();
        assert!(!config.has_enabled_strategies());
    }

    #[test]
    fn load_nonexistent_file_returns_error() {
        let result = BotConfig::load("/tmp/nonexistent_predator_config_12345.toml");
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::ReadError { path, .. } => {
                assert!(path.contains("nonexistent"));
            }
            other => panic!("Expected ReadError, got: {:?}", other),
        }
    }

    #[test]
    fn default_lst_arb_has_8_lsts() {
        let config = BotConfig::default();
        assert_eq!(config.strategies.lst_arb.lsts.len(), 8);
        assert!(config.strategies.lst_arb.lsts.contains(&"JitoSOL".to_string()));
        assert!(config.strategies.lst_arb.lsts.contains(&"mSOL".to_string()));
        assert!(config.strategies.lst_arb.lsts.contains(&"dSOL".to_string()));
    }

    #[test]
    fn default_flash_loan_provider_order() {
        let config = BotConfig::default();
        assert_eq!(config.flash_loan.provider_order[0], "juplend");
        assert_eq!(config.flash_loan.provider_order[1], "kamino");
        assert_eq!(config.flash_loan.provider_order[2], "save");
    }

    #[test]
    fn default_crash_scan_intervals() {
        let config = BotConfig::default();
        assert_eq!(config.crash_prediction.scan_intervals.green, 60);
        assert_eq!(config.crash_prediction.scan_intervals.yellow, 30);
        assert_eq!(config.crash_prediction.scan_intervals.orange, 15);
        assert_eq!(config.crash_prediction.scan_intervals.red, 5);
    }
}
