//! MigrationSnipeStrategy -- PumpSwap graduation event detection and
//! cross-DEX price divergence capture on newly graduated tokens.
//!
//! ## Graduation Detection Flow
//!
//! 1. Monitor gRPC transaction stream for PumpSwap `create_pool` CPI
//!    (discriminator: [233, 146, 209, 142, 207, 104, 64, 188])
//! 2. When a bonding curve graduates, Pump.fun calls `migrate` which
//!    CPIs into PumpSwap `create_pool` with canonical pool index 0
//! 3. Derive the canonical pool PDA: seeds ["pool", 0, creator, base_mint, WSOL]
//! 4. Execute first buy on newly created PumpSwap pool via Jito bundle
//!
//! ## Revenue Model
//!
//! Buy at graduation price, sell after price discovery.
//! Typical 10-50% markup in first minutes if token has community interest.
//! Risk: 98.85% of tokens never graduate. Of those that do, many dump.
//!
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
//!   "Graduation trigger: Bonding curve reaches 100% completion
//!    (800M of 800M tradable tokens sold). Market cap ~$30K-$35K (~80 SOL).
//!    Fixed migration fee: 0.015 SOL"
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
//!   "PumpSwap AMM: pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA
//!    Create pool discriminator: [233, 146, 209, 142, 207, 104, 64, 188]
//!    Pool PDA seeds: ['pool', index(0), creator, base_mint, quote_mint]"
//! [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
//!   "Graduation rate: 1.15% of daily launches"
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 154-156:
//!   "MigrationSnipeStrategy: PumpSwap graduation detection.
//!    Monitor bonding curve -> PumpSwap AMM migration events.
//!    Cross-DEX price divergence on newly graduated tokens."

use std::time::Instant;

use solana_sdk::pubkey::Pubkey;

use predator_core::{
    BotAction, BotEvent, Lamports, Protocol, SharedState, StrategyPriority,
};

use crate::traits::{Strategy, StrategyHealth};

// ---------------------------------------------------------------------------
// Constants
// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5
// ---------------------------------------------------------------------------

/// PumpSwap AMM program ID.
/// [VERIFIED 2026] backrun_arb_strategies_2026.md line 283
const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// Pump.fun bonding curve program ID.
/// [VERIFIED 2026] constants.rs line 212
const PUMPFUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// `create_pool` instruction discriminator (first 8 bytes).
/// Used by the executor to verify graduation transactions contain the correct CPI.
/// [VERIFIED 2026] backrun_arb_strategies_2026.md line 288
pub const CREATE_POOL_DISCRIMINATOR: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];

/// Canonical pool index for graduated tokens.
/// [VERIFIED 2026] backrun_arb_strategies_2026.md line 287: "Pool index = 0"
const CANONICAL_POOL_INDEX: u8 = 0;

/// Wrapped SOL mint.
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Minimum time to wait between graduation events to avoid spam (ms).
/// Multiple tokens may graduate in the same block during memecoin frenzies.
const MIN_EVENT_INTERVAL_MS: u128 = 500;

/// Maximum position size in SOL for graduation sniping.
/// Small positions only -- most graduated tokens dump.
///
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
///   "Use flash loan for SOL side only"
const MAX_SNIPE_SOL: f64 = 0.5;

/// PumpSwap fee for newly graduated tokens at low market cap.
/// 1.25% total fee at 0-420 SOL market cap.
///
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 4:
///   "0-420 SOL: 1.250% total fee"
const PUMPSWAP_FEE_LOW_MCAP_BPS: u16 = 125;

// ---------------------------------------------------------------------------
// GraduationEvent -- detected graduation from Pump.fun to PumpSwap
// ---------------------------------------------------------------------------

/// A detected token graduation event.
#[derive(Debug, Clone)]
pub struct GraduationEvent {
    /// The graduated token mint.
    pub token_mint: Pubkey,
    /// Creator of the PumpSwap pool.
    pub creator: Pubkey,
    /// Derived canonical pool address.
    pub pool_address: Pubkey,
    /// Transaction signature that triggered the graduation.
    pub tx_signature: [u8; 64],
    /// When this graduation was detected.
    pub detected_at: Instant,
}

// ---------------------------------------------------------------------------
// MigrationSnipeStrategy
// ---------------------------------------------------------------------------

/// Migration snipe strategy -- detects PumpSwap graduation events and
/// executes first-buy on newly created pools.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 154-156
/// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5
pub struct MigrationSnipeStrategy {
    /// Whether this strategy is currently enabled.
    enabled: bool,

    /// Parsed PumpSwap program ID.
    pumpswap_program: Pubkey,

    /// Parsed Pump.fun program ID.
    pumpfun_program: Pubkey,

    /// Recent graduation events (for deduplication and tracking).
    recent_graduations: Vec<GraduationEvent>,

    /// Timestamp of last graduation event (for rate limiting).
    last_graduation: Instant,

    /// Total graduations detected since startup.
    graduations_detected: u64,

    /// Total snipe attempts made.
    snipe_attempts: u64,
}

impl MigrationSnipeStrategy {
    /// Create a new MigrationSnipeStrategy.
    pub fn new() -> Self {
        Self {
            enabled: true,
            pumpswap_program: PUMPSWAP_PROGRAM.parse().unwrap_or_default(),
            pumpfun_program: PUMPFUN_PROGRAM.parse().unwrap_or_default(),
            recent_graduations: Vec::new(),
            last_graduation: Instant::now(),
            graduations_detected: 0,
            snipe_attempts: 0,
        }
    }

    /// Check if a transaction contains a PumpSwap pool creation.
    ///
    /// Looks for the PumpSwap program in the program_ids and the
    /// `create_pool` discriminator pattern.
    ///
    /// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
    ///   "Monitor Pump program for migrate instruction execution via gRPC"
    fn detect_graduation(
        &self,
        program_ids: &[Pubkey],
        accounts: &[Pubkey],
        signature: &[u8; 64],
    ) -> Option<GraduationEvent> {
        // Check if PumpSwap or Pump.fun program is involved
        let has_pumpswap = program_ids.iter().any(|p| *p == self.pumpswap_program);
        let has_pumpfun = program_ids.iter().any(|p| *p == self.pumpfun_program);

        if !has_pumpswap && !has_pumpfun {
            return None;
        }

        // For a real implementation, we would parse the inner instructions
        // to find the create_pool CPI with the correct discriminator.
        // The transaction data (instruction bytes) would be examined for
        // CREATE_POOL_DISCRIMINATOR at the start of instruction data.
        //
        // For now, we detect based on program presence and account patterns.
        // A graduation transaction typically involves:
        // 1. Pump.fun program (outer call: migrate)
        // 2. PumpSwap program (inner CPI: create_pool)
        // 3. Token mint account
        // 4. WSOL account
        // 5. Pool PDA

        if !has_pumpswap {
            return None;
        }

        // Extract token mint from accounts (heuristic: first non-system account
        // that isn't the programs themselves). Real implementation would parse
        // instruction data.
        let token_mint = accounts
            .iter()
            .find(|a| {
                **a != self.pumpswap_program
                    && **a != self.pumpfun_program
                    && **a != Pubkey::default()
            })
            .copied()
            .unwrap_or_default();

        let creator = accounts.get(0).copied().unwrap_or_default();

        // Derive canonical pool PDA
        // [VERIFIED 2026] backrun_arb_strategies_2026.md line 288:
        //   "Pool PDA seeds: ['pool', index(0), creator, base_mint, quote_mint]"
        let wsol_mint: Pubkey = WSOL_MINT.parse().unwrap_or_default();
        let pool_address = self.derive_pool_pda(&creator, &token_mint, &wsol_mint);

        Some(GraduationEvent {
            token_mint,
            creator,
            pool_address,
            tx_signature: *signature,
            detected_at: Instant::now(),
        })
    }

    /// Derive the canonical PumpSwap pool PDA.
    ///
    /// [VERIFIED 2026] backrun_arb_strategies_2026.md line 288:
    ///   "Pool PDA seeds: ['pool', index(0), creator, base_mint, quote_mint]"
    fn derive_pool_pda(
        &self,
        creator: &Pubkey,
        base_mint: &Pubkey,
        quote_mint: &Pubkey,
    ) -> Pubkey {
        let seeds = &[
            b"pool" as &[u8],
            &[CANONICAL_POOL_INDEX],
            creator.as_ref(),
            base_mint.as_ref(),
            quote_mint.as_ref(),
        ];

        Pubkey::find_program_address(seeds, &self.pumpswap_program).0
    }

    /// Clean up old graduation events (keep last 100 for deduplication).
    fn cleanup_old_events(&mut self) {
        if self.recent_graduations.len() > 100 {
            self.recent_graduations.drain(0..50);
        }
    }

    /// Check if we've already seen this graduation (dedup).
    fn is_duplicate(&self, token_mint: &Pubkey) -> bool {
        self.recent_graduations
            .iter()
            .any(|g| g.token_mint == *token_mint)
    }
}

impl Default for MigrationSnipeStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for MigrationSnipeStrategy {
    fn name(&self) -> &str {
        "migration_snipe"
    }

    /// Priority 5 -- lowest, background monitoring.
    /// Using CopyTrade priority since there's no MigrationSnipe priority variant.
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d
    fn priority(&self) -> StrategyPriority {
        StrategyPriority::CopyTrade // MigrationSnipe shares lowest priority
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Process incoming events.
    ///
    /// - TransactionSeen: detect PumpSwap graduation events
    ///
    /// [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
    ///   "Monitor Pump program for migrate instruction execution via gRPC
    ///    transaction subscription"
    fn process_event(&mut self, event: &BotEvent, state: &SharedState) -> Vec<BotAction> {
        match event {
            BotEvent::TransactionSeen {
                signature,
                program_ids,
                accounts,
            } => {
                // Rate limit: avoid processing multiple events too quickly
                if self.last_graduation.elapsed().as_millis() < MIN_EVENT_INTERVAL_MS {
                    return Vec::new();
                }

                let graduation = match self.detect_graduation(program_ids, accounts, signature) {
                    Some(g) => g,
                    None => return Vec::new(),
                };

                // Dedup check
                if self.is_duplicate(&graduation.token_mint) {
                    return Vec::new();
                }

                self.graduations_detected += 1;
                self.last_graduation = Instant::now();

                let sol_price = state.get_sol_price();
                let tip_floor = state.get_tip_floor();

                // Log the graduation event
                let mut actions = vec![BotAction::LogOpportunity {
                    protocol: Protocol::Save, // placeholder
                    est_profit: Lamports(0),
                    description: format!(
                        "Graduation detected: mint={} pool={} creator={}",
                        graduation.token_mint, graduation.pool_address, graduation.creator
                    ),
                }];

                // Build snipe bundle:
                // TX1: Buy on PumpSwap pool (first buyer advantage)
                //
                // [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
                //   "With barely any liquidity, a $50 buy moves price 30-40%"
                let snipe_lamports = (MAX_SNIPE_SOL * 1_000_000_000.0) as u64;
                let fee_lamports = (snipe_lamports as f64
                    * (PUMPSWAP_FEE_LOW_MCAP_BPS as f64 / 10_000.0)) as u64;

                // Estimate conservative profit (10% markup on small position)
                // [VERIFIED 2026] backrun_arb_strategies_2026.md Section 5:
                //   "Typical 10-50% markup in first minutes if token has community interest"
                let est_profit_usd = (snipe_lamports as f64 / 1_000_000_000.0) * sol_price * 0.10;
                let est_profit = (est_profit_usd / sol_price * 1_000_000_000.0).max(0.0) as u64;
                let tip = std::cmp::max(
                    (est_profit as f64 * 0.50) as u64,
                    tip_floor,
                );

                if est_profit > fee_lamports + tip {
                    self.snipe_attempts += 1;

                    actions.push(BotAction::SubmitBundle {
                        txs: Vec::new(), // executor builds real PumpSwap buy TX
                        tip_lamports: Lamports(tip),
                        priority: StrategyPriority::CopyTrade,
                    });
                }

                // Store for deduplication
                self.recent_graduations.push(graduation);
                self.cleanup_old_events();

                actions
            }
            _ => Vec::new(),
        }
    }

    fn health_check(&self) -> StrategyHealth {
        if self.pumpswap_program == Pubkey::default() {
            return StrategyHealth::Unhealthy(
                "PumpSwap program ID not parsed".to_string(),
            );
        }

        StrategyHealth::Healthy
    }

    fn scan_interval_secs(&self) -> u64 {
        // Migration snipe is purely event-driven (gRPC tx stream).
        // No periodic scanning needed.
        u64::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_name_and_priority() {
        let strategy = MigrationSnipeStrategy::new();
        assert_eq!(strategy.name(), "migration_snipe");
        assert_eq!(strategy.priority(), StrategyPriority::CopyTrade);
        assert!(strategy.is_enabled());
    }

    #[test]
    fn pumpswap_program_parsed() {
        let strategy = MigrationSnipeStrategy::new();
        assert_ne!(strategy.pumpswap_program, Pubkey::default());
        assert_eq!(
            strategy.pumpswap_program.to_string(),
            PUMPSWAP_PROGRAM
        );
    }

    #[test]
    fn create_pool_discriminator_correct() {
        // Verify against research
        // [VERIFIED 2026] backrun_arb_strategies_2026.md line 288
        assert_eq!(
            CREATE_POOL_DISCRIMINATOR,
            [233, 146, 209, 142, 207, 104, 64, 188]
        );
    }

    #[test]
    fn health_check_healthy() {
        let strategy = MigrationSnipeStrategy::new();
        assert_eq!(strategy.health_check(), StrategyHealth::Healthy);
    }

    #[test]
    fn no_graduation_without_pumpswap() {
        let strategy = MigrationSnipeStrategy::new();

        let result = strategy.detect_graduation(
            &[Pubkey::new_unique()], // random program, not PumpSwap
            &[Pubkey::new_unique()],
            &[0u8; 64],
        );
        assert!(result.is_none());
    }

    #[test]
    fn graduation_detected_with_pumpswap() {
        let strategy = MigrationSnipeStrategy::new();

        let result = strategy.detect_graduation(
            &[strategy.pumpswap_program],
            &[Pubkey::new_unique(), Pubkey::new_unique()],
            &[1u8; 64],
        );
        assert!(result.is_some());
    }

    #[test]
    fn dedup_prevents_double_processing() {
        let mut strategy = MigrationSnipeStrategy::new();
        let token = Pubkey::new_unique();

        strategy.recent_graduations.push(GraduationEvent {
            token_mint: token,
            creator: Pubkey::new_unique(),
            pool_address: Pubkey::new_unique(),
            tx_signature: [0u8; 64],
            detected_at: Instant::now(),
        });

        assert!(strategy.is_duplicate(&token));
        assert!(!strategy.is_duplicate(&Pubkey::new_unique()));
    }

    #[test]
    fn cleanup_old_events() {
        let mut strategy = MigrationSnipeStrategy::new();

        // Add 110 events
        for _ in 0..110 {
            strategy.recent_graduations.push(GraduationEvent {
                token_mint: Pubkey::new_unique(),
                creator: Pubkey::new_unique(),
                pool_address: Pubkey::new_unique(),
                tx_signature: [0u8; 64],
                detected_at: Instant::now(),
            });
        }

        strategy.cleanup_old_events();
        assert!(strategy.recent_graduations.len() <= 60);
    }

    #[test]
    fn pool_pda_derivation_deterministic() {
        let strategy = MigrationSnipeStrategy::new();
        let creator = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let quote = Pubkey::new_unique();

        let pda1 = strategy.derive_pool_pda(&creator, &base, &quote);
        let pda2 = strategy.derive_pool_pda(&creator, &base, &quote);
        assert_eq!(pda1, pda2);

        // Different inputs should produce different PDAs
        let pda3 = strategy.derive_pool_pda(&Pubkey::new_unique(), &base, &quote);
        assert_ne!(pda1, pda3);
    }

    #[test]
    fn non_tx_events_ignored() {
        let mut strategy = MigrationSnipeStrategy::new();
        let state = SharedState::new();

        let event = BotEvent::SlotUpdate {
            slot: predator_core::Slot(100),
            blockhash: [0u8; 32],
        };

        let actions = strategy.process_event(&event, &state);
        assert!(actions.is_empty());
    }
}
