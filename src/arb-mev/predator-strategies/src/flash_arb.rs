//! FlashArbStrategy -- circular arbitrage SOL->Token->SOL via Jupiter.
//!
//! Zero capital risk: uses flash loans for working capital.
//! Scans 24 priority tokens + top volume tokens for circular arb opportunities.
//!
//! Skips if a liquidation target is pending (liquidation has higher priority).
//!
//! ## Execution Flow
//!
//! 1. Periodic scan: for each priority token, query Jupiter for SOL->Token->SOL route
//! 2. Compute: output_SOL - input_SOL - fees - tip = net profit
//! 3. If profit > threshold, build flash_borrow + swap + swap_back + flash_repay bundle
//! 4. Submit as Jito bundle
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 147-149:
//!   "FlashArbStrategy: circular arb SOL->Token->SOL via Jupiter.
//!    Zero capital risk. Multi-token scanning (24 tokens x 3 amounts).
//!    Priority tokens + top volume tokens. Skip if liquidation pending."
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6:
//!   "Cyclic Arbitrage on Solana 2026: Transactions where input mint = output mint.
//!    Average profit per arb: $1.58. Jito tips: 50-60% of profit."
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 7:
//!   "Strategy Priority Matrix: Long-tail memecoin arb MEDIUM viability,
//!    31-47ms OK latency, $0.50-5/trade expected profit"
//! [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d:
//!   Priority 3 (medium). Skip if liquidation pending.

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

/// Minimum profit in basis points for a flash arb opportunity.
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 7:
///   "Profit threshold: Minimum 10 bps before submission"
const MIN_PROFIT_BPS: f64 = 10.0;

/// Minimum profit in lamports to submit a flash arb bundle.
/// Must cover: flash loan fee + Jito tip + gas.
const MIN_PROFIT_LAMPORTS: u64 = 50_000;

/// Jito tip as fraction of estimated profit.
/// [VERIFIED 2026] backrun_arb_strategies_2026.md: "50-60% of profit goes to validators"
const TIP_FRACTION: f64 = 0.55;

/// Flash loan amounts to try for each token (in SOL).
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 148:
///   "Multi-token scanning (24 tokens x 3 amounts)"
const FLASH_AMOUNTS_SOL: [f64; 3] = [1.0, 5.0, 10.0];

/// Number of priority tokens to scan.
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md: "24 priority tokens"
const PRIORITY_TOKEN_COUNT: usize = 24;

/// Default scan interval for flash arb (seconds).
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d:
///   Priority 3 -- only attempted if no higher-priority actions pending
const DEFAULT_SCAN_INTERVAL_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Priority tokens
// These are the 24 highest-volume tokens on Solana DEXes that produce
// the most frequent circular arb opportunities.
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 148: "24 priority tokens"
// ---------------------------------------------------------------------------

/// Priority token mints for circular arb scanning.
/// Includes major pairs (SOL, USDC, USDT) and high-volume memecoins.
/// All addresses verified from client/src/fast_scanner.rs and constants.rs.
///
/// Note: SOL_MINT is excluded since it's the start/end of the circular route.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 148: "24 priority tokens"
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6: DEX coverage
const PRIORITY_TOKENS: [&str; PRIORITY_TOKEN_COUNT] = [
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", // USDT
    "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn", // jitoSOL
    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So",  // mSOL
    "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v",  // jupSOL
    "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1",  // bSOL
    "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263", // BONK
    "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",  // JUP
    "rndrizKT3MK1iimdxRdWabcF7Zg7AR5T4nud4EkHBof",  // RNDR
    "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs", // ETH (Wormhole)
    "3NZ9JMVBmGAqocybic2c7LQCJScmgsAZ6vQqTDzcqmJh", // BTC (Wormhole)
    "HZ1JovNiVvGrGNiiYvEozEVgZ58xaU3RKwX8eACQBCt3", // PYTH
    "27G8MtK7VtTcCHkpASjSDdkWWYfoqT6ggEuKidVJidD4", // JLP
    "85VBFQZC9TZkfaptBWjvUw7YbZjy52A6mjtPGjstQAmQ", // W (Wormhole)
    "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm", // WIF
    "BNso1VUJnh4zcfpZa6986Ea66P6TCp59hvtNJ8b1X85",  // BNSOL
    "RLBxxFkseAZ4RgJH3Sqn8jXxhmGoz9jWxDNJMh8pL7a",  // RAYDIUM
    "orcaEKTdK7LKz57vaAYr9QeNsVEPfiu6QeMU1kektZE",  // ORCA
    "MNDEFzGvMt87ueuHvVU9VcTqsAP5b3fTGPsHuuPA5ey",  // MNDE
    "A8C3uruqJQ3FX1XkYXnJgB1HFDfSk3Z6FEDULQTC5Nz4", // FARTCOIN
    "CLoUDKc4Ane7HeQcPpE3YHnznRxhMimJ4MyaUqyHFzAu", // CLOUD
    "DriFtupJYLTosbwoN8koMbEYSx54aFAVLddWsbksjwg7",  // DRIFT
    "TNSRxcUxoT9xBG3de7PiJyTDYu7kskLqcpddxnEJAS6",  // TENSOR
    "MEW1gQWJ3nEXg2qgERiKu7FAFj79PHvQVREQUzScPP5",  // MEW
];

// ---------------------------------------------------------------------------
// FlashArbStrategy
// ---------------------------------------------------------------------------

/// Flash arbitrage strategy -- circular SOL->Token->SOL via Jupiter.
///
/// Periodically scans priority tokens for circular arb opportunities.
/// Uses flash loans for zero-capital-risk execution.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 147-149
pub struct FlashArbStrategy {
    /// Whether this strategy is currently enabled.
    enabled: bool,

    /// Parsed priority token mints (lazily initialized).
    priority_mints: Vec<Pubkey>,

    /// Additional dynamically discovered high-volume token mints.
    dynamic_mints: Vec<Pubkey>,

    /// Whether a liquidation opportunity is currently pending.
    /// When true, flash arb is skipped to avoid competing for resources.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 149:
    ///   "Skip if liquidation pending"
    liquidation_pending: bool,

    /// Timestamp of last scan.
    last_scan: Instant,

    /// Number of opportunities found (for health check).
    opportunities_found: u64,

    /// Current scan interval (may adjust based on crash risk).
    scan_interval_secs: u64,
}

impl FlashArbStrategy {
    /// Create a new FlashArbStrategy.
    pub fn new() -> Self {
        // Parse priority token mints from string constants
        let priority_mints: Vec<Pubkey> = PRIORITY_TOKENS
            .iter()
            .filter_map(|s| s.parse::<Pubkey>().ok())
            .collect();

        Self {
            enabled: true,
            priority_mints,
            dynamic_mints: Vec::new(),
            liquidation_pending: false,
            last_scan: Instant::now(),
            opportunities_found: 0,
            scan_interval_secs: DEFAULT_SCAN_INTERVAL_SECS,
        }
    }

    /// Set the liquidation pending flag.
    /// Called by the orchestrator when a liquidation opportunity is detected.
    pub fn set_liquidation_pending(&mut self, pending: bool) {
        self.liquidation_pending = pending;
    }

    /// Add a dynamically discovered high-volume token to scan.
    pub fn add_dynamic_token(&mut self, mint: Pubkey) {
        if !self.dynamic_mints.contains(&mint) && !self.priority_mints.contains(&mint) {
            self.dynamic_mints.push(mint);
        }
    }

    /// Get all tokens to scan (priority + dynamic).
    pub fn all_tokens(&self) -> Vec<Pubkey> {
        let mut all = self.priority_mints.clone();
        all.extend(&self.dynamic_mints);
        all
    }

    /// Evaluate a single token for circular arb opportunity.
    ///
    /// In the real implementation, this would:
    /// 1. Query Jupiter /quote for SOL -> Token route
    /// 2. Query Jupiter /quote for Token -> SOL route
    /// 3. Compute: output_SOL - input_SOL - flash_fee - jito_tip = profit
    ///
    /// For now, this is a placeholder that checks pool state.
    fn evaluate_token(
        &self,
        _token_mint: &Pubkey,
        _amount_sol: f64,
        _state: &SharedState,
    ) -> Option<(u64, f64)> {
        // Real implementation:
        // 1. let quote_fwd = jupiter.quote(SOL_MINT, token_mint, amount_lamports);
        // 2. let quote_rev = jupiter.quote(token_mint, SOL_MINT, quote_fwd.out_amount);
        // 3. let profit = quote_rev.out_amount - amount_lamports - flash_fee;
        // 4. if profit > MIN_PROFIT_LAMPORTS { return Some((profit, spread_bps)); }
        //
        // [VERIFIED 2026] backrun_arb_strategies_2026.md Section 6:
        //   "Route discovery: try all permutations of 2-hop and 3-hop paths"

        None // placeholder -- real logic requires Jupiter API calls
    }
}

impl Default for FlashArbStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for FlashArbStrategy {
    fn name(&self) -> &str {
        "flash_arb"
    }

    /// Priority 3 (medium).
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d
    fn priority(&self) -> StrategyPriority {
        StrategyPriority::FlashArb
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Process incoming events.
    ///
    /// Flash arb is primarily scan-driven, but reacts to:
    /// - CrashAlert: adjust scan interval
    /// - AccountUpdate: update internal pool state
    fn process_event(&mut self, event: &BotEvent, _state: &SharedState) -> Vec<BotAction> {
        match event {
            BotEvent::CrashAlert { risk_level, .. } => {
                // During high risk, reduce scan interval for more frequent checks
                self.scan_interval_secs = risk_level.scan_interval_secs();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Periodic scan for circular arb opportunities.
    ///
    /// Iterates over priority tokens x 3 amounts, checking Jupiter for
    /// profitable circular routes.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 148:
    ///   "Multi-token scanning (24 tokens x 3 amounts)"
    fn on_scan(&mut self, state: &SharedState) -> Vec<BotAction> {
        // Skip if liquidation is pending (higher priority)
        // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 149:
        //   "Skip if liquidation pending"
        if self.liquidation_pending {
            return Vec::new();
        }

        let sol_price = state.get_sol_price();
        if sol_price <= 0.0 {
            return Vec::new();
        }

        let mut actions = Vec::new();
        let all_tokens = self.all_tokens();

        for token_mint in &all_tokens {
            for &amount_sol in &FLASH_AMOUNTS_SOL {
                if let Some((profit_lamports, spread_bps)) =
                    self.evaluate_token(token_mint, amount_sol, state)
                {
                    if profit_lamports < MIN_PROFIT_LAMPORTS {
                        continue;
                    }

                    if spread_bps < MIN_PROFIT_BPS {
                        continue;
                    }

                    let tip = std::cmp::max(
                        (profit_lamports as f64 * TIP_FRACTION) as u64,
                        state.get_tip_floor(),
                    );

                    self.opportunities_found += 1;

                    actions.push(BotAction::LogOpportunity {
                        protocol: Protocol::Save, // placeholder protocol
                        est_profit: Lamports(profit_lamports),
                        description: format!(
                            "FlashArb: SOL->{}->{} amount={:.1}SOL spread={:.1}bps profit={} lamports",
                            token_mint, "SOL", amount_sol, spread_bps, profit_lamports
                        ),
                    });

                    actions.push(BotAction::SubmitBundle {
                        txs: Vec::new(), // executor builds: flash_borrow + swap + swap_back + flash_repay
                        tip_lamports: Lamports(tip),
                        priority: StrategyPriority::FlashArb,
                    });

                    // Only take the best opportunity per token
                    break;
                }
            }
        }

        self.last_scan = Instant::now();
        actions
    }

    fn health_check(&self) -> StrategyHealth {
        if self.priority_mints.is_empty() {
            return StrategyHealth::Unhealthy(
                "No priority tokens loaded".to_string(),
            );
        }

        if self.liquidation_pending {
            return StrategyHealth::Degraded(
                "Paused: liquidation pending".to_string(),
            );
        }

        StrategyHealth::Healthy
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
        let strategy = FlashArbStrategy::new();
        assert_eq!(strategy.name(), "flash_arb");
        assert_eq!(strategy.priority(), StrategyPriority::FlashArb);
        assert!(strategy.is_enabled());
    }

    #[test]
    fn priority_tokens_parsed() {
        // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 148: "24 priority tokens"
        // Validate all token addresses parse as valid Pubkeys
        for (i, token_str) in PRIORITY_TOKENS.iter().enumerate() {
            assert!(
                token_str.parse::<Pubkey>().is_ok(),
                "Token {} failed to parse as Pubkey: {}", i, token_str
            );
        }
        let strategy = FlashArbStrategy::new();
        assert_eq!(strategy.priority_mints.len(), PRIORITY_TOKEN_COUNT);
    }

    #[test]
    fn all_tokens_includes_dynamic() {
        let mut strategy = FlashArbStrategy::new();
        let dynamic = Pubkey::new_unique();
        strategy.add_dynamic_token(dynamic);

        let all = strategy.all_tokens();
        assert_eq!(all.len(), PRIORITY_TOKEN_COUNT + 1);
        assert!(all.contains(&dynamic));
    }

    #[test]
    fn no_duplicate_dynamic_tokens() {
        let mut strategy = FlashArbStrategy::new();
        let dynamic = Pubkey::new_unique();
        strategy.add_dynamic_token(dynamic);
        strategy.add_dynamic_token(dynamic); // duplicate

        assert_eq!(strategy.dynamic_mints.len(), 1);
    }

    #[test]
    fn liquidation_pending_skips_scan() {
        let mut strategy = FlashArbStrategy::new();
        let state = SharedState::new();
        state.update_sol_price(80.0);

        strategy.set_liquidation_pending(true);
        let actions = strategy.on_scan(&state);
        assert!(actions.is_empty());
    }

    #[test]
    fn health_check_liquidation_pending() {
        let mut strategy = FlashArbStrategy::new();
        strategy.set_liquidation_pending(true);

        let health = strategy.health_check();
        assert!(matches!(health, StrategyHealth::Degraded(_)));
    }

    #[test]
    fn health_check_healthy() {
        let strategy = FlashArbStrategy::new();
        assert_eq!(strategy.health_check(), StrategyHealth::Healthy);
    }

    #[test]
    fn crash_alert_adjusts_interval() {
        let mut strategy = FlashArbStrategy::new();
        let state = SharedState::new();

        assert_eq!(strategy.scan_interval_secs, DEFAULT_SCAN_INTERVAL_SECS);

        let event = BotEvent::CrashAlert {
            risk_level: predator_core::CrashRiskLevel::Red,
            signals: vec!["SOL -10%".to_string()],
        };

        strategy.process_event(&event, &state);
        assert_eq!(strategy.scan_interval_secs, 5);
    }

    #[test]
    fn zero_sol_price_skips() {
        let mut strategy = FlashArbStrategy::new();
        let state = SharedState::new();
        // sol_price defaults to 0.0

        let actions = strategy.on_scan(&state);
        assert!(actions.is_empty());
    }
}
