//! predator-protocols — lending protocol adapters, oracle parsing, flash loans,
//! and Jupiter swap integration for the Predator bot.
//!
//! ## Modules
//!
//! - `traits` — `ProtocolAdapter` trait and `LiquidateParams` struct for uniform
//!   protocol interaction across Save, Kamino, MarginFi, and JupLend.
//! - `oracle` — `OracleParser` for Pyth PriceUpdateV2/PriceFeedAccount (offset 74),
//!   Switchboard V2 (offset 366), and Switchboard On-Demand (offset 2261).
//! - `jupiter` — `JupiterClient` wrapping V2 /build and V1 /quote + /swap-instructions
//!   with atomic rate limiting (110ms spacing).
//! - `flash_loan` — `FlashLoanRouter` selecting cheapest provider (JupLend 0% >
//!   Kamino 0.001% > Save 0.05%) and building borrow/repay instructions.
//! - Protocol subdirectories (`kamino/`, `save/`, `marginfi/`, `juplend/`) for
//!   protocol-specific `ProtocolAdapter` implementations.
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 70-129: crate layout
//! [VERIFIED 2026] code_structure_patterns_2026.md Section 1: module organization
//! [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2: oracle parsing
//! [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1: Jupiter V2
//! [VERIFIED 2026] flash_loan_deep_dive_2026.md: flash loan providers

pub mod traits;
pub mod oracle;
pub mod jupiter;
pub mod flash_loan;
/// Pyth oracle cranking — PostUpdateAtomic instruction builder.
/// [VERIFIED 2026] pyth_post_update_atomic_2026.md — complete instruction reference
pub mod pyth_crank;

// Protocol-specific adapter implementations.
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 93-119: protocol adapter modules
// [VERIFIED 2026] scanner_deep_research_2026.md Sections 1,3,4: Save, JupLend, MarginFi
// [VERIFIED 2026] batch1_gap_resolution_2026.md GAP #3: MarginFi LIVE under P0
pub mod kamino;
pub mod save;
pub mod marginfi;
pub mod juplend;

// Re-export commonly used items at crate root for convenience.
pub use traits::{LiquidateParams, ObligationInfo, ProtocolAdapter};
pub use oracle::OracleParser;
pub use jupiter::{JupiterClient, JupiterQuote, SwapInstructions, parse_instruction_json};
pub use flash_loan::{FlashLoanProvider, FlashLoanRouter};
