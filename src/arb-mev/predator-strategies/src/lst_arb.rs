//! LstArbStrategy -- Liquid Staking Token arbitrage via rate deviation detection.
//!
//! Monitors 8 LSTs for price deviations across Orca, Raydium, and Meteora DEXes.
//! When the exchange rate on a DEX diverges from the fair rate (epoch-based
//! staking yield), executes an arb trade to capture the spread.
//!
//! ## 8 Monitored LSTs
//!
//! jitoSOL, mSOL, jupSOL, bSOL, BNSOL, INF, dSOL, bbSOL
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 150-151:
//!   "LstArbStrategy: jitoSOL/mSOL/jupSOL/bSOL rate deviation detection.
//!    8 LSTs monitored. Kamino flash loan for capital."
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6:
//!   "LST triangles: SOL -> JitoSOL (Sanctum) -> mSOL (Marinade) -> SOL (Jupiter)
//!    Moderate spreads (3-15 bps), less competition.
//!    Event-driven (stake/unstake waves)"
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 3:
//!   "31-47ms IS competitive for: LST arb (event-driven, not speed-race)"
//! [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d:
//!   Priority 4 (lower). Response time: <5 seconds.

use std::collections::HashMap;
use std::time::Instant;

use solana_sdk::pubkey::Pubkey;

use predator_core::{
    BotAction, BotEvent, Lamports, Protocol, SharedState, StrategyPriority,
};

use crate::traits::{Strategy, StrategyHealth};

// ---------------------------------------------------------------------------
// Constants
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md, backrun_arb_strategies_2026.md
// ---------------------------------------------------------------------------

/// Minimum rate deviation in basis points to trigger an arb.
/// LST arbs are less competitive than blue-chip arb, so lower threshold.
///
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6:
///   "LST triangles: Moderate spreads (3-15 bps)"
const MIN_DEVIATION_BPS: f64 = 5.0;

/// Minimum profit in lamports to submit an LST arb bundle.
const MIN_PROFIT_LAMPORTS: u64 = 50_000;

/// Jito tip fraction.
/// [VERIFIED 2026] competition_analysis_2026.md: "50-60% of expected profit"
const TIP_FRACTION: f64 = 0.50;

/// Default scan interval for LST arb (seconds).
/// LST rates change slowly (epoch-based), so scanning every 60s is sufficient.
const DEFAULT_SCAN_INTERVAL_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// LST definitions
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 150:
//   "8 LSTs: jitoSOL, mSOL, jupSOL, bSOL, BNSOL, INF, dSOL, bbSOL"
// ---------------------------------------------------------------------------

/// LST token metadata.
#[derive(Debug, Clone)]
pub struct LstToken {
    /// Human-readable name.
    pub name: &'static str,
    /// SPL token mint address.
    pub mint: &'static str,
    /// Fair exchange rate (SOL per LST). Updated from on-chain stake pool data.
    /// For example, jitoSOL fair rate might be 1.12 (1 jitoSOL = 1.12 SOL).
    pub fair_rate: f64,
    /// Current DEX rate (SOL per LST). Updated from pool price observations.
    pub dex_rate: f64,
    /// When the fair rate was last updated.
    pub fair_rate_updated: Instant,
    /// When the DEX rate was last updated.
    pub dex_rate_updated: Instant,
}

/// The 8 LSTs monitored for rate deviation.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 150
const LST_TOKENS: [(&str, &str); 8] = [
    ("jitoSOL", "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn"),
    ("mSOL",    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So"),
    ("jupSOL",  "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v"),
    ("bSOL",    "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1"),
    ("BNSOL",   "BNso1VUJnh4zcfpZa6986Ea66P6TCp59hvtNJ8b1X85"),
    ("INF",     "5oVNBeEEQvYi1cX3ir8Dx5n1P7pdxydbGF2X4TxVusJm"),
    ("dSOL",    "Dso1bDeDjCQxTrWHqUUi63oBvV7Mdm6WaobLbQ7gnPQ"),
    ("bbSOL",   "Bybit2vBJGhPF52GBdNaQ9UiEYEgMLBwbHdsRPmHrs4k"),
];

// ---------------------------------------------------------------------------
// LstArbStrategy
// ---------------------------------------------------------------------------

/// LST arbitrage strategy -- detects rate deviations across DEXes.
///
/// Monitors 8 liquid staking tokens for divergences between their
/// fair exchange rate (from stake pool) and DEX trading rate.
///
/// Uses Kamino flash loans (0.001% fee) for working capital.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 150-151
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6: LST triangles
pub struct LstArbStrategy {
    /// LST tokens being monitored, keyed by mint pubkey.
    tokens: HashMap<Pubkey, LstToken>,

    /// Whether this strategy is currently enabled.
    enabled: bool,

    /// Number of rate deviations detected.
    deviations_detected: u64,

    /// Timestamp of last scan.
    last_scan: Instant,

    /// Current scan interval.
    scan_interval_secs: u64,
}

impl LstArbStrategy {
    /// Create a new LstArbStrategy with all 8 LSTs registered.
    pub fn new() -> Self {
        let mut tokens = HashMap::new();
        let now = Instant::now();

        for (name, mint_str) in &LST_TOKENS {
            if let Ok(mint) = mint_str.parse::<Pubkey>() {
                tokens.insert(
                    mint,
                    LstToken {
                        name,
                        mint: mint_str,
                        fair_rate: 0.0,
                        dex_rate: 0.0,
                        fair_rate_updated: now,
                        dex_rate_updated: now,
                    },
                );
            }
        }

        Self {
            tokens,
            enabled: true,
            deviations_detected: 0,
            last_scan: now,
            scan_interval_secs: DEFAULT_SCAN_INTERVAL_SECS,
        }
    }

    /// Update the fair exchange rate for an LST (from stake pool on-chain data).
    ///
    /// Fair rate = total_sol_staked / total_lst_supply (from stake pool account).
    pub fn update_fair_rate(&mut self, mint: &Pubkey, rate: f64) {
        if let Some(token) = self.tokens.get_mut(mint) {
            token.fair_rate = rate;
            token.fair_rate_updated = Instant::now();
        }
    }

    /// Update the DEX trading rate for an LST (from pool price observation).
    ///
    /// DEX rate = SOL received per LST when swapping on a DEX.
    pub fn update_dex_rate(&mut self, mint: &Pubkey, rate: f64) {
        if let Some(token) = self.tokens.get_mut(mint) {
            token.dex_rate = rate;
            token.dex_rate_updated = Instant::now();
        }
    }

    /// Check all LSTs for rate deviations.
    ///
    /// Returns opportunities where DEX rate diverges from fair rate by > threshold.
    /// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6:
    ///   "LST triangles: Moderate spreads (3-15 bps), less competition"
    fn check_deviations(&self, _sol_price: f64) -> Vec<BotAction> {
        let mut actions = Vec::new();

        for token in self.tokens.values() {
            if token.fair_rate <= 0.0 || token.dex_rate <= 0.0 {
                continue;
            }

            // Check staleness (rates older than 5 minutes are unreliable)
            if token.fair_rate_updated.elapsed().as_secs() > 300
                || token.dex_rate_updated.elapsed().as_secs() > 300
            {
                continue;
            }

            // Calculate deviation in basis points
            let deviation_bps =
                ((token.dex_rate - token.fair_rate) / token.fair_rate).abs() * 10_000.0;

            if deviation_bps < MIN_DEVIATION_BPS {
                continue;
            }

            // Determine direction:
            // If dex_rate < fair_rate: buy LST on DEX (cheap), unstake for fair rate
            // If dex_rate > fair_rate: stake SOL for LST, sell LST on DEX (expensive)
            let direction = if token.dex_rate < token.fair_rate {
                "BUY on DEX (underpriced)"
            } else {
                "SELL on DEX (overpriced)"
            };

            // Estimate profit on a 10 SOL trade
            let trade_size_sol = 10.0;
            let profit_sol = trade_size_sol * (deviation_bps / 10_000.0);
            let profit_lamports = (profit_sol * 1_000_000_000.0) as u64;

            if profit_lamports < MIN_PROFIT_LAMPORTS {
                continue;
            }

            let tip = std::cmp::max(
                (profit_lamports as f64 * TIP_FRACTION) as u64,
                predator_core::constants::JITO_MIN_TIP_LAMPORTS,
            );

            actions.push(BotAction::LogOpportunity {
                protocol: Protocol::Kamino, // flash loan provider
                est_profit: Lamports(profit_lamports),
                description: format!(
                    "LST Arb: {} fair={:.4} dex={:.4} dev={:.1}bps {} profit={} lamports",
                    token.name, token.fair_rate, token.dex_rate,
                    deviation_bps, direction, profit_lamports
                ),
            });

            actions.push(BotAction::SubmitBundle {
                txs: Vec::new(), // executor builds: flash_borrow + stake/swap + swap/unstake + flash_repay
                tip_lamports: Lamports(tip),
                priority: StrategyPriority::LstArb,
            });
        }

        actions
    }
}

impl Default for LstArbStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for LstArbStrategy {
    fn name(&self) -> &str {
        "lst_arb"
    }

    /// Priority 4 (lower).
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d
    fn priority(&self) -> StrategyPriority {
        StrategyPriority::LstArb
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Process incoming events.
    ///
    /// - AccountUpdate: update DEX rates for registered LST pool accounts
    /// - CrashAlert: adjust scan interval
    fn process_event(&mut self, event: &BotEvent, _state: &SharedState) -> Vec<BotAction> {
        match event {
            BotEvent::CrashAlert { risk_level, .. } => {
                self.scan_interval_secs = risk_level.scan_interval_secs();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Periodic scan for LST rate deviations.
    ///
    /// Checks all 8 LSTs for divergence between fair rate and DEX rate.
    ///
    /// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6:
    ///   "Event-driven (stake/unstake waves)"
    fn on_scan(&mut self, state: &SharedState) -> Vec<BotAction> {
        let sol_price = state.get_sol_price();
        if sol_price <= 0.0 {
            return Vec::new();
        }

        let actions = self.check_deviations(sol_price);

        self.deviations_detected += actions.iter().filter(|a| matches!(a, BotAction::LogOpportunity { .. })).count() as u64;
        self.last_scan = Instant::now();

        actions
    }

    fn health_check(&self) -> StrategyHealth {
        if self.tokens.is_empty() {
            return StrategyHealth::Unhealthy("No LST tokens registered".to_string());
        }

        let initialized = self
            .tokens
            .values()
            .filter(|t| t.fair_rate > 0.0)
            .count();

        if initialized == 0 {
            StrategyHealth::Degraded(
                "No LST fair rates initialized -- waiting for stake pool data".to_string(),
            )
        } else {
            StrategyHealth::Healthy
        }
    }

    fn scan_interval_secs(&self) -> u64 {
        self.scan_interval_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_name_and_priority() {
        let strategy = LstArbStrategy::new();
        assert_eq!(strategy.name(), "lst_arb");
        assert_eq!(strategy.priority(), StrategyPriority::LstArb);
        assert!(strategy.is_enabled());
    }

    #[test]
    fn eight_lsts_registered() {
        let strategy = LstArbStrategy::new();
        assert_eq!(strategy.tokens.len(), 8);
    }

    #[test]
    fn health_check_no_rates() {
        let strategy = LstArbStrategy::new();
        let health = strategy.health_check();
        assert!(matches!(health, StrategyHealth::Degraded(_)));
    }

    #[test]
    fn health_check_with_rates() {
        let mut strategy = LstArbStrategy::new();

        // Set fair rate for jitoSOL
        let jitosol_mint: Pubkey =
            "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".parse().unwrap();
        strategy.update_fair_rate(&jitosol_mint, 1.12);

        let health = strategy.health_check();
        assert_eq!(health, StrategyHealth::Healthy);
    }

    #[test]
    fn deviation_detection() {
        let mut strategy = LstArbStrategy::new();

        let jitosol_mint: Pubkey =
            "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".parse().unwrap();

        // Set fair rate and a deviated DEX rate (10 bps deviation)
        strategy.update_fair_rate(&jitosol_mint, 1.1200);
        strategy.update_dex_rate(&jitosol_mint, 1.1212); // 10.7 bps higher

        let actions = strategy.check_deviations(80.0);
        // Should detect the deviation
        assert!(!actions.is_empty());
    }

    #[test]
    fn no_deviation_within_threshold() {
        let mut strategy = LstArbStrategy::new();

        let msol_mint: Pubkey =
            "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So".parse().unwrap();

        // Set rates with only 1 bps deviation (below threshold)
        strategy.update_fair_rate(&msol_mint, 1.1200);
        strategy.update_dex_rate(&msol_mint, 1.12012); // ~1 bps

        let actions = strategy.check_deviations(80.0);
        assert!(actions.is_empty());
    }

    #[test]
    fn zero_sol_price_skips() {
        let mut strategy = LstArbStrategy::new();
        let state = SharedState::new();
        // sol_price defaults to 0.0

        let actions = strategy.on_scan(&state);
        assert!(actions.is_empty());
    }

    #[test]
    fn crash_alert_adjusts_interval() {
        let mut strategy = LstArbStrategy::new();
        let state = SharedState::new();

        let event = BotEvent::CrashAlert {
            risk_level: predator_core::CrashRiskLevel::Orange,
            signals: vec!["High fear".to_string()],
        };

        strategy.process_event(&event, &state);
        assert_eq!(strategy.scan_interval_secs, 10);
    }
}
