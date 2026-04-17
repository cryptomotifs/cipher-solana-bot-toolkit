//! Bot action definitions — the outbound commands that strategies produce.
//!
//! Follows the Artemis Collector -> Strategy -> Executor pipeline pattern.
//! Strategies emit `BotAction` values which the Executor (T12) processes from
//! a priority queue, submitting transactions via the MultiPathSubmitter.
//!
//! All variants are verified against 2026 research:
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 53-54: "BotAction enum (SubmitBundle,
//!   SubmitTx, LogOpportunity, RescanProtocol, UpdateTip, EmergencyScan)"
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1287-1313: Full BotAction enum definition
//! - [VERIFIED 2026] code_structure_patterns_2026.md lines 146-151: BotAction adapted for Solana
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 136-137: "StrategyPriority enum,
//!   priority ordering: Liquidation(1) > Backrun(2) > FlashArb(3) > LstArb(4) > CopyTrade(5)"
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 848-856: MultiPathSubmitter with
//!   Jito, bloXroute, Helius Sender, Nozomi
//! - [VERIFIED 2026] code_structure_patterns_2026.md lines 280-284: SubmitMethod enum

use std::fmt;

use crate::types::{Lamports, Protocol};

// ---------------------------------------------------------------------------
// BotAction
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1287-1313
// [VERIFIED 2026] code_structure_patterns_2026.md lines 146-151
// ---------------------------------------------------------------------------

/// Actions produced by strategies for the Executor to process.
///
/// Actions are placed into a priority queue ordered by `StrategyPriority`.
/// The Executor dequeues them and submits via the appropriate path.
#[derive(Debug)]
pub enum BotAction {
    /// Submit a Jito bundle (atomic multi-transaction execution).
    ///
    /// Used by: LiquidationStrategy (flash+crank+liquidate+swap+repay),
    /// BackrunStrategy (detect+arb), FlashArbStrategy (circular arb).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1290-1295
    SubmitBundle {
        /// Serialized transactions to include in the bundle.
        /// Each is a `VersionedTransaction` serialized to bytes.
        txs: Vec<Vec<u8>>,
        /// Jito tip in lamports. Dynamic: 50% of estimated profit, min 50K lamports.
        /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 170
        tip_lamports: Lamports,
        /// Priority determines queue ordering when multiple actions compete.
        priority: StrategyPriority,
    },

    /// Submit a single transaction (non-bundle) via a specific submission method.
    ///
    /// Used for: non-atomic operations, single-IX transactions, fallback submission.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1297-1300
    SubmitTransaction {
        /// Serialized `VersionedTransaction` bytes.
        tx: Vec<u8>,
        /// Which submission path to use.
        method: SubmitMethod,
    },

    /// Log an opportunity without executing (dry-run mode or below profit threshold).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1302-1307
    LogOpportunity {
        /// Which protocol the opportunity was found on.
        protocol: Protocol,
        /// Estimated profit in lamports.
        est_profit: Lamports,
        /// Human-readable description (e.g. "Kamino Main: HF=0.92, debt=$1200 USDC").
        description: String,
    },

    /// Request a full re-scan of obligations for a specific protocol.
    ///
    /// Triggered when a protocol's state may have changed significantly
    /// (e.g. after a large liquidation or governance parameter change).
    RescanProtocol {
        /// Which protocol to re-scan.
        protocol: Protocol,
    },

    /// Emergency scan triggered by external signal (crash alert, whale movement).
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 1309-1312
    EmergencyScan {
        /// What triggered the emergency scan (e.g. "SOL -8% in 1h",
        /// "whale deposited 50K SOL to Save").
        trigger: String,
    },

    /// Update the Jito tip floor based on market conditions or crash risk level.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 54: "UpdateTip"
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 1096: adaptive tip multiplier
    UpdateTipFloor {
        /// New minimum tip in lamports.
        new_floor: Lamports,
    },
}

impl BotAction {
    /// Returns a short label for logging/metrics.
    pub fn label(&self) -> &'static str {
        match self {
            BotAction::SubmitBundle { .. } => "submit_bundle",
            BotAction::SubmitTransaction { .. } => "submit_tx",
            BotAction::LogOpportunity { .. } => "log_opportunity",
            BotAction::RescanProtocol { .. } => "rescan_protocol",
            BotAction::EmergencyScan { .. } => "emergency_scan",
            BotAction::UpdateTipFloor { .. } => "update_tip_floor",
        }
    }
}

// ---------------------------------------------------------------------------
// SubmitMethod
// [VERIFIED 2026] code_structure_patterns_2026.md lines 280-284: SubmitMethod enum
//   (JitoBundle, JitoTransaction, RpcSendTransaction)
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 848-856: MultiPathSubmitter
//   with jito, bloxroute, helius, nozomi
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 171-179: per-submitter details
// ---------------------------------------------------------------------------

/// Transaction submission method — which infrastructure path to use.
///
/// The MultiPathSubmitter coordinates parallel submission across multiple paths
/// for maximum landing probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubmitMethod {
    /// Jito block engine — primary path. 6 regional endpoints.
    /// Supports both bundles and single transactions.
    /// ~95% validator coverage. Min tip ~50K lamports for competitive landing.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 851
    Jito,

    /// bloXroute — secondary path. fastBestEffort routes through both Jito + staked.
    /// Free tier: 60 credits/60s (~1 RPS). Min tip 1025 lamports.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 171-172
    BloXroute,

    /// Helius Sender — SWQoS staked connections. Dual-path: Jito + SWQoS simultaneously.
    /// Min tip 10K lamports. Best as fallback for non-bundle single transactions.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 174-176
    HeliusSender,

    /// Nozomi — sendTransaction replacement (NOT bundles). 6 regional endpoints.
    /// Min tip 0.001 SOL (100K lamports). Only for high-profit liquidations (>$50).
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 177-179
    Nozomi,

    /// Direct RPC sendTransaction — last resort fallback.
    /// No tips, no priority guarantees. Used when all other paths fail.
    DirectRpc,
}

impl SubmitMethod {
    /// Returns the minimum tip in lamports for this submission method.
    ///
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 170-179
    pub fn min_tip_lamports(&self) -> u64 {
        match self {
            SubmitMethod::Jito => 50_000,         // 50K lamports
            SubmitMethod::BloXroute => 1_025,     // 1025 lamports
            SubmitMethod::HeliusSender => 10_000, // 10K lamports
            SubmitMethod::Nozomi => 100_000,      // 0.001 SOL = 100K lamports
            SubmitMethod::DirectRpc => 0,          // No tip
        }
    }

    /// Whether this method supports bundle submission (multi-TX atomic).
    pub fn supports_bundles(&self) -> bool {
        matches!(self, SubmitMethod::Jito | SubmitMethod::BloXroute)
    }
}

impl fmt::Display for SubmitMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubmitMethod::Jito => write!(f, "Jito"),
            SubmitMethod::BloXroute => write!(f, "bloXroute"),
            SubmitMethod::HeliusSender => write!(f, "Helius-Sender"),
            SubmitMethod::Nozomi => write!(f, "Nozomi"),
            SubmitMethod::DirectRpc => write!(f, "DirectRPC"),
        }
    }
}

// ---------------------------------------------------------------------------
// StrategyPriority
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 136-138:
//   "StrategyPriority enum, PriorityQueue<BotAction>,
//    priority ordering: Liquidation(1) > Backrun(2) > FlashArb(3) > LstArb(4) > CopyTrade(5)"
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 417-422: Task priority assignments
// ---------------------------------------------------------------------------

/// Strategy priority for the executor's action queue.
///
/// Lower numeric value = higher priority. Liquidation is highest because:
/// 1. Liquidation opportunities are time-critical (health can recover quickly)
/// 2. Liquidation bonuses (3.5-5%) are the highest reliable profit source
/// 3. Competition window is narrow (other liquidators racing)
///
/// Values are repr(u8) for compact storage in priority queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StrategyPriority {
    /// Liquidation — highest priority. Time-critical, highest bonus.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 417: T7 HIGHEST
    Liquidation = 1,

    /// Backrun — high priority. Must land in same/next slot as target TX.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 418: T8 HIGH
    Backrun = 2,

    /// Flash arbitrage — medium priority. Circular arb, timer-driven.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 419: T9 MEDIUM
    FlashArb = 3,

    /// LST arbitrage — lower priority. Rate deviation detection.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 420: T10 LOW
    LstArb = 4,

    /// Copy trading — lowest priority. Whale wallet mirroring.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 421: T11 LOW
    CopyTrade = 5,
}

impl StrategyPriority {
    /// Returns the numeric priority value (lower = higher priority).
    #[inline]
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }

    /// Returns true if this priority is higher than (numerically less than) `other`.
    #[inline]
    pub fn is_higher_than(&self, other: &Self) -> bool {
        (*self as u8) < (*other as u8)
    }
}

impl Ord for StrategyPriority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Lower numeric value = higher priority, so reverse the comparison
        // so that in a max-heap priority queue, Liquidation(1) comes before CopyTrade(5).
        (*other as u8).cmp(&(*self as u8))
    }
}

impl PartialOrd for StrategyPriority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for StrategyPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StrategyPriority::Liquidation => write!(f, "Liquidation[1]"),
            StrategyPriority::Backrun => write!(f, "Backrun[2]"),
            StrategyPriority::FlashArb => write!(f, "FlashArb[3]"),
            StrategyPriority::LstArb => write!(f, "LstArb[4]"),
            StrategyPriority::CopyTrade => write!(f, "CopyTrade[5]"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Protocol;

    #[test]
    fn bot_action_labels() {
        let action = BotAction::SubmitBundle {
            txs: vec![vec![1, 2, 3]],
            tip_lamports: Lamports(50_000),
            priority: StrategyPriority::Liquidation,
        };
        assert_eq!(action.label(), "submit_bundle");

        let action2 = BotAction::LogOpportunity {
            protocol: Protocol::Save,
            est_profit: Lamports(1_000_000),
            description: "test".to_string(),
        };
        assert_eq!(action2.label(), "log_opportunity");

        assert_eq!(
            BotAction::EmergencyScan {
                trigger: "test".to_string()
            }
            .label(),
            "emergency_scan"
        );

        assert_eq!(
            BotAction::RescanProtocol {
                protocol: Protocol::Kamino
            }
            .label(),
            "rescan_protocol"
        );

        assert_eq!(
            BotAction::UpdateTipFloor {
                new_floor: Lamports(100_000)
            }
            .label(),
            "update_tip_floor"
        );
    }

    #[test]
    fn submit_method_min_tips() {
        assert_eq!(SubmitMethod::Jito.min_tip_lamports(), 50_000);
        assert_eq!(SubmitMethod::BloXroute.min_tip_lamports(), 1_025);
        assert_eq!(SubmitMethod::HeliusSender.min_tip_lamports(), 10_000);
        assert_eq!(SubmitMethod::Nozomi.min_tip_lamports(), 100_000);
        assert_eq!(SubmitMethod::DirectRpc.min_tip_lamports(), 0);
    }

    #[test]
    fn submit_method_bundle_support() {
        assert!(SubmitMethod::Jito.supports_bundles());
        assert!(SubmitMethod::BloXroute.supports_bundles());
        assert!(!SubmitMethod::HeliusSender.supports_bundles());
        assert!(!SubmitMethod::Nozomi.supports_bundles());
        assert!(!SubmitMethod::DirectRpc.supports_bundles());
    }

    #[test]
    fn submit_method_display() {
        assert_eq!(SubmitMethod::Jito.to_string(), "Jito");
        assert_eq!(SubmitMethod::BloXroute.to_string(), "bloXroute");
        assert_eq!(SubmitMethod::HeliusSender.to_string(), "Helius-Sender");
        assert_eq!(SubmitMethod::Nozomi.to_string(), "Nozomi");
        assert_eq!(SubmitMethod::DirectRpc.to_string(), "DirectRPC");
    }

    #[test]
    fn strategy_priority_values() {
        assert_eq!(StrategyPriority::Liquidation.as_u8(), 1);
        assert_eq!(StrategyPriority::Backrun.as_u8(), 2);
        assert_eq!(StrategyPriority::FlashArb.as_u8(), 3);
        assert_eq!(StrategyPriority::LstArb.as_u8(), 4);
        assert_eq!(StrategyPriority::CopyTrade.as_u8(), 5);
    }

    #[test]
    fn strategy_priority_ordering() {
        // Higher priority (lower number) should sort as "greater" for max-heap behavior
        assert!(StrategyPriority::Liquidation > StrategyPriority::Backrun);
        assert!(StrategyPriority::Backrun > StrategyPriority::FlashArb);
        assert!(StrategyPriority::FlashArb > StrategyPriority::LstArb);
        assert!(StrategyPriority::LstArb > StrategyPriority::CopyTrade);
    }

    #[test]
    fn strategy_priority_is_higher_than() {
        assert!(StrategyPriority::Liquidation.is_higher_than(&StrategyPriority::CopyTrade));
        assert!(!StrategyPriority::CopyTrade.is_higher_than(&StrategyPriority::Liquidation));
        assert!(!StrategyPriority::Liquidation.is_higher_than(&StrategyPriority::Liquidation));
    }

    #[test]
    fn strategy_priority_display() {
        assert_eq!(StrategyPriority::Liquidation.to_string(), "Liquidation[1]");
        assert_eq!(StrategyPriority::CopyTrade.to_string(), "CopyTrade[5]");
    }

    #[test]
    fn submit_transaction_action() {
        let action = BotAction::SubmitTransaction {
            tx: vec![0xDE, 0xAD],
            method: SubmitMethod::HeliusSender,
        };
        assert_eq!(action.label(), "submit_tx");
    }
}
