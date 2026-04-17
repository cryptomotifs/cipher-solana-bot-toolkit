//! predator-strategies -- Strategy implementations for the Predator MEV bot.
//!
//! Each strategy implements the `Strategy` trait, processing `BotEvent` values
//! from the ingestion layer and producing `BotAction` values for the executor.
//!
//! ## Module Index
//!
//! - `traits` -- `Strategy` trait definition (core interface all strategies implement)
//! - `priority` -- `ActionQueue` backed by `BinaryHeap<PrioritizedAction>` for
//!   priority-ordered action dispatch to the executor
//! - `scanner` -- `UnifiedScanner` with tiered obligation cache (HOT/WARM/COLD)
//!   for efficient multi-protocol obligation monitoring
//! - `liquidation` -- `LiquidationStrategy` -- on oracle price change, re-check
//!   cached obligations, build flash+crank+liquidate+swap+repay bundle.
//!   Multi-protocol: Save(5%) > Kamino(2-10%) > MarginFi(2.5%) > JupLend(0.1%).
//! - `backrun` -- `BackrunStrategy` -- on large swap detection via gRPC tx stream,
//!   check cross-DEX spread, build Jupiter-based arb bundle.
//! - `flash_arb` -- `FlashArbStrategy` -- circular arb SOL->Token->SOL via Jupiter.
//!   24 priority tokens + top volume tokens. Skip if liquidation pending.
//! - `lst_arb` -- `LstArbStrategy` -- jitoSOL/mSOL/jupSOL/bSOL/BNSOL/INF/dSOL/bbSOL
//!   rate deviation detection across Orca/Raydium/Meteora.
//! - `migration_snipe` -- `MigrationSnipeStrategy` -- PumpSwap graduation detection.
//!   Monitor bonding curve completion -> PumpSwap pool creation -> cross-DEX arb.
//!
//! ## Research Citations
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 131-157: strategy specs
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1245-1358: Strategy trait
//! [VERIFIED 2026] code_structure_patterns_2026.md Section 2: Artemis model
//! [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d: priority scheduling
//! [VERIFIED 2026] scanner_deep_research_2026.md: tiered cache, scan optimization
//! [VERIFIED 2026] protocol_liquidation_mechanics_2026.md: instruction sequences
//! [VERIFIED 2026] backrun_arb_strategies_2026.md: backrun viability, graduation sniping
//! [VERIFIED 2026] competition_analysis_2026.md: tip dynamics, timing

pub mod traits;
pub mod priority;
pub mod scanner;
pub mod liquidation;
pub mod backrun;
pub mod flash_arb;
pub mod lst_arb;
pub mod migration_snipe;

// Re-export the Strategy trait and all strategy implementations at crate root.
pub use traits::Strategy;
pub use priority::{ActionQueue, PrioritizedAction};
pub use scanner::{UnifiedScanner, ObligationTier, Opportunity, CachedObligation};
pub use liquidation::LiquidationStrategy;
/// Re-export the Save liquidation executor for use by the orchestrator.
/// [VERIFIED 2026] protocol_liquidation_mechanics_2026.md Section 1
pub use liquidation::SaveLiquidationExecutor;
pub use backrun::BackrunStrategy;
pub use flash_arb::FlashArbStrategy;
pub use lst_arb::LstArbStrategy;
pub use migration_snipe::MigrationSnipeStrategy;
