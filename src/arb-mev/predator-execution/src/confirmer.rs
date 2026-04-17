//! ConfirmationPoller — two-phase bundle confirmation tracking.
//!
//! Design decisions from [VERIFIED 2026] research:
//!
//! - Phase 1: `getInflightBundleStatuses` — fast, in-memory, covers 5-minute window.
//!   Returns "Pending", "Landed", "Failed", or "Invalid" immediately.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189:
//!     "ConfirmationPoller: getInflightBundleStatuses (immediate)"
//!   [VERIFIED 2026] existing jito.rs: get_bundle_status Phase 1
//!
//! - Phase 2: `getBundleStatuses` — historical, permanent record.
//!   Returns confirmationStatus ("confirmed", "finalized") after bundle has landed.
//!   Used as fallback when inflight cache has expired (>5 minutes).
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 188:
//!     "getBundleStatuses (historical, after 30s)"
//!   [VERIFIED 2026] existing jito.rs: get_bundle_status Phase 2
//!
//! - Poll with exponential backoff: 500ms -> 1s -> 2s -> 4s, max ~30s total.
//!   Bundles typically land within 1-2 slots (400-800ms).
//!
//! - Jito bundles are all-or-nothing: if they land, ALL TXs succeeded.
//!   No need to check individual TX status.
//!   [VERIFIED 2026] operational_data_2026.md Section 7:
//!     "Bundle lands but TX fails: Not possible — bundles can only contain successful TXs"

// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189: ConfirmationPoller design
use anyhow::Result;
use std::time::Duration;

use predator_core::SubmitMethod;
use crate::jito::{JitoSubmitter, BundleStatus};

// ---------------------------------------------------------------------------
// ConfirmResult
// ---------------------------------------------------------------------------

/// Result of confirmation polling.
#[derive(Debug, Clone)]
pub struct ConfirmResult {
    /// Whether the bundle/transaction was confirmed on-chain.
    pub landed: bool,

    /// Slot at which the bundle landed. 0 if not landed or unknown.
    pub slot: u64,

    /// Which submission method was used.
    pub method: SubmitMethod,
}

impl ConfirmResult {
    /// Create a "not landed" result.
    pub fn not_landed(method: SubmitMethod) -> Self {
        Self {
            landed: false,
            slot: 0,
            method,
        }
    }

    /// Create a "landed" result.
    pub fn landed_at(slot: u64, method: SubmitMethod) -> Self {
        Self {
            landed: true,
            slot,
            method,
        }
    }
}

// ---------------------------------------------------------------------------
// ConfirmationPoller
// ---------------------------------------------------------------------------

/// Polls Jito for bundle confirmation with exponential backoff.
///
/// Two-phase approach:
/// 1. `getInflightBundleStatuses` — fast, in-memory, 5-minute window.
/// 2. `getBundleStatuses` — historical, permanent, for older bundles.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189
pub struct ConfirmationPoller;

impl ConfirmationPoller {
    /// Poll for bundle confirmation with exponential backoff.
    ///
    /// Starts at 500ms intervals, doubles each attempt up to `timeout`.
    /// Returns as soon as a definitive status is reached (Landed, Failed, Invalid).
    ///
    /// Typical flow:
    /// - Poll 1 (500ms): usually "Pending"
    /// - Poll 2 (1s): often "Landed" (1 slot = ~400ms)
    /// - Poll 3 (2s): should have landed by now
    /// - Poll 4+ (4s+): something is wrong; fall through to historical
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 187-189
    /// [VERIFIED 2026] existing jito.rs: get_bundle_status two-phase pattern
    pub async fn poll_confirmation(
        http: &reqwest::Client,
        jito: &JitoSubmitter,
        bundle_id: &str,
        timeout: Duration,
    ) -> Result<ConfirmResult> {
        let start = tokio::time::Instant::now();
        let mut interval = Duration::from_millis(500);
        let max_interval = Duration::from_secs(4);

        loop {
            if start.elapsed() > timeout {
                tracing::warn!(
                    "Confirmation timeout after {:.1}s for bundle {}",
                    timeout.as_secs_f64(),
                    bundle_id,
                );
                return Ok(ConfirmResult::not_landed(SubmitMethod::Jito));
            }

            // Phase 1: Inflight status (fast, 5-min window).
            match jito.get_inflight_status(http, bundle_id).await {
                Ok(BundleStatus::Landed { slot }) => {
                    tracing::info!(
                        "Bundle {} confirmed LANDED at slot {} ({:.1}s)",
                        bundle_id,
                        slot,
                        start.elapsed().as_secs_f64(),
                    );
                    return Ok(ConfirmResult::landed_at(slot, SubmitMethod::Jito));
                }
                Ok(BundleStatus::Failed) => {
                    tracing::warn!("Bundle {} FAILED", bundle_id);
                    return Ok(ConfirmResult::not_landed(SubmitMethod::Jito));
                }
                Ok(BundleStatus::Invalid) => {
                    tracing::warn!("Bundle {} INVALID", bundle_id);
                    return Ok(ConfirmResult::not_landed(SubmitMethod::Jito));
                }
                Ok(BundleStatus::Pending) => {
                    tracing::debug!(
                        "Bundle {} still pending ({:.1}s elapsed)",
                        bundle_id,
                        start.elapsed().as_secs_f64(),
                    );
                }
                Ok(BundleStatus::Unknown) => {
                    // Inflight cache may have expired — try historical.
                    match jito.get_bundle_status(http, bundle_id).await {
                        Ok(BundleStatus::Landed { slot }) => {
                            tracing::info!(
                                "Bundle {} confirmed via historical lookup at slot {}",
                                bundle_id,
                                slot,
                            );
                            return Ok(ConfirmResult::landed_at(slot, SubmitMethod::Jito));
                        }
                        Ok(BundleStatus::Failed) => {
                            return Ok(ConfirmResult::not_landed(SubmitMethod::Jito));
                        }
                        _ => {
                            tracing::debug!(
                                "Bundle {} status unknown from both inflight and historical",
                                bundle_id,
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("Inflight status check error: {}. Continuing poll.", e);
                }
            }

            // Wait before next poll with exponential backoff.
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(max_interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_result_not_landed() {
        let result = ConfirmResult::not_landed(SubmitMethod::Jito);
        assert!(!result.landed);
        assert_eq!(result.slot, 0);
        assert_eq!(result.method, SubmitMethod::Jito);
    }

    #[test]
    fn confirm_result_landed() {
        let result = ConfirmResult::landed_at(300_000_000, SubmitMethod::BloXroute);
        assert!(result.landed);
        assert_eq!(result.slot, 300_000_000);
        assert_eq!(result.method, SubmitMethod::BloXroute);
    }
}
