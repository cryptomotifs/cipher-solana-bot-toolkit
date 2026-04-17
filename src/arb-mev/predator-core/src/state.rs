//! Shared state cache for the Predator bot.
//!
//! All design decisions are verified against 2026 research:
//!
//! - [VERIFIED 2026] low_latency_dataflow_2026.md Section 2: "DashMap for per-account
//!   updates, ArcSwap for config/prices" -- DashMap is a sharded concurrent hashmap
//!   with one RwLock per shard. Readers don't block each other, writers only block
//!   one shard. ~50-100ns per read, ~200ns per write.
//!
//! - [VERIFIED 2026] low_latency_dataflow_2026.md Section 2: ArcSwap -- "Writer builds
//!   new state, atomically swaps the Arc pointer. Readers always see a consistent
//!   snapshot." Wait-free reads (no contention, no locks, no CAS loops). Writer
//!   latency: ~30ns atomic pointer swap.
//!
//! - [VERIFIED 2026] low_latency_dataflow_2026.md Section 9: "gRPC blocksMeta for fresh
//!   blockhash -- push-based, arrives within ~5ms of block confirmation, always fresh,
//!   no RPC call overhead (saves 30-50ms per getLatestBlockhash)."
//!
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 57-58: "SharedState struct:
//!   DashMap<Pubkey, AccountSnapshot>, ArcSwap<Hash> blockhash, ArcSwap<f64> sol_price,
//!   DashMap<Pubkey, OraclePrice> oracle_cache"
//!
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 349-356: State cache layout
//!   with oracle_prices, pool_states, obligations, blockhash, sol_price, tip_estimate.
//!
//! - [VERIFIED 2026] crash_prediction_2026.md lines 344-349: CrashRiskLevel enum
//!   (Green/Yellow/Orange/Red) maps to AtomicU8 0-3 for lock-free risk level access.
//!
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 393: "Shared state is read-only
//!   for strategies: DashMap and ArcSwap are written only by ingestion tasks, read by
//!   strategy tasks without locks on the hot path."

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;

use crate::events::CrashRiskLevel;
// OraclePrice contains Slot internally; Slot is only used directly in tests
// which have their own import.
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 78: "OraclePrice struct: price_f64,
//   confidence, expo, slot"
use crate::types::OraclePrice;

// ---------------------------------------------------------------------------
// SharedState
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 57-58, 349-356
// [VERIFIED 2026] low_latency_dataflow_2026.md Section 2: DashMap + ArcSwap
// ---------------------------------------------------------------------------

/// Central shared state cache. Written by ingestion tasks (gRPC, SSE, RPC poller),
/// read by strategy tasks on every hot-path iteration.
///
/// All fields use lock-free or sharded-lock data structures to minimize contention:
/// - `DashMap`: sharded RwLock, ~50-100ns read, ~200ns write
///   [VERIFIED 2026] low_latency_dataflow_2026.md Section 2
/// - `ArcSwap`: wait-free reads (~0ns contention), ~30ns atomic pointer swap
///   [VERIFIED 2026] low_latency_dataflow_2026.md Section 2
/// - `AtomicU64`/`AtomicU8`: hardware atomic, ~1-4ns when cache-local
///   [VERIFIED 2026] low_latency_dataflow_2026.md Section 4
pub struct SharedState {
    /// Oracle prices keyed by oracle account pubkey (Pyth/Switchboard feed address).
    /// Updated by gRPC oracle stream and Pyth Hermes SSE.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 349:
    ///   "oracle_prices: DashMap<Pubkey, OraclePrice>"
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 2:
    ///   "DashMap -- Best for 'update individual entries' pattern"
    pub oracle_prices: DashMap<Pubkey, OraclePrice>,

    /// Raw pool account data keyed by pool/vault pubkey.
    /// Stored as raw bytes for zero-copy parsing via bytemuck on the hot path.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 350:
    ///   "pool_states: DashMap<Pubkey, PoolSnapshot>"
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1:
    ///   "Raw Byte Slicing (Fastest -- 0 allocations)"
    pub pool_states: DashMap<Pubkey, Vec<u8>>,

    /// Latest blockhash from gRPC blocksMeta subscription.
    /// Atomically swapped on each new block (~every 400ms).
    ///
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 9:
    ///   "gRPC blocksMeta for fresh blockhash -- push-based, arrives within ~5ms"
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 2:
    ///   "ArcSwap -- Best for 'replace entire state' pattern"
    pub blockhash: ArcSwap<Hash>,

    /// Current SOL/USD price. Updated by oracle ingestion.
    /// Read by every strategy for USD-denominated profit calculations.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 353:
    ///   "sol_price: ArcSwap<f64>"
    pub sol_price: ArcSwap<f64>,

    /// Jito tip floor in lamports. Updated by tip estimation logic.
    /// Strategies read this to decide minimum tip for bundle submission.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1556:
    ///   "min_tip_lamports = 50000"
    pub tip_floor: AtomicU64,

    /// Current crash risk level (0=Green, 1=Yellow, 2=Orange, 3=Red).
    /// Updated by the crash prediction engine. Strategies use this to
    /// adjust scan intervals and tip amounts.
    ///
    /// [VERIFIED 2026] crash_prediction_2026.md lines 344-349
    pub risk_level: AtomicU8,

    /// Timestamp of the last blockhash update, for staleness detection.
    last_blockhash_update: ArcSwap<Instant>,

    /// Block height associated with the current blockhash, for expiry tracking.
    ///
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 9:
    ///   "Enhancement: Track Block Height for Expiry"
    pub block_height: AtomicU64,
}

impl SharedState {
    /// Create a new SharedState with default/empty values.
    pub fn new() -> Self {
        Self {
            oracle_prices: DashMap::new(),
            pool_states: DashMap::new(),
            blockhash: ArcSwap::new(Arc::new(Hash::default())),
            sol_price: ArcSwap::new(Arc::new(0.0)),
            tip_floor: AtomicU64::new(50_000), // 50K lamports default [PREDATOR_ARCHITECTURE_2026.md line 1556]
            risk_level: AtomicU8::new(0),       // Green
            last_blockhash_update: ArcSwap::new(Arc::new(Instant::now())),
            block_height: AtomicU64::new(0),
        }
    }

    // -----------------------------------------------------------------------
    // Oracle methods
    // -----------------------------------------------------------------------

    /// Insert or update an oracle price entry.
    ///
    /// Only updates if the new price is from a newer or equal slot to prevent
    /// stale data from overwriting fresher data (gRPC can deliver out-of-order).
    pub fn update_oracle(&self, feed_pubkey: Pubkey, price: OraclePrice) {
        // Check existing entry -- only update if slot is >= current
        if let Some(existing) = self.oracle_prices.get(&feed_pubkey) {
            if price.slot.0 < existing.slot.0 {
                return; // stale update, ignore
            }
        }
        self.oracle_prices.insert(feed_pubkey, price);
    }

    /// Get the oracle price for a given feed pubkey.
    #[inline]
    pub fn get_oracle(&self, feed_pubkey: &Pubkey) -> Option<OraclePrice> {
        self.oracle_prices.get(feed_pubkey).map(|r| *r)
    }

    /// Get the number of oracle feeds currently cached.
    pub fn oracle_count(&self) -> usize {
        self.oracle_prices.len()
    }

    // -----------------------------------------------------------------------
    // Pool state methods
    // -----------------------------------------------------------------------

    /// Insert or replace raw pool data for a given pool/vault pubkey.
    ///
    /// Strategies parse this data using bytemuck zero-copy on the hot path.
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1
    pub fn update_pool(&self, pool_pubkey: Pubkey, data: Vec<u8>) {
        self.pool_states.insert(pool_pubkey, data);
    }

    /// Get a reference guard to the raw pool data.
    ///
    /// Returns `None` if no data is cached for this pool.
    #[inline]
    pub fn get_pool(&self, pool_pubkey: &Pubkey) -> Option<dashmap::mapref::one::Ref<'_, Pubkey, Vec<u8>>> {
        self.pool_states.get(pool_pubkey)
    }

    /// Get the number of pool states currently cached.
    pub fn pool_count(&self) -> usize {
        self.pool_states.len()
    }

    // -----------------------------------------------------------------------
    // Blockhash methods
    // [VERIFIED 2026] low_latency_dataflow_2026.md Section 9
    // -----------------------------------------------------------------------

    /// Atomically update the cached blockhash and associated block height.
    ///
    /// Called by the gRPC blocksMeta handler on each new block (~every 400ms).
    pub fn update_blockhash(&self, hash: Hash, height: u64) {
        self.blockhash.store(Arc::new(hash));
        self.block_height.store(height, Ordering::Release);
        self.last_blockhash_update.store(Arc::new(Instant::now()));
    }

    /// Get the current cached blockhash. Wait-free read.
    ///
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 2:
    ///   "Reader latency: Wait-free reads (no contention, no locks, no CAS loops)"
    #[inline]
    pub fn get_blockhash(&self) -> Arc<Hash> {
        self.blockhash.load_full()
    }

    /// Get the block height associated with the current blockhash.
    #[inline]
    pub fn get_block_height(&self) -> u64 {
        self.block_height.load(Ordering::Acquire)
    }

    /// Check if the cached blockhash is stale (no update for > threshold).
    ///
    /// Blockhashes are valid for ~150 blocks (60-90 seconds).
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 9:
    ///   "Valid for: 150 blocks (NOT slots). Real-world duration: 60-90 seconds"
    pub fn is_blockhash_stale(&self, max_age: std::time::Duration) -> bool {
        let last_update = self.last_blockhash_update.load();
        last_update.elapsed() > max_age
    }

    // -----------------------------------------------------------------------
    // SOL price methods
    // -----------------------------------------------------------------------

    /// Atomically update the cached SOL/USD price.
    pub fn update_sol_price(&self, price: f64) {
        self.sol_price.store(Arc::new(price));
    }

    /// Get the current SOL/USD price. Wait-free read.
    #[inline]
    pub fn get_sol_price(&self) -> f64 {
        **self.sol_price.load()
    }

    // -----------------------------------------------------------------------
    // Tip floor methods
    // -----------------------------------------------------------------------

    /// Update the Jito tip floor in lamports.
    pub fn update_tip_floor(&self, lamports: u64) {
        self.tip_floor.store(lamports, Ordering::Release);
    }

    /// Get the current Jito tip floor in lamports.
    #[inline]
    pub fn get_tip_floor(&self) -> u64 {
        self.tip_floor.load(Ordering::Acquire)
    }

    // -----------------------------------------------------------------------
    // Risk level methods
    // [VERIFIED 2026] crash_prediction_2026.md lines 344-349
    // -----------------------------------------------------------------------

    /// Update the crash risk level (called by the crash prediction engine).
    ///
    /// [VERIFIED 2026] crash_prediction_2026.md lines 344-349: CrashRiskLevel
    pub fn update_risk_level(&self, level: CrashRiskLevel) {
        let val = match level {
            CrashRiskLevel::Green => 0u8,
            CrashRiskLevel::Yellow => 1u8,
            CrashRiskLevel::Orange => 2u8,
            CrashRiskLevel::Red => 3u8,
        };
        self.risk_level.store(val, Ordering::Release);
    }

    /// Get the current crash risk level.
    ///
    /// [VERIFIED 2026] crash_prediction_2026.md lines 344-349: CrashRiskLevel
    #[inline]
    pub fn get_risk_level(&self) -> CrashRiskLevel {
        match self.risk_level.load(Ordering::Acquire) {
            0 => CrashRiskLevel::Green,
            1 => CrashRiskLevel::Yellow,
            2 => CrashRiskLevel::Orange,
            _ => CrashRiskLevel::Red, // clamp unknown values to Red (safest)
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OracleSource, Slot};

    #[test]
    fn new_shared_state_has_sane_defaults() {
        let state = SharedState::new();
        assert_eq!(state.oracle_count(), 0);
        assert_eq!(state.pool_count(), 0);
        assert_eq!(*state.get_blockhash(), Hash::default());
        assert!((state.get_sol_price() - 0.0).abs() < f64::EPSILON);
        assert_eq!(state.get_tip_floor(), 50_000);
        assert_eq!(state.get_risk_level(), CrashRiskLevel::Green);
        assert_eq!(state.get_block_height(), 0);
    }

    #[test]
    fn update_and_get_oracle() {
        let state = SharedState::new();
        let feed = Pubkey::new_unique();
        let price = OraclePrice {
            price_f64: 80.50,
            confidence: 0.05,
            expo: -8,
            slot: Slot(100),
            source: OracleSource::PythOnChain,
        };

        state.update_oracle(feed, price);
        let got = state.get_oracle(&feed).unwrap();
        assert!((got.price_f64 - 80.50).abs() < f64::EPSILON);
        assert_eq!(got.slot.0, 100);
        assert_eq!(state.oracle_count(), 1);
    }

    #[test]
    fn oracle_rejects_stale_slot() {
        let state = SharedState::new();
        let feed = Pubkey::new_unique();

        // Insert at slot 100
        let fresh = OraclePrice {
            price_f64: 80.0,
            confidence: 0.05,
            expo: -8,
            slot: Slot(100),
            source: OracleSource::PythOnChain,
        };
        state.update_oracle(feed, fresh);

        // Try to overwrite with slot 50 -- should be rejected
        let stale = OraclePrice {
            price_f64: 70.0,
            confidence: 0.05,
            expo: -8,
            slot: Slot(50),
            source: OracleSource::PythOnChain,
        };
        state.update_oracle(feed, stale);

        // Should still have the fresh price
        let got = state.get_oracle(&feed).unwrap();
        assert!((got.price_f64 - 80.0).abs() < f64::EPSILON);
    }

    #[test]
    fn update_and_get_pool() {
        let state = SharedState::new();
        let pool = Pubkey::new_unique();
        let data = vec![1u8, 2, 3, 4, 5];

        state.update_pool(pool, data.clone());
        let got = state.get_pool(&pool).unwrap();
        assert_eq!(got.value(), &data);
        assert_eq!(state.pool_count(), 1);
    }

    #[test]
    fn update_and_get_blockhash() {
        let state = SharedState::new();
        let hash = Hash::new_unique();
        state.update_blockhash(hash, 42);

        assert_eq!(*state.get_blockhash(), hash);
        assert_eq!(state.get_block_height(), 42);
        assert!(!state.is_blockhash_stale(std::time::Duration::from_secs(10)));
    }

    #[test]
    fn update_and_get_sol_price() {
        let state = SharedState::new();
        state.update_sol_price(82.30);
        assert!((state.get_sol_price() - 82.30).abs() < f64::EPSILON);
    }

    #[test]
    fn update_and_get_tip_floor() {
        let state = SharedState::new();
        state.update_tip_floor(100_000);
        assert_eq!(state.get_tip_floor(), 100_000);
    }

    #[test]
    fn crash_risk_level_roundtrip() {
        let state = SharedState::new();

        state.update_risk_level(CrashRiskLevel::Yellow);
        assert_eq!(state.get_risk_level(), CrashRiskLevel::Yellow);

        state.update_risk_level(CrashRiskLevel::Red);
        assert_eq!(state.get_risk_level(), CrashRiskLevel::Red);

        state.update_risk_level(CrashRiskLevel::Green);
        assert_eq!(state.get_risk_level(), CrashRiskLevel::Green);
    }

    // CrashRiskLevel tests (from_score, scan_interval, display, ordering) are
    // in events.rs where the type is defined.
    // [VERIFIED 2026] crash_prediction_2026.md lines 344-349: CrashRiskLevel definition
    // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1087-1098: CrashPredictionEngine

    #[test]
    fn get_missing_oracle_returns_none() {
        let state = SharedState::new();
        assert!(state.get_oracle(&Pubkey::new_unique()).is_none());
    }

    #[test]
    fn get_missing_pool_returns_none() {
        let state = SharedState::new();
        assert!(state.get_pool(&Pubkey::new_unique()).is_none());
    }
}
