"""
AtomicArbBuilder — True zero-capital flash loan arb in one Solana transaction.

Full execution flow (all in a SINGLE VersionedTransaction):

  ComputeBudget ix:     set priority fee + compute unit limit
  [optional] Init ix:   create MarginFi account if not exists
  [optional] Init ATA:  create user USDC ATA if not exists
  StartFlashloan ix:    borrow USDC from MarginFi USDC pool
  BuySwap ix(es):       USDC → token  (via Jupiter)
  SellSwap ix(es):      token → USDC  (via Jupiter)
  EndFlashloan ix:      repay USDC + 0.09% fee, keep profit

If the USDC balance after sell is insufficient to repay, the runtime
rejects the ENTIRE transaction. Loan never taken. Gas only cost.

Uses:
  - Jupiter /swap-instructions endpoint for composable raw instructions
  - MarginFi v2 flash loan program instructions (marginfi_flash.py)
  - solders for VersionedTransaction + MessageV0 + AddressLookupTable
  - RPC sendTransaction for broadcast

Integration:
  builder = AtomicArbBuilder(rpc_url, wallet, session)
  await builder.init()
  result = await builder.execute_flash_arb(buy_quote, sell_quote, loan_usdc)
"""

from __future__ import annotations

import asyncio
import base64
import json
import struct
import uuid
from dataclasses import dataclass, field
from typing import Optional

import aiohttp
import structlog

from solders.address_lookup_table_account import AddressLookupTableAccount
from solders.hash import Hash
from solders.instruction import AccountMeta, Instruction
from solders.message import MessageV0
from solders.pubkey import Pubkey
from solders.transaction import VersionedTransaction

from src.execution.marginfi_flash import (
    MARGINFI_GROUP,
    MARGINFI_PROGRAM,
    MarginFiBankInfo,
    MarginFiFlash,
    TOKEN_PROGRAM,
    USDC_MINT,
    build_end_flashloan_ix,
    build_init_marginfi_account_ix,
    build_start_flashloan_ix,
    derive_marginfi_account,
    derive_usdc_ata,
)

logger = structlog.get_logger(__name__)

# ── Jupiter API ────────────────────────────────────────────────────────────

JUPITER_SWAP_INSTRUCTIONS_URL = "https://api.jup.ag/swap/v1/swap-instructions"

# Compute budget program
COMPUTE_BUDGET_PROGRAM = Pubkey.from_string("ComputeBudget111111111111111111111111111111")
TOKEN_PROGRAM_2022 = Pubkey.from_string("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")

# Default execution parameters
DEFAULT_COMPUTE_UNITS   = 400_000
DEFAULT_PRIORITY_FEE_MICROLAMPORTS = 100_000  # ~0.0001 SOL priority fee
LOAN_USDC_LAMPORTS      = 500_000_000   # $500 USDC (6 decimals)


# ── Result dataclass ───────────────────────────────────────────────────────

@dataclass
class FlashArbTxResult:
    success: bool
    signature: str = ""
    profit_usdc_lamports: int = 0
    fee_usdc_lamports: int = 0
    gas_sol: float = 0.0
    error: str = ""
    tx_size_bytes: int = 0

    @property
    def profit_usd(self) -> float:
        return self.profit_usdc_lamports / 1_000_000

    @property
    def net_profit_usd(self) -> float:
        return self.profit_usd - self.gas_sol


# ── Instruction conversion helpers ─────────────────────────────────────────

def _pubkey(s: str) -> Pubkey:
    return Pubkey.from_string(s)


def _ix_from_jupiter(jup_ix: dict) -> Instruction:
    """
    Convert a Jupiter instruction dict to a solders Instruction.

    Jupiter format:
        {
            "programId": "...",
            "accounts": [{"pubkey": "...", "isSigner": bool, "isWritable": bool}],
            "data": "base64_encoded_bytes"
        }
    """
    prog = _pubkey(jup_ix["programId"])
    accounts = [
        AccountMeta(
            pubkey     = _pubkey(a["pubkey"]),
            is_signer  = a.get("isSigner", False),
            is_writable= a.get("isWritable", False),
        )
        for a in jup_ix.get("accounts", [])
    ]
    data = base64.b64decode(jup_ix.get("data", ""))
    return Instruction(prog, data, accounts)


def _compute_budget_ix(compute_units: int) -> Instruction:
    """SetComputeUnitLimit instruction."""
    # Discriminator 0x02 = SetComputeUnitLimit, value as u32 LE
    data = bytes([2]) + struct.pack("<I", compute_units)
    return Instruction(COMPUTE_BUDGET_PROGRAM, data, [])


def _priority_fee_ix(micro_lamports: int) -> Instruction:
    """SetComputeUnitPrice instruction."""
    # Discriminator 0x03 = SetComputeUnitPrice, value as u64 LE
    data = bytes([3]) + struct.pack("<Q", micro_lamports)
    return Instruction(COMPUTE_BUDGET_PROGRAM, data, [])


# ── Address Lookup Table fetcher ────────────────────────────────────────────

async def fetch_alt(
    pubkey_str: str,
    session: aiohttp.ClientSession,
    rpc_url: str,
) -> Optional[AddressLookupTableAccount]:
    """
    Fetch an AddressLookupTable account from the network.

    Jupiter uses ALTs to compress transaction size. We must include them
    when building a VersionedTransaction with Jupiter instructions.
    """
    payload = {
        "jsonrpc": "2.0",
        "id": str(uuid.uuid4()),
        "method": "getAccountInfo",
        "params": [pubkey_str, {"encoding": "base64"}],
    }
    try:
        async with session.post(
            rpc_url,
            json=payload,
            timeout=aiohttp.ClientTimeout(total=8),
        ) as resp:
            data = await resp.json()
            result = (data.get("result") or {}).get("value")
            if not result:
                return None

            raw_data = base64.b64decode(result["data"][0])
            # ALT account layout (solana-program): 4 bytes type_index, 8 bytes deactivation_slot
            # 8 bytes last_extended_slot, 1 byte last_extended_slot_start_index
            # Then: 1 byte padding, then N × 32 bytes of addresses
            HEADER_SIZE = 4 + 8 + 8 + 1 + 1  # = 22 bytes
            if len(raw_data) < HEADER_SIZE:
                return None

            addresses_data = raw_data[HEADER_SIZE:]
            if len(addresses_data) % 32 != 0:
                # Try to align
                n = (len(addresses_data) // 32) * 32
                addresses_data = addresses_data[:n]

            addresses = [
                Pubkey.from_bytes(addresses_data[i:i+32])
                for i in range(0, len(addresses_data), 32)
            ]
            if not addresses:
                return None

            return AddressLookupTableAccount(
                key=_pubkey(pubkey_str),
                addresses=addresses,
            )
    except Exception as exc:
        logger.debug("atomic_arb.alt_fetch_failed", pubkey=pubkey_str[:12], error=str(exc))
        return None


async def fetch_recent_blockhash(
    session: aiohttp.ClientSession,
    rpc_url: str,
) -> Optional[str]:
    """Fetch a recent blockhash from the RPC."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getLatestBlockhash",
        "params": [{"commitment": "confirmed"}],
    }
    try:
        async with session.post(
            rpc_url,
            json=payload,
            timeout=aiohttp.ClientTimeout(total=8),
        ) as resp:
            data = await resp.json()
            value = (data.get("result") or {}).get("value", {})
            return value.get("blockhash")
    except Exception as exc:
        logger.warning("atomic_arb.blockhash_fetch_failed", error=str(exc))
        return None


# ── Core Builder ────────────────────────────────────────────────────────────

class AtomicArbBuilder:
    """
    Composes atomic flash loan arb transactions.

    One instance per bot session — call init() once at startup.
    Then call execute_flash_arb() for each arb opportunity.
    """

    def __init__(
        self,
        rpc_url: str,
        wallet,                          # src.core.wallet.Wallet instance
        session: Optional[aiohttp.ClientSession] = None,
        loan_usdc_lamports: int = LOAN_USDC_LAMPORTS,
    ) -> None:
        self._rpc_url = rpc_url
        self._wallet  = wallet
        self._session = session
        self._own_session = session is None
        self._loan_usdc   = loan_usdc_lamports

        self._mf: Optional[MarginFiFlash] = None
        self._marginfi_account: Optional[Pubkey] = None
        self._usdc_ata: Optional[Pubkey] = None
        self._needs_account_init = False

        self._initialized = False
        self._arbs_built  = 0
        self._arbs_sent   = 0
        self._arbs_ok     = 0

    # ── Lifecycle ──────────────────────────────────────────────────────────

    async def init(self) -> bool:
        """
        Initialize: fetch MarginFi bank info and check wallet accounts.

        Must be called once before execute_flash_arb().
        """
        if self._initialized:
            return True

        if self._own_session or self._session is None:
            self._session = aiohttp.ClientSession()

        wallet_pk = _pubkey(self._wallet.public_key) if self._wallet.public_key else None
        if wallet_pk is None:
            logger.error("atomic_arb.wallet_not_loaded")
            return False

        # Init MarginFi flash helper
        self._mf = MarginFiFlash(self._rpc_url, self._session)
        ok = await self._mf.init()
        if not ok:
            logger.error("atomic_arb.marginfi_init_failed")
            return False

        # Derive wallet accounts
        self._marginfi_account, _ = derive_marginfi_account(wallet_pk)
        self._usdc_ata = derive_usdc_ata(wallet_pk)

        # Check if marginfi_account exists
        _, self._needs_account_init = await self._mf.check_or_create_account(
            wallet_pk, self._rpc_url
        )
        if self._needs_account_init:
            logger.warning(
                "atomic_arb.needs_marginfi_account",
                note="MarginFi account will be created in the first arb tx",
            )

        self._initialized = True
        logger.info(
            "atomic_arb.initialized",
            marginfi_account=str(self._marginfi_account)[:12] + "...",
            usdc_ata=str(self._usdc_ata)[:12] + "...",
            loan_usdc=self._loan_usdc / 1_000_000,
            needs_account_init=self._needs_account_init,
        )
        return True

    async def close(self) -> None:
        if self._mf:
            await self._mf.close()
        if self._own_session and self._session and not self._session.closed:
            await self._session.close()

    # ── Main execution ─────────────────────────────────────────────────────

    async def execute_flash_arb(
        self,
        buy_quote: dict,   # Raw Jupiter /quote response for buy leg
        sell_quote: dict,  # Raw Jupiter /quote response for sell leg
        loan_usdc_lamports: Optional[int] = None,
    ) -> FlashArbTxResult:
        """
        Build and broadcast one atomic flash loan arb transaction.

        Args:
            buy_quote:  Jupiter quote for USDC → token (buy leg).
            sell_quote: Jupiter quote for token → USDC (sell leg).
            loan_usdc_lamports: How much USDC to flash-borrow (default: self._loan_usdc).

        Returns:
            FlashArbTxResult with success/failure and profit info.
        """
        if not self._initialized:
            return FlashArbTxResult(success=False, error="not_initialized")
        if not self._wallet or not self._wallet.public_key:
            return FlashArbTxResult(success=False, error="wallet_not_loaded")
        if not self._mf or not self._mf.usdc_bank:
            return FlashArbTxResult(success=False, error="marginfi_not_initialized")

        loan = loan_usdc_lamports or self._loan_usdc
        wallet_pk = _pubkey(self._wallet.public_key)
        bank = self._mf.usdc_bank
        repay_amount = self._mf.calc_repay_amount(loan)

        # 1. Fetch Jupiter swap instructions for both legs
        buy_ixs_resp  = await self._get_swap_instructions(buy_quote,  wallet_pk)
        sell_ixs_resp = await self._get_swap_instructions(sell_quote, wallet_pk)

        if buy_ixs_resp is None:
            return FlashArbTxResult(success=False, error="buy_swap_instructions_failed")
        if sell_ixs_resp is None:
            return FlashArbTxResult(success=False, error="sell_swap_instructions_failed")

        # 2. Convert Jupiter instructions to solders Instructions
        buy_ixs  = self._extract_instructions(buy_ixs_resp)
        sell_ixs = self._extract_instructions(sell_ixs_resp)

        # 3. Collect all Address Lookup Table addresses
        all_alt_addrs: set[str] = set()
        for resp in (buy_ixs_resp, sell_ixs_resp):
            for addr in resp.get("addressLookupTableAddresses", []):
                all_alt_addrs.add(addr)

        # 4. Fetch ALT accounts concurrently
        alt_tasks = [
            fetch_alt(addr, self._session, self._rpc_url)
            for addr in all_alt_addrs
        ]
        alt_results = await asyncio.gather(*alt_tasks, return_exceptions=True)
        alts = [r for r in alt_results if isinstance(r, AddressLookupTableAccount)]

        # 5. Fetch fresh blockhash
        blockhash_str = await fetch_recent_blockhash(self._session, self._rpc_url)
        if not blockhash_str:
            return FlashArbTxResult(success=False, error="blockhash_fetch_failed")

        # 6. Assemble instruction list
        all_ixs: list[Instruction] = []

        # Compute budget (always first)
        all_ixs.append(_priority_fee_ix(DEFAULT_PRIORITY_FEE_MICROLAMPORTS))
        all_ixs.append(_compute_budget_ix(DEFAULT_COMPUTE_UNITS))

        # Optionally initialize MarginFi account
        if self._needs_account_init:
            all_ixs.append(build_init_marginfi_account_ix(
                marginfi_account=self._marginfi_account,
                marginfi_group=MARGINFI_GROUP,
                signer=wallet_pk,
                fee_payer=wallet_pk,
            ))

        # Flash loan start — end index is known after we count all remaining ixs
        start_ix_index = len(all_ixs)
        all_ixs.append(None)  # placeholder, we'll fill end_index after

        # Jupiter buy swap instructions
        all_ixs.extend(buy_ixs)

        # Jupiter sell swap instructions
        all_ixs.extend(sell_ixs)

        # Flash loan end — this is the last instruction
        end_ix_index = len(all_ixs)
        all_ixs.append(build_end_flashloan_ix(
            marginfi_account=self._marginfi_account,
            signer=wallet_pk,
            bank_info=bank,
            signer_usdc_ata=self._usdc_ata,
        ))

        # Fill in the start flashloan instruction now that we know end_ix_index
        all_ixs[start_ix_index] = build_start_flashloan_ix(
            marginfi_account=self._marginfi_account,
            signer=wallet_pk,
            end_ix_index=end_ix_index,
        )

        # Filter out any None slots (shouldn't happen after fill-in)
        all_ixs = [ix for ix in all_ixs if ix is not None]

        # 7. Build VersionedTransaction
        try:
            blockhash = Hash.from_string(blockhash_str)
            msg = MessageV0.try_compile(
                payer=wallet_pk,
                instructions=all_ixs,
                address_lookup_table_accounts=alts,
                recent_blockhash=blockhash,
            )
        except Exception as exc:
            logger.error("atomic_arb.tx_compile_failed", error=str(exc))
            return FlashArbTxResult(success=False, error=f"tx_compile_failed: {exc}")

        # 8. Sign
        try:
            signed_tx = self._sign_message(msg)
        except Exception as exc:
            logger.error("atomic_arb.signing_failed", error=str(exc))
            return FlashArbTxResult(success=False, error=f"signing_failed: {exc}")

        tx_bytes = bytes(signed_tx)
        self._arbs_built += 1

        logger.info(
            "atomic_arb.tx_built",
            ix_count=len(all_ixs),
            alts=len(alts),
            tx_size=len(tx_bytes),
            loan_usdc=loan / 1_000_000,
        )

        # 9. Broadcast
        result = await self._broadcast(signed_tx, tx_bytes)

        if result.success:
            self._arbs_ok += 1
            # If marginfi account was just created, update flag
            if self._needs_account_init:
                self._needs_account_init = False
            # Estimate profit
            usdc_out = int(sell_quote.get("outAmount", 0))
            profit_lamports = max(0, usdc_out - repay_amount)
            result.profit_usdc_lamports = profit_lamports
            result.fee_usdc_lamports    = repay_amount - loan
            result.tx_size_bytes        = len(tx_bytes)
            logger.info(
                "atomic_arb.success",
                signature=result.signature[:20] + "...",
                profit_usd=round(profit_lamports / 1_000_000, 4),
            )

        self._arbs_sent += 1
        return result

    async def execute_single_swap_flash_arb(
        self,
        swap_quote: dict,
        loan_usdc_lamports: Optional[int] = None,
    ) -> "FlashArbTxResult":
        """
        Atomic flash loan for a single multi-hop Jupiter quote (triangular arb).

        Used when Jupiter composes all 3 hops (USDC→A→B→USDC) in a single
        swap instruction set. Same MarginFi flash borrow/repay envelope as
        execute_flash_arb, but with only one set of swap instructions.

        Args:
            swap_quote: Full Jupiter /quote response for the round-trip route.
            loan_usdc_lamports: How much USDC to flash-borrow.

        Returns:
            FlashArbTxResult with success/failure and profit info.
        """
        if not self._initialized:
            return FlashArbTxResult(success=False, error="not_initialized")
        if not self._wallet or not self._wallet.public_key:
            return FlashArbTxResult(success=False, error="wallet_not_loaded")
        if not self._mf or not self._mf.usdc_bank:
            return FlashArbTxResult(success=False, error="marginfi_not_initialized")

        loan = loan_usdc_lamports or self._loan_usdc
        wallet_pk = _pubkey(self._wallet.public_key)
        bank = self._mf.usdc_bank
        repay_amount = self._mf.calc_repay_amount(loan)

        # Get swap instructions for the full round-trip quote
        swap_ixs_resp = await self._get_swap_instructions(swap_quote, wallet_pk)
        if swap_ixs_resp is None:
            return FlashArbTxResult(success=False, error="swap_instructions_failed")

        swap_ixs = self._extract_instructions(swap_ixs_resp)

        # Fetch ALTs
        all_alt_addrs: set[str] = set(swap_ixs_resp.get("addressLookupTableAddresses", []))
        alt_tasks = [fetch_alt(addr, self._session, self._rpc_url) for addr in all_alt_addrs]
        alt_results = await asyncio.gather(*alt_tasks, return_exceptions=True)
        alts = [r for r in alt_results if isinstance(r, AddressLookupTableAccount)]

        # Fresh blockhash
        blockhash_str = await fetch_recent_blockhash(self._session, self._rpc_url)
        if not blockhash_str:
            return FlashArbTxResult(success=False, error="blockhash_fetch_failed")

        # Assemble instructions
        all_ixs: list[Instruction] = []
        all_ixs.append(_priority_fee_ix(DEFAULT_PRIORITY_FEE_MICROLAMPORTS))
        all_ixs.append(_compute_budget_ix(DEFAULT_COMPUTE_UNITS))

        if self._needs_account_init:
            all_ixs.append(build_init_marginfi_account_ix(
                marginfi_account=self._marginfi_account,
                marginfi_group=MARGINFI_GROUP,
                signer=wallet_pk,
                fee_payer=wallet_pk,
            ))

        start_ix_index = len(all_ixs)
        all_ixs.append(None)  # placeholder for start flashloan

        all_ixs.extend(swap_ixs)

        end_ix_index = len(all_ixs)
        all_ixs.append(build_end_flashloan_ix(
            marginfi_account=self._marginfi_account,
            signer=wallet_pk,
            bank_info=bank,
            signer_usdc_ata=self._usdc_ata,
        ))

        all_ixs[start_ix_index] = build_start_flashloan_ix(
            marginfi_account=self._marginfi_account,
            signer=wallet_pk,
            end_ix_index=end_ix_index,
        )
        all_ixs = [ix for ix in all_ixs if ix is not None]

        try:
            blockhash = Hash.from_string(blockhash_str)
            msg = MessageV0.try_compile(
                payer=wallet_pk,
                instructions=all_ixs,
                address_lookup_table_accounts=alts,
                recent_blockhash=blockhash,
            )
        except Exception as exc:
            logger.error("atomic_arb.single_swap_compile_failed", error=str(exc))
            return FlashArbTxResult(success=False, error=f"tx_compile_failed: {exc}")

        try:
            signed_tx = self._sign_message(msg)
        except Exception as exc:
            logger.error("atomic_arb.single_swap_signing_failed", error=str(exc))
            return FlashArbTxResult(success=False, error=f"signing_failed: {exc}")

        tx_bytes = bytes(signed_tx)
        self._arbs_built += 1

        logger.info(
            "atomic_arb.single_swap_tx_built",
            ix_count=len(all_ixs),
            alts=len(alts),
            tx_size=len(tx_bytes),
            loan_usdc=loan / 1_000_000,
        )

        result = await self._broadcast(signed_tx, tx_bytes)

        if result.success:
            self._arbs_ok += 1
            if self._needs_account_init:
                self._needs_account_init = False
            usdc_out = int(swap_quote.get("outAmount", 0))
            profit_lamports = max(0, usdc_out - repay_amount)
            result.profit_usdc_lamports = profit_lamports
            result.fee_usdc_lamports    = repay_amount - loan
            result.tx_size_bytes        = len(tx_bytes)
            logger.info(
                "atomic_arb.single_swap_success",
                signature=result.signature[:20] + "...",
                profit_usd=round(profit_lamports / 1_000_000, 4),
            )

        self._arbs_sent += 1
        return result

    # ── Helpers ────────────────────────────────────────────────────────────

    async def _get_swap_instructions(
        self,
        quote_response: dict,
        wallet_pk: Pubkey,
    ) -> Optional[dict]:
        """
        Call Jupiter /swap-instructions to get raw composable instructions.

        This endpoint returns individual instructions (not a full transaction)
        so we can embed them inside our own VersionedTransaction.
        """
        if not self._session:
            return None

        payload = {
            "quoteResponse":             quote_response,
            "userPublicKey":             str(wallet_pk),
            "wrapAndUnwrapSol":          True,
            "useSharedAccounts":         True,
            "asLegacyTransaction":       False,
            "computeUnitPriceMicroLamports": DEFAULT_PRIORITY_FEE_MICROLAMPORTS,
        }
        try:
            async with self._session.post(
                JUPITER_SWAP_INSTRUCTIONS_URL,
                json=payload,
                timeout=aiohttp.ClientTimeout(total=12),
            ) as resp:
                if resp.status != 200:
                    body = await resp.text()
                    logger.warning(
                        "atomic_arb.swap_instructions_error",
                        status=resp.status,
                        body=body[:200],
                    )
                    return None
                return await resp.json()
        except Exception as exc:
            logger.warning("atomic_arb.swap_instructions_exception", error=str(exc))
            return None

    def _extract_instructions(self, jup_resp: dict) -> list[Instruction]:
        """
        Extract and convert all instructions from a Jupiter /swap-instructions response.

        Jupiter returns: setupInstructions, swapInstruction, cleanupInstruction.
        We skip computeBudget instructions since we add our own.
        """
        all_ixs: list[Instruction] = []
        skip_programs = {
            str(COMPUTE_BUDGET_PROGRAM),   # we set our own
        }

        # Setup instructions (create token accounts etc.)
        for ix_dict in jup_resp.get("setupInstructions", []):
            if ix_dict.get("programId") not in skip_programs:
                try:
                    all_ixs.append(_ix_from_jupiter(ix_dict))
                except Exception as exc:
                    logger.debug("atomic_arb.setup_ix_skip", error=str(exc))

        # The main swap instruction
        swap_ix = jup_resp.get("swapInstruction")
        if swap_ix:
            try:
                all_ixs.append(_ix_from_jupiter(swap_ix))
            except Exception as exc:
                logger.warning("atomic_arb.swap_ix_failed", error=str(exc))

        # Cleanup instructions
        cleanup_ix = jup_resp.get("cleanupInstruction")
        if cleanup_ix:
            try:
                all_ixs.append(_ix_from_jupiter(cleanup_ix))
            except Exception as exc:
                logger.debug("atomic_arb.cleanup_ix_skip", error=str(exc))

        return all_ixs

    def _sign_message(self, msg: MessageV0) -> VersionedTransaction:
        """Sign a MessageV0 with the wallet keypair using solders."""
        if not self._wallet._keypair:
            raise RuntimeError("Wallet not loaded")
        return VersionedTransaction(msg, [self._wallet._keypair])

    async def _broadcast(
        self,
        signed_tx: VersionedTransaction,
        tx_bytes: bytes,
    ) -> FlashArbTxResult:
        """Submit signed transaction to the RPC."""
        if not self._session:
            return FlashArbTxResult(success=False, error="no_session")

        tx_b64 = base64.b64encode(tx_bytes).decode()
        payload = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "sendTransaction",
            "params": [
                tx_b64,
                {
                    "encoding": "base64",
                    "preflightCommitment": "confirmed",
                    "skipPreflight": False,
                    "maxRetries": 3,
                },
            ],
        }
        try:
            async with self._session.post(
                self._rpc_url,
                json=payload,
                timeout=aiohttp.ClientTimeout(total=30),
            ) as resp:
                data = await resp.json()

                if "result" in data:
                    sig = data["result"]
                    return FlashArbTxResult(
                        success=True,
                        signature=sig,
                        gas_sol=0.002,
                    )

                rpc_err = data.get("error", {})
                err_msg = rpc_err.get("message", str(rpc_err)) if isinstance(rpc_err, dict) else str(rpc_err)

                # Simulation failure = arb wasn't profitable, tx rejected safely
                if any(k in err_msg.lower() for k in ("simulation", "preflight", "insufficient")):
                    logger.info(
                        "atomic_arb.simulation_rejected",
                        reason=err_msg[:120],
                        note="Arb was not profitable at execution time — no funds at risk",
                    )
                    return FlashArbTxResult(success=False, error=f"not_profitable: {err_msg[:80]}")

                logger.warning("atomic_arb.rpc_error", error=err_msg[:120])
                return FlashArbTxResult(success=False, error=f"rpc_error: {err_msg[:80]}")

        except asyncio.TimeoutError:
            return FlashArbTxResult(success=False, error="broadcast_timeout")
        except Exception as exc:
            return FlashArbTxResult(success=False, error=f"broadcast_exception: {exc}")

    # ── Stats ──────────────────────────────────────────────────────────────

    def get_stats(self) -> dict:
        return {
            "initialized":        self._initialized,
            "arbs_built":         self._arbs_built,
            "arbs_sent":          self._arbs_sent,
            "arbs_ok":            self._arbs_ok,
            "loan_usdc":          self._loan_usdc / 1_000_000,
            "needs_account_init": self._needs_account_init,
            "marginfi_account":   str(self._marginfi_account)[:16] + "..." if self._marginfi_account else None,
        }
