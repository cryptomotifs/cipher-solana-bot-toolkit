//! ATA (Associated Token Account) management — pre-create and close.
//!
//! Design decisions from [VERIFIED 2026] research:
//!
//! - Pre-create ATAs for top 8 tokens at startup to eliminate ATA creation
//!   from the hot path. Saves ~200 CU and 1 instruction per TX.
//!   [VERIFIED 2026] operational_data_2026.md Section 4:
//!     "Pre-Create (Recommended for Bot): Eliminates ATA creation from hot path"
//!     "Known collateral tokens can be pre-created at startup"
//!
//! - Cost: 0.00203928 SOL (2,039,280 lamports) per ATA. One-time deposit,
//!   100% recoverable by closing the account.
//!   [VERIFIED 2026] operational_data_2026.md Section 4:
//!     "Rent-exempt minimum: 0.00203928 SOL (2,039,280 lamports)"
//!     "100% recoverable by closing the account"
//!
//! - Use CreateIdempotent — succeeds even if ATA already exists.
//!   [VERIFIED 2026] operational_data_2026.md Section 4:
//!     "Use CreateIdempotent — succeeds even if ATA already exists"
//!
//! - Close empty ATAs to recover rent after liquidation.
//!   After liquidation: if received collateral is fully swapped, close the ATA.
//!   [VERIFIED 2026] operational_data_2026.md Section 4:
//!     "100% recoverable by closing the account"
//!     "Token balance must be zero"
//!
//! - TOP_MINTS: SOL, USDC, USDT, JitoSOL, mSOL, bSOL, jupSOL, wstETH.
//!   These are the most common collateral/debt tokens across Save/Kamino/JupLend.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 193-194:
//!     "Pre-create for top 8 tokens (SOL, USDC, USDT, JitoSOL, mSOL, bSOL, jupSOL, wstETH)"

// [VERIFIED 2026] operational_data_2026.md Section 4: ATA management design
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
};
use spl_associated_token_account::{
    get_associated_token_address,
    instruction::create_associated_token_account_idempotent,
};
use std::str::FromStr;

use predator_core::constants;

// ---------------------------------------------------------------------------
// Top mints for pre-creation
// ---------------------------------------------------------------------------

/// Top 8 token mints to pre-create ATAs for at startup.
///
/// These are the most common collateral and debt tokens across
/// Save, Kamino, MarginFi, and JupLend lending markets.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 193-194
/// [VERIFIED 2026] operational_data_2026.md Section 4: "Pre-create ATAs for top 8"
/// [VERIFIED 2026] constants.rs: mint addresses verified on-chain April 2026
pub const TOP_MINTS: [&str; 8] = [
    constants::SOL_MINT,     // Wrapped SOL (native mint)
    constants::USDC_MINT,    // USDC — most common debt token
    constants::USDT_MINT,    // USDT — second most common debt token
    constants::JITOSOL_MINT, // JitoSOL — common LST collateral
    constants::MSOL_MINT,    // mSOL — Marinade staked SOL
    constants::BSOL_MINT,    // bSOL — BlazeStake SOL
    constants::JUPSOL_MINT,  // jupSOL — Jupiter staked SOL
    // wstETH — Lido wrapped staked ETH on Solana.
    // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 194
    "ZScHuTtqZukUrtZS43teTKGs2VqkKL8k4QCouR2n6Uo",
];

/// ATA rent-exempt minimum in lamports.
/// [VERIFIED 2026] operational_data_2026.md Section 4: "0.00203928 SOL (2,039,280 lamports)"
pub const ATA_RENT_LAMPORTS: u64 = 2_039_280;

// ---------------------------------------------------------------------------
// AtaManager
// ---------------------------------------------------------------------------

/// Manages Associated Token Accounts — pre-creation and cleanup.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 193-196
/// [VERIFIED 2026] operational_data_2026.md Section 4
pub struct AtaManager;

impl AtaManager {
    /// Create an instruction to ensure an ATA exists for the given wallet + mint.
    ///
    /// Uses `create_associated_token_account_idempotent` which succeeds even if
    /// the ATA already exists (no-op in that case, no error).
    ///
    /// [VERIFIED 2026] operational_data_2026.md Section 4:
    ///   "Use CreateIdempotent — succeeds even if ATA already exists"
    pub fn ensure_ata(wallet: &Pubkey, mint: &Pubkey) -> Instruction {
        create_associated_token_account_idempotent(
            wallet,       // payer (funds rent if needed)
            wallet,       // wallet (owner of the new ATA)
            mint,         // token mint
            &spl_token::id(), // token program ID
        )
    }

    /// Create an instruction to close an empty ATA and recover rent.
    ///
    /// The ATA must have zero token balance. All lamports (rent deposit)
    /// are transferred to the wallet.
    ///
    /// [VERIFIED 2026] operational_data_2026.md Section 4:
    ///   "Token balance must be zero"
    ///   "All lamports (0.00203928 SOL) returned to destination"
    ///   "Frozen accounts: Can still be closed if balance is zero"
    pub fn close_empty_ata(
        wallet: &Pubkey,
        ata: &Pubkey,
        _mint: &Pubkey,
    ) -> Instruction {
        spl_token::instruction::close_account(
            &spl_token::id(),
            ata,        // account to close
            wallet,     // destination for recovered lamports
            wallet,     // authority (owner)
            &[],        // multisig signers (none)
        )
        .expect("close_account instruction should not fail with valid inputs")
    }

    /// Get the deterministic ATA address for a wallet + mint.
    ///
    /// ATA address = PDA(wallet, token_program, mint).
    /// [VERIFIED 2026] operational_data_2026.md Section 4:
    ///   "Deterministic: ATA address = hash(wallet, token_program, mint)"
    pub fn get_ata_address(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
        get_associated_token_address(wallet, mint)
    }

    /// Generate instructions to pre-create ATAs for all TOP_MINTS.
    ///
    /// Each instruction uses CreateIdempotent — safe to call even if ATAs
    /// already exist. This eliminates ATA creation from the liquidation hot path.
    ///
    /// Total cost for 8 ATAs: 8 * 0.00204 SOL = ~0.0163 SOL.
    ///
    /// [VERIFIED 2026] operational_data_2026.md Section 4:
    ///   "Pre-create ATAs for top 8 collateral tokens"
    ///   "Cost 0.00204 SOL each"
    pub fn pre_create_atas(wallet: &Pubkey) -> Vec<Instruction> {
        // Pre-allocate with known capacity.
        // [VERIFIED 2026] low_latency_dataflow_2026.md Section 7: with_capacity pattern
        let mut instructions = Vec::with_capacity(TOP_MINTS.len());

        for mint_str in TOP_MINTS.iter() {
            if let Ok(mint) = Pubkey::from_str(mint_str) {
                instructions.push(Self::ensure_ata(wallet, &mint));
            } else {
                tracing::warn!("Invalid mint in TOP_MINTS: {}", mint_str);
            }
        }

        tracing::info!(
            "Generated {} ATA pre-creation instructions (total rent: ~{:.4} SOL)",
            instructions.len(),
            instructions.len() as f64 * ATA_RENT_LAMPORTS as f64 / 1e9,
        );

        instructions
    }

    /// Get all ATA addresses for a wallet across TOP_MINTS.
    ///
    /// Useful for monitoring and cleanup — check balances of all ATAs
    /// and close any with zero balance to recover rent.
    pub fn all_top_ata_addresses(wallet: &Pubkey) -> Vec<(Pubkey, Pubkey)> {
        TOP_MINTS
            .iter()
            .filter_map(|mint_str| {
                Pubkey::from_str(mint_str).ok().map(|mint| {
                    let ata = Self::get_ata_address(wallet, &mint);
                    (ata, mint)
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_mints_count() {
        // [VERIFIED 2026] 8 top mints
        assert_eq!(TOP_MINTS.len(), 8);
    }

    #[test]
    fn top_mints_are_valid_pubkeys() {
        for mint_str in TOP_MINTS.iter() {
            assert!(
                Pubkey::from_str(mint_str).is_ok(),
                "Invalid mint pubkey: {}",
                mint_str
            );
        }
    }

    #[test]
    fn ata_rent_constant() {
        // [VERIFIED 2026] 2,039,280 lamports
        assert_eq!(ATA_RENT_LAMPORTS, 2_039_280);
    }

    #[test]
    fn ensure_ata_produces_instruction() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::from_str(constants::USDC_MINT).unwrap();
        let ix = AtaManager::ensure_ata(&wallet, &mint);

        // Should target the Associated Token Account program
        assert_eq!(
            ix.program_id,
            Pubkey::from_str(constants::ASSOCIATED_TOKEN_PROGRAM).unwrap()
        );
    }

    #[test]
    fn get_ata_address_deterministic() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::from_str(constants::USDC_MINT).unwrap();

        let addr1 = AtaManager::get_ata_address(&wallet, &mint);
        let addr2 = AtaManager::get_ata_address(&wallet, &mint);

        // Same inputs must produce same address.
        assert_eq!(addr1, addr2);
    }

    #[test]
    fn get_ata_address_different_mints() {
        let wallet = Pubkey::new_unique();
        let usdc = Pubkey::from_str(constants::USDC_MINT).unwrap();
        let usdt = Pubkey::from_str(constants::USDT_MINT).unwrap();

        let ata_usdc = AtaManager::get_ata_address(&wallet, &usdc);
        let ata_usdt = AtaManager::get_ata_address(&wallet, &usdt);

        // Different mints produce different ATAs.
        assert_ne!(ata_usdc, ata_usdt);
    }

    #[test]
    fn pre_create_atas_generates_all() {
        let wallet = Pubkey::new_unique();
        let instructions = AtaManager::pre_create_atas(&wallet);
        assert_eq!(instructions.len(), TOP_MINTS.len());
    }

    #[test]
    fn all_top_ata_addresses_count() {
        let wallet = Pubkey::new_unique();
        let addresses = AtaManager::all_top_ata_addresses(&wallet);
        assert_eq!(addresses.len(), TOP_MINTS.len());
    }

    #[test]
    fn close_empty_ata_instruction() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::from_str(constants::USDC_MINT).unwrap();
        let ata = AtaManager::get_ata_address(&wallet, &mint);

        let ix = AtaManager::close_empty_ata(&wallet, &ata, &mint);
        // Should target the Token program
        assert_eq!(ix.program_id, spl_token::id());
    }

    #[test]
    fn sol_mint_in_top_mints() {
        assert!(TOP_MINTS.contains(&constants::SOL_MINT));
    }

    #[test]
    fn usdc_mint_in_top_mints() {
        assert!(TOP_MINTS.contains(&constants::USDC_MINT));
    }
}
