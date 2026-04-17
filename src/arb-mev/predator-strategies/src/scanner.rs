//! Unified tiered obligation scanner for multi-protocol liquidation monitoring.
//!
//! Implements the three-tier obligation cache pattern from research:
//!
//! - **HOT** (health 0.9-1.1): checked on every oracle price update (~400ms)
//! - **WARM** (health 1.1-1.3): checked every 30 seconds via batched RPC
//! - **COLD** (health > 1.3): checked every 5 minutes via getProgramAccounts
//!
//! Obligations are promoted/demoted between tiers as their health factors change.
//! Expired entries (not refreshed within TTL) are evicted.
//!
//! [VERIFIED 2026] scanner_deep_research_2026.md Section 3:
//!   "Strategy: Tiered obligation cache"
//!   "Tier 1 (HOT): Health ratio 0.9-1.1 -- check every oracle update (~400ms)"
//!   "Tier 2 (WARM): Health ratio 1.1-1.3 -- check every 30 seconds"
//!   "Tier 3 (COLD): Health ratio > 1.3 -- check every 5 minutes"
//! [VERIFIED 2026] scanner_deep_research_2026.md:
//!   "Don't scan ALL obligations. Only monitor the ones CLOSE to liquidation."
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 139-141:
//!   "LiquidationStrategy: on oracle price change, re-check cached obligations"
//! [VERIFIED 2026] bot_architecture_deep_2026.md Section 1a:
//!   "Data flows in ONE direction through typed channels"

use std::time::{Duration, Instant};

use dashmap::DashMap;
use solana_sdk::pubkey::Pubkey;

use predator_core::{HealthFactor, Lamports, Protocol};
use predator_protocols::ProtocolAdapter;

// ---------------------------------------------------------------------------
// Cache TTL and tier thresholds
// [VERIFIED 2026] scanner_deep_research_2026.md Section 3
// ---------------------------------------------------------------------------

/// Maximum age before an obligation is evicted from cache if not refreshed.
/// Set to 300 seconds (5 minutes) -- obligations not seen in a full COLD
/// scan cycle are stale and should be re-discovered.
/// [VERIFIED 2026] scanner_deep_research_2026.md: "check every 5 minutes"
const CACHE_TTL_SECS: u64 = 300;

/// Health factor threshold between HOT and WARM tiers.
/// Obligations with HF < 1.1 are HOT (checked every oracle update).
/// [VERIFIED 2026] scanner_deep_research_2026.md: "Health ratio 0.9-1.1"
const HOT_THRESHOLD: f64 = 1.1;

/// Health factor threshold between WARM and COLD tiers.
/// Obligations with HF 1.1-1.3 are WARM (checked every 30s).
/// [VERIFIED 2026] scanner_deep_research_2026.md: "Health ratio 1.1-1.3"
const WARM_THRESHOLD: f64 = 1.3;

/// Scan interval for WARM tier obligations (seconds).
/// [VERIFIED 2026] scanner_deep_research_2026.md: "check every 30 seconds"
const WARM_SCAN_INTERVAL_SECS: u64 = 30;

/// Scan interval for COLD tier obligations (seconds).
/// [VERIFIED 2026] scanner_deep_research_2026.md: "check every 5 minutes"
const COLD_SCAN_INTERVAL_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// ObligationTier
// [VERIFIED 2026] scanner_deep_research_2026.md: "Tier 1 (HOT) / Tier 2 (WARM) / Tier 3 (COLD)"
// ---------------------------------------------------------------------------

/// Tier classification for obligation monitoring frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObligationTier {
    /// Health 0.9-1.1: checked on every oracle update (~400ms).
    /// These are the positions closest to liquidation.
    /// [VERIFIED 2026] scanner_deep_research_2026.md: Tier 1
    Hot,

    /// Health 1.1-1.3: checked every 30 seconds via batched RPC.
    /// These positions could become HOT with a 10-20% price move.
    /// [VERIFIED 2026] scanner_deep_research_2026.md: Tier 2
    Warm,

    /// Health > 1.3: checked every 5 minutes.
    /// Safe positions that only become relevant during crashes.
    /// [VERIFIED 2026] scanner_deep_research_2026.md: Tier 3
    Cold,
}

impl ObligationTier {
    /// Classify an obligation into a tier based on its health factor.
    ///
    /// [VERIFIED 2026] scanner_deep_research_2026.md Section 3:
    ///   HOT: 0.9-1.1, WARM: 1.1-1.3, COLD: >1.3
    pub fn from_health(health: f64) -> Self {
        if health < HOT_THRESHOLD {
            ObligationTier::Hot
        } else if health < WARM_THRESHOLD {
            ObligationTier::Warm
        } else {
            ObligationTier::Cold
        }
    }

    /// Scan interval for this tier in seconds.
    pub fn scan_interval_secs(&self) -> u64 {
        match self {
            ObligationTier::Hot => 0, // checked on every oracle update
            ObligationTier::Warm => WARM_SCAN_INTERVAL_SECS,
            ObligationTier::Cold => COLD_SCAN_INTERVAL_SECS,
        }
    }
}

// ---------------------------------------------------------------------------
// CachedObligation
// [VERIFIED 2026] scanner_deep_research_2026.md: CachedObligation struct
// ---------------------------------------------------------------------------

/// Cached obligation data for tiered scanning.
///
/// Stores the parsed health data plus metadata for tier management.
/// Raw obligation bytes are NOT stored here -- they live in SharedState
/// or are fetched on-demand from RPC.
///
/// [VERIFIED 2026] scanner_deep_research_2026.md Section 3:
///   "For each obligation, maintain: pubkey, borrowed_value, unhealthy_value,
///    health_ratio, deposit_reserves, borrow_reserves, last_checked, tier"
#[derive(Debug, Clone)]
pub struct CachedObligation {
    /// On-chain address of the obligation account.
    pub pubkey: Pubkey,
    /// Which protocol this obligation belongs to.
    pub protocol: Protocol,
    /// Lending market this obligation belongs to.
    pub market: Pubkey,
    /// Total borrow value in USD.
    pub borrowed_value: f64,
    /// Unhealthy borrow threshold in USD.
    pub unhealthy_value: f64,
    /// Computed health ratio (unhealthy / borrowed). < 1.0 = liquidatable.
    pub health_factor: HealthFactor,
    /// Deposit reserve pubkeys (for oracle -> obligation mapping).
    pub deposit_reserves: Vec<Pubkey>,
    /// Borrow reserve pubkeys (for oracle -> obligation mapping).
    pub borrow_reserves: Vec<Pubkey>,
    /// When this obligation was last checked/refreshed.
    pub last_checked: Instant,
    /// Current tier classification.
    pub tier: ObligationTier,
}

impl CachedObligation {
    /// Check if this cache entry has expired (not refreshed within TTL).
    ///
    /// [VERIFIED 2026] scanner_deep_research_2026.md: "expire positions
    ///  after 300s if not refreshed"
    pub fn is_expired(&self) -> bool {
        self.last_checked.elapsed() > Duration::from_secs(CACHE_TTL_SECS)
    }

    /// Check if this obligation is due for a tier-based re-scan.
    pub fn needs_scan(&self) -> bool {
        let interval = self.tier.scan_interval_secs();
        if interval == 0 {
            // HOT tier: always needs scan (driven by oracle updates, not timer)
            return false;
        }
        self.last_checked.elapsed() > Duration::from_secs(interval)
    }

    /// Reclassify this obligation's tier based on its current health factor.
    /// Returns true if the tier changed.
    ///
    /// [VERIFIED 2026] scanner_deep_research_2026.md:
    ///   "Reclassify tier if health ratio changed significantly"
    pub fn reclassify(&mut self) -> bool {
        let new_tier = ObligationTier::from_health(self.health_factor.0);
        if new_tier != self.tier {
            self.tier = new_tier;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Opportunity -- a detected liquidation opportunity
// ---------------------------------------------------------------------------

/// A detected liquidation opportunity ready for execution.
///
/// Produced by the scanner when an obligation's health factor drops below 1.0.
/// Contains all information needed to build a liquidation bundle.
#[derive(Debug, Clone)]
pub struct Opportunity {
    /// Which protocol this opportunity is on.
    pub protocol: Protocol,
    /// The obligation account to liquidate.
    pub obligation: Pubkey,
    /// Current health factor (< 1.0 = liquidatable).
    pub health: HealthFactor,
    /// Estimated profit in lamports (gross, before Jito tip and fees).
    pub est_profit: Lamports,
    /// Lending market that owns this obligation.
    pub market: Pubkey,
    /// Debt value in USD (for sizing the liquidation amount).
    pub debt_usd: f64,
}

// ---------------------------------------------------------------------------
// UnifiedScanner
// [VERIFIED 2026] scanner_deep_research_2026.md: "Efficient Obligation Monitoring"
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 144: "UnifiedScanner"
// ---------------------------------------------------------------------------

/// Unified scanner managing obligation caches for all lending protocols.
///
/// Maintains a `DashMap` of `CachedObligation` entries keyed by obligation
/// pubkey. The scanner is responsible for:
/// 1. Populating the cache from periodic `getProgramAccounts` scans (COLD)
/// 2. Refreshing WARM entries via batched `getMultipleAccounts` every 30s
/// 3. Providing HOT entries for instant re-evaluation on oracle price changes
/// 4. Promoting/demoting entries between tiers as health changes
/// 5. Evicting expired entries (TTL = 300s)
///
/// [VERIFIED 2026] scanner_deep_research_2026.md Section 3:
///   "Don't scan ALL obligations. Only monitor the ones CLOSE to liquidation."
pub struct UnifiedScanner {
    /// All cached obligations, keyed by obligation pubkey.
    obligations: DashMap<Pubkey, CachedObligation>,

    /// Mapping from reserve pubkey to set of obligation pubkeys that reference it.
    /// Used for efficient "oracle changed -> which obligations are affected?" lookup.
    ///
    /// [VERIFIED 2026] scanner_deep_research_2026.md:
    ///   "1. Identify which reserves use this oracle
    ///    2. Find all HOT obligations that borrow/deposit those reserves"
    reserve_to_obligations: DashMap<Pubkey, Vec<Pubkey>>,

    /// Last time each tier was fully scanned.
    last_warm_scan: Instant,
    last_cold_scan: Instant,
}

impl UnifiedScanner {
    /// Create a new empty scanner.
    pub fn new() -> Self {
        Self {
            obligations: DashMap::new(),
            reserve_to_obligations: DashMap::new(),
            last_warm_scan: Instant::now(),
            last_cold_scan: Instant::now(),
        }
    }

    /// Insert or update an obligation in the cache.
    ///
    /// Automatically classifies into the correct tier and updates the
    /// reserve -> obligation index.
    pub fn upsert(&self, obl: CachedObligation) {
        let pubkey = obl.pubkey;

        // Update reserve -> obligation index
        for reserve in obl.deposit_reserves.iter().chain(obl.borrow_reserves.iter()) {
            self.reserve_to_obligations
                .entry(*reserve)
                .or_insert_with(Vec::new)
                .push(pubkey);
        }

        self.obligations.insert(pubkey, obl);
    }

    /// Get all HOT tier obligations that reference a given reserve.
    ///
    /// Used on oracle price change: find all near-liquidation positions
    /// affected by this price feed, then re-evaluate their health.
    ///
    /// [VERIFIED 2026] scanner_deep_research_2026.md:
    ///   "Find all HOT obligations that borrow/deposit those reserves"
    pub fn get_hot_for_reserve(&self, reserve: &Pubkey) -> Vec<CachedObligation> {
        let obl_keys = match self.reserve_to_obligations.get(reserve) {
            Some(keys) => keys.clone(),
            None => return Vec::new(),
        };

        obl_keys
            .iter()
            .filter_map(|key| {
                self.obligations.get(key).and_then(|obl| {
                    if obl.tier == ObligationTier::Hot {
                        Some(obl.clone())
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Get all obligations in a specific tier.
    pub fn get_by_tier(&self, tier: ObligationTier) -> Vec<CachedObligation> {
        self.obligations
            .iter()
            .filter(|entry| entry.value().tier == tier)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Scan all obligations using the provided protocol adapters.
    ///
    /// Iterates over all cached obligations, re-evaluates health using
    /// the appropriate adapter's `parse_health`, and returns any
    /// newly-liquidatable opportunities.
    ///
    /// This is NOT the full RPC scan -- it re-evaluates cached data.
    /// Full RPC scans are triggered by the orchestrator on a timer.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 140:
    ///   "scan_all(rpc, protocols) -> Vec<Opportunity>"
    pub fn scan_all(
        &self,
        adapters: &[Box<dyn ProtocolAdapter>],
        sol_price: f64,
    ) -> Vec<Opportunity> {
        let mut opportunities = Vec::new();

        for entry in self.obligations.iter() {
            let obl = entry.value();

            // Skip expired entries
            if obl.is_expired() {
                continue;
            }

            // Only check obligations that are near liquidation
            if !obl.health_factor.is_liquidatable() {
                continue;
            }

            // Find matching adapter for this protocol
            let adapter = match adapters.iter().find(|a| a.protocol() == obl.protocol) {
                Some(a) => a,
                None => continue,
            };

            // Estimate profit based on protocol bonus and debt value
            let bonus_bps = adapter.get_bonus_bps();
            let close_factor = adapter.get_close_factor();
            let max_repay_usd = obl.borrowed_value * close_factor;
            let bonus_usd = max_repay_usd * bonus_bps.as_fraction();

            // Convert USD profit to lamports (rough estimate)
            let profit_lamports = if sol_price > 0.0 {
                ((bonus_usd / sol_price) * 1_000_000_000.0) as u64
            } else {
                0
            };

            opportunities.push(Opportunity {
                protocol: obl.protocol,
                obligation: obl.pubkey,
                health: obl.health_factor,
                est_profit: Lamports(profit_lamports),
                market: obl.market,
                debt_usd: obl.borrowed_value,
            });
        }

        // Sort by estimated profit descending (most profitable first)
        opportunities.sort_by(|a, b| b.est_profit.cmp(&a.est_profit));
        opportunities
    }

    /// Promote or demote obligations between tiers based on updated health.
    ///
    /// Called after re-evaluating health factors from new oracle prices.
    /// Returns the number of obligations that changed tier.
    ///
    /// [VERIFIED 2026] scanner_deep_research_2026.md:
    ///   "Reclassify tier if health ratio changed significantly"
    pub fn reclassify_all(&self) -> usize {
        let mut changed = 0;
        for mut entry in self.obligations.iter_mut() {
            if entry.value_mut().reclassify() {
                changed += 1;
            }
        }
        changed
    }

    /// Remove expired obligations from the cache.
    ///
    /// Returns the number of evicted entries.
    /// [VERIFIED 2026] scanner_deep_research_2026.md: cache TTL 300s
    pub fn evict_expired(&self) -> usize {
        let before = self.obligations.len();
        self.obligations.retain(|_, obl| !obl.is_expired());
        let after = self.obligations.len();
        before - after
    }

    /// Check if the WARM tier is due for a re-scan.
    pub fn warm_scan_due(&self) -> bool {
        self.last_warm_scan.elapsed() > Duration::from_secs(WARM_SCAN_INTERVAL_SECS)
    }

    /// Check if the COLD tier is due for a re-scan.
    pub fn cold_scan_due(&self) -> bool {
        self.last_cold_scan.elapsed() > Duration::from_secs(COLD_SCAN_INTERVAL_SECS)
    }

    /// Mark that a WARM scan was just completed.
    pub fn mark_warm_scanned(&mut self) {
        self.last_warm_scan = Instant::now();
    }

    /// Mark that a COLD scan was just completed.
    pub fn mark_cold_scanned(&mut self) {
        self.last_cold_scan = Instant::now();
    }

    /// Total number of cached obligations.
    pub fn total_count(&self) -> usize {
        self.obligations.len()
    }

    /// Count of obligations in each tier.
    pub fn tier_counts(&self) -> (usize, usize, usize) {
        let mut hot = 0;
        let mut warm = 0;
        let mut cold = 0;
        for entry in self.obligations.iter() {
            match entry.value().tier {
                ObligationTier::Hot => hot += 1,
                ObligationTier::Warm => warm += 1,
                ObligationTier::Cold => cold += 1,
            }
        }
        (hot, warm, cold)
    }

    /// Get a specific obligation by pubkey.
    pub fn get(&self, pubkey: &Pubkey) -> Option<CachedObligation> {
        self.obligations.get(pubkey).map(|r| r.clone())
    }

    /// Update the health factor and last_checked timestamp for an obligation.
    /// Returns true if the obligation was found and updated.
    pub fn update_health(&self, pubkey: &Pubkey, borrowed: f64, unhealthy: f64) -> bool {
        if let Some(mut entry) = self.obligations.get_mut(pubkey) {
            let health = if borrowed > 0.0 {
                unhealthy / borrowed
            } else {
                f64::MAX
            };
            entry.borrowed_value = borrowed;
            entry.unhealthy_value = unhealthy;
            entry.health_factor = HealthFactor(health);
            entry.last_checked = Instant::now();
            entry.reclassify();
            true
        } else {
            false
        }
    }
}

impl Default for UnifiedScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_obligation(health: f64, protocol: Protocol) -> CachedObligation {
        CachedObligation {
            pubkey: Pubkey::new_unique(),
            protocol,
            market: Pubkey::new_unique(),
            borrowed_value: 1000.0,
            unhealthy_value: 1000.0 * health,
            health_factor: HealthFactor(health),
            deposit_reserves: vec![Pubkey::new_unique()],
            borrow_reserves: vec![Pubkey::new_unique()],
            last_checked: Instant::now(),
            tier: ObligationTier::from_health(health),
        }
    }

    #[test]
    fn tier_classification() {
        assert_eq!(ObligationTier::from_health(0.5), ObligationTier::Hot);
        assert_eq!(ObligationTier::from_health(0.95), ObligationTier::Hot);
        assert_eq!(ObligationTier::from_health(1.0), ObligationTier::Hot);
        assert_eq!(ObligationTier::from_health(1.09), ObligationTier::Hot);
        assert_eq!(ObligationTier::from_health(1.1), ObligationTier::Warm);
        assert_eq!(ObligationTier::from_health(1.2), ObligationTier::Warm);
        assert_eq!(ObligationTier::from_health(1.29), ObligationTier::Warm);
        assert_eq!(ObligationTier::from_health(1.3), ObligationTier::Cold);
        assert_eq!(ObligationTier::from_health(2.0), ObligationTier::Cold);
    }

    #[test]
    fn upsert_and_get() {
        let scanner = UnifiedScanner::new();
        let obl = make_obligation(0.95, Protocol::Save);
        let pk = obl.pubkey;

        scanner.upsert(obl);
        assert_eq!(scanner.total_count(), 1);

        let got = scanner.get(&pk).unwrap();
        assert!(got.health_factor.is_liquidatable());
        assert_eq!(got.tier, ObligationTier::Hot);
    }

    #[test]
    fn tier_counts() {
        let scanner = UnifiedScanner::new();
        scanner.upsert(make_obligation(0.95, Protocol::Save)); // HOT
        scanner.upsert(make_obligation(1.05, Protocol::Kamino)); // HOT
        scanner.upsert(make_obligation(1.2, Protocol::Save)); // WARM
        scanner.upsert(make_obligation(1.5, Protocol::Kamino)); // COLD
        scanner.upsert(make_obligation(2.0, Protocol::MarginFi)); // COLD

        let (hot, warm, cold) = scanner.tier_counts();
        assert_eq!(hot, 2);
        assert_eq!(warm, 1);
        assert_eq!(cold, 2);
    }

    #[test]
    fn update_health_reclassifies() {
        let scanner = UnifiedScanner::new();
        let obl = make_obligation(1.5, Protocol::Save);
        let pk = obl.pubkey;

        scanner.upsert(obl);
        assert_eq!(scanner.get(&pk).unwrap().tier, ObligationTier::Cold);

        // Health drops to HOT tier
        scanner.update_health(&pk, 1000.0, 950.0);
        let updated = scanner.get(&pk).unwrap();
        assert_eq!(updated.tier, ObligationTier::Hot);
        assert!(updated.health_factor.is_liquidatable());
    }

    #[test]
    fn get_hot_for_reserve() {
        let scanner = UnifiedScanner::new();
        let shared_reserve = Pubkey::new_unique();

        let mut hot_obl = make_obligation(0.95, Protocol::Save);
        hot_obl.deposit_reserves = vec![shared_reserve];
        let hot_pk = hot_obl.pubkey;

        let mut cold_obl = make_obligation(2.0, Protocol::Save);
        cold_obl.deposit_reserves = vec![shared_reserve];

        scanner.upsert(hot_obl);
        scanner.upsert(cold_obl);

        let hot_results = scanner.get_hot_for_reserve(&shared_reserve);
        assert_eq!(hot_results.len(), 1);
        assert_eq!(hot_results[0].pubkey, hot_pk);
    }

    #[test]
    fn obligation_expiry() {
        let scanner = UnifiedScanner::new();
        let mut obl = make_obligation(0.95, Protocol::Save);
        // Manually set last_checked to far in the past
        obl.last_checked = Instant::now() - Duration::from_secs(CACHE_TTL_SECS + 10);
        scanner.upsert(obl);

        assert_eq!(scanner.total_count(), 1);
        let evicted = scanner.evict_expired();
        assert_eq!(evicted, 1);
        assert_eq!(scanner.total_count(), 0);
    }

    #[test]
    fn warm_cold_scan_timing() {
        let scanner = UnifiedScanner::new();
        // Freshly created scanner should not be due for scan
        assert!(!scanner.warm_scan_due());
        assert!(!scanner.cold_scan_due());
    }

    #[test]
    fn reclassify_all() {
        let scanner = UnifiedScanner::new();
        let obl = make_obligation(1.5, Protocol::Save);
        let pk = obl.pubkey;
        scanner.upsert(obl);

        // Manually change health without reclassifying
        if let Some(mut entry) = scanner.obligations.get_mut(&pk) {
            entry.health_factor = HealthFactor(0.95);
        }

        let changed = scanner.reclassify_all();
        assert_eq!(changed, 1);
        assert_eq!(scanner.get(&pk).unwrap().tier, ObligationTier::Hot);
    }
}
