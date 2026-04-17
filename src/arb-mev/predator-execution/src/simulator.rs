//! TransactionSimulator — pre-simulation and CU estimation.
//!
//! Design decisions from [VERIFIED 2026] research:
//!
//! - `replaceRecentBlockhash=true`: allows simulation with any blockhash,
//!   even expired ones. Essential for testing flash loan TXs without waiting
//!   for a fresh blockhash.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 9:
//!     "simulateTransaction vs Preflight Checks"
//!
//! - `sigVerify=false`: skip signature verification during simulation.
//!   We control the signing — no need to waste CU verifying signatures
//!   in a dry-run.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 9
//!
//! - CU estimation: simulate with max CU (1.4M), read `units_consumed`,
//!   then set CU limit to actual + 10% margin. Over-requesting CU lowers
//!   scheduler priority.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4D:
//!     "Two-Phase Simulate-Then-Submit"
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4A:
//!     "Scheduler DIVIDES by CU requested — over-requesting CU LOWERS effective priority"
//!
//! - Pre-simulation before bundle submission: fail-fast on errors.
//!   Jito bundles are all-or-nothing — if any TX would fail, entire bundle reverts.
//!   Pre-simulation catches errors before wasting network bandwidth.
//!   [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9

use anyhow::{Result, anyhow};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSimulateTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    transaction::VersionedTransaction,
};

// ---------------------------------------------------------------------------
// SimulationResult
// ---------------------------------------------------------------------------

/// Result of a transaction simulation.
///
/// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4D:
///   CU estimation via simulation
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Whether the simulation succeeded (no error).
    pub success: bool,

    /// Error message if simulation failed. None on success.
    pub error: Option<String>,

    /// Compute units consumed during simulation.
    /// Used for CU right-sizing: set CU limit to `cu_consumed * 1.1`.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4D
    pub cu_consumed: u64,

    /// Program logs from the simulation. Useful for debugging flash loan
    /// instruction errors.
    pub logs: Vec<String>,
}

impl SimulationResult {
    /// Returns the recommended CU limit based on simulation: actual + 10% margin.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4D:
    ///   "((actual_cu as f64 * 1.1) as u32).min(1_400_000)"
    pub fn recommended_cu_limit(&self) -> u32 {
        if self.cu_consumed == 0 {
            return 400_000; // Safe default if simulation didn't report CU
        }
        ((self.cu_consumed as f64 * 1.1) as u32).min(1_400_000)
    }
}

// ---------------------------------------------------------------------------
// TransactionSimulator
// ---------------------------------------------------------------------------

/// Simulates transactions without execution for CU estimation and error checking.
///
/// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 9
/// [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9: pre-simulation
pub struct TransactionSimulator;

impl TransactionSimulator {
    /// Simulate a transaction without sending it.
    ///
    /// Uses `replaceRecentBlockhash=true` and `sigVerify=false` to allow
    /// simulation with any blockhash and without valid signatures.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 9:
    ///   "simulateTransaction: replaceRecentBlockhash=true, sigVerify=false"
    pub fn simulate(
        rpc: &RpcClient,
        tx: &VersionedTransaction,
    ) -> Result<SimulationResult> {
        let config = RpcSimulateTransactionConfig {
            sig_verify: false,
            replace_recent_blockhash: true,
            commitment: Some(CommitmentConfig::processed()),
            ..Default::default()
        };

        let result = rpc
            .simulate_transaction_with_config(tx, config)
            .map_err(|e| anyhow!("Simulation RPC error: {}", e))?;

        let sim_value = result.value;

        let success = sim_value.err.is_none();
        let error = sim_value.err.map(|e| format!("{:?}", e));
        let cu_consumed = sim_value.units_consumed.unwrap_or(0);
        let logs = sim_value.logs.unwrap_or_default();

        Ok(SimulationResult {
            success,
            error,
            cu_consumed,
            logs,
        })
    }

    /// Estimate CU for a transaction by simulating it.
    ///
    /// Returns the actual CU consumed + 10% margin, capped at 1.4M.
    /// On simulation failure, returns a safe default.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4D:
    ///   "Two-Phase Simulate-Then-Submit"
    pub fn estimate_cu(
        rpc: &RpcClient,
        tx: &VersionedTransaction,
    ) -> u32 {
        match Self::simulate(rpc, tx) {
            Ok(result) => {
                if result.success {
                    let recommended = result.recommended_cu_limit();
                    tracing::debug!(
                        "CU estimation: consumed={}, recommended={}",
                        result.cu_consumed,
                        recommended
                    );
                    recommended
                } else {
                    tracing::warn!(
                        "CU estimation simulation failed: {:?}. Using default 400K.",
                        result.error
                    );
                    400_000
                }
            }
            Err(e) => {
                tracing::warn!(
                    "CU estimation RPC error: {}. Using default 400K.",
                    e
                );
                400_000
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulation_result_recommended_cu() {
        // 100K consumed -> 110K recommended (10% margin)
        let result = SimulationResult {
            success: true,
            error: None,
            cu_consumed: 100_000,
            logs: vec![],
        };
        assert_eq!(result.recommended_cu_limit(), 110_000);
    }

    #[test]
    fn simulation_result_recommended_cu_capped() {
        // 1.4M consumed -> 1.4M cap (not 1.54M)
        let result = SimulationResult {
            success: true,
            error: None,
            cu_consumed: 1_400_000,
            logs: vec![],
        };
        assert_eq!(result.recommended_cu_limit(), 1_400_000);
    }

    #[test]
    fn simulation_result_zero_cu_default() {
        let result = SimulationResult {
            success: true,
            error: None,
            cu_consumed: 0,
            logs: vec![],
        };
        assert_eq!(result.recommended_cu_limit(), 400_000);
    }

    #[test]
    fn simulation_result_fields() {
        let result = SimulationResult {
            success: false,
            error: Some("custom error 0x1771".to_string()),
            cu_consumed: 450_000,
            logs: vec!["Program log: something".to_string()],
        };
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("0x1771"));
        assert_eq!(result.cu_consumed, 450_000);
        assert_eq!(result.logs.len(), 1);
    }
}
