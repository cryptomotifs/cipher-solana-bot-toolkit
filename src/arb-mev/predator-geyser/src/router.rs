//! EventRouter — O(1) FxHashMap lookup for dispatching gRPC updates to strategies.
//!
//! The router maintains a map of `Pubkey -> AccountType`, enabling filtered dispatch:
//! each gRPC account update is routed ONLY to the strategy that handles that account
//! type. No broadcast, no wasted clones.
//!
//! [VERIFIED 2026] low_latency_dataflow_2026.md Section 3 (Pattern B: Filtered dispatch):
//!   "One mpsc per strategy, router does O(1) lookup to determine destination.
//!    No wasted clones — events go only to the strategy that handles that account type."
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 215-217:
//!   "EventRouter: FxHashMap<Pubkey, AccountType> O(1) lookup.
//!    Routes decoded events to correct strategy mpsc channel.
//!    Filtered dispatch (not broadcast) — no wasted clones."
//!
//! [VERIFIED 2026] low_latency_dataflow_2026.md Section 5:
//!   "Our EventRouter uses FxHashMap lookup (O(1)) which is already optimal."

use std::fmt;

use rustc_hash::FxHashMap;
use solana_sdk::pubkey::Pubkey;

use crate::subscriber::AccountUpdate;

// ---------------------------------------------------------------------------
// AccountType enum
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 215:
//   "AccountType enum (Oracle, PoolVault, Obligation, VaultState, Other)"
// ---------------------------------------------------------------------------

/// Classification of a Solana account by its role in our strategies.
///
/// The EventRouter uses this to determine which strategy channel receives each update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountType {
    /// Pyth/Switchboard oracle price feed.
    /// Routed to: LiquidationStrategy, FlashArbStrategy (price-dependent).
    Oracle,

    /// DEX pool vault account (token reserve).
    /// Routed to: BackrunStrategy (detects balance changes from swaps).
    PoolVault,

    /// Lending protocol obligation/position account.
    /// Routed to: LiquidationStrategy (health factor tracking).
    Obligation,

    /// Lending protocol vault state (e.g., JupLend vault, Save reserve).
    /// Routed to: LiquidationStrategy (utilization, interest rate).
    VaultState,

    /// LST pool or stake pool account.
    /// Routed to: LstArbStrategy (depeg detection).
    LstPool,

    /// Account type not mapped to any strategy — will be dropped.
    Other,
}

impl fmt::Display for AccountType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountType::Oracle => write!(f, "Oracle"),
            AccountType::PoolVault => write!(f, "PoolVault"),
            AccountType::Obligation => write!(f, "Obligation"),
            AccountType::VaultState => write!(f, "VaultState"),
            AccountType::LstPool => write!(f, "LstPool"),
            AccountType::Other => write!(f, "Other"),
        }
    }
}

// ---------------------------------------------------------------------------
// RoutedEvent enum
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 216:
//   "RoutedEvent enum with typed variants per AccountType"
// ---------------------------------------------------------------------------

/// A routed event — an AccountUpdate tagged with its classification.
///
/// Strategies receive RoutedEvent instead of raw AccountUpdate, so they know
/// the account type without re-checking the routing table.
#[derive(Debug, Clone)]
pub enum RoutedEvent {
    /// Oracle price feed update.
    OracleUpdate {
        pubkey: Pubkey,
        data: Vec<u8>,
        slot: u64,
        write_version: u64,
    },

    /// DEX pool vault balance change.
    PoolVaultUpdate {
        pubkey: Pubkey,
        data: Vec<u8>,
        slot: u64,
        write_version: u64,
    },

    /// Lending obligation/position change.
    ObligationUpdate {
        pubkey: Pubkey,
        data: Vec<u8>,
        slot: u64,
        write_version: u64,
    },

    /// Lending vault state change.
    VaultStateUpdate {
        pubkey: Pubkey,
        data: Vec<u8>,
        slot: u64,
        write_version: u64,
    },

    /// LST pool state change.
    LstPoolUpdate {
        pubkey: Pubkey,
        data: Vec<u8>,
        slot: u64,
        write_version: u64,
    },
}

impl RoutedEvent {
    /// Returns the event's account type for logging.
    pub fn account_type(&self) -> AccountType {
        match self {
            RoutedEvent::OracleUpdate { .. } => AccountType::Oracle,
            RoutedEvent::PoolVaultUpdate { .. } => AccountType::PoolVault,
            RoutedEvent::ObligationUpdate { .. } => AccountType::Obligation,
            RoutedEvent::VaultStateUpdate { .. } => AccountType::VaultState,
            RoutedEvent::LstPoolUpdate { .. } => AccountType::LstPool,
        }
    }

    /// Returns the slot number for ordering/dedup.
    pub fn slot(&self) -> u64 {
        match self {
            RoutedEvent::OracleUpdate { slot, .. }
            | RoutedEvent::PoolVaultUpdate { slot, .. }
            | RoutedEvent::ObligationUpdate { slot, .. }
            | RoutedEvent::VaultStateUpdate { slot, .. }
            | RoutedEvent::LstPoolUpdate { slot, .. } => *slot,
        }
    }

    /// Returns the pubkey for the account that was updated.
    pub fn pubkey(&self) -> &Pubkey {
        match self {
            RoutedEvent::OracleUpdate { pubkey, .. }
            | RoutedEvent::PoolVaultUpdate { pubkey, .. }
            | RoutedEvent::ObligationUpdate { pubkey, .. }
            | RoutedEvent::VaultStateUpdate { pubkey, .. }
            | RoutedEvent::LstPoolUpdate { pubkey, .. } => pubkey,
        }
    }
}

// ---------------------------------------------------------------------------
// EventRouter
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 215:
//   "EventRouter: FxHashMap<Pubkey, AccountType> O(1) lookup"
// [VERIFIED 2026] low_latency_dataflow_2026.md Section 3:
//   "Pattern B: Filtered dispatch — Router decides who gets what"
// ---------------------------------------------------------------------------

/// Routes gRPC account updates to strategy-specific channels.
///
/// Uses FxHashMap for O(1) lookup (FxHash is 2-3x faster than std HashMap for
/// fixed-size keys like Pubkey).
///
/// **Filtered dispatch**: Only the strategy that handles a given account type
/// receives the event. Other strategies never see it. This eliminates the
/// overhead of broadcast + filter-at-consumer that wastes CPU on irrelevant events.
pub struct EventRouter {
    /// Pubkey -> AccountType mapping for O(1) classification.
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 5:
    ///   "Our EventRouter uses FxHashMap lookup (O(1)) which is already optimal"
    routing_table: FxHashMap<Pubkey, AccountType>,
}

impl EventRouter {
    /// Create a new EventRouter with an empty routing table.
    pub fn new() -> Self {
        Self {
            routing_table: FxHashMap::default(),
        }
    }

    /// Create a new EventRouter pre-allocated for `capacity` entries.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            routing_table: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
        }
    }

    /// Register a pubkey with its account type.
    ///
    /// After registration, any AccountUpdate with this pubkey will be routed
    /// to the appropriate strategy channel.
    pub fn register(&mut self, pubkey: Pubkey, account_type: AccountType) {
        self.routing_table.insert(pubkey, account_type);
    }

    /// Unregister a pubkey (e.g., when an obligation is repaid or a pool is removed).
    pub fn unregister(&mut self, pubkey: &Pubkey) {
        self.routing_table.remove(pubkey);
    }

    /// Classify an account update and produce a typed RoutedEvent.
    ///
    /// Returns `None` for accounts not in the routing table or classified as `Other`.
    /// This is the hot-path function — must be O(1) with minimal overhead.
    ///
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 5:
    ///   "FxHashMap lookup — O(1), ~30-50ns per lookup"
    #[inline]
    pub fn route(&self, update: &AccountUpdate) -> Option<RoutedEvent> {
        let account_type = self.routing_table.get(&update.pubkey)?;

        match account_type {
            AccountType::Oracle => Some(RoutedEvent::OracleUpdate {
                pubkey: update.pubkey,
                data: update.data.clone(),
                slot: update.slot,
                write_version: update.write_version,
            }),
            AccountType::PoolVault => Some(RoutedEvent::PoolVaultUpdate {
                pubkey: update.pubkey,
                data: update.data.clone(),
                slot: update.slot,
                write_version: update.write_version,
            }),
            AccountType::Obligation => Some(RoutedEvent::ObligationUpdate {
                pubkey: update.pubkey,
                data: update.data.clone(),
                slot: update.slot,
                write_version: update.write_version,
            }),
            AccountType::VaultState => Some(RoutedEvent::VaultStateUpdate {
                pubkey: update.pubkey,
                data: update.data.clone(),
                slot: update.slot,
                write_version: update.write_version,
            }),
            AccountType::LstPool => Some(RoutedEvent::LstPoolUpdate {
                pubkey: update.pubkey,
                data: update.data.clone(),
                slot: update.slot,
                write_version: update.write_version,
            }),
            AccountType::Other => None,
        }
    }

    /// Get the account type for a pubkey without producing an event.
    #[inline]
    pub fn classify(&self, pubkey: &Pubkey) -> Option<AccountType> {
        self.routing_table.get(pubkey).copied()
    }

    /// Number of entries in the routing table.
    pub fn len(&self) -> usize {
        self.routing_table.len()
    }

    /// Whether the routing table is empty.
    pub fn is_empty(&self) -> bool {
        self.routing_table.is_empty()
    }
}

impl Default for EventRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_update(pubkey: Pubkey) -> AccountUpdate {
        AccountUpdate {
            pubkey,
            data: vec![1, 2, 3, 4],
            slot: 100,
            write_version: 1,
        }
    }

    #[test]
    fn register_and_route() {
        let mut router = EventRouter::new();
        let oracle_pk = Pubkey::new_unique();
        let pool_pk = Pubkey::new_unique();
        let unknown_pk = Pubkey::new_unique();

        router.register(oracle_pk, AccountType::Oracle);
        router.register(pool_pk, AccountType::PoolVault);

        // Oracle update routed correctly
        let event = router.route(&make_update(oracle_pk)).unwrap();
        assert_eq!(event.account_type(), AccountType::Oracle);
        assert_eq!(*event.pubkey(), oracle_pk);
        assert_eq!(event.slot(), 100);

        // Pool vault update routed correctly
        let event = router.route(&make_update(pool_pk)).unwrap();
        assert_eq!(event.account_type(), AccountType::PoolVault);

        // Unknown pubkey returns None
        assert!(router.route(&make_update(unknown_pk)).is_none());
    }

    #[test]
    fn unregister_stops_routing() {
        let mut router = EventRouter::new();
        let pk = Pubkey::new_unique();

        router.register(pk, AccountType::Obligation);
        assert!(router.route(&make_update(pk)).is_some());

        router.unregister(&pk);
        assert!(router.route(&make_update(pk)).is_none());
    }

    #[test]
    fn other_type_returns_none() {
        let mut router = EventRouter::new();
        let pk = Pubkey::new_unique();

        router.register(pk, AccountType::Other);
        assert!(
            router.route(&make_update(pk)).is_none(),
            "AccountType::Other should not produce a RoutedEvent"
        );
    }

    #[test]
    fn classify_returns_type() {
        let mut router = EventRouter::new();
        let pk = Pubkey::new_unique();

        router.register(pk, AccountType::VaultState);
        assert_eq!(router.classify(&pk), Some(AccountType::VaultState));
        assert_eq!(router.classify(&Pubkey::new_unique()), None);
    }

    #[test]
    fn len_and_is_empty() {
        let mut router = EventRouter::new();
        assert!(router.is_empty());
        assert_eq!(router.len(), 0);

        router.register(Pubkey::new_unique(), AccountType::Oracle);
        assert!(!router.is_empty());
        assert_eq!(router.len(), 1);
    }

    #[test]
    fn with_capacity_works() {
        let router = EventRouter::with_capacity(100);
        assert!(router.is_empty());
    }

    #[test]
    fn all_account_types_route() {
        let mut router = EventRouter::new();

        let types = [
            AccountType::Oracle,
            AccountType::PoolVault,
            AccountType::Obligation,
            AccountType::VaultState,
            AccountType::LstPool,
        ];

        for &at in &types {
            let pk = Pubkey::new_unique();
            router.register(pk, at);
            let event = router.route(&make_update(pk)).unwrap();
            assert_eq!(event.account_type(), at);
        }
    }

    #[test]
    fn account_type_display() {
        assert_eq!(AccountType::Oracle.to_string(), "Oracle");
        assert_eq!(AccountType::PoolVault.to_string(), "PoolVault");
        assert_eq!(AccountType::Obligation.to_string(), "Obligation");
        assert_eq!(AccountType::VaultState.to_string(), "VaultState");
        assert_eq!(AccountType::LstPool.to_string(), "LstPool");
        assert_eq!(AccountType::Other.to_string(), "Other");
    }

    #[test]
    fn routed_event_slot_and_pubkey() {
        let pk = Pubkey::new_unique();
        let event = RoutedEvent::OracleUpdate {
            pubkey: pk,
            data: vec![],
            slot: 42,
            write_version: 7,
        };
        assert_eq!(event.slot(), 42);
        assert_eq!(*event.pubkey(), pk);
    }
}
