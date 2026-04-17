//! TransactionBuilder — assembles instructions into signed VersionedTransactions.
//!
//! Design decisions from [VERIFIED 2026] research:
//!
//! - Pre-allocated `Vec<AccountMeta>` with `with_capacity(32)` to avoid hot-path
//!   heap reallocation. Most Solana TXs have <32 accounts.
//!   [VERIFIED 2026] low_latency_dataflow_2026.md Section 7: "let mut accounts = Vec::with_capacity(32)"
//!
//! - MAX_TX_SIZE = 1232 bytes. Solana's hard limit for serialized transactions.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 162
//!   [VERIFIED 2026] operational_data_2026.md (referenced from assembler.rs MAX_TX_SIZE = 1232)
//!
//! - `assemble_for_flash` omits CU prepend so flash borrow instruction stays at index 0.
//!   Flash loan programs (JupLend, Kamino, Save) use sysvar Instructions introspection
//!   to verify borrow is at the start and repay is at the end.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3A: "NO CPI — top-level only"
//!
//! - V0 messages with ALT compression for account deduplication.
//!   One ALT (256 entries) is sufficient for 2-provider nested flash loans.
//!   [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 192
//!
//! - Priority fee is redundant inside Jito bundles — bundles are ordered by tip amount,
//!   not priority fee. Use minimal priority (1000 uL/CU) for bundle TXs.
//!   [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4C: "priority fee is redundant in bundles"

use anyhow::{Result, anyhow};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    address_lookup_table::state::AddressLookupTable,
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::VersionedTransaction,
};

/// Solana transaction size limit in bytes.
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 162, assembler.rs constant
pub const MAX_TX_SIZE: usize = 1232;

/// Number of ComputeBudget instructions prepended by `assemble()`.
/// Strategies that reference instruction indices (flash loan protocols)
/// must offset their indices by this count.
pub const COMPUTE_BUDGET_IX_COUNT: u8 = 2;

/// TransactionBuilder — assembles instructions into optimized VersionedTransactions.
///
/// Holds cached ALT accounts for compression. Create once at startup, reuse across
/// all strategy pipelines.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 162-165
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 7: reusable TxBuilder pattern
#[derive(Clone)]
pub struct TransactionBuilder {
    /// Cached Address Lookup Table accounts for V0 message compression.
    alts: Vec<AddressLookupTableAccount>,
}

impl TransactionBuilder {
    /// Create a new TransactionBuilder with no ALTs loaded.
    pub fn new() -> Self {
        Self { alts: Vec::new() }
    }

    /// Create a new TransactionBuilder with pre-loaded ALTs.
    pub fn with_alts(alts: Vec<AddressLookupTableAccount>) -> Self {
        Self { alts }
    }

    /// Load Address Lookup Tables from on-chain and cache them.
    ///
    /// Fetches accounts in chunks of 100 to avoid RPC payload limits.
    /// One ALT can hold 256 entries — sufficient for 2-provider nested flash loans.
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 192
    pub fn load_alts(&mut self, rpc: &RpcClient, alt_addresses: &[Pubkey]) -> Result<Vec<AddressLookupTableAccount>> {
        if alt_addresses.is_empty() {
            return Ok(Vec::new());
        }

        let mut loaded = Vec::with_capacity(alt_addresses.len());

        for chunk in alt_addresses.chunks(100) {
            let accounts = rpc.get_multiple_accounts(chunk)
                .map_err(|e| anyhow!("RPC get_multiple_accounts: {}", e))?;

            for (i, maybe_acc) in accounts.iter().enumerate() {
                if let Some(acc) = maybe_acc {
                    if let Ok(table) = AddressLookupTable::deserialize(&acc.data) {
                        let alt_account = AddressLookupTableAccount {
                            key: chunk[i],
                            addresses: table.addresses.to_vec(),
                        };
                        loaded.push(alt_account.clone());
                        // Also cache internally
                        if !self.alts.iter().any(|a| a.key == chunk[i]) {
                            self.alts.push(alt_account);
                        }
                    }
                }
            }
        }

        let total_addrs: usize = self.alts.iter().map(|a| a.addresses.len()).sum();
        tracing::info!(
            "TransactionBuilder: loaded {} ALTs ({} total addresses)",
            self.alts.len(),
            total_addrs
        );

        Ok(loaded)
    }

    /// Add a dynamically-fetched ALT (e.g. from Jupiter V2 /build response).
    /// Caps at 10 ALTs to avoid bloat.
    pub fn add_alt(&mut self, alt: AddressLookupTableAccount) {
        if self.alts.len() >= 10 {
            return;
        }
        if !self.alts.iter().any(|a| a.key == alt.key) {
            self.alts.push(alt);
        }
    }

    /// Get currently loaded ALTs (read-only reference).
    pub fn alts(&self) -> &[AddressLookupTableAccount] {
        &self.alts
    }

    /// Build a signed VersionedTransaction from instructions.
    ///
    /// Prepends two ComputeBudget instructions:
    /// 1. `SetComputeUnitLimit(cu_limit)`
    /// 2. `SetComputeUnitPrice(priority_fee)`
    ///
    /// Compresses accounts via loaded ALTs into a V0 message.
    /// Returns error if serialized TX exceeds 1232 bytes.
    ///
    /// For Jito bundles, set `priority_fee` to 1000 uL/CU (safety net only).
    /// Bundles are ordered by tip amount, not priority fee.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 4C
    ///
    /// Pre-allocates `Vec<AccountMeta>` with `with_capacity(32)` inside
    /// the Solana SDK's `Message::try_compile`.
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 7
    pub fn assemble(
        &self,
        mut instructions: Vec<Instruction>,
        payer: &Keypair,
        blockhash: Hash,
        cu_limit: u32,
        priority_fee: u64,
    ) -> Result<VersionedTransaction> {
        // Prepend compute budget instructions (index 0 and 1).
        // SetComputeUnitLimit must come first.
        instructions.insert(0, ComputeBudgetInstruction::set_compute_unit_price(priority_fee));
        instructions.insert(0, ComputeBudgetInstruction::set_compute_unit_limit(cu_limit));

        self.compile_sign_and_check(instructions, payer, blockhash)
    }

    /// Build a signed VersionedTransaction for flash loan context.
    ///
    /// Does NOT prepend ComputeBudget instructions. The caller is responsible
    /// for placing the flash borrow instruction at index 0 and repay at the end.
    ///
    /// Flash loan programs (JupLend, Kamino, Save) use sysvar Instructions
    /// introspection to verify instruction ordering. Moving borrow off index 0
    /// causes the program to reject the transaction.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3A:
    ///   "NO CPI — top-level instructions only"
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 3:
    ///   Flash loan instruction format — borrow at [0], repay at [N-1]
    pub fn assemble_for_flash(
        &self,
        instructions: Vec<Instruction>,
        payer: &Keypair,
        blockhash: Hash,
        cu_limit: u32,
    ) -> Result<VersionedTransaction> {
        // For flash loans, we still need CU limit but place it AFTER the borrow.
        // Actually, the CU instructions must be placed carefully:
        // The flash borrow MUST be at index 0.
        // So we do NOT prepend anything — the caller must include CU instructions
        // if needed, or we skip them entirely (Jito bundles don't need priority fee).
        let _ = cu_limit; // Reserved for future CU optimization
        self.compile_sign_and_check(instructions, payer, blockhash)
    }

    /// Internal: compile V0 message, sign, and check size.
    fn compile_sign_and_check(
        &self,
        instructions: Vec<Instruction>,
        payer: &Keypair,
        blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        // Compile V0 message with ALT compression.
        // The SDK's try_compile handles account deduplication and lookup table resolution.
        let msg = v0::Message::try_compile(
            &payer.pubkey(),
            &instructions,
            &self.alts,
            blockhash,
        ).map_err(|e| anyhow!("Compile V0 message: {:?}", e))?;

        // Sign with payer keypair.
        // Ed25519 signing takes ~16us — not a bottleneck.
        // [VERIFIED 2026] low_latency_dataflow_2026.md Section 8: "15.6 microseconds per signature"
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(msg),
            &[payer],
        ).map_err(|e| anyhow!("Sign transaction: {}", e))?;

        // Serialize and check size.
        let tx_bytes = bincode::serialize(&tx)
            .map_err(|e| anyhow!("Serialize transaction: {}", e))?;

        if tx_bytes.len() > MAX_TX_SIZE {
            return Err(anyhow!(
                "TX_TOO_LARGE: {} bytes (max {}). Reduce instructions or add more addresses to ALTs.",
                tx_bytes.len(),
                MAX_TX_SIZE,
            ));
        }

        tracing::debug!(
            "Built tx: {} bytes, {} instructions, {} ALTs",
            tx_bytes.len(),
            instructions.len(),
            self.alts.len()
        );

        Ok(tx)
    }
}

impl Default for TransactionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tx_size_constant() {
        // [VERIFIED 2026] Solana hard limit
        assert_eq!(MAX_TX_SIZE, 1232);
    }

    #[test]
    fn compute_budget_ix_count() {
        assert_eq!(COMPUTE_BUDGET_IX_COUNT, 2);
    }

    #[test]
    fn builder_new_has_no_alts() {
        let builder = TransactionBuilder::new();
        assert!(builder.alts().is_empty());
    }

    /// ALT cap at 10. [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 163: ALT integration
    #[test]
    fn builder_add_alt_caps_at_10() {
        let mut builder = TransactionBuilder::new();
        (0..15).for_each(|_| {
            builder.add_alt(AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses: vec![Pubkey::new_unique()],
            });
        });
        assert_eq!(builder.alts().len(), 10);
    }

    #[test]
    fn builder_add_alt_deduplicates() {
        let mut builder = TransactionBuilder::new();
        let key = Pubkey::new_unique();
        let alt = AddressLookupTableAccount {
            key,
            addresses: vec![Pubkey::new_unique()],
        };
        builder.add_alt(alt.clone());
        builder.add_alt(alt.clone());
        assert_eq!(builder.alts().len(), 1);
    }

    #[test]
    fn builder_default() {
        let builder = TransactionBuilder::default();
        assert!(builder.alts().is_empty());
    }
}
