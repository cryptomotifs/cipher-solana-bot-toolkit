//! predator-geyser — gRPC account streaming, oracle SSE, and event routing.
//!
//! This crate manages all real-time data ingestion for the Predator bot:
//!
//! - **subscriber**: GeyserManager with 2-3 independent gRPC streams (oracles, pools, blocksMeta)
//! - **filters**: Subscription filter builders (owner-based, account-based, blocksMeta)
//! - **decoder**: Zero-copy account data parsing (50ns per field extraction)
//! - **router**: FxHashMap-based O(1) event routing to per-strategy channels
//! - **oracle_engine**: Dual-source oracle aggregation (Pyth SSE + gRPC on-chain)
//! - **pyth_sse**: Pyth Hermes SSE streaming client (400ms off-chain price updates)
//! - **blockhash**: BlockhashProvider from gRPC blocksMeta (eliminates RPC calls)
//!
//! All design decisions are verified against 2026 research:
//!
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 198-230: predator-geyser crate layout
//! - [VERIFIED 2026] bot_architecture_deep_2026.md Section 2: gRPC optimization, owner filter trick
//! - [VERIFIED 2026] oracle_monitoring_deep_2026.md: Pyth SSE streaming, Switchboard offsets
//! - [VERIFIED 2026] low_latency_dataflow_2026.md: zero-copy decoding, channel patterns, event routing

pub mod subscriber;
pub mod filters;
pub mod decoder;
pub mod router;
pub mod oracle_engine;
pub mod pyth_sse;
pub mod blockhash;

// Re-export primary types used by downstream crates.
// Re-export GeyserChannels and GeyserEndpoint for orchestrator wiring.
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 202-207: GeyserManager + GeyserChannels
// [VERIFIED 2026] code_structure_patterns_2026.md Section 4: Arc-Based DI
pub use subscriber::{GeyserManager, GeyserStream, AccountUpdate, GeyserChannels, GeyserEndpoint};
pub use filters::{build_oracle_owner_filter, build_account_filter, build_blocks_filter};
pub use decoder::{decode_oracle_price, decode_pool_state, PoolPrice};
pub use router::{EventRouter, AccountType, RoutedEvent};
pub use oracle_engine::OracleEngine;
pub use pyth_sse::PythSseClient;
pub use blockhash::BlockhashProvider;
