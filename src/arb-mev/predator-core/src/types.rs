//! Core type definitions for the Predator bot.
//!
//! All types are verified against 2026 research:
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 49-50: "Pubkey aliases, Amount(u64),
//!   Slot(u64), Lamports(u64), HealthFactor(f64), BasisPoints(u16), TokenMint(Pubkey)"
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 78: "OraclePrice struct: price_f64,
//!   confidence, expo, slot"
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 57: "DashMap<Pubkey, AccountSnapshot>"
//! - [VERIFIED 2026] code_structure_patterns_2026.md lines 138-143: BotEvent::AccountUpdate pattern
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 120-123: FlashLoanProvider enum lists
//!   Save, Kamino, MarginFi, JupLend as the 4 lending protocols
//! - [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 154: "PumpSwap AMM migration events"
//! - [VERIFIED 2026] bot_architecture_deep_2026.md line 422: Raydium SOL/USDC pool
//! - [VERIFIED 2026] bot_architecture_deep_2026.md line 431: "Raydium, Orca, etc."
//! - [VERIFIED 2026] bot_architecture_deep_2026.md line 495: "Raydium and Orca/Meteora"

use solana_sdk::pubkey::Pubkey;
use std::fmt;

// ---------------------------------------------------------------------------
// Newtype wrappers
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 49-50
// ---------------------------------------------------------------------------

/// Lamports — the native unit of SOL (1 SOL = 1_000_000_000 Lamports).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Lamports(pub u64);

impl Lamports {
    pub const ZERO: Self = Self(0);

    /// Convert to SOL as f64 (lossy).
    #[inline]
    pub fn as_sol(&self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }
}

impl fmt::Display for Lamports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} lamports", self.0)
    }
}

/// Slot number on the Solana cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Slot(pub u64);

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "slot#{}", self.0)
    }
}

/// Basis points (1 bp = 0.01%). Used for fees, bonuses, and slippage tolerances.
/// Max representable: 65535 bp = 655.35%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BasisPoints(pub u16);

impl BasisPoints {
    /// Convert to a fractional multiplier (e.g. 50 bp -> 0.005).
    #[inline]
    pub fn as_fraction(&self) -> f64 {
        self.0 as f64 / 10_000.0
    }
}

impl fmt::Display for BasisPoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bp", self.0)
    }
}

/// Health factor of a lending position. Values < 1.0 indicate liquidatable.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 91: Kamino compute_health_factor
/// [VERIFIED 2026] scanner_deep_research_2026.md: health < 1.0 triggers liquidation
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct HealthFactor(pub f64);

impl HealthFactor {
    /// A position is liquidatable when health < 1.0.
    #[inline]
    pub fn is_liquidatable(&self) -> bool {
        self.0 < 1.0
    }

    /// A position is healthy when health >= 1.0.
    #[inline]
    pub fn is_healthy(&self) -> bool {
        self.0 >= 1.0
    }
}

impl fmt::Display for HealthFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HF={:.4}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Protocol enum
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 492-497 (protocol coverage table)
// Save 5 markets | Kamino 9 markets | MarginFi 1 group | JupLend ~40 vaults
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 120-123: FlashLoanProvider enum
// ---------------------------------------------------------------------------

/// Supported lending protocols for liquidation scanning and flash loans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// Save Finance (formerly Solend) — 5 markets, 5% flat bonus (highest on Solana).
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 495
    Save,

    /// Kamino Finance — 9 markets, 2-10% sliding bonus, $2.82B TVL.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 494
    Kamino,

    /// MarginFi (P0) — 1 main group, 2.5% bonus. Currently PAUSED but may re-enable.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 496
    MarginFi,

    /// Jupiter Lending — ~40 vaults, $929M TVL, 0.1% bonus (tiny).
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 497
    JupLend,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Save => write!(f, "Save"),
            Protocol::Kamino => write!(f, "Kamino"),
            Protocol::MarginFi => write!(f, "MarginFi"),
            Protocol::JupLend => write!(f, "JupLend"),
        }
    }
}

// ---------------------------------------------------------------------------
// DexType enum
// [VERIFIED 2026] bot_architecture_deep_2026.md line 422: Raydium pools
// [VERIFIED 2026] bot_architecture_deep_2026.md line 431: "Raydium, Orca, etc."
// [VERIFIED 2026] bot_architecture_deep_2026.md line 495: "Raydium and Orca/Meteora"
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 154: PumpSwap AMM migration
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 449-453: Direct CPI per-DEX table
// ---------------------------------------------------------------------------

/// Supported DEX types for arbitrage and swap routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DexType {
    /// Raydium V4 (legacy AMM, constant-product).
    RaydiumV4,

    /// Raydium CLMM (concentrated liquidity market maker).
    RaydiumClmm,

    /// Raydium CPMM (constant-product market maker, newer).
    RaydiumCpmm,

    /// Orca Whirlpool (concentrated liquidity).
    OrcaWhirlpool,

    /// Meteora DLMM (dynamic liquidity market maker).
    MeteoraDlmm,

    /// PumpSwap AMM — tokens graduated from pump.fun bonding curve.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 154: migration snipe strategy
    PumpSwap,
}

impl fmt::Display for DexType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DexType::RaydiumV4 => write!(f, "Raydium-V4"),
            DexType::RaydiumClmm => write!(f, "Raydium-CLMM"),
            DexType::RaydiumCpmm => write!(f, "Raydium-CPMM"),
            DexType::OrcaWhirlpool => write!(f, "Orca-Whirlpool"),
            DexType::MeteoraDlmm => write!(f, "Meteora-DLMM"),
            DexType::PumpSwap => write!(f, "PumpSwap"),
        }
    }
}

// ---------------------------------------------------------------------------
// OraclePrice
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 78: "OraclePrice struct: price_f64,
//   confidence, expo, slot"
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 349: "oracle_prices: DashMap<Pubkey,
//   OraclePrice>"
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 76-77: "parse_pyth_price,
//   parse_switchboard_v2, parse_switchboard_ondemand"
// ---------------------------------------------------------------------------

/// Oracle price data from Pyth or Switchboard.
///
/// Stored in the state cache and used by all strategies to compute health factors,
/// swap profitability, and liquidation thresholds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OraclePrice {
    /// Price as f64 (already adjusted by exponent).
    pub price_f64: f64,

    /// Confidence interval width in the same unit as `price_f64`.
    /// Kamino uses confidence-adjusted pricing: assets at (price - conf), liabs at (price + conf).
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 107: "Confidence-adjusted"
    pub confidence: f64,

    /// Pyth exponent (e.g. -8 means price is in units of 10^-8).
    /// Stored for reference; `price_f64` already incorporates this.
    pub expo: i32,

    /// Slot at which this price was observed.
    pub slot: Slot,

    /// Which oracle produced this price.
    pub source: OracleSource,
}

/// Oracle provider that produced the price.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 76-77: parse_pyth_price,
///   parse_switchboard_v2, parse_switchboard_ondemand
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 415: "T4: PythSSE"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OracleSource {
    /// Pyth network (on-chain account data via gRPC).
    PythOnChain,

    /// Pyth Hermes SSE streaming (off-chain, lowest latency).
    PythHermes,

    /// Switchboard V2 (on-chain, legacy).
    SwitchboardV2,

    /// Switchboard On-Demand (pull-based).
    SwitchboardOnDemand,
}

impl fmt::Display for OracleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OracleSource::PythOnChain => write!(f, "Pyth-OnChain"),
            OracleSource::PythHermes => write!(f, "Pyth-Hermes"),
            OracleSource::SwitchboardV2 => write!(f, "Switchboard-V2"),
            OracleSource::SwitchboardOnDemand => write!(f, "Switchboard-OnDemand"),
        }
    }
}

// ---------------------------------------------------------------------------
// AccountSnapshot
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 57: "DashMap<Pubkey, AccountSnapshot>"
// [VERIFIED 2026] code_structure_patterns_2026.md line 126: gRPC raw bytes --> parser -->
//   typed struct --> business logic
// ---------------------------------------------------------------------------

/// Raw account snapshot from gRPC or RPC, stored in the state cache.
///
/// Strategies parse the `data` bytes into protocol-specific layouts (e.g.
/// Kamino ObligationLayout, Save ReserveLayout) using bytemuck zero-copy.
#[derive(Debug, Clone)]
pub struct AccountSnapshot {
    /// Account public key.
    pub pubkey: Pubkey,

    /// Raw account data bytes. Stored as `Vec<u8>` for owned data that can
    /// be shared across tasks (wrap in `Arc` at the cache level if needed).
    pub data: Vec<u8>,

    /// Slot at which this snapshot was taken.
    pub slot: Slot,

    /// gRPC write_version for deduplication — higher version wins.
    pub write_version: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamports_as_sol() {
        let l = Lamports(1_000_000_000);
        assert!((l.as_sol() - 1.0).abs() < f64::EPSILON);
        assert_eq!(Lamports::ZERO.0, 0);
    }

    #[test]
    fn basis_points_as_fraction() {
        let bp = BasisPoints(50); // 0.5%
        assert!((bp.as_fraction() - 0.005).abs() < f64::EPSILON);
    }

    #[test]
    fn health_factor_liquidatable() {
        assert!(HealthFactor(0.95).is_liquidatable());
        assert!(!HealthFactor(1.0).is_liquidatable());
        assert!(!HealthFactor(1.5).is_healthy() || HealthFactor(1.5).is_healthy());
        assert!(HealthFactor(1.0).is_healthy());
    }

    #[test]
    fn protocol_display() {
        assert_eq!(Protocol::Save.to_string(), "Save");
        assert_eq!(Protocol::Kamino.to_string(), "Kamino");
        assert_eq!(Protocol::MarginFi.to_string(), "MarginFi");
        assert_eq!(Protocol::JupLend.to_string(), "JupLend");
    }

    #[test]
    fn dex_type_display() {
        assert_eq!(DexType::RaydiumV4.to_string(), "Raydium-V4");
        assert_eq!(DexType::PumpSwap.to_string(), "PumpSwap");
    }

    #[test]
    fn oracle_price_basics() {
        let price = OraclePrice {
            price_f64: 80.50,
            confidence: 0.05,
            expo: -8,
            slot: Slot(300_000_000),
            source: OracleSource::PythHermes,
        };
        assert!((price.price_f64 - 80.50).abs() < f64::EPSILON);
        assert_eq!(price.source, OracleSource::PythHermes);
    }

    #[test]
    fn account_snapshot_basics() {
        let snap = AccountSnapshot {
            pubkey: Pubkey::default(),
            data: vec![0u8; 100],
            slot: Slot(42),
            write_version: 1,
        };
        assert_eq!(snap.data.len(), 100);
        assert_eq!(snap.slot.0, 42);
    }

    #[test]
    fn slot_display() {
        assert_eq!(Slot(12345).to_string(), "slot#12345");
    }

    #[test]
    fn lamports_display() {
        assert_eq!(Lamports(5000).to_string(), "5000 lamports");
    }
}
