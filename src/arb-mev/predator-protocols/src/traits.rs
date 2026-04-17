//! ProtocolAdapter trait — uniform interface for all lending protocol adapters.
//!
//! Each protocol (Kamino, Save, MarginFi, JupLend) implements this trait to
//! provide a consistent API for obligation parsing, health checking, and
//! liquidation instruction building.
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 74-75: ProtocolAdapter trait
//! [VERIFIED 2026] code_structure_patterns_2026.md Section 2: Trait-based strategy pattern
//! [VERIFIED 2026] scanner_deep_research_2026.md: Save, Kamino, MarginFi, JupLend layouts

use anyhow::Result;
use predator_core::{BasisPoints, HealthFactor, Protocol};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// ObligationInfo — parsed obligation data
// ---------------------------------------------------------------------------

/// Parsed obligation data from any lending protocol.
#[derive(Debug, Clone)]
pub struct ObligationInfo {
    /// On-chain address of the obligation account.
    pub address: Pubkey,
    /// Owner wallet of this obligation.
    pub owner: Pubkey,
    /// Lending market this obligation belongs to.
    pub market: Pubkey,
    /// Total borrow value in USD (protocol-native calculation).
    pub debt_usd: f64,
    /// Unhealthy borrow threshold in USD.
    pub unhealthy_threshold_usd: f64,
    /// Computed health factor (threshold / debt). < 1.0 = liquidatable.
    pub health_factor: HealthFactor,
    /// Deposit reserve addresses referenced by this obligation.
    pub deposit_reserves: Vec<Pubkey>,
    /// Borrow reserve addresses referenced by this obligation.
    pub borrow_reserves: Vec<Pubkey>,
}

// ---------------------------------------------------------------------------
// LiquidateParams — input to build_liquidate_ix
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 100-101:
//   build_liquidate_and_redeem_ix (Save variant 17, 15 accounts)
// [VERIFIED 2026] scanner_deep_research_2026.md: obligation, reserves, market, authority
// ---------------------------------------------------------------------------

/// Parameters for building a liquidation instruction against any lending protocol.
///
/// The specific accounts required vary by protocol, but every liquidation needs:
/// - The borrower's obligation / margin account
/// - The debt reserve being repaid
/// - The collateral reserve being seized
/// - The lending market and its authority PDA
/// - The liquidator's wallet for signing
#[derive(Debug, Clone)]
pub struct LiquidateParams {
    /// Liquidator's wallet — signer and fee payer.
    pub wallet: Pubkey,

    /// The borrower's obligation / margin account being liquidated.
    /// Save: 1300-byte obligation. Kamino: 3344-byte obligation.
    /// MarginFi: 2312-byte marginfi_account. JupLend: 71-byte position.
    /// [VERIFIED 2026] scanner_deep_research_2026.md Sections 1-4
    pub obligation: Pubkey,

    /// Reserve account for the debt token being repaid.
    /// Contains mint, oracle, liquidity vault info.
    pub debt_reserve: Pubkey,

    /// Reserve account for the collateral token being seized.
    /// The liquidator receives collateral at a discount (bonus).
    pub collateral_reserve: Pubkey,

    /// The lending market / group that owns this obligation.
    /// Save: LendingMarket (290 bytes). Kamino: LendingMarket.
    /// MarginFi: Group. JupLend: N/A (vault-based).
    pub market: Pubkey,

    /// Market authority PDA — signs for vault withdrawals during liquidation.
    /// Derived differently per protocol:
    /// - Save: PDA seeds [lending_market_bytes]
    /// - Kamino: PDA seeds [b"lma", lending_market_bytes]
    /// - MarginFi: Group authority
    pub market_authority: Pubkey,

    /// Amount of debt tokens to repay (in base units, e.g. lamports for SOL).
    /// Must respect close factor: Save 20%, Kamino 50%.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 92: 50% close factor (Kamino)
    /// [VERIFIED 2026] scanner_deep_research_2026.md line 54: 20% close factor (Save)
    pub amount: u64,
}

// ---------------------------------------------------------------------------
// ProtocolAdapter trait
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 74-75:
//   "ProtocolAdapter trait: parse_obligation, build_liquidation_ix,
//    build_refresh_ixs, get_bonus_bps, get_close_factor"
// [VERIFIED 2026] code_structure_patterns_2026.md Section 2: Artemis trait pattern
// ---------------------------------------------------------------------------

/// Unified interface for lending protocol adapters.
///
/// Implementations live in protocol-specific submodules (kamino/, save/, etc.).
/// The trait is object-safe (`Send + Sync`) so adapters can be stored as
/// `Box<dyn ProtocolAdapter>` and dispatched dynamically.
///
/// # Obligation Health Parsing
///
/// `parse_health` extracts (borrowed_value, unhealthy_borrow_value) from raw
/// obligation bytes. Health factor = unhealthy / borrowed. Values < 1.0 are
/// liquidatable.
///
/// # Account Offsets
///
/// All byte offsets used in implementations are sourced from [VERIFIED 2026]
/// research and on-chain verification:
/// - Save: borrowed_value@90, unhealthy_value@122 (Decimal u128)
/// - Kamino: debt_value_sf@2208, unhealthy_value_sf@2256 (u128 scaled fraction)
/// - MarginFi: 16 balance slots x 104 bytes, I80F48 math
/// - JupLend: tick-based health with ratio=1.0015^tick
pub trait ProtocolAdapter: Send + Sync {
    /// Which protocol this adapter handles.
    fn protocol(&self) -> Protocol;

    /// Program ID for this protocol's on-chain program.
    fn program_id(&self) -> Pubkey;

    /// Parse an obligation from raw account data bytes.
    fn parse_obligation(&self, address: &Pubkey, data: &[u8]) -> Result<ObligationInfo>;

    /// Extract (borrowed_value, unhealthy_borrow_value) from raw obligation bytes.
    ///
    /// Returns `None` if the data is too short or has an invalid discriminator.
    /// Health factor = unhealthy / borrowed. When borrowed > unhealthy, the
    /// position is liquidatable.
    ///
    /// # Offsets by protocol
    /// - Save: borrowed@90 (u128 Decimal), unhealthy@122 (u128 Decimal)
    ///   [VERIFIED 2026] scanner_deep_research_2026.md line 58
    /// - Kamino: debt@2208 (u128 SF), unhealthy@2256 (u128 SF)
    ///   [VERIFIED 2026] scanner_deep_research_2026.md line 89
    fn parse_health(&self, obligation_data: &[u8]) -> Option<(f64, f64)>;

    /// Check if obligation data represents a liquidatable position.
    ///
    /// Default: borrowed > unhealthy (health < 1.0).
    /// Override for protocols with different liquidation logic (e.g. JupLend ticks).
    fn is_liquidatable(&self, obligation_data: &[u8]) -> bool {
        self.parse_health(obligation_data)
            .map(|(borrowed, unhealthy)| {
                borrowed > 0.0 && unhealthy > 0.0 && borrowed > unhealthy
            })
            .unwrap_or(false)
    }

    /// Build refresh instructions that must precede a liquidation.
    ///
    /// Overload accepting raw bytes + reserve map for use in hot-path scanning
    /// where we have not yet fully parsed the obligation.
    ///
    /// - Save: refresh_reserve (per reserve) + refresh_obligation
    ///   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 98-99
    /// - Kamino: refresh_reserve + refresh_obligation
    /// - MarginFi: Usually none (banks refresh lazily)
    ///
    /// `reserves` maps reserve Pubkey -> raw reserve account data, used to
    /// extract oracle addresses and other per-reserve configuration.
    fn build_refresh_ixs_raw(
        &self,
        obligation_data: &[u8],
        reserves: &HashMap<Pubkey, Vec<u8>>,
    ) -> Vec<Instruction>;

    /// Build refresh instructions from a parsed ObligationInfo.
    /// Must be called in the same slot as the liquidation.
    fn build_refresh_ixs(&self, obligation: &ObligationInfo) -> Result<Vec<Instruction>>;

    /// Build the core liquidation instruction(s).
    ///
    /// Returns a `Vec<Instruction>` because some protocols require multiple
    /// instructions for a single liquidation (e.g. MarginFi: start + repay +
    /// withdraw + end).
    ///
    /// - Save: `LiquidateObligationAndRedeemReserveCollateral` (variant 17, 15 accounts)
    ///   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 100
    /// - Kamino: `liquidate_obligation_and_redeem_reserve_collateral` (25 accounts)
    ///   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 86-87
    fn build_liquidate_ix(&self, params: &LiquidateParams) -> Result<Vec<Instruction>>;

    /// Protocol's liquidation bonus in basis points.
    ///
    /// Returns (min_bps, max_bps) to accommodate sliding-scale bonuses.
    /// - Save: (500, 500) — flat 5%, highest on Solana
    ///   [VERIFIED 2026] scanner_deep_research_2026.md line 52
    /// - Kamino: (200, 1000) — 2-10% sliding scale based on health
    ///   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 91
    /// - MarginFi: (250, 250) — 2.5% to liquidator
    ///   [VERIFIED 2026] scanner_deep_research_2026.md line 160
    /// - JupLend: (10, 10) — 0.1% penalty
    ///   [VERIFIED 2026] scanner_deep_research_2026.md line 128
    fn get_bonus_bps(&self) -> BasisPoints;

    /// Maximum fraction of debt repayable in a single liquidation.
    ///
    /// - Save: 0.20 (20%)
    ///   [VERIFIED 2026] scanner_deep_research_2026.md line 54
    /// - Kamino: 0.50 (50%)
    ///   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 92
    /// - MarginFi: 1.0 (100% — full liquidation)
    /// - JupLend: variable (batch tick-based)
    fn get_close_factor(&self) -> f64;

    /// Expected obligation account size in bytes, for gRPC dataSize filters.
    ///
    /// - Save: 1300 bytes  [VERIFIED 2026] scanner_deep_research_2026.md line 57
    /// - Kamino: 3344 bytes [VERIFIED 2026] scanner_deep_research_2026.md line 87
    /// - MarginFi: 2312 bytes [VERIFIED 2026] scanner_deep_research_2026.md line 163
    /// - JupLend: 71 bytes  [VERIFIED 2026] scanner_deep_research_2026.md line 131
    fn obligation_size(&self) -> usize;

    /// Obligation account discriminator (first 8 bytes).
    fn obligation_discriminator(&self) -> [u8; 8];

    /// Byte offset of the market/group pubkey within the obligation account data.
    ///
    /// Used to filter obligations by market when scanning via gRPC.
    /// The market pubkey at this offset determines which market the obligation
    /// belongs to, enabling multi-market scanning (e.g. Save's 5 markets,
    /// Kamino's 9 markets).
    fn market_offset(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquidate_params_debug() {
        let params = LiquidateParams {
            wallet: Pubkey::default(),
            obligation: Pubkey::default(),
            debt_reserve: Pubkey::default(),
            collateral_reserve: Pubkey::default(),
            market: Pubkey::default(),
            market_authority: Pubkey::default(),
            amount: 1_000_000,
        };
        let _s = format!("{:?}", params);
        assert_eq!(params.amount, 1_000_000);
    }

    #[test]
    fn obligation_info_debug() {
        let info = ObligationInfo {
            address: Pubkey::default(),
            owner: Pubkey::default(),
            market: Pubkey::default(),
            debt_usd: 100.0,
            unhealthy_threshold_usd: 90.0,
            health_factor: HealthFactor(0.9),
            deposit_reserves: vec![],
            borrow_reserves: vec![],
        };
        assert!(info.health_factor.is_liquidatable());
    }
}
