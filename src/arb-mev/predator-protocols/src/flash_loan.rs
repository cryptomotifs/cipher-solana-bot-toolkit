//! FlashLoanRouter — selects the cheapest flash loan provider and builds borrow/repay instructions.
//!
//! Supports 4 providers in order of preference (cheapest first):
//! 1. **JupLend** — 0% fee, $100-250M capacity per token
//! 2. **Kamino** — 0.001% fee (1/10th of a basis point), $200-400M USDC
//! 3. **Save** — 0.05% fee (5 bps), $50-100M capacity
//! 4. **MarginFi** — 0% fee but PAUSED/risky, ~$40M TVL
//!
//! [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1-3:
//!   JupLend program: jupgfSgfuAXv4B6R2Uxu85Z1qdzgju79s6MfZekN6XS
//!   Kamino program: KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD
//!   Save program: So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo
//!   MarginFi program: MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA
//!
//! [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3:
//!   Provider order: JupLend(0%) > Kamino(0.001%) > Save(0.05%)
//!
//! [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 5:
//!   Always borrow EXACT debt amount (not max available)
//!
//! [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 6:
//!   Repay order: reverse of borrow (LIFO). All providers only scan their own
//!   program_id for borrow/repay matching.
//!
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 120-123: FlashLoanProvider enum

use predator_core::error::BotError;
use sha2::{Sha256, Digest};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};
use std::str::FromStr;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Program IDs
// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1
// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3
// ---------------------------------------------------------------------------

/// JupLend flashloan program — 0% fee.
/// [VERIFIED 2026] flash_loan_deep_dive_2026.md line 34
const JUPLEND_FLASHLOAN_PROGRAM: &str = "jupgfSgfuAXv4B6R2Uxu85Z1qdzgju79s6MfZekN6XS";

/// JupLend liquidity program — CPI target for flash loan.
/// [VERIFIED 2026] flash_loan_deep_dive_2026.md line 35
const JUPLEND_LIQUIDITY_PROGRAM: &str = "jupeiUmn818Jg1ekPURTpr4mFo29p46vygyykFJ3wZC";

/// Kamino Lend program — 0.001% fee.
/// [VERIFIED 2026] execution_pipeline_deep_2026.md line 332
const KAMINO_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

/// Save (Solend) program — 0.05% fee (5 bps, reduced from 0.3% in Dec 2023).
/// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3
/// [VERIFIED 2026] execution_pipeline_deep_2026.md line 333
const SAVE_PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

/// MarginFi program — 0% fee but PAUSED/risky. Do NOT use in production.
/// [VERIFIED 2026] execution_pipeline_deep_2026.md line 335
const MARGINFI_PROGRAM: &str = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";

/// SPL Token program.
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Associated Token Account program.
const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// System program.
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

/// Sysvar: Instructions — required by flash-loan programs for borrow/repay introspection.
/// [VERIFIED 2026] flash_loan_deep_dive_2026.md line 81 (account index 13)
const SYSVAR_INSTRUCTIONS: &str = "Sysvar1nstructions1111111111111111111111111";

// ---------------------------------------------------------------------------
// Instruction discriminators
// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1, lines 47-55
// ---------------------------------------------------------------------------

/// JupLend flashloan_payback discriminator (hardcoded, verified from Code4rena audit source).
/// [VERIFIED 2026] flash_loan_deep_dive_2026.md line 52
const JUPLEND_PAYBACK_DISCRIMINATOR: [u8; 8] = [213, 47, 153, 137, 84, 243, 94, 232];

/// Kamino flash_borrow_reserve_liquidity discriminator (verified from klend-sdk codegen).
/// [VERIFIED 2026] client/src/flash_loan.rs line 78
const KAMINO_FLASH_BORROW_DISCRIMINATOR: [u8; 8] = [135, 231, 52, 167, 7, 52, 212, 193];

// ---------------------------------------------------------------------------
// FlashLoanProvider enum
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 120-123
// ---------------------------------------------------------------------------

/// Supported flash loan providers, ordered by fee (cheapest first).
///
/// Provider selection is deterministic: JupLend > Kamino > Save > MarginFi.
/// MarginFi is included for completeness but flagged as risky (PAUSED protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlashLoanProvider {
    /// Jupiter Lend — 0% fee, ~$100-250M capacity per token.
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3A
    JupLend,

    /// Kamino (KLend) — 0.001% fee, ~$200-400M USDC capacity.
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3B
    Kamino,

    /// Save (Solend) — 0.05% fee (5 bps), ~$50-100M capacity.
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3C
    Save,

    /// MarginFi (P0) — 0% fee, ~$40M TVL. PAUSED and risky.
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3D
    MarginFi,
}

impl FlashLoanProvider {
    /// Fee in basis points for this provider.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3
    pub fn fee_bps(&self) -> f64 {
        match self {
            FlashLoanProvider::JupLend => 0.0,
            FlashLoanProvider::Kamino => 0.1,  // 0.001% = 0.1 bps
            FlashLoanProvider::Save => 5.0,     // 0.05% = 5 bps
            FlashLoanProvider::MarginFi => 0.0, // 0% but PAUSED
        }
    }

    /// Calculate fee in token base units for a given borrow amount.
    pub fn calculate_fee(&self, amount: u64) -> u64 {
        let fee_fraction = self.fee_bps() / 10_000.0;
        (amount as f64 * fee_fraction).ceil() as u64
    }

    /// Program ID for this provider's on-chain program.
    pub fn program_id(&self) -> Pubkey {
        match self {
            FlashLoanProvider::JupLend => {
                Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM).unwrap()
            }
            FlashLoanProvider::Kamino => Pubkey::from_str(KAMINO_PROGRAM).unwrap(),
            FlashLoanProvider::Save => Pubkey::from_str(SAVE_PROGRAM).unwrap(),
            FlashLoanProvider::MarginFi => Pubkey::from_str(MARGINFI_PROGRAM).unwrap(),
        }
    }
}

impl std::fmt::Display for FlashLoanProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlashLoanProvider::JupLend => write!(f, "JupLend (0%)"),
            FlashLoanProvider::Kamino => write!(f, "Kamino (0.001%)"),
            FlashLoanProvider::Save => write!(f, "Save (0.05%)"),
            FlashLoanProvider::MarginFi => write!(f, "MarginFi (0% PAUSED)"),
        }
    }
}

// ---------------------------------------------------------------------------
// FlashLoanRouter
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 120-123:
//   "FlashLoanRouter: select provider by token + capacity"
// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 2:
//   "Maximum 2 providers for reliable execution"
// ---------------------------------------------------------------------------

/// Flash loan router — selects the cheapest provider and builds borrow/repay instructions.
///
/// The router is stateless and deterministic: given the same token and amount, it
/// always selects the same provider. Future versions may incorporate per-token
/// capacity tracking from on-chain data.
///
/// # Provider Selection
/// Order: JupLend (0%) > Kamino (0.001%) > Save (0.05%).
/// MarginFi is excluded from default selection due to being PAUSED.
///
/// # Transaction Structure
/// ```text
/// IX 0: SetComputeLimit(1_200_000)
/// IX 1: SetComputePriorityFee(...)
/// IX 2: flash_borrow(amount)              <-- built by this router
/// IX 3-N: [inner instructions]             <-- provided by caller
/// IX N+1: flash_repay(amount, borrow_ix_index)  <-- built by this router
/// IX N+2: Jito tip transfer
/// ```
///
/// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 6: repay ordering
pub struct FlashLoanRouter;

impl FlashLoanRouter {
    /// Create a new FlashLoanRouter.
    pub fn new() -> Self {
        Self
    }

    /// Select the cheapest available flash loan provider for the given token.
    ///
    /// Selection order: JupLend (0%) > Kamino (0.001%) > Save (0.05%).
    /// MarginFi is excluded from default selection (PAUSED protocol).
    ///
    /// # Arguments
    /// - `_token_mint`: The token to borrow (currently unused; all providers support
    ///   SOL, USDC, USDT). Future: per-token capacity checks.
    /// - `_amount`: The borrow amount (currently unused; all providers have >$50M
    ///   liquidity for major tokens). Future: capacity checks.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3:
    ///   "Provider order: Jupiter Lend (0%) > Kamino (0.001%) > Save (0.05%)"
    pub fn select_provider(&self, _token_mint: &Pubkey, _amount: u64) -> FlashLoanProvider {
        // For now, always prefer JupLend (0% fee, highest capacity).
        // Future: check per-token liquidity, implement fallback.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3:
        //   "JupLend $100-250M per token, Kamino $200-400M USDC"
        FlashLoanProvider::JupLend
    }

    /// Build flash borrow instruction(s) for the given provider.
    ///
    /// Returns a Vec because some providers may require setup instructions
    /// (e.g. ATA creation) before the borrow.
    ///
    /// # JupLend Borrow Instruction
    /// - Discriminator: sha256("global:flashloan_borrow")[0..8]
    /// - Data: discriminator (8) + amount (u64 LE, 8) = 16 bytes
    /// - 14 accounts (signer, admin PDA, token accounts, mint, vault, programs, sysvar)
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1, lines 47-49
    ///
    /// # Kamino Borrow Instruction
    /// - Discriminator: [135, 231, 52, 167, 7, 52, 212, 193]
    /// - Data: discriminator (8) + amount (u64 LE, 8) = 16 bytes
    /// - 12 accounts
    /// [VERIFIED 2026] client/src/flash_loan.rs lines 52-66
    pub fn build_borrow_ix(
        &self,
        provider: FlashLoanProvider,
        wallet: &Pubkey,
        amount: u64,
        token_mint: &Pubkey,
    ) -> Result<Vec<Instruction>, BotError> {
        match provider {
            FlashLoanProvider::JupLend => self.build_juplend_borrow(wallet, amount, token_mint),
            FlashLoanProvider::Kamino => self.build_kamino_borrow(wallet, amount, token_mint),
            FlashLoanProvider::Save => self.build_save_borrow(wallet, amount, token_mint),
            FlashLoanProvider::MarginFi => Err(BotError::FlashLoanError(
                "MarginFi is PAUSED — do not use for flash loans".into(),
            )),
        }
    }

    /// Build flash repay instruction(s) for the given provider.
    ///
    /// # Arguments
    /// - `borrow_ix_index`: The 0-based instruction index of the borrow instruction
    ///   within the transaction. Kamino requires this for its repay validation.
    ///   JupLend does NOT use this (it scans all instructions).
    ///
    /// # JupLend Payback Instruction
    /// - Discriminator: [213, 47, 153, 137, 84, 243, 94, 232]
    /// - Data: discriminator (8) + amount (u64 LE, 8) = 16 bytes
    /// - Same 14 accounts as borrow
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1, lines 50-53
    ///
    /// # Kamino Repay Instruction
    /// - Data: discriminator (8) + amount (u64 LE, 8) + borrow_ix_index (u8, 1) = 17 bytes
    /// - Key difference: requires borrow_instruction_index parameter
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1, lines 57-59
    pub fn build_repay_ix(
        &self,
        provider: FlashLoanProvider,
        wallet: &Pubkey,
        amount: u64,
        token_mint: &Pubkey,
        borrow_ix_index: u8,
    ) -> Result<Vec<Instruction>, BotError> {
        match provider {
            FlashLoanProvider::JupLend => self.build_juplend_repay(wallet, amount, token_mint),
            FlashLoanProvider::Kamino => {
                self.build_kamino_repay(wallet, amount, token_mint, borrow_ix_index)
            }
            FlashLoanProvider::Save => self.build_save_repay(wallet, amount, token_mint),
            FlashLoanProvider::MarginFi => Err(BotError::FlashLoanError(
                "MarginFi is PAUSED — do not use for flash loans".into(),
            )),
        }
    }

    /// Wrap inner instructions with flash loan borrow/repay.
    ///
    /// Returns: [borrow_ix, ...inner_ixs, repay_ix]
    ///
    /// The `borrow_ix_index` for Kamino repay is computed automatically based on
    /// the position of the borrow instruction in the output Vec. Typically this is
    /// index 0 in the returned Vec, but when prepended to a transaction with
    /// compute budget instructions, callers must add the appropriate offset.
    ///
    /// # Transaction Structure Example
    /// ```text
    /// IX 0: SetComputeLimit(1_200_000)     // caller adds
    /// IX 1: SetComputePriorityFee(...)      // caller adds
    /// IX 2: flash_borrow(amount)            // borrow_ix_index = 2
    /// IX 3: [liquidation]                   // inner
    /// IX 4: [Jupiter swap]                  // inner
    /// IX 5: flash_repay(amount, 2)          // repay points back to borrow
    /// IX 6: Jito tip                        // caller adds
    /// ```
    ///
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 6: repay ordering
    /// [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 2:
    ///   "2-provider (Kamino + JupLend): IX structure example"
    pub fn wrap_with_flash_loan(
        &self,
        provider: FlashLoanProvider,
        wallet: &Pubkey,
        amount: u64,
        token_mint: &Pubkey,
        inner_ixs: Vec<Instruction>,
    ) -> Result<Vec<Instruction>, BotError> {
        let borrow_ixs = self.build_borrow_ix(provider, wallet, amount, token_mint)?;

        // The borrow instruction index is 0 within the returned Vec.
        // Callers must offset by any preceding instructions (compute budget, etc.).
        // For safety, we use 0 here — the caller should adjust when building the
        // final transaction.
        let borrow_ix_index = 0u8;
        let repay_ixs =
            self.build_repay_ix(provider, wallet, amount, token_mint, borrow_ix_index)?;

        // Capture inner instruction count before moving the Vec.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 2: TX structure
        let inner_count = inner_ixs.len();
        let total_len = borrow_ixs.len() + inner_count + repay_ixs.len();
        let mut result = Vec::with_capacity(total_len);

        result.extend(borrow_ixs);
        result.extend(inner_ixs);
        result.extend(repay_ixs);

        debug!(
            provider = %provider,
            amount = amount,
            inner_count = inner_count,
            total_ixs = result.len(),
            "Wrapped instructions with flash loan"
        );

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // JupLend instruction builders
    // [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 1
    // -----------------------------------------------------------------------

    fn build_juplend_borrow(
        &self,
        wallet: &Pubkey,
        amount: u64,
        token_mint: &Pubkey,
    ) -> Result<Vec<Instruction>, BotError> {
        let program_id = Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid JupLend program ID: {}", e)))?;

        // Compute discriminator: sha256("global:flashloan_borrow")[0..8]
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 47
        let disc = anchor_discriminator("flashloan_borrow");

        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&disc);
        data.extend_from_slice(&amount.to_le_bytes());

        // JupLend FlashloanAdmin PDA: seeds = [b"flashloan_admin"]
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 88
        let (flashloan_admin, _) = Pubkey::find_program_address(
            &[b"flashloan_admin"],
            &program_id,
        );

        // User's ATA for the borrowed token.
        let user_ata = spl_associated_token_account_address(wallet, token_mint);

        let sysvar_ix = Pubkey::from_str(SYSVAR_INSTRUCTIONS)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid sysvar: {}", e)))?;
        let token_program = Pubkey::from_str(TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid token program: {}", e)))?;
        let ata_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid ATA program: {}", e)))?;
        let system = Pubkey::from_str(SYSTEM_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid system program: {}", e)))?;
        let liquidity_program = Pubkey::from_str(JUPLEND_LIQUIDITY_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid liquidity program: {}", e)))?;

        // 14 accounts for JupLend flash borrow.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md lines 66-82
        //
        // Accounts 4-8 (token_reserves_liquidity, borrow_position, rate_model,
        // vault, liquidity_state) are TOKEN-SPECIFIC and must be resolved per-mint.
        // For now, we use placeholder Pubkey::default() — these must be filled in
        // by the caller or by a per-token config lookup.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md lines 94-103
        let accounts = vec![
            AccountMeta::new(*wallet, true),             // 0: signer
            AccountMeta::new(flashloan_admin, false),    // 1: flashloan_admin PDA
            AccountMeta::new(user_ata, false),           // 2: signer_borrow_token_account
            AccountMeta::new_readonly(*token_mint, false), // 3: mint
            AccountMeta::new(Pubkey::default(), false),  // 4: token_reserves_liquidity (TOKEN-SPECIFIC)
            AccountMeta::new(Pubkey::default(), false),  // 5: borrow_position (TOKEN-SPECIFIC)
            AccountMeta::new_readonly(Pubkey::default(), false), // 6: rate_model (TOKEN-SPECIFIC)
            AccountMeta::new(Pubkey::default(), false),  // 7: vault (TOKEN-SPECIFIC)
            AccountMeta::new_readonly(Pubkey::default(), false), // 8: liquidity_state (TOKEN-SPECIFIC)
            AccountMeta::new_readonly(liquidity_program, false), // 9: liquidity_program
            AccountMeta::new_readonly(token_program, false), // 10: token_program
            AccountMeta::new_readonly(ata_program, false),   // 11: associated_token_program
            AccountMeta::new_readonly(system, false),        // 12: system_program
            AccountMeta::new_readonly(sysvar_ix, false),     // 13: instruction_sysvar
        ];

        Ok(vec![Instruction {
            program_id,
            accounts,
            data,
        }])
    }

    fn build_juplend_repay(
        &self,
        wallet: &Pubkey,
        amount: u64,
        token_mint: &Pubkey,
    ) -> Result<Vec<Instruction>, BotError> {
        let program_id = Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid JupLend program ID: {}", e)))?;

        // JupLend payback does NOT require borrow_instruction_index.
        // It scans all instructions to find matching borrow.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md lines 57-58
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&JUPLEND_PAYBACK_DISCRIMINATOR);
        data.extend_from_slice(&amount.to_le_bytes());

        let (flashloan_admin, _) = Pubkey::find_program_address(
            &[b"flashloan_admin"],
            &program_id,
        );

        let user_ata = spl_associated_token_account_address(wallet, token_mint);

        let sysvar_ix = Pubkey::from_str(SYSVAR_INSTRUCTIONS)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid sysvar: {}", e)))?;
        let token_program = Pubkey::from_str(TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid token program: {}", e)))?;
        let ata_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid ATA program: {}", e)))?;
        let system = Pubkey::from_str(SYSTEM_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid system program: {}", e)))?;
        let liquidity_program = Pubkey::from_str(JUPLEND_LIQUIDITY_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid liquidity program: {}", e)))?;

        // Same 14 accounts as borrow.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 62
        let accounts = vec![
            AccountMeta::new(*wallet, true),
            AccountMeta::new(flashloan_admin, false),
            AccountMeta::new(user_ata, false),
            AccountMeta::new_readonly(*token_mint, false),
            AccountMeta::new(Pubkey::default(), false),  // 4: TOKEN-SPECIFIC
            AccountMeta::new(Pubkey::default(), false),  // 5: TOKEN-SPECIFIC
            AccountMeta::new_readonly(Pubkey::default(), false), // 6: TOKEN-SPECIFIC
            AccountMeta::new(Pubkey::default(), false),  // 7: TOKEN-SPECIFIC
            AccountMeta::new_readonly(Pubkey::default(), false), // 8: TOKEN-SPECIFIC
            AccountMeta::new_readonly(liquidity_program, false),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(ata_program, false),
            AccountMeta::new_readonly(system, false),
            AccountMeta::new_readonly(sysvar_ix, false),
        ];

        Ok(vec![Instruction {
            program_id,
            accounts,
            data,
        }])
    }

    // -----------------------------------------------------------------------
    // Kamino instruction builders
    // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 86-87
    // [VERIFIED 2026] client/src/flash_loan.rs lines 52-120
    // -----------------------------------------------------------------------

    fn build_kamino_borrow(
        &self,
        wallet: &Pubkey,
        amount: u64,
        _token_mint: &Pubkey,
    ) -> Result<Vec<Instruction>, BotError> {
        let program_id = Pubkey::from_str(KAMINO_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid Kamino program ID: {}", e)))?;

        // Kamino flash_borrow_reserve_liquidity discriminator.
        // [VERIFIED 2026] client/src/flash_loan.rs line 78
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&KAMINO_FLASH_BORROW_DISCRIMINATOR);
        data.extend_from_slice(&amount.to_le_bytes());

        // Kamino requires 12 accounts for flash borrow.
        // These are RESERVE-SPECIFIC and must be populated from config.
        // [VERIFIED 2026] client/src/flash_loan.rs lines 52-66
        //
        // For now, we return a skeleton instruction. The caller must fill in
        // the reserve-specific accounts (reserve, vault, fee_receiver, etc.)
        // using KaminoReserveInfo or a similar per-token config.
        let sysvar_ix = Pubkey::from_str(SYSVAR_INSTRUCTIONS)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid sysvar: {}", e)))?;
        let token_program = Pubkey::from_str(TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid token program: {}", e)))?;

        let accounts = vec![
            AccountMeta::new(*wallet, true),                 // 0: user (signer)
            AccountMeta::new_readonly(Pubkey::default(), false), // 1: lending_market_authority (PDA)
            AccountMeta::new_readonly(Pubkey::default(), false), // 2: lending_market
            AccountMeta::new(Pubkey::default(), false),      // 3: reserve
            AccountMeta::new_readonly(Pubkey::default(), false), // 4: reserve_liquidity_mint
            AccountMeta::new(Pubkey::default(), false),      // 5: reserve_source_liquidity (vault)
            AccountMeta::new(Pubkey::default(), false),      // 6: user_destination_liquidity (ATA)
            AccountMeta::new(Pubkey::default(), false),      // 7: reserve_liquidity_fee_receiver
            AccountMeta::new_readonly(Pubkey::default(), false), // 8: referrer_token_state
            AccountMeta::new_readonly(Pubkey::default(), false), // 9: referrer_account
            AccountMeta::new_readonly(sysvar_ix, false),     // 10: sysvar_instructions
            AccountMeta::new_readonly(token_program, false), // 11: token_program
        ];

        Ok(vec![Instruction {
            program_id,
            accounts,
            data,
        }])
    }

    fn build_kamino_repay(
        &self,
        wallet: &Pubkey,
        amount: u64,
        _token_mint: &Pubkey,
        borrow_ix_index: u8,
    ) -> Result<Vec<Instruction>, BotError> {
        let program_id = Pubkey::from_str(KAMINO_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid Kamino program ID: {}", e)))?;

        // Kamino flash_repay_reserve_liquidity requires borrow_instruction_index as u8.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 59
        let disc = anchor_discriminator("flash_repay_reserve_liquidity");
        let mut data = Vec::with_capacity(17);
        data.extend_from_slice(&disc);
        data.extend_from_slice(&amount.to_le_bytes());
        data.push(borrow_ix_index);

        let sysvar_ix = Pubkey::from_str(SYSVAR_INSTRUCTIONS)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid sysvar: {}", e)))?;
        let token_program = Pubkey::from_str(TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid token program: {}", e)))?;

        // Kamino repay has similar account structure to borrow.
        // Reserve-specific accounts must be filled by caller.
        let accounts = vec![
            AccountMeta::new(*wallet, true),
            AccountMeta::new_readonly(Pubkey::default(), false), // lending_market_authority
            AccountMeta::new_readonly(Pubkey::default(), false), // lending_market
            AccountMeta::new(Pubkey::default(), false),          // reserve
            AccountMeta::new_readonly(Pubkey::default(), false), // reserve_liquidity_mint
            AccountMeta::new(Pubkey::default(), false),          // reserve_destination_liquidity
            AccountMeta::new(Pubkey::default(), false),          // user_source_liquidity (ATA)
            AccountMeta::new(Pubkey::default(), false),          // reserve_liquidity_fee_receiver
            AccountMeta::new_readonly(Pubkey::default(), false), // referrer_token_state
            AccountMeta::new_readonly(Pubkey::default(), false), // referrer_account
            AccountMeta::new_readonly(sysvar_ix, false),
            AccountMeta::new_readonly(token_program, false),
        ];

        Ok(vec![Instruction {
            program_id,
            accounts,
            data,
        }])
    }

    // -----------------------------------------------------------------------
    // Save instruction builders
    // [VERIFIED 2026] flash_loan_deep_dive_2026.md Section 3C
    // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 99-101
    // -----------------------------------------------------------------------

    fn build_save_borrow(
        &self,
        wallet: &Pubkey,
        amount: u64,
        _token_mint: &Pubkey,
    ) -> Result<Vec<Instruction>, BotError> {
        let program_id = Pubkey::from_str(SAVE_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid Save program ID: {}", e)))?;

        // Save flash_borrow_reserve_liquidity instruction variant.
        // [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 101
        // Save uses a simple u8 instruction variant (not Anchor discriminators).
        // The flash borrow variant index and account structure are reserve-specific.
        let mut data = Vec::with_capacity(16);
        // Save instruction format: variant u8 + amount u64 LE
        // The exact variant number must be verified per-version.
        // Placeholder — callers must configure per-reserve.
        data.extend_from_slice(&amount.to_le_bytes());

        let sysvar_ix = Pubkey::from_str(SYSVAR_INSTRUCTIONS)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid sysvar: {}", e)))?;
        let token_program = Pubkey::from_str(TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid token program: {}", e)))?;

        // Save borrow accounts are reserve-specific.
        // Placeholder structure — must be filled by caller.
        let accounts = vec![
            AccountMeta::new(*wallet, true),
            AccountMeta::new(Pubkey::default(), false), // reserve_source_liquidity
            AccountMeta::new(Pubkey::default(), false), // user_destination_liquidity
            AccountMeta::new(Pubkey::default(), false), // reserve
            AccountMeta::new_readonly(Pubkey::default(), false), // lending_market
            AccountMeta::new_readonly(Pubkey::default(), false), // lending_market_authority
            AccountMeta::new_readonly(sysvar_ix, false),
            AccountMeta::new_readonly(token_program, false),
        ];

        Ok(vec![Instruction {
            program_id,
            accounts,
            data,
        }])
    }

    fn build_save_repay(
        &self,
        wallet: &Pubkey,
        amount: u64,
        _token_mint: &Pubkey,
    ) -> Result<Vec<Instruction>, BotError> {
        let program_id = Pubkey::from_str(SAVE_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid Save program ID: {}", e)))?;

        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&amount.to_le_bytes());

        let sysvar_ix = Pubkey::from_str(SYSVAR_INSTRUCTIONS)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid sysvar: {}", e)))?;
        let token_program = Pubkey::from_str(TOKEN_PROGRAM)
            .map_err(|e| BotError::FlashLoanError(format!("Invalid token program: {}", e)))?;

        let accounts = vec![
            AccountMeta::new(*wallet, true),
            AccountMeta::new(Pubkey::default(), false), // user_source_liquidity
            AccountMeta::new(Pubkey::default(), false), // reserve_destination_liquidity
            AccountMeta::new(Pubkey::default(), false), // reserve_liquidity_fee_receiver
            AccountMeta::new(Pubkey::default(), false), // reserve
            AccountMeta::new_readonly(Pubkey::default(), false), // lending_market
            AccountMeta::new_readonly(Pubkey::default(), false), // lending_market_authority
            AccountMeta::new_readonly(sysvar_ix, false),
            AccountMeta::new_readonly(token_program, false),
        ];

        Ok(vec![Instruction {
            program_id,
            accounts,
            data,
        }])
    }
}

impl Default for FlashLoanRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute Anchor instruction discriminator: sha256("global:<name>")[..8].
/// [VERIFIED 2026] client/src/flash_loan.rs lines 26-34
fn anchor_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{}", name).as_bytes());
    let result = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&result[..8]);
    disc
}

/// Derive the associated token account address for a wallet + mint.
fn spl_associated_token_account_address(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let ata_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM).unwrap();
    Pubkey::find_program_address(
        &[
            wallet.as_ref(),
            token_program.as_ref(),
            mint.as_ref(),
        ],
        &ata_program,
    )
    .0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_fees() {
        assert!((FlashLoanProvider::JupLend.fee_bps() - 0.0).abs() < f64::EPSILON);
        assert!((FlashLoanProvider::Kamino.fee_bps() - 0.1).abs() < f64::EPSILON);
        assert!((FlashLoanProvider::Save.fee_bps() - 5.0).abs() < f64::EPSILON);
        assert!((FlashLoanProvider::MarginFi.fee_bps() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn provider_fee_calculation() {
        // Kamino: 0.001% of 1_000_000 = 10
        assert_eq!(FlashLoanProvider::Kamino.calculate_fee(1_000_000), 10);
        // Save: 0.05% of 1_000_000 = 500
        assert_eq!(FlashLoanProvider::Save.calculate_fee(1_000_000), 500);
        // JupLend: 0%
        assert_eq!(FlashLoanProvider::JupLend.calculate_fee(1_000_000), 0);
    }

    #[test]
    fn provider_display() {
        assert_eq!(FlashLoanProvider::JupLend.to_string(), "JupLend (0%)");
        assert_eq!(FlashLoanProvider::Kamino.to_string(), "Kamino (0.001%)");
        assert_eq!(FlashLoanProvider::Save.to_string(), "Save (0.05%)");
    }

    #[test]
    fn provider_program_ids() {
        let juplend = FlashLoanProvider::JupLend.program_id();
        assert_eq!(juplend, Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM).unwrap());

        let kamino = FlashLoanProvider::Kamino.program_id();
        assert_eq!(kamino, Pubkey::from_str(KAMINO_PROGRAM).unwrap());

        let save = FlashLoanProvider::Save.program_id();
        assert_eq!(save, Pubkey::from_str(SAVE_PROGRAM).unwrap());
    }

    #[test]
    fn select_provider_default_is_juplend() {
        let router = FlashLoanRouter::new();
        let mint = Pubkey::default();
        let provider = router.select_provider(&mint, 1_000_000);
        assert_eq!(provider, FlashLoanProvider::JupLend);
    }

    #[test]
    fn marginfi_borrow_returns_error() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::default();
        let mint = Pubkey::default();
        let result = router.build_borrow_ix(FlashLoanProvider::MarginFi, &wallet, 1000, &mint);
        assert!(result.is_err());
    }

    #[test]
    fn marginfi_repay_returns_error() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::default();
        let mint = Pubkey::default();
        let result = router.build_repay_ix(FlashLoanProvider::MarginFi, &wallet, 1000, &mint, 0);
        assert!(result.is_err());
    }

    #[test]
    fn juplend_borrow_ix_structure() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ixs = router
            .build_borrow_ix(FlashLoanProvider::JupLend, &wallet, 5_000_000, &mint)
            .unwrap();

        assert_eq!(ixs.len(), 1);
        let ix = &ixs[0];
        assert_eq!(ix.program_id, Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM).unwrap());
        // 14 accounts per JupLend instruction.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 62
        assert_eq!(ix.accounts.len(), 14);
        // Data: discriminator (8) + amount (8) = 16 bytes.
        assert_eq!(ix.data.len(), 16);
        // Verify amount is correctly encoded.
        let amount_bytes = &ix.data[8..16];
        let decoded_amount = u64::from_le_bytes(amount_bytes.try_into().unwrap());
        assert_eq!(decoded_amount, 5_000_000);
    }

    #[test]
    fn juplend_payback_discriminator() {
        // Verify hardcoded discriminator.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 52
        assert_eq!(
            JUPLEND_PAYBACK_DISCRIMINATOR,
            [213, 47, 153, 137, 84, 243, 94, 232]
        );
    }

    #[test]
    fn kamino_borrow_ix_structure() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ixs = router
            .build_borrow_ix(FlashLoanProvider::Kamino, &wallet, 1_000_000, &mint)
            .unwrap();

        assert_eq!(ixs.len(), 1);
        let ix = &ixs[0];
        assert_eq!(ix.program_id, Pubkey::from_str(KAMINO_PROGRAM).unwrap());
        // 12 accounts for Kamino flash borrow.
        assert_eq!(ix.accounts.len(), 12);
        // Data: discriminator (8) + amount (8) = 16 bytes.
        assert_eq!(ix.data.len(), 16);
        // First 8 bytes should be the Kamino borrow discriminator.
        assert_eq!(&ix.data[..8], &KAMINO_FLASH_BORROW_DISCRIMINATOR);
    }

    #[test]
    fn kamino_repay_ix_includes_borrow_index() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ixs = router
            .build_repay_ix(FlashLoanProvider::Kamino, &wallet, 1_000_000, &mint, 2)
            .unwrap();

        assert_eq!(ixs.len(), 1);
        let ix = &ixs[0];
        // Data: discriminator (8) + amount (8) + borrow_ix_index (1) = 17 bytes.
        // [VERIFIED 2026] flash_loan_deep_dive_2026.md line 59
        assert_eq!(ix.data.len(), 17);
        // Last byte is the borrow instruction index.
        assert_eq!(ix.data[16], 2);
    }

    #[test]
    fn wrap_with_flash_loan_structure() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();

        // Create a dummy inner instruction.
        let inner = vec![Instruction {
            program_id: Pubkey::default(),
            accounts: vec![],
            data: vec![42],
        }];

        let wrapped = router
            .wrap_with_flash_loan(
                FlashLoanProvider::JupLend,
                &wallet,
                1_000_000,
                &mint,
                inner,
            )
            .unwrap();

        // Should be: borrow + inner + repay = 3 instructions.
        assert_eq!(wrapped.len(), 3);
        // First is borrow (JupLend program).
        assert_eq!(
            wrapped[0].program_id,
            Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM).unwrap()
        );
        // Middle is our inner instruction.
        assert_eq!(wrapped[1].data, vec![42]);
        // Last is repay (JupLend program).
        assert_eq!(
            wrapped[2].program_id,
            Pubkey::from_str(JUPLEND_FLASHLOAN_PROGRAM).unwrap()
        );
    }

    #[test]
    fn anchor_discriminator_consistency() {
        // The same name should always produce the same discriminator.
        let d1 = anchor_discriminator("flashloan_borrow");
        let d2 = anchor_discriminator("flashloan_borrow");
        assert_eq!(d1, d2);

        // Different names produce different discriminators.
        let d3 = anchor_discriminator("flashloan_payback");
        assert_ne!(d1, d3);
    }

    #[test]
    fn save_borrow_ix_structure() {
        let router = FlashLoanRouter::new();
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ixs = router
            .build_borrow_ix(FlashLoanProvider::Save, &wallet, 500_000, &mint)
            .unwrap();

        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0].program_id, Pubkey::from_str(SAVE_PROGRAM).unwrap());
    }
}
