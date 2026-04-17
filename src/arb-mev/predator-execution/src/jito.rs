//! JitoSubmitter — Jito Block Engine bundle submission with multi-region support.
//!
//! Design decisions from [VERIFIED 2026] research:
//!
//! - 6 regional endpoints for parallel submission. Same bundle deduplicates by
//!   SHA-256 of transaction signatures. Safe to send to all regions simultaneously.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 166-169
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2: Jito regional endpoints
//!   [VERIFIED 2026] constants.rs: JITO_REGIONAL_ENDPOINTS array
//!
//! - 8 tip accounts — randomly rotate to avoid write-lock CU exhaustion.
//!   [VERIFIED 2026] constants.rs: JITO_TIP_ACCOUNTS array
//!   [VERIFIED 2026] jito_multiregion_2026.md
//!
//! - base64 encoding with `{"encoding": "base64"}` parameter.
//!   [VERIFIED 2026] existing jito.rs: base64::engine::general_purpose::STANDARD
//!
//! - VersionedTransaction wrapping for legacy tip TXs.
//!   Block engine expects all entries as VersionedTransaction.
//!   [VERIFIED 2026] existing jito.rs line 129: "VersionedTransaction::from(tip_tx.clone())"
//!
//! - Dynamic tip: 50% of profit, min 50K lamports, floored by API tip_floor.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 170
//!   [VERIFIED 2026] constants.rs: JITO_TIP_PERCENT = 0.50, JITO_MIN_TIP_LAMPORTS = 50_000
//!
//! - Bundle status: two-phase lookup — getInflightBundleStatuses (fast, 5-min window)
//!   then getBundleStatuses (historical).
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189
//!   [VERIFIED 2026] existing jito.rs: get_bundle_status implementation
//!
//! - Jito bundles are all-or-nothing. If bundle doesn't land, NO fees are charged.
//!   [VERIFIED 2026] operational_data_2026.md Section 7: "Failed bundle cost: $0.00"
//!
//! - 92% of Solana validators run Jito-Solana client.
//!   [VERIFIED 2026] advanced_mev_techniques_2026.md Section 7: "92% validator coverage"

use anyhow::{Result, anyhow};
// [VERIFIED 2026] existing jito.rs: base64 encoding + system_instruction for tip transfers
use base64::Engine;
use rand::Rng;
#[allow(deprecated)] // system_instruction re-export still works in solana-sdk 2.2.x
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::{Transaction, VersionedTransaction},
};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use predator_core::constants;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 6 Jito regional bundle endpoints for concurrent submission.
/// Same bundle deduplicates by SHA-256 hash of tx signatures — safe to send to all.
/// [VERIFIED 2026] constants.rs: JITO_REGIONAL_ENDPOINTS
pub const JITO_ENDPOINTS: [&str; 6] = constants::JITO_REGIONAL_ENDPOINTS;

// ---------------------------------------------------------------------------
// BundleStatus
// ---------------------------------------------------------------------------

/// Status of a Jito bundle as returned by the block engine.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189
/// [VERIFIED 2026] existing jito.rs: get_bundle_status implementation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleStatus {
    /// Bundle is in the processing pipeline, not yet included in a block.
    Pending,

    /// Bundle was successfully included in a block at the given slot.
    Landed { slot: u64 },

    /// Bundle failed simulation or execution.
    Failed,

    /// Bundle was rejected as invalid (e.g. malformed, too many TXs).
    Invalid,

    /// Status could not be determined (e.g. expired from both inflight and historical caches).
    Unknown,
}

impl BundleStatus {
    /// Returns true if the bundle was successfully included on-chain.
    pub fn is_landed(&self) -> bool {
        matches!(self, BundleStatus::Landed { .. })
    }
}

impl std::fmt::Display for BundleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleStatus::Pending => write!(f, "Pending"),
            BundleStatus::Landed { slot } => write!(f, "Landed(slot={})", slot),
            BundleStatus::Failed => write!(f, "Failed"),
            BundleStatus::Invalid => write!(f, "Invalid"),
            BundleStatus::Unknown => write!(f, "Unknown"),
        }
    }
}

// ---------------------------------------------------------------------------
// JitoSubmitter
// ---------------------------------------------------------------------------

/// Jito Block Engine bundle submitter with multi-region support.
///
/// Submits bundles to all 6 regional endpoints in parallel. First to land wins;
/// duplicates are automatically dropped by the block engine (SHA-256 dedup).
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 166-170
/// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 2
pub struct JitoSubmitter {
    /// Cached tip floor from Jito API (refreshed periodically).
    /// [VERIFIED 2026] existing jito.rs: TIP_FLOOR AtomicU64
    tip_floor: AtomicU64,
}

impl JitoSubmitter {
    pub fn new() -> Self {
        Self {
            tip_floor: AtomicU64::new(constants::JITO_MIN_TIP_LAMPORTS),
        }
    }

    /// Submit a bundle of VersionedTransactions + tip TX to all 6 Jito regions.
    ///
    /// Returns the bundle ID on first successful response.
    /// All regions receive the same bundle — deduplication is handled by the block engine.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 166-169
    /// [VERIFIED 2026] existing jito.rs: submit_mixed_bundle (multi-region pattern)
    pub async fn submit_bundle(
        &self,
        http: &reqwest::Client,
        txs: &[VersionedTransaction],
        tip_tx: &Transaction,
    ) -> Result<String> {
        // Encode all versioned TXs to base64.
        let mut encoded_txs: Vec<String> = txs
            .iter()
            .map(|tx| {
                bincode::serialize(tx)
                    .map(|b| base64::engine::general_purpose::STANDARD.encode(&b))
                    .map_err(|e| anyhow!("Serialize versioned tx: {}", e))
            })
            .collect::<Result<Vec<_>>>()?;

        // Wrap tip TX as VersionedTransaction (legacy message inside versioned wrapper).
        // Block engine expects all entries as VersionedTransaction.
        // [VERIFIED 2026] existing jito.rs line 129
        let tip_versioned = VersionedTransaction::from(tip_tx.clone());
        let tip_bytes = bincode::serialize(&tip_versioned)
            .map_err(|e| anyhow!("Serialize tip tx: {}", e))?;
        encoded_txs.push(base64::engine::general_purpose::STANDARD.encode(&tip_bytes));

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [encoded_txs, {"encoding": "base64"}]
        });

        // Submit to all 6 regional endpoints in parallel.
        // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 166-169
        let futs: Vec<_> = JITO_ENDPOINTS
            .iter()
            .map(|url| http.post(*url).json(&body).send())
            .collect();

        let results = futures_util::future::join_all(futs).await;

        // Take first successful response.
        let mut last_err = String::new();
        for result in results {
            match result {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(data) = resp.json::<serde_json::Value>().await {
                        if data.get("error").is_none() {
                            let bundle_id = data
                                .get("result")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            return Ok(bundle_id);
                        } else if let Some(err) = data.get("error") {
                            last_err = format!("Jito error: {}", err);
                        }
                    }
                }
                Ok(resp) => {
                    last_err = format!(
                        "{}: {}",
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    );
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
        }

        Err(anyhow!(
            "Jito all 6 regions failed: {}",
            &last_err[..last_err.len().min(200)]
        ))
    }

    /// Create a Jito tip instruction (SOL transfer to random tip account).
    ///
    /// Randomly selects from 8 verified tip accounts to distribute write-lock
    /// contention across accounts.
    ///
    /// [VERIFIED 2026] constants.rs: JITO_TIP_ACCOUNTS (8 accounts verified 2026-04-07)
    /// [VERIFIED 2026] existing jito.rs: get_random_tip_account pattern
    pub fn create_tip_instruction(payer: &Pubkey, amount: u64) -> Instruction {
        let tip_account = Self::random_tip_account();
        system_instruction::transfer(payer, &tip_account, amount)
    }

    /// Create a signed tip transaction.
    ///
    /// [VERIFIED 2026] existing jito.rs: create_tip_transaction
    pub fn create_tip_transaction(
        payer: &Keypair,
        amount: u64,
        blockhash: Hash,
    ) -> Transaction {
        let tip_ix = Self::create_tip_instruction(&payer.pubkey(), amount);
        let mut tx = Transaction::new_with_payer(&[tip_ix], Some(&payer.pubkey()));
        tx.sign(&[payer], blockhash);
        tx
    }

    /// Calculate tip: max(tip_floor, 50% of profit, minimum).
    ///
    /// Always tips enough to land, keeps remaining profit.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 170:
    ///   "Dynamic tip: 50% of profit, min 50K lamports"
    /// [VERIFIED 2026] constants.rs: JITO_TIP_PERCENT = 0.50, JITO_MIN_TIP_LAMPORTS = 50_000
    /// [VERIFIED 2026] existing jito.rs: calculate_tip implementation
    pub fn calculate_tip(&self, profit_lamports: u64) -> u64 {
        let floor = self.tip_floor.load(Ordering::Relaxed);
        let profit_tip = (profit_lamports as f64 * constants::JITO_TIP_PERCENT) as u64;
        profit_tip.max(floor).max(constants::JITO_MIN_TIP_LAMPORTS)
    }

    /// Refresh the tip floor from Jito's REST API.
    ///
    /// Call periodically (every 5 minutes) to stay current with network conditions.
    ///
    /// [VERIFIED 2026] existing jito.rs: refresh_tip_floor implementation
    /// [VERIFIED 2026] constants.rs: JITO_TIP_FLOOR_URL
    pub async fn refresh_tip_floor(&self, http: &reqwest::Client) {
        match http.get(constants::JITO_TIP_FLOOR_URL).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(data) = resp.json::<Vec<serde_json::Value>>().await {
                    if let Some(first) = data.first() {
                        // Use 50th percentile as our floor.
                        if let Some(p50) = first
                            .get("landed_tips_50th_percentile")
                            .and_then(|v| v.as_f64())
                        {
                            let floor_lamports = (p50 * 1e9) as u64;
                            if floor_lamports > 0 {
                                self.tip_floor.store(floor_lamports, Ordering::Relaxed);
                                tracing::info!(
                                    "JITO tip floor updated: {} lamports ({:.6} SOL)",
                                    floor_lamports,
                                    floor_lamports as f64 / 1e9
                                );
                            }
                        }
                    }
                }
            }
            _ => {
                tracing::warn!("JITO tip floor refresh failed — keeping current floor");
            }
        }
    }

    /// Get inflight bundle status (fast, in-memory, 5-minute window).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 188
    /// [VERIFIED 2026] existing jito.rs: getInflightBundleStatuses
    pub async fn get_inflight_status(
        &self,
        http: &reqwest::Client,
        bundle_id: &str,
    ) -> Result<BundleStatus> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getInflightBundleStatuses",
            "params": [[bundle_id]]
        });

        let resp = http
            .post(constants::JITO_BUNDLE_URL)
            .json(&body)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;

        let status_str = json
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|s| s.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");

        match status_str {
            "Landed" => {
                let slot = json
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|s| s.get("landed_slot"))
                    .and_then(|s| s.as_u64())
                    .unwrap_or(0);
                Ok(BundleStatus::Landed { slot })
            }
            "Failed" => Ok(BundleStatus::Failed),
            "Pending" => Ok(BundleStatus::Pending),
            "Invalid" => Ok(BundleStatus::Invalid),
            _ => Ok(BundleStatus::Unknown),
        }
    }

    /// Get bundle status — tries inflight first (fast), then falls back to
    /// historical getBundleStatuses.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189
    /// [VERIFIED 2026] existing jito.rs: get_bundle_status two-phase pattern
    pub async fn get_bundle_status(
        &self,
        http: &reqwest::Client,
        bundle_id: &str,
    ) -> Result<BundleStatus> {
        // Phase 1: Try inflight (fast, in-memory, 5-min window).
        match self.get_inflight_status(http, bundle_id).await {
            Ok(status) if status != BundleStatus::Unknown => return Ok(status),
            _ => {} // Fall through to historical
        }

        // Phase 2: Try getBundleStatuses (historical, permanent record).
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getBundleStatuses",
            "params": [[bundle_id]]
        });

        let resp = http
            .post(constants::JITO_BUNDLE_URL)
            .json(&body)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;

        // Parse historical status.
        let status_str = json
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|s| s.get("confirmation_status"))
            .or_else(|| {
                json.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|s| s.get("confirmationStatus"))
            })
            .and_then(|s| s.as_str())
            .unwrap_or("");

        match status_str {
            "confirmed" | "finalized" | "processed" => {
                let slot = json
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|s| s.get("slot"))
                    .and_then(|s| s.as_u64())
                    .unwrap_or(0);
                Ok(BundleStatus::Landed { slot })
            }
            "failed" => Ok(BundleStatus::Failed),
            _ => Ok(BundleStatus::Unknown),
        }
    }

    /// Get current tip floor in lamports.
    pub fn current_tip_floor(&self) -> u64 {
        self.tip_floor.load(Ordering::Relaxed)
    }

    /// Pick a random Jito tip account from the 8 verified accounts.
    fn random_tip_account() -> Pubkey {
        let mut rng = rand::thread_rng();
        let idx = rng.gen_range(0..constants::JITO_TIP_ACCOUNTS.len());
        Pubkey::from_str(constants::JITO_TIP_ACCOUNTS[idx])
            .expect("JITO_TIP_ACCOUNTS must be valid base58 pubkeys")
    }
}

impl Default for JitoSubmitter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jito_endpoints_count() {
        // [VERIFIED 2026] 6 regional endpoints
        assert_eq!(JITO_ENDPOINTS.len(), 6);
    }

    #[test]
    fn tip_accounts_are_valid_pubkeys() {
        // [VERIFIED 2026] constants.rs: 8 tip accounts verified 2026-04-07
        for acc in constants::JITO_TIP_ACCOUNTS.iter() {
            assert!(
                Pubkey::from_str(acc).is_ok(),
                "Invalid tip account pubkey: {}",
                acc
            );
        }
    }

    #[test]
    fn calculate_tip_minimum() {
        let submitter = JitoSubmitter::new();
        // With zero profit, should return min tip.
        let tip = submitter.calculate_tip(0);
        assert_eq!(tip, constants::JITO_MIN_TIP_LAMPORTS);
    }

    #[test]
    fn calculate_tip_percentage() {
        let submitter = JitoSubmitter::new();
        // With 1M lamports profit: 50% = 500K, which is > min tip.
        let tip = submitter.calculate_tip(1_000_000);
        assert_eq!(tip, 500_000);
    }

    #[test]
    fn calculate_tip_floor_wins() {
        let submitter = JitoSubmitter::new();
        // Set a high floor
        submitter.tip_floor.store(1_000_000, Ordering::Relaxed);
        // With 100K profit: 50% = 50K, but floor = 1M, so floor wins.
        let tip = submitter.calculate_tip(100_000);
        assert_eq!(tip, 1_000_000);
    }

    #[test]
    fn bundle_status_display() {
        assert_eq!(BundleStatus::Pending.to_string(), "Pending");
        assert_eq!(BundleStatus::Landed { slot: 42 }.to_string(), "Landed(slot=42)");
        assert_eq!(BundleStatus::Failed.to_string(), "Failed");
        assert_eq!(BundleStatus::Invalid.to_string(), "Invalid");
        assert_eq!(BundleStatus::Unknown.to_string(), "Unknown");
    }

    #[test]
    fn bundle_status_is_landed() {
        assert!(BundleStatus::Landed { slot: 1 }.is_landed());
        assert!(!BundleStatus::Pending.is_landed());
        assert!(!BundleStatus::Failed.is_landed());
    }

    #[test]
    fn random_tip_account_is_valid() {
        // Call multiple times to test randomness
        for _ in 0..20 {
            let account = JitoSubmitter::random_tip_account();
            // Verify it's one of the known tip accounts
            let account_str = account.to_string();
            assert!(
                constants::JITO_TIP_ACCOUNTS.contains(&account_str.as_str()),
                "Random tip account {} not in JITO_TIP_ACCOUNTS",
                account_str
            );
        }
    }

    #[test]
    fn create_tip_instruction_produces_transfer() {
        let payer = Pubkey::new_unique();
        let ix = JitoSubmitter::create_tip_instruction(&payer, 50_000);
        // System program transfer
        assert_eq!(ix.program_id, solana_sdk::system_program::id());
        assert_eq!(ix.accounts.len(), 2);
        assert_eq!(ix.accounts[0].pubkey, payer);
    }

    #[test]
    fn default_tip_floor() {
        let submitter = JitoSubmitter::new();
        assert_eq!(
            submitter.current_tip_floor(),
            constants::JITO_MIN_TIP_LAMPORTS
        );
    }
}
