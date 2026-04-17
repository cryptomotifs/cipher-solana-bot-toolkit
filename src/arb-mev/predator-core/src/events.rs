//! Bot event definitions — the inbound signal types that strategies process.
//!
//! Follows the Artemis Collector -> Strategy -> Executor pipeline pattern.
//!
//! All variants are verified against 2026 research:
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 51-52: "BotEvent enum (AccountUpdate,
//!   PriceUpdate, SlotUpdate, TransactionSeen, CrashAlert, TickerUpdate)"
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1260-1285: Full BotEvent enum definition
//! - [VERIFIED 2026] code_structure_patterns_2026.md lines 137-143: BotEvent adapted for Solana
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 415: "T4: PythSSE"
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 76-77: parse_pyth_price,
//!   parse_switchboard_v2, parse_switchboard_ondemand, parse_by_owner
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1087-1098: CrashPredictionEngine,
//!   7-signal composite risk system (0-100 score)

use solana_sdk::pubkey::Pubkey;
use std::fmt;

use crate::types::{OraclePrice, Slot};

// ---------------------------------------------------------------------------
// BotEvent
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1256-1285
// [VERIFIED 2026] code_structure_patterns_2026.md lines 137-143
// ---------------------------------------------------------------------------

/// Events flowing from collectors (gRPC, SSE, RPC) into the strategy pipeline.
///
/// Dispatched by the EventRouter (T6) to per-strategy channels based on event type.
/// Strategies receive only the events they subscribed to.
///
/// Design note: `data` uses `Vec<u8>` here (not `Arc<[u8]>`) for simplicity in
/// Phase 1. The state cache layer wraps in `Arc` for zero-copy sharing between tasks.
#[derive(Debug, Clone)]
pub enum BotEvent {
    /// Account data changed (from gRPC Yellowstone account subscription).
    ///
    /// Used by: LiquidationStrategy (obligation accounts), FlashArbStrategy (pool vaults),
    /// LstArbStrategy (stake pool accounts).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1260-1264
    AccountUpdate {
        /// The account's public key.
        pubkey: Pubkey,
        /// Raw account data bytes (protocol-specific, parsed by strategy).
        data: Vec<u8>,
        /// Slot at which this update was observed.
        slot: Slot,
    },

    /// Oracle price update (from Pyth Hermes SSE or gRPC on-chain oracle accounts).
    ///
    /// Used by: LiquidationStrategy (health factor recomputation on every price tick).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1266-1272
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 415: T4 PythSSE task
    PriceUpdate {
        /// Pyth feed ID (32 bytes) or oracle account pubkey hash.
        feed_id: [u8; 32],
        /// Parsed oracle price with confidence and source metadata.
        price: OraclePrice,
        /// Which price delivery mechanism produced this update.
        source: PriceSource,
    },

    /// New slot observed (from gRPC blocksMeta stream).
    ///
    /// Used by: All strategies (freshness tracking), Executor (blockhash updates).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1281-1282
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 414: T3 GeyserBlocks
    SlotUpdate {
        /// The new slot number.
        slot: Slot,
        /// Recent blockhash for transaction construction. 32 bytes.
        blockhash: [u8; 32],
    },

    /// Large or interesting transaction detected (from gRPC transaction stream).
    ///
    /// Used by: BackrunStrategy (swap detection for cross-DEX arb),
    /// CopyTradeStrategy (whale wallet monitoring).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1274-1280
    /// [VERIFIED 2026] code_structure_patterns_2026.md lines 140-141
    TransactionSeen {
        /// Transaction signature (64 bytes).
        signature: [u8; 64],
        /// Program IDs invoked in this transaction (for filtering by DEX/protocol).
        program_ids: Vec<Pubkey>,
        /// All account keys referenced in the transaction.
        accounts: Vec<Pubkey>,
    },

    /// Crash risk level changed (from CrashPredictionEngine).
    ///
    /// Triggers adaptive behavior: scan interval reduction, tip multiplier increase,
    /// pre-built TX skeleton activation.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1283-1284
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1087-1098: 7-signal composite
    CrashAlert {
        /// Current risk level (derived from composite score thresholds).
        risk_level: CrashRiskLevel,
        /// Human-readable signals that triggered this alert (e.g. "SOL -8% 1h",
        /// "Fear&Greed 12/100", "BTC correlation breakdown").
        signals: Vec<String>,
    },
}

impl BotEvent {
    /// Returns a short label for logging/metrics.
    pub fn label(&self) -> &'static str {
        match self {
            BotEvent::AccountUpdate { .. } => "account_update",
            BotEvent::PriceUpdate { .. } => "price_update",
            BotEvent::SlotUpdate { .. } => "slot_update",
            BotEvent::TransactionSeen { .. } => "transaction_seen",
            BotEvent::CrashAlert { .. } => "crash_alert",
        }
    }
}

// ---------------------------------------------------------------------------
// PriceSource
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 76-77: oracle parsers
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 415: T4 PythSSE
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 412: gRPC Stream 1 oracle owner filter
// ---------------------------------------------------------------------------

/// How the price update was delivered to the bot.
///
/// Determines latency characteristics and whether VAA data is available for cranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriceSource {
    /// Pyth Hermes Server-Sent Events (SSE) — off-chain, sub-second latency.
    /// Primary price source. Includes VAA data for on-chain cranking.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 415
    PythSSE,

    /// Pyth on-chain account (via gRPC account subscription).
    /// Slightly higher latency than SSE but confirms on-chain state.
    PythOnChain,

    /// Switchboard V2 on-chain oracle (legacy, still used by some Save markets).
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 77: parse_switchboard_v2
    SwitchboardV2,

    /// Switchboard On-Demand (pull oracle, newer).
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 77: parse_switchboard_ondemand
    SwitchboardOnDemand,
}

impl fmt::Display for PriceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PriceSource::PythSSE => write!(f, "Pyth-SSE"),
            PriceSource::PythOnChain => write!(f, "Pyth-OnChain"),
            PriceSource::SwitchboardV2 => write!(f, "Switchboard-V2"),
            PriceSource::SwitchboardOnDemand => write!(f, "Switchboard-OnDemand"),
        }
    }
}

// ---------------------------------------------------------------------------
// CrashRiskLevel
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1087-1098:
//   CrashPredictionEngine with 7-signal composite risk system (0-100 score)
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1130-1168: Adaptive behavior thresholds
// ---------------------------------------------------------------------------

/// Crash risk severity level, derived from the 7-signal composite score.
///
/// Thresholds (from PREDATOR_ARCHITECTURE_2026.md):
/// - Green:  composite < 25 — normal conditions
/// - Yellow: composite 25-49 — elevated awareness, scan interval halved
/// - Orange: composite 50-74 — high risk, scan every 10s, tips doubled
/// - Red:    composite >= 75 — imminent crash, scan every 5s, tips 5x, pre-built TXs active
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrashRiskLevel {
    /// Normal market conditions. Score < 25.
    Green = 0,
    /// Elevated risk. Score 25-49.
    Yellow = 1,
    /// High risk, aggressive scanning. Score 50-74.
    Orange = 2,
    /// Imminent crash, maximum readiness. Score >= 75.
    Red = 3,
}

impl CrashRiskLevel {
    /// Derive risk level from the composite score (0-100).
    pub fn from_score(score: u64) -> Self {
        match score {
            0..=24 => CrashRiskLevel::Green,
            25..=49 => CrashRiskLevel::Yellow,
            50..=74 => CrashRiskLevel::Orange,
            _ => CrashRiskLevel::Red,
        }
    }

    /// Returns a tip multiplier for Jito bundles at this risk level.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1096: "tip_multiplier: 1x -> 5x"
    pub fn tip_multiplier(&self) -> f64 {
        match self {
            CrashRiskLevel::Green => 1.0,
            CrashRiskLevel::Yellow => 1.5,
            CrashRiskLevel::Orange => 2.5,
            CrashRiskLevel::Red => 5.0,
        }
    }

    /// Returns the scan interval in seconds at this risk level.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1095: "scan_interval: 60s -> 5s"
    pub fn scan_interval_secs(&self) -> u64 {
        match self {
            CrashRiskLevel::Green => 60,
            CrashRiskLevel::Yellow => 30,
            CrashRiskLevel::Orange => 10,
            CrashRiskLevel::Red => 5,
        }
    }
}

impl fmt::Display for CrashRiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrashRiskLevel::Green => write!(f, "GREEN"),
            CrashRiskLevel::Yellow => write!(f, "YELLOW"),
            CrashRiskLevel::Orange => write!(f, "ORANGE"),
            CrashRiskLevel::Red => write!(f, "RED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OracleSource, Slot};

    #[test]
    fn bot_event_label() {
        let ev = BotEvent::SlotUpdate {
            slot: Slot(100),
            blockhash: [0u8; 32],
        };
        assert_eq!(ev.label(), "slot_update");

        let ev2 = BotEvent::CrashAlert {
            risk_level: CrashRiskLevel::Red,
            signals: vec!["SOL -10%".to_string()],
        };
        assert_eq!(ev2.label(), "crash_alert");
    }

    #[test]
    fn crash_risk_from_score() {
        assert_eq!(CrashRiskLevel::from_score(0), CrashRiskLevel::Green);
        assert_eq!(CrashRiskLevel::from_score(24), CrashRiskLevel::Green);
        assert_eq!(CrashRiskLevel::from_score(25), CrashRiskLevel::Yellow);
        assert_eq!(CrashRiskLevel::from_score(49), CrashRiskLevel::Yellow);
        assert_eq!(CrashRiskLevel::from_score(50), CrashRiskLevel::Orange);
        assert_eq!(CrashRiskLevel::from_score(74), CrashRiskLevel::Orange);
        assert_eq!(CrashRiskLevel::from_score(75), CrashRiskLevel::Red);
        assert_eq!(CrashRiskLevel::from_score(100), CrashRiskLevel::Red);
    }

    #[test]
    fn crash_risk_ordering() {
        assert!(CrashRiskLevel::Green < CrashRiskLevel::Yellow);
        assert!(CrashRiskLevel::Yellow < CrashRiskLevel::Orange);
        assert!(CrashRiskLevel::Orange < CrashRiskLevel::Red);
    }

    #[test]
    fn crash_risk_tip_multiplier() {
        assert!((CrashRiskLevel::Green.tip_multiplier() - 1.0).abs() < f64::EPSILON);
        assert!((CrashRiskLevel::Red.tip_multiplier() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn crash_risk_scan_interval() {
        assert_eq!(CrashRiskLevel::Green.scan_interval_secs(), 60);
        assert_eq!(CrashRiskLevel::Red.scan_interval_secs(), 5);
    }

    #[test]
    fn price_source_display() {
        assert_eq!(PriceSource::PythSSE.to_string(), "Pyth-SSE");
        assert_eq!(PriceSource::SwitchboardV2.to_string(), "Switchboard-V2");
    }

    #[test]
    fn account_update_event() {
        let ev = BotEvent::AccountUpdate {
            pubkey: Pubkey::default(),
            data: vec![1, 2, 3],
            slot: Slot(999),
        };
        assert_eq!(ev.label(), "account_update");
        if let BotEvent::AccountUpdate { data, slot, .. } = &ev {
            assert_eq!(data.len(), 3);
            assert_eq!(slot.0, 999);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn price_update_event() {
        let price = OraclePrice {
            price_f64: 80.0,
            confidence: 0.01,
            expo: -8,
            slot: Slot(500),
            source: OracleSource::PythHermes,
        };
        let ev = BotEvent::PriceUpdate {
            feed_id: [0xAB; 32],
            price,
            source: PriceSource::PythSSE,
        };
        assert_eq!(ev.label(), "price_update");
    }

    #[test]
    fn transaction_seen_event() {
        let ev = BotEvent::TransactionSeen {
            signature: [0xFF; 64],
            program_ids: vec![Pubkey::default()],
            accounts: vec![Pubkey::default(), Pubkey::default()],
        };
        assert_eq!(ev.label(), "transaction_seen");
        if let BotEvent::TransactionSeen { program_ids, accounts, .. } = &ev {
            assert_eq!(program_ids.len(), 1);
            assert_eq!(accounts.len(), 2);
        }
    }

    #[test]
    fn crash_risk_display() {
        assert_eq!(CrashRiskLevel::Green.to_string(), "GREEN");
        assert_eq!(CrashRiskLevel::Orange.to_string(), "ORANGE");
    }
}
