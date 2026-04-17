//! Subscription filter builders for Yellowstone gRPC streams.
//!
//! Each function builds a HashMap<String, SubscribeRequestFilter*> suitable for
//! inclusion in a SubscribeRequest. We use wrapper types so that the subscriber
//! module can construct the final SubscribeRequest from these components.
//!
//! QuickNode 1-filter-per-stream workaround:
//!   QuickNode Premium allows 1 filter per stream, max 10 accounts per filter,
//!   and up to 10 owner filters per stream. To watch 76+ oracle accounts
//!   efficiently, we use the OWNER filter on the Pyth Push Oracle program
//!   instead of listing individual pubkeys. This catches ALL oracle updates
//!   in a single filter.
//!
//!   [VERIFIED 2026] bot_architecture_deep_2026.md Section 2a:
//!     "QuickNode allows 1 filter per stream AND up to 10 owner filters per stream"
//!   [VERIFIED 2026] bot_architecture_deep_2026.md Section 2b:
//!     "The Owner Filter Trick — subscribe to the OWNER of those accounts"
//!
//! For pool vault monitoring, we list specific pubkeys (max 10 per QuickNode
//! stream). Accounts are pre-sorted by TVL (highest first) so we watch the
//! vaults that matter most for backrun arbitrage.
//!
//!   [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c:
//!     "Stream 2: Pools — specific pool vault pubkeys (top 10-50)"

use std::collections::HashMap;

use solana_sdk::pubkey::Pubkey;
use tracing::info;

// ---------------------------------------------------------------------------
// Pyth Push Oracle program ID
// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2b:
//   owner: "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH" (Pyth Push Oracle)
// ---------------------------------------------------------------------------

/// Pyth Push Oracle program address — owns all PriceFeedAccount accounts.
/// Auto-updated every ~400ms by the Pyth Price Scheduler.
/// [VERIFIED 2026] save_oracle_onchain_check_2026.md: confirmed Save uses this program
/// [VERIFIED 2026] get_all_strategies_online_2026.md s1: "correct program is pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT"
/// [VERIFIED 2026] pyth_post_update_atomic_2026.md s1: "Pyth Push Oracle Program"
/// NOTE: The previous value FsJ3A3u2... was Legacy Pyth v2 (DEPRECATED June 2024), which is why gRPC events = 0
pub const PYTH_PUSH_ORACLE_PROGRAM: &str = "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT";

// ---------------------------------------------------------------------------
// SubscribeFilter — lightweight wrapper representing gRPC filter parameters
// ---------------------------------------------------------------------------

/// Represents a single gRPC subscription filter for accounts.
///
/// Maps to yellowstone_grpc_proto::SubscribeRequestFilterAccounts,
/// but decoupled from the proto dependency for testability.
#[derive(Debug, Clone)]
pub struct SubscribeFilter {
    /// Filter label (e.g., "pyth_oracles", "pool_vaults", "bh").
    pub label: String,
    /// Specific account pubkeys to monitor (empty if using owner filter).
    pub account_pubkeys: Vec<String>,
    /// Owner program pubkeys — streams ALL accounts owned by these programs.
    pub owner_pubkeys: Vec<String>,
    /// Data slice parameters: (offset, length) pairs.
    /// Reduces bandwidth by requesting only specific byte ranges.
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2d: accounts_data_slice
    pub data_slices: Vec<(u64, u64)>,
    /// Whether this is a blocksMeta filter (vs account filter).
    pub is_blocks_meta: bool,
}

/// Build the oracle owner filter — catches ALL Pyth oracle account updates
/// in a single filter using the owner program ID.
///
/// Uses accounts_data_slice to request only the price bytes:
///   offset 74, length 20: i64 price (8) + i64 conf (8) + i32 expo (4) = 20 bytes
///   This reduces bandwidth by ~97% compared to streaming full account data.
///
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2b: Owner Filter Trick
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2d:
///   "Pyth price accounts: price is at offset 74, 20 bytes"
pub fn build_oracle_owner_filter() -> SubscribeFilter {
    info!(
        "filters: building oracle owner filter (owner={})",
        PYTH_PUSH_ORACLE_PROGRAM
    );

    SubscribeFilter {
        label: "pyth_oracles".to_string(),
        account_pubkeys: vec![],
        owner_pubkeys: vec![PYTH_PUSH_ORACLE_PROGRAM.to_string()],
        // Stream full account data (no data_slice) — ~134 bytes per update, ~12 KB/s.
        // This gives us feed_id directly from the account data without a lookup table.
        // [VERIFIED 2026] fix_grpc_pyth_sse_2026.md s1:
        //   "Recommend removing data_slices temporarily so we get feed_id directly"
        //   "accounts_data_slice may be incompatible with owner filters on QuickNode"
        data_slices: vec![],
        is_blocks_meta: false,
    }
}

/// Build an account filter for specific pubkeys (e.g., pool vaults).
///
/// QuickNode enforces max 10 accounts per filter per stream. If more than 10
/// are provided, only the first 10 are included (accounts should be pre-sorted
/// by TVL/importance).
///
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2a:
///   "Account pubkeys per stream: Up to 10 (Premium), Up to 50 (Velocity)"
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c:
///   "Stream 2: Pools — specific pool vault pubkeys (top 10-50)"
pub fn build_account_filter(pubkeys: &[Pubkey]) -> SubscribeFilter {
    // QuickNode Premium limit: 10 accounts per filter.
    let max_accounts = 10;
    let account_keys: Vec<String> = pubkeys
        .iter()
        .take(max_accounts)
        .map(|pk| pk.to_string())
        .collect();

    if pubkeys.len() > max_accounts {
        info!(
            "filters: using top {} of {} accounts (TVL-sorted, QuickNode 1-filter limit)",
            account_keys.len(),
            pubkeys.len()
        );
    }

    info!("filters: building account filter with {} pubkeys", account_keys.len());

    SubscribeFilter {
        label: "pool_vaults".to_string(),
        account_pubkeys: account_keys,
        owner_pubkeys: vec![],
        data_slices: vec![], // Full account data for pools (need reserves, sqrt_price, etc.)
        is_blocks_meta: false,
    }
}

/// Build a blocksMeta filter for fresh blockhash streaming.
///
/// blocksMeta provides:
///   - blockhash (string) — THE blockhash for transaction construction
///   - block_height (u64) — for expiry prediction (valid 150 blocks = 60-90s)
///   - slot (u64) — slot number
///
/// Eliminates getLatestBlockhash RPC calls entirely.
///
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 9: "gRPC blocksMeta for fresh blockhash"
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 205: "blocksMeta for fresh blockhash"
pub fn build_blocks_filter() -> SubscribeFilter {
    info!("filters: building blocksMeta filter");

    SubscribeFilter {
        label: "bh".to_string(),
        account_pubkeys: vec![],
        owner_pubkeys: vec![],
        data_slices: vec![],
        is_blocks_meta: true,
    }
}

/// Convert a SubscribeFilter into the HashMap<String, _> format expected by
/// SubscribeRequest.accounts or SubscribeRequest.blocks_meta.
///
/// This is a helper for building the final gRPC request.
pub fn filter_to_accounts_map(filter: &SubscribeFilter) -> HashMap<String, AccountFilterParams> {
    let mut map = HashMap::new();
    map.insert(
        filter.label.clone(),
        AccountFilterParams {
            account: filter.account_pubkeys.clone(),
            owner: filter.owner_pubkeys.clone(),
            data_slices: filter.data_slices.clone(),
        },
    );
    map
}

/// Parameters for an account subscription filter.
/// Maps to SubscribeRequestFilterAccounts fields.
#[derive(Debug, Clone)]
pub struct AccountFilterParams {
    /// Specific account pubkeys (base58).
    pub account: Vec<String>,
    /// Owner program pubkeys (base58).
    pub owner: Vec<String>,
    /// Data slices: (offset, length) pairs.
    pub data_slices: Vec<(u64, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_filter_uses_owner() {
        let filter = build_oracle_owner_filter();
        assert_eq!(filter.label, "pyth_oracles");
        assert!(filter.account_pubkeys.is_empty(), "oracle filter should use owner, not individual accounts");
        assert_eq!(filter.owner_pubkeys.len(), 1);
        assert_eq!(filter.owner_pubkeys[0], PYTH_PUSH_ORACLE_PROGRAM);
        assert!(!filter.is_blocks_meta);
    }

    #[test]
    fn oracle_filter_has_data_slice() {
        let filter = build_oracle_owner_filter();
        // Pyth price: offset 74, 20 bytes (i64 price + i64 conf + i32 expo)
        assert_eq!(filter.data_slices.len(), 1);
        assert_eq!(filter.data_slices[0], (74, 20));
    }

    #[test]
    fn account_filter_respects_10_limit() {
        // Create 15 pubkeys
        let pubkeys: Vec<Pubkey> = (0..15).map(|_| Pubkey::new_unique()).collect();
        let filter = build_account_filter(&pubkeys);

        assert_eq!(filter.label, "pool_vaults");
        assert_eq!(
            filter.account_pubkeys.len(),
            10,
            "QuickNode Premium limit: max 10 accounts per filter"
        );
        assert!(filter.owner_pubkeys.is_empty());
        assert!(filter.data_slices.is_empty(), "pools need full data");
        assert!(!filter.is_blocks_meta);
    }

    #[test]
    fn account_filter_under_limit() {
        let pubkeys: Vec<Pubkey> = (0..5).map(|_| Pubkey::new_unique()).collect();
        let filter = build_account_filter(&pubkeys);
        assert_eq!(filter.account_pubkeys.len(), 5);
    }

    #[test]
    fn account_filter_empty() {
        let filter = build_account_filter(&[]);
        assert!(filter.account_pubkeys.is_empty());
    }

    #[test]
    fn blocks_filter_is_meta() {
        let filter = build_blocks_filter();
        assert_eq!(filter.label, "bh");
        assert!(filter.is_blocks_meta);
        assert!(filter.account_pubkeys.is_empty());
        assert!(filter.owner_pubkeys.is_empty());
    }

    #[test]
    fn filter_to_map_works() {
        let filter = build_oracle_owner_filter();
        let map = filter_to_accounts_map(&filter);
        assert!(map.contains_key("pyth_oracles"));
        let params = &map["pyth_oracles"];
        assert_eq!(params.owner.len(), 1);
        assert_eq!(params.data_slices.len(), 1);
    }
}
