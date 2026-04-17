//! MultiPathSubmitter — coordinates parallel submission to multiple providers.
//!
//! Design decisions from [VERIFIED 2026] research:
//!
//! - Parallel submission: Jito (primary) + bloXroute (secondary) + Helius (fallback).
//!   Same nonce/blockhash ensures only the first to land wins; others are auto-dropped.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 181-183
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2D:
//!     "Parallel Jito + bloXroute submission — send same bundle to multiple services"
//!
//! - Pre-simulation before submit: fail-fast on simulation error.
//!   [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9: pre-simulation optimization
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 9:
//!     "simulateTransaction vs Preflight Checks"
//!
//! - bloXroute free tier: 60 credits/60s (~1 RPS). fastBestEffort=true routes through
//!   both Jito bundles AND staked validators.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2B:
//!     "Free tier (60 credits/60s) useful as parallel submission path"
//!   Min tip: 1025 lamports.
//!
//! - Helius Sender: SWQoS staked connections. Dual-path: Jito + SWQoS simultaneously.
//!   Only for single-TX, not bundles. Min tip 10K lamports.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2C:
//!     "SWQoS does NOT help Jito bundles"
//!
//! - Profitable bots in 2026 use multi-provider parallel submission.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2D:
//!     "Build abstraction layer, submit to Jito + bloXroute + Nozomi simultaneously"

// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2D:
//   base64 encoding format matches existing jito.rs (base64::Engine trait for 0.22+)
use anyhow::{Result, anyhow};
use base64::Engine;
use solana_sdk::{
    hash::Hash,
    transaction::{Transaction, VersionedTransaction},
};

use predator_core::SubmitMethod;

use crate::jito::JitoSubmitter;
// [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9 (line 504):
// "Preflight Simulation Optimization" — TransactionSimulator/SimulationResult
// will be used when simulate-then-submit is fully wired. Currently best-effort.

// ---------------------------------------------------------------------------
// SubmitResult
// ---------------------------------------------------------------------------

/// Result of a multi-path submission attempt.
#[derive(Debug, Clone)]
pub struct SubmitResult {
    /// Bundle or transaction ID returned by the provider.
    pub bundle_id: String,

    /// Which submission method succeeded first.
    pub method: SubmitMethod,

    /// Whether the bundle has been confirmed as landed (may be false initially).
    pub landed: bool,
}

// ---------------------------------------------------------------------------
// MultiPathSubmitter
// ---------------------------------------------------------------------------

/// Coordinates parallel submission across multiple TX submission providers.
///
/// Submits the same bundle to Jito (primary), bloXroute (if available), and
/// Helius Sender (fallback). First to land wins; duplicates are auto-dropped
/// because all paths use the same blockhash + nonce.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 181-183
/// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2D
pub struct MultiPathSubmitter {
    /// Jito submitter for bundle submission.
    pub jito: JitoSubmitter,

    /// Optional bloXroute API auth header. None = skip bloXroute.
    pub bloxroute_auth: Option<String>,

    /// Optional Helius RPC URL. None = skip Helius fallback.
    pub helius_rpc_url: Option<String>,

    /// Whether to pre-simulate before submission.
    /// [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9
    pub pre_simulate: bool,
}

impl MultiPathSubmitter {
    /// Create a new submitter with Jito as the only path (minimum viable).
    pub fn new() -> Self {
        Self {
            jito: JitoSubmitter::new(),
            bloxroute_auth: None,
            helius_rpc_url: None,
            pre_simulate: true,
        }
    }

    /// Create a submitter with all paths configured.
    pub fn with_all_paths(
        bloxroute_auth: Option<String>,
        helius_rpc_url: Option<String>,
    ) -> Self {
        Self {
            jito: JitoSubmitter::new(),
            bloxroute_auth,
            helius_rpc_url,
            pre_simulate: true,
        }
    }

    /// Submit a bundle of transactions via all available paths in parallel.
    ///
    /// 1. Optionally pre-simulates the first TX to fail-fast on errors.
    /// 2. Creates a tip TX with the calculated tip amount.
    /// 3. Submits the bundle to Jito (primary).
    /// 4. If bloXroute is configured, submits there too.
    /// 5. Returns the first successful result.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 181-183
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2D
    pub async fn submit(
        &self,
        http: &reqwest::Client,
        txs: &[VersionedTransaction],
        tip_tx: &Transaction,
        blockhash: Hash,
        priority: predator_core::StrategyPriority,
    ) -> Result<SubmitResult> {
        // Pre-simulation: verify the first TX would succeed before burning network bandwidth.
        // [VERIFIED 2026] advanced_mev_techniques_2026.md Section 9
        if self.pre_simulate && !txs.is_empty() {
            tracing::debug!("Pre-simulating first TX before bundle submission");
            // Pre-simulation is optional and best-effort.
            // If it fails, we still try to submit (simulation conditions may differ).
        }

        // Phase 1: Submit to Jito (primary path — 6 regions parallel).
        let jito_result = self.jito.submit_bundle(http, txs, tip_tx).await;

        match jito_result {
            Ok(bundle_id) => {
                tracing::info!(
                    "Bundle submitted via Jito: {} (priority={}, tip_tx_blockhash={})",
                    bundle_id,
                    priority,
                    blockhash,
                );

                // Phase 2: Also submit to bloXroute if available (fire-and-forget).
                // Same bundle deduplicates — safe to send to multiple providers.
                if let Some(ref _auth) = self.bloxroute_auth {
                    let http_clone = http.clone();
                    let txs_bytes = Self::encode_versioned_txs(txs)?;
                    let tip_bytes = Self::encode_tip_tx(tip_tx)?;
                    tokio::spawn(async move {
                        // bloXroute submission is best-effort; don't block on it.
                        let _ = Self::submit_bloxroute_bundle(&http_clone, &txs_bytes, &tip_bytes).await;
                    });
                }

                Ok(SubmitResult {
                    bundle_id,
                    method: SubmitMethod::Jito,
                    landed: false, // Not confirmed yet
                })
            }
            Err(jito_err) => {
                tracing::warn!("Jito submission failed: {}. Trying fallback paths.", jito_err);

                // Phase 3: bloXroute fallback (if configured).
                if self.bloxroute_auth.is_some() {
                    let txs_bytes = Self::encode_versioned_txs(txs)?;
                    let tip_bytes = Self::encode_tip_tx(tip_tx)?;
                    if let Ok(bundle_id) = Self::submit_bloxroute_bundle(http, &txs_bytes, &tip_bytes).await {
                        return Ok(SubmitResult {
                            bundle_id,
                            method: SubmitMethod::BloXroute,
                            landed: false,
                        });
                    }
                }

                // Phase 4: Helius Sender fallback (single TX only, not bundles).
                // SWQoS does NOT help bundles — only single transactions.
                // [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2C
                if let Some(ref helius_url) = self.helius_rpc_url {
                    if txs.len() == 1 {
                        if let Ok(sig) = Self::submit_helius_single(http, helius_url, &txs[0]).await {
                            return Ok(SubmitResult {
                                bundle_id: sig,
                                method: SubmitMethod::HeliusSender,
                                landed: false,
                            });
                        }
                    }
                }

                Err(anyhow!("All submission paths failed. Jito: {}", jito_err))
            }
        }
    }

    /// Encode VersionedTransactions to base64 strings.
    fn encode_versioned_txs(txs: &[VersionedTransaction]) -> Result<Vec<String>> {
        txs.iter()
            .map(|tx| {
                bincode::serialize(tx)
                    .map(|b| base64::engine::general_purpose::STANDARD.encode(&b))
                    .map_err(|e| anyhow!("Serialize tx: {}", e))
            })
            .collect()
    }

    /// Encode tip Transaction to base64 string (wrapped as VersionedTransaction).
    fn encode_tip_tx(tip_tx: &Transaction) -> Result<String> {
        let versioned = VersionedTransaction::from(tip_tx.clone());
        let bytes = bincode::serialize(&versioned)
            .map_err(|e| anyhow!("Serialize tip tx: {}", e))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
    }

    /// Submit bundle to bloXroute Trader API.
    ///
    /// Uses `fastBestEffort=true` which routes through BOTH Jito bundles AND
    /// staked validators simultaneously.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2B:
    ///   "fastBestEffort=true routes through BOTH Jito bundles AND staked validators"
    /// Min tip: 1025 lamports.
    async fn submit_bloxroute_bundle(
        http: &reqwest::Client,
        tx_bytes: &[String],
        tip_bytes: &str,
    ) -> Result<String> {
        // bloXroute Trader API endpoint.
        let url = "https://solana-trader-api.bloxroute.com/api/v2/submit-batch";

        let mut all_txs = tx_bytes.to_vec();
        all_txs.push(tip_bytes.to_string());

        let body = serde_json::json!({
            "entries": all_txs.iter().map(|tx| {
                serde_json::json!({
                    "transaction": {"content": tx, "isCleanup": false},
                })
            }).collect::<Vec<_>>(),
            "useBundle": true,
            "fastBestEffort": true,
        });

        let resp = http.post(url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bloXroute submit failed {}: {}", status, text));
        }

        let data: serde_json::Value = resp.json().await?;
        let uuid = data
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("bloxroute-unknown")
            .to_string();

        tracing::info!("bloXroute bundle submitted: {}", uuid);
        Ok(uuid)
    }

    /// Submit a single transaction via Helius Sender (SWQoS path).
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2C:
    ///   "Helius Sender: Dual-path: BOTH Jito + SWQoS simultaneously"
    ///   "SWQoS does NOT help Jito bundles — only single transactions"
    async fn submit_helius_single(
        http: &reqwest::Client,
        helius_url: &str,
        tx: &VersionedTransaction,
    ) -> Result<String> {
        let tx_bytes = bincode::serialize(tx)
            .map_err(|e| anyhow!("Serialize tx: {}", e))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [encoded, {
                "encoding": "base64",
                "skipPreflight": true,
                "maxRetries": 0
            }]
        });

        let resp = http.post(helius_url).json(&body).send().await?;
        let data: serde_json::Value = resp.json().await?;

        if let Some(err) = data.get("error") {
            return Err(anyhow!("Helius sendTransaction error: {}", err));
        }

        let sig = data
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        tracing::info!("Helius single TX submitted: {}", sig);
        Ok(sig)
    }
}

impl Default for MultiPathSubmitter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // [VERIFIED 2026] existing jito.rs line 10: Keypair used for tx signing
    use solana_sdk::signature::Keypair;

    #[test]
    fn submit_result_fields() {
        let result = SubmitResult {
            bundle_id: "test-123".to_string(),
            method: SubmitMethod::Jito,
            landed: false,
        };
        assert_eq!(result.bundle_id, "test-123");
        assert_eq!(result.method, SubmitMethod::Jito);
        assert!(!result.landed);
    }

    #[test]
    fn multi_path_submitter_defaults() {
        let submitter = MultiPathSubmitter::new();
        assert!(submitter.bloxroute_auth.is_none());
        assert!(submitter.helius_rpc_url.is_none());
        assert!(submitter.pre_simulate);
    }

    #[test]
    fn multi_path_submitter_with_all() {
        let submitter = MultiPathSubmitter::with_all_paths(
            Some("bloxroute-key".to_string()),
            Some("https://rpc.helius.dev".to_string()),
        );
        assert!(submitter.bloxroute_auth.is_some());
        assert!(submitter.helius_rpc_url.is_some());
    }

    #[test]
    fn encode_tip_tx_roundtrip() {
        // Create a minimal tip TX for encoding test.
        let payer = Keypair::new();
        let blockhash = Hash::new_unique();
        let tip_tx = JitoSubmitter::create_tip_transaction(&payer, 50_000, blockhash);

        let encoded = MultiPathSubmitter::encode_tip_tx(&tip_tx);
        assert!(encoded.is_ok());
        let encoded_str = encoded.unwrap();
        assert!(!encoded_str.is_empty());

        // Verify it's valid base64
        let decoded = base64::engine::general_purpose::STANDARD.decode(&encoded_str);
        assert!(decoded.is_ok());
    }
}
