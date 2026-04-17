//! predator-core — foundational types, events, and actions for the Predator bot.
//!
//! This crate defines the shared vocabulary used across all other crates in the workspace:
//! - `types`: Newtype wrappers (Lamports, Slot, etc.), Protocol/DexType enums, OraclePrice
//! - `events`: BotEvent enum — inbound signals from gRPC, SSE, and RPC collectors
//! - `actions`: BotAction enum — outbound commands from strategies to the executor
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 43-54: predator-core crate layout
//! [VERIFIED 2026] code_structure_patterns_2026.md: Artemis Strategy<Event, Action> pattern

pub mod types;
pub mod events;
pub mod actions;
/// Program IDs, market addresses, tip accounts, system programs, CU limits, fee constants.
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 63: constants.rs layout
pub mod constants;
/// BotError enum (thiserror), per-domain error variants.
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 48: error.rs layout
pub mod error;
/// BotConfig struct (deserialized from config.toml).
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 47: config.rs layout
/// [VERIFIED 2026] code_structure_patterns_2026.md Section 6: Layered Config with Hot Reload
pub mod config;
/// MetricsRegistry: CachePadded<AtomicU64> counters, per-strategy P&L tracking.
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 54-56: MetricsRegistry layout
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 4: CachePadded atomics
pub mod metrics;
/// SharedState struct: DashMap + ArcSwap for lock-free concurrent access.
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 57-58: SharedState layout
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 2: DashMap + ArcSwap patterns
pub mod state;

// Re-export commonly used items at crate root for convenience.
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 46: "Re-exports: BotConfig, BotError, Result, types"
// [VERIFIED 2026] code_structure_patterns_2026.md Section 1: "Re-export commonly used items at crate root"
pub use types::{
    AccountSnapshot, BasisPoints, DexType, HealthFactor, Lamports, OraclePrice, OracleSource,
    Protocol, Slot,
};

pub use events::{BotEvent, CrashRiskLevel, PriceSource};

pub use actions::{BotAction, StrategyPriority, SubmitMethod};

pub use error::{BotError, Result};

pub use config::{BotConfig, GeyserConfig};
// GeyserConfig re-export backed by:
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 46: "Re-exports: BotConfig, BotError, Result, types"
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1445: "[grpc] section"
pub use metrics::{MetricsRegistry, MetricsSnapshot, StrategyId};
pub use state::SharedState;
