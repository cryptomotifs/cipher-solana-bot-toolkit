//! predator-execution — transaction building, submission, and confirmation.
//!
//! This crate implements the execution pipeline for the PREDATOR bot:
//!
//! 1. **builder.rs** — `TransactionBuilder`: instruction assembly, compute budget,
//!    ALT integration, V0 message construction, signing.
//!    Pre-allocated `Vec<AccountMeta>` with `with_capacity(32)`.
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 162-165
//!    [VERIFIED 2026] low_latency_dataflow_2026.md Section 7: pre-allocated instruction buffers
//!
//! 2. **jito.rs** — `JitoSubmitter`: 6-region parallel bundle submission,
//!    tip account rotation (8 accounts), dynamic tip calculation.
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 166-170
//!    [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2: Jito bundle submission
//!
//! 3. **submitter.rs** — `MultiPathSubmitter`: coordinates parallel submission to
//!    Jito (primary) + bloXroute (secondary) + Helius (fallback).
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 181-183
//!    [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2D: parallel submission
//!
//! 4. **simulator.rs** — `TransactionSimulator`: pre-simulation with
//!    `replaceRecentBlockhash=true`, `sigVerify=false`. CU estimation.
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 184-186
//!    [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9: pre-simulation
//!
//! 5. **confirmer.rs** — `ConfirmationPoller`: two-phase confirmation using
//!    `getInflightBundleStatuses` (fast) then `getBundleStatuses` (historical).
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189
//!
//! 6. **alt.rs** — Address Lookup Table management: create, extend, load.
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 190-192
//!
//! 7. **ata.rs** — Associated Token Account management: pre-create for top 8 tokens,
//!    close empty ATAs to recover rent.
//!    [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 193-196
//!    [VERIFIED 2026] operational_data_2026.md Section 4: ATA management

pub mod builder;
pub mod jito;
pub mod submitter;
pub mod simulator;
pub mod confirmer;
pub mod alt;
pub mod ata;

// Re-export key types for convenience.
pub use builder::TransactionBuilder;
pub use jito::{JitoSubmitter, BundleStatus};
pub use submitter::{MultiPathSubmitter, SubmitResult};
pub use simulator::{TransactionSimulator, SimulationResult};
pub use confirmer::{ConfirmationPoller, ConfirmResult};
pub use alt::AltManager;
pub use ata::AtaManager;
