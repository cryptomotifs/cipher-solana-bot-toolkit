//! Strategy trait -- the core interface ALL strategies implement.
//!
//! Follows the Artemis Collector -> Strategy -> Executor pipeline pattern.
//! Each strategy receives `BotEvent` values, maintains internal state, and
//! produces `BotAction` values when opportunities are found.
//!
//! ## Hot Path Contract
//!
//! `process_event` is the HOT PATH -- called on every gRPC event. It must
//! complete in <1ms for oracle/pool events. Heavy work (RPC calls, route
//! discovery) is deferred to `on_scan` or background tasks.
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1323-1357: Strategy trait
//! [VERIFIED 2026] code_structure_patterns_2026.md Section 2: Artemis model
//!   Strategy trait with sync_state, process_event, health_check
//! [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d: Priority scheduling
//!   Liquidation(1) > Backrun(2) > FlashArb(3) > LstArb(4) > CopyTrade(5)

use predator_core::{BotAction, BotEvent, SharedState, StrategyPriority};

/// Health status of a strategy, reported via `health_check()`.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1316-1321: StrategyHealth
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyHealth {
    /// Strategy is operating normally.
    Healthy,
    /// Strategy is functional but experiencing issues (e.g. stale data, high latency).
    Degraded(String),
    /// Strategy is not functioning (e.g. RPC errors, missing state).
    Unhealthy(String),
}

/// The core Strategy trait -- ALL strategies implement this.
///
/// Strategies are driven by two event sources:
/// 1. `process_event` -- called reactively on each `BotEvent` (push-based)
/// 2. `on_scan` -- called periodically by the scanner loop (timer-based)
///
/// Both return `Vec<BotAction>` rather than `Option<BotAction>` to support
/// multi-liquidation bundles (up to 4 liquidations per Jito bundle).
/// [VERIFIED 2026] advanced_mev_techniques_2026.md Section 1: multi-liquidation
///
/// The trait is object-safe (`Send + Sync`) so strategies can be stored as
/// `Vec<Box<dyn Strategy>>` and dispatched dynamically by the orchestrator.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1323-1357
/// [VERIFIED 2026] code_structure_patterns_2026.md Section 2: trait definition
pub trait Strategy: Send + Sync {
    /// Human-readable name for logging and metrics.
    ///
    /// Must return a stable, unique identifier (e.g. "liquidation", "backrun").
    /// Used as the key in `MetricsRegistry` and dashboard display.
    fn name(&self) -> &str;

    /// Priority level for action queue ordering.
    ///
    /// Determines position in the executor's priority queue when multiple
    /// strategies produce actions in the same slot.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 136-138:
    ///   Liquidation(1) > Backrun(2) > FlashArb(3) > LstArb(4) > CopyTrade(5)
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 1d: priority table
    fn priority(&self) -> StrategyPriority;

    /// Whether this strategy is currently enabled.
    ///
    /// Disabled strategies are skipped in the event loop and scan cycle.
    /// Can be toggled at runtime via config hot-reload.
    fn is_enabled(&self) -> bool;

    /// Process an incoming event. Returns actions if opportunities are found.
    ///
    /// This is the HOT PATH -- must complete in <1ms for oracle/pool events.
    /// No RPC calls. No blocking I/O. Read from `SharedState` caches only.
    ///
    /// Returns empty `Vec` if no opportunity is found (the common case).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1337:
    ///   "This is the HOT PATH -- must complete in <1ms for oracle/pool events."
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 1c:
    ///   "Strategies read from shared caches, never from RPC"
    fn process_event(&mut self, event: &BotEvent, state: &SharedState) -> Vec<BotAction>;

    /// Called periodically by the scanner (every `scan_interval_secs` seconds).
    ///
    /// For strategies that need active scanning beyond event-driven detection.
    /// May perform batched RPC calls (via the state cache, not directly).
    ///
    /// Default: no periodic scanning needed (returns empty Vec).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1342-1345
    fn on_scan(&mut self, state: &SharedState) -> Vec<BotAction> {
        let _ = state; // suppress unused warning in default impl
        Vec::new()
    }

    /// Strategy-specific health check.
    ///
    /// Called by the health monitor task to detect degraded strategies.
    /// Default: always healthy. Override to check internal state staleness,
    /// cache freshness, etc.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1350-1352
    fn health_check(&self) -> StrategyHealth {
        StrategyHealth::Healthy
    }

    /// Estimated scan interval in seconds (for adaptive scheduling).
    ///
    /// The scanner loop uses this to determine how often to call `on_scan`.
    /// Strategies may return different values based on crash risk level.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1355-1356
    fn scan_interval_secs(&self) -> u64 {
        60
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_health_variants() {
        let h = StrategyHealth::Healthy;
        assert_eq!(h, StrategyHealth::Healthy);

        let d = StrategyHealth::Degraded("stale oracle".to_string());
        assert!(matches!(d, StrategyHealth::Degraded(_)));

        let u = StrategyHealth::Unhealthy("RPC down".to_string());
        assert!(matches!(u, StrategyHealth::Unhealthy(_)));
    }

    /// Verify that the Strategy trait is object-safe (can be used as dyn).
    #[test]
    fn trait_is_object_safe() {
        fn _accepts_dyn(_s: &dyn Strategy) {}
        // Compilation of this function proves object safety.
    }
}
