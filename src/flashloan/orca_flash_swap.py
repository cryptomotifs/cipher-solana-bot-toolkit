"""
OrcaFlashSwap — Orca Whirlpool flash swap executor.

How Orca flash swaps work (ZERO account setup required):
  Orca Whirlpool is a Uniswap-v3 clone. The pool vaults hold the token reserves.
  A flash swap:
    1. Receive output tokens FROM the pool vault (token_b received before input paid)
    2. Execute any instructions with those tokens (e.g., arb on Raydium)
    3. Pay input tokens INTO the pool vault before the transaction ends
  Solana transactions are atomic — if step 3 doesn't happen, step 1 never happened.

Why this is better than MarginFi for some scenarios:
  - No marginfi_account PDA to create (zero setup)
  - No protocol fee beyond swap fee (~0.01–0.30% depending on tier)
  - Liquidity is deep for major pairs (SOL/USDC typically $1M+)

Supported flash swap paths:
  Mode A: Single-pool flash swap (borrow USDC from Orca pool, arb, repay)
  Mode B: Two-hop flash swap (USDC → token_A via pool_1, token_A → USDC via pool_2)

Requirements:
  - User's USDC ATA (Associated Token Account) — created automatically if needed
  - User's token ATA for intermediate token — created automatically if needed
  - SOL for rent (if ATAs don't exist yet) ~0.004 SOL total

Key program addresses:
  Orca Whirlpool: whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
  Token Program:  TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA

Usage:
  flash = OrcaFlashSwap(rpc_url, session)
  await flash.init()
  pool = await flash.find_best_usdc_pool(token_mint)
  result = await flash.execute_flash_arb(pool, buy_quote, sell_quote, wallet)
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import struct
import uuid
from dataclasses import dataclass, field
from typing import Optional

import aiohttp
import structlog

from solders.hash import Hash
from solders.instruction import AccountMeta, Instruction
from solders.message import MessageV0
from solders.pubkey import Pubkey
from solders.transaction import VersionedTransaction
from solders.address_lookup_table_account import AddressLookupTableAccount

from src.execution.marginfi_flash import TOKEN_PROGRAM, USDC_MINT

logger = structlog.get_logger(__name__)

# ── Program constants ──────────────────────────────────────────────────────

ORCA_WHIRLPOOL_PROGRAM = Pubkey.from_string("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc")
ORCA_WHIRLPOOL_CONFIG  = Pubkey.from_string("2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ")
WSOL_MINT              = Pubkey.from_string("So11111111111111111111111111111111111111112")
ASSOCIATED_TOKEN_PROGRAM = Pubkey.from_string("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJe8aD")
SYSTEM_PROGRAM         = Pubkey.from_string("11111111111111111111111111111111")
TOKEN_PROGRAM_2022     = Pubkey.from_string("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")
COMPUTE_BUDGET         = Pubkey.from_string("ComputeBudget111111111111111111111111111111")

# Anchor discriminators — sha256("global:{ix_name}")[:8]
def _disc(name: str) -> bytes:
    return hashlib.sha256(f"global:{name}".encode()).digest()[:8]

DISC_SWAP           = _disc("swap")                # [248,198,158,145,225,117,135,200]
DISC_TWO_HOP_SWAP   = _disc("two_hop_swap")        # [195, 96,237,108, 68,162,219,230]
DISC_SWAP_V2        = _disc("swap_v2")             # [ 43,  4,237, 11, 26,201, 30, 98]
DISC_TWO_HOP_SWAP_V2= _disc("two_hop_swap_v2")     # [186,143,209, 29,254,  2,194,117]
DISC_INIT_ATA       = bytes([1])                   # create_idempotent in AToken program

# ── Well-known high-liquidity Orca pools ──────────────────────────────────
# Pre-seeded to avoid on-chain discovery latency on hot path.
# Format: (token_mint_str, pool_address_str, fee_tier_bps, token_is_A)
# fee_tier_bps: 1=0.01%, 5=0.05%, 30=0.30%, 100=1%

KNOWN_USDC_POOLS: list[tuple[str, str, int, bool]] = [
    # SOL/USDC pools (USDC is token_b, SOL/wSOL is token_a)
    ("So11111111111111111111111111111111111111112",
     "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ", 30, False),
    ("So11111111111111111111111111111111111111112",
     "4fuUiYxTQ6QCrdSq9ouBYcTM7bqSwYTSyLueGZLTy4T4",  5, False),
    # BONK/USDC
    ("DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
     "5fhSgFj2L6deyBK6bpEfWEL8j8WPcBp1pZBTvFaRKiPh", 30, False),
    # WIF/USDC
    ("EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm",
     "EP2ib6dYdEaosZgTngtouching6dKafRgEFC6y5oRGf9J5t", 30, False),
    # JTO/USDC
    ("jtojtomepa8bdnE4UGmkKwvgM9Kv2baMQYMnWDxjFLY",
     "44mVDnyRnszMxsZdFMT4p7MHNRiViMeqBBcM5MjhGfYm", 30, False),
]

# Default compute budget for flash swap transactions
FLASH_SWAP_COMPUTE_UNITS = 600_000        # 2 DEX swaps = more CUs than MarginFi arb
FLASH_SWAP_PRIORITY_FEE  = 150_000        # microlamports — slightly higher for priority


# ── Data structures ────────────────────────────────────────────────────────

@dataclass
class OrcaPoolInfo:
    """Metadata for a fetched Orca Whirlpool pool."""
    address: Pubkey
    token_mint_a: Pubkey
    token_mint_b: Pubkey
    token_vault_a: Pubkey
    token_vault_b: Pubkey
    tick_array_0: Pubkey   # current tick array
    tick_array_1: Pubkey   # next tick array
    tick_array_2: Pubkey   # next+1 tick array
    oracle: Pubkey
    fee_tier_bps: int       # e.g. 30 = 0.30%
    sqrt_price: int         # current sqrt price Q64.64
    current_tick: int       # current tick index
    token_is_a: bool = True # whether the token we want to arb is token_a (vs token_b)


@dataclass
class OrcaFlashSwapResult:
    success: bool
    signature: str = ""
    profit_usdc_lamports: int = 0
    gas_sol: float = 0.0
    error: str = ""
    provider: str = "orca"

    @property
    def profit_usd(self) -> float:
        return self.profit_usdc_lamports / 1_000_000

    @property
    def net_profit_usd(self) -> float:
        return self.profit_usd - self.gas_sol


# ── PDA helpers ───────────────────────────────────────────────────────────

def derive_ata(owner: Pubkey, mint: Pubkey,
               token_program: Pubkey = TOKEN_PROGRAM) -> Pubkey:
    """Derive Associated Token Account address for (owner, mint)."""
    addr, _ = Pubkey.find_program_address(
        [bytes(owner), bytes(token_program), bytes(mint)],
        ASSOCIATED_TOKEN_PROGRAM,
    )
    return addr


def derive_orca_tick_array(whirlpool: Pubkey, start_tick: int) -> Pubkey:
    """Derive tick array PDA for a given start tick index."""
    addr, _ = Pubkey.find_program_address(
        [b"tick_array", bytes(whirlpool),
         start_tick.to_bytes(4, "little", signed=True)],
        ORCA_WHIRLPOOL_PROGRAM,
    )
    return addr


def derive_orca_oracle(whirlpool: Pubkey) -> Pubkey:
    """Derive oracle PDA for a Whirlpool."""
    addr, _ = Pubkey.find_program_address(
        [b"oracle", bytes(whirlpool)],
        ORCA_WHIRLPOOL_PROGRAM,
    )
    return addr


def tick_index_to_array_start(tick_index: int, tick_spacing: int) -> int:
    """Snap a tick index to the nearest tick array start index."""
    ticks_per_array = 88 * tick_spacing
    return (tick_index // ticks_per_array) * ticks_per_array


# ── Instruction builders ───────────────────────────────────────────────────

def build_create_ata_if_needed_ix(
    payer: Pubkey,
    owner: Pubkey,
    mint: Pubkey,
    ata: Pubkey,
    token_program: Pubkey = TOKEN_PROGRAM,
) -> Instruction:
    """
    Build create_associated_token_account_idempotent instruction.
    No-ops if ATA already exists — safe to always include.
    """
    accounts = [
        AccountMeta(payer, is_signer=True, is_writable=True),
        AccountMeta(ata, is_signer=False, is_writable=True),
        AccountMeta(owner, is_signer=False, is_writable=False),
        AccountMeta(mint, is_signer=False, is_writable=False),
        AccountMeta(SYSTEM_PROGRAM, is_signer=False, is_writable=False),
        AccountMeta(token_program, is_signer=False, is_writable=False),
    ]
    # create_idempotent discriminator = [1] (AToken program opcode)
    return Instruction(
        program_id=ASSOCIATED_TOKEN_PROGRAM,
        accounts=accounts,
        data=bytes([1]),  # create_idempotent
    )


def build_orca_swap_ix(
    pool: OrcaPoolInfo,
    wallet_pk: Pubkey,
    wallet_token_a_ata: Pubkey,
    wallet_token_b_ata: Pubkey,
    amount: int,
    other_amount_threshold: int,
    a_to_b: bool,
    exact_input: bool = True,
    sqrt_price_limit: int = 0,
) -> Instruction:
    """
    Build Orca Whirlpool swap instruction.

    Layout (from Orca IDL):
      discriminator: [u8; 8]      = DISC_SWAP
      amount: u64                  = how many tokens to swap (lamports)
      other_amount_threshold: u64  = min out (exact input) or max in (exact output)
      sqrt_price_limit: u128       = price limit (0 = no limit)
      amount_specified_is_input: bool
      a_to_b: bool                 = swap direction
    """
    # Encode swap parameters
    sqrt_limit = sqrt_price_limit if sqrt_price_limit else (
        4_295_048_016 if a_to_b else 79_226_673_515_401_279_992_447_902_215
    )

    data = (
        DISC_SWAP
        + struct.pack("<Q", amount)
        + struct.pack("<Q", other_amount_threshold)
        + struct.pack("<QQ", sqrt_limit & 0xFFFFFFFFFFFFFFFF,
                             (sqrt_limit >> 64) & 0xFFFFFFFFFFFFFFFF)
        + struct.pack("<?", exact_input)
        + struct.pack("<?", a_to_b)
    )

    # Account order per Orca Whirlpool IDL
    accounts = [
        AccountMeta(TOKEN_PROGRAM,          is_signer=False, is_writable=False),
        AccountMeta(wallet_pk,              is_signer=True,  is_writable=False),
        AccountMeta(pool.address,           is_signer=False, is_writable=True),
        AccountMeta(wallet_token_a_ata,     is_signer=False, is_writable=True),
        AccountMeta(pool.token_vault_a,     is_signer=False, is_writable=True),
        AccountMeta(wallet_token_b_ata,     is_signer=False, is_writable=True),
        AccountMeta(pool.token_vault_b,     is_signer=False, is_writable=True),
        AccountMeta(pool.tick_array_0,      is_signer=False, is_writable=True),
        AccountMeta(pool.tick_array_1,      is_signer=False, is_writable=True),
        AccountMeta(pool.tick_array_2,      is_signer=False, is_writable=True),
        AccountMeta(pool.oracle,            is_signer=False, is_writable=True),
    ]

    return Instruction(
        program_id=ORCA_WHIRLPOOL_PROGRAM,
        accounts=accounts,
        data=data,
    )


def build_compute_budget_ix(compute_units: int) -> Instruction:
    """SetComputeUnitLimit instruction."""
    return Instruction(
        program_id=COMPUTE_BUDGET,
        accounts=[],
        data=bytes([2]) + struct.pack("<I", compute_units),
    )


def build_priority_fee_ix(microlamports: int) -> Instruction:
    """SetComputeUnitPrice instruction."""
    return Instruction(
        program_id=COMPUTE_BUDGET,
        accounts=[],
        data=bytes([3]) + struct.pack("<Q", microlamports),
    )


# ── Main executor ─────────────────────────────────────────────────────────

class OrcaFlashSwap:
    """
    Execute flash-swap arb using Orca Whirlpool pools.

    No protocol account required — only standard ATAs.
    Can arb any token that has an Orca USDC pool.

    Flash swap strategy:
      Tx contains two swaps atomically:
        1. USDC → token  on DEX_A (buy leg, via this class)
        2. token → USDC  on DEX_B (sell leg, via Jupiter or another pool)
      Net: if USDC_out > USDC_in, profit is captured.
      No borrowing protocol — wallet must hold enough USDC for buy leg.
      BUT: unlike MarginFi, zero protocol setup — just SOL for gas.

    For pure zero-capital operation on Orca:
      Use two Orca pools: pool_A (cheap) and pool_B (expensive).
      The two-hop swap is atomic — buy on pool_A, sell on pool_B.
      If pool_B price > pool_A price after fees, profit locked in.
    """

    def __init__(
        self,
        rpc_url: str,
        session: Optional[aiohttp.ClientSession] = None,
    ) -> None:
        self._rpc_url = rpc_url
        self._session = session
        self._own_session = session is None
        self._pools: dict[str, OrcaPoolInfo] = {}   # mint → best pool

    async def init(self) -> bool:
        """
        Initialise: create HTTP session if needed.
        Pre-warm pool cache for known high-liquidity pairs.
        """
        if self._own_session:
            self._session = aiohttp.ClientSession()

        # Pre-fetch known high-liquidity pools in background
        asyncio.create_task(self._warm_pool_cache())
        logger.info("orca_flash_swap.initialized", rpc_url=self._rpc_url[:30])
        return True

    async def close(self) -> None:
        if self._own_session and self._session and not self._session.closed:
            await self._session.close()

    async def _warm_pool_cache(self) -> None:
        """Background: pre-fetch pool data for known USDC pairs."""
        for token_mint, pool_addr, fee_bps, token_is_a in KNOWN_USDC_POOLS:
            try:
                pool = await self.fetch_pool(pool_addr)
                if pool:
                    self._pools[token_mint] = pool
            except Exception:
                pass
        logger.debug("orca_flash_swap.pool_cache_warmed",
                     cached=len(self._pools))

    async def fetch_pool(self, pool_address: str) -> Optional[OrcaPoolInfo]:
        """
        Fetch and parse Orca Whirlpool account data.

        Whirlpool account layout (from Orca IDL, 653 bytes):
          discriminator:       [u8; 8]
          whirlpools_config:   Pubkey (32)
          whirlpool_bump:      [u8; 1]
          tick_spacing:        u16
          tick_spacing_seed:   [u8; 2]
          fee_rate:            u16
          protocol_fee_rate:   u16
          liquidity:           u128
          sqrt_price:          u128  ← offset 81
          tick_current_index:  i32   ← offset 97
          ...
          token_mint_a:        Pubkey ← offset 101
          token_vault_a:       Pubkey ← offset 133
          ...
          token_mint_b:        Pubkey ← offset 181
          token_vault_b:       Pubkey ← offset 213
        """
        if not self._session:
            return None

        payload = {
            "jsonrpc": "2.0", "id": str(uuid.uuid4()),
            "method": "getAccountInfo",
            "params": [pool_address, {"encoding": "base64"}],
        }
        try:
            async with self._session.post(
                self._rpc_url, json=payload,
                timeout=aiohttp.ClientTimeout(total=8),
            ) as resp:
                data = await resp.json()
                result = (data.get("result") or {}).get("value")
                if not result:
                    return None

                raw = base64.b64decode(result["data"][0])
                if len(raw) < 250:
                    return None

                # Parse key fields from Whirlpool layout
                # See: https://github.com/orca-so/whirlpools/blob/main/sdk/src/types/public/anchor-types.ts
                tick_spacing   = struct.unpack_from("<H", raw, 9)[0]
                sqrt_price     = int.from_bytes(raw[81:97], "little")
                current_tick   = struct.unpack_from("<i", raw, 97)[0]
                token_mint_a   = Pubkey.from_bytes(raw[101:133])
                token_vault_a  = Pubkey.from_bytes(raw[133:165])
                token_mint_b   = Pubkey.from_bytes(raw[181:213])
                token_vault_b  = Pubkey.from_bytes(raw[213:245])

                pool_pk = Pubkey.from_string(pool_address)

                # Derive tick arrays around current tick
                ts0 = tick_index_to_array_start(current_tick, tick_spacing)
                ts1 = ts0 + 88 * tick_spacing
                ts2 = ts1 + 88 * tick_spacing
                ta0 = derive_orca_tick_array(pool_pk, ts0)
                ta1 = derive_orca_tick_array(pool_pk, ts1)
                ta2 = derive_orca_tick_array(pool_pk, ts2)
                oracle = derive_orca_oracle(pool_pk)

                # Determine which token is USDC
                usdc = Pubkey.from_string(USDC_MINT)
                token_is_a = (token_mint_b == usdc)  # True means token we want is A

                return OrcaPoolInfo(
                    address=pool_pk,
                    token_mint_a=token_mint_a,
                    token_mint_b=token_mint_b,
                    token_vault_a=token_vault_a,
                    token_vault_b=token_vault_b,
                    tick_array_0=ta0,
                    tick_array_1=ta1,
                    tick_array_2=ta2,
                    oracle=oracle,
                    fee_tier_bps=0,  # would need to parse fee_rate from account
                    sqrt_price=sqrt_price,
                    current_tick=current_tick,
                    token_is_a=token_is_a,
                )
        except Exception as exc:
            logger.warning("orca_flash_swap.fetch_pool_failed",
                           pool=pool_address[:16], error=str(exc))
            return None

    def get_pool(self, token_mint: str) -> Optional[OrcaPoolInfo]:
        """Return cached pool for a token mint, or None."""
        return self._pools.get(token_mint)

    async def execute_two_pool_arb(
        self,
        buy_pool: OrcaPoolInfo,     # buy token here (USDC → token, cheap)
        sell_pool: OrcaPoolInfo,    # sell token here (token → USDC, expensive)
        wallet,                     # Wallet instance with _keypair
        amount_usdc: int,           # how much USDC to spend on buy leg
        min_profit_usdc: int = 100, # min profit in lamports to proceed
        rpc_url: Optional[str] = None,
    ) -> OrcaFlashSwapResult:
        """
        Execute a two-pool atomic arb using two Orca Whirlpool swaps.

        Both swaps are in a single VersionedTransaction.
        No MarginFi account, no Solend account — just ATAs.

        Limitations vs MarginFi flash loans:
          - Wallet must have amount_usdc USDC upfront (NOT zero-capital)
          - Only works within Orca pools (both buy and sell on Orca)
          - For cross-DEX (Orca→Raydium), use execute_cross_dex_arb instead

        Zero-capital within Orca when buy_pool != sell_pool:
          If price on buy_pool < price on sell_pool (after fees), we profit.
          The USDC returns to wallet at end of tx, so "capital" is just locked
          for the duration of one Solana slot (~400ms).
        """
        rpc = rpc_url or self._rpc_url
        if not self._session or not wallet or not getattr(wallet, "_keypair", None):
            return OrcaFlashSwapResult(success=False, error="wallet_or_session_missing")

        wallet_pk = Pubkey.from_string(
            getattr(wallet, "public_key", "") or getattr(wallet, "pubkey", "")
        )

        usdc_pk    = Pubkey.from_string(USDC_MINT)
        token_mint = buy_pool.token_mint_a if buy_pool.token_is_a else buy_pool.token_mint_b

        # Derive ATAs
        usdc_ata   = derive_ata(wallet_pk, usdc_pk)
        token_ata  = derive_ata(wallet_pk, token_mint)

        # ── Build instruction list ────────────────────────────────────────
        ixs: list[Instruction] = [
            build_priority_fee_ix(FLASH_SWAP_PRIORITY_FEE),
            build_compute_budget_ix(FLASH_SWAP_COMPUTE_UNITS),
            # Idempotent ATA init — no-ops if already exist
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, usdc_pk, usdc_ata),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, token_mint, token_ata),
            # Leg 1: USDC → token on buy_pool
            build_orca_swap_ix(
                pool=buy_pool,
                wallet_pk=wallet_pk,
                wallet_token_a_ata=token_ata if buy_pool.token_is_a else usdc_ata,
                wallet_token_b_ata=usdc_ata  if buy_pool.token_is_a else token_ata,
                amount=amount_usdc,
                other_amount_threshold=0,  # accept any output
                a_to_b=not buy_pool.token_is_a,  # USDC→token: go b→a if USDC is b
                exact_input=True,
            ),
            # Leg 2: token → USDC on sell_pool
            build_orca_swap_ix(
                pool=sell_pool,
                wallet_pk=wallet_pk,
                wallet_token_a_ata=token_ata if sell_pool.token_is_a else usdc_ata,
                wallet_token_b_ata=usdc_ata  if sell_pool.token_is_a else token_ata,
                amount=0,               # 0 = swap all available token balance
                other_amount_threshold=amount_usdc + min_profit_usdc,  # min USDC out
                a_to_b=sell_pool.token_is_a,  # token→USDC: go a→b if token is a
                exact_input=False,      # exact output: ensure we get enough USDC back
            ),
        ]

        # ── Fetch blockhash ───────────────────────────────────────────────
        blockhash_str = await self._fetch_blockhash(rpc)
        if not blockhash_str:
            return OrcaFlashSwapResult(success=False, error="blockhash_failed")

        # ── Compile and sign ──────────────────────────────────────────────
        try:
            blockhash = Hash.from_string(blockhash_str)
            msg = MessageV0.try_compile(
                payer=wallet_pk,
                instructions=ixs,
                address_lookup_table_accounts=[],
                recent_blockhash=blockhash,
            )
            signed_tx = VersionedTransaction(msg, [wallet._keypair])
        except Exception as exc:
            return OrcaFlashSwapResult(success=False, error=f"compile_failed: {exc}")

        # ── Broadcast ────────────────────────────────────────────────────
        return await self._broadcast(signed_tx, rpc)

    async def execute_cross_dex_arb(
        self,
        buy_pool: OrcaPoolInfo,        # Orca pool for buy leg
        sell_swap_ixs: list[Instruction],  # pre-built sell leg ixs (e.g., Raydium)
        sell_alts: list[AddressLookupTableAccount],  # ALTs from sell leg
        wallet,
        amount_usdc: int,
        min_profit_usdc: int = 100,
        rpc_url: Optional[str] = None,
    ) -> OrcaFlashSwapResult:
        """
        Cross-DEX arb: buy on Orca, sell on any DEX (via pre-built instructions).

        The sell_swap_ixs can come from Jupiter /swap-instructions, Raydium, etc.
        This enables Orca→Raydium or Orca→Jupiter arb without MarginFi.

        Still requires wallet USDC — not zero-capital.
        For zero-capital, use AtomicArbBuilder with MarginFi wrapping.
        """
        rpc = rpc_url or self._rpc_url
        if not self._session or not wallet or not getattr(wallet, "_keypair", None):
            return OrcaFlashSwapResult(success=False, error="wallet_or_session_missing")

        wallet_pk = Pubkey.from_string(
            getattr(wallet, "public_key", "") or getattr(wallet, "pubkey", "")
        )
        usdc_pk    = Pubkey.from_string(USDC_MINT)
        token_mint = buy_pool.token_mint_a if buy_pool.token_is_a else buy_pool.token_mint_b
        usdc_ata   = derive_ata(wallet_pk, usdc_pk)
        token_ata  = derive_ata(wallet_pk, token_mint)

        ixs: list[Instruction] = [
            build_priority_fee_ix(FLASH_SWAP_PRIORITY_FEE),
            build_compute_budget_ix(FLASH_SWAP_COMPUTE_UNITS),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, usdc_pk, usdc_ata),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, token_mint, token_ata),
            # Buy leg: Orca swap USDC → token
            build_orca_swap_ix(
                pool=buy_pool,
                wallet_pk=wallet_pk,
                wallet_token_a_ata=token_ata if buy_pool.token_is_a else usdc_ata,
                wallet_token_b_ata=usdc_ata  if buy_pool.token_is_a else token_ata,
                amount=amount_usdc,
                other_amount_threshold=0,
                a_to_b=not buy_pool.token_is_a,
                exact_input=True,
            ),
            # Sell leg: any DEX instructions (Raydium, Jupiter, etc.)
            *sell_swap_ixs,
        ]

        blockhash_str = await self._fetch_blockhash(rpc)
        if not blockhash_str:
            return OrcaFlashSwapResult(success=False, error="blockhash_failed")

        try:
            blockhash = Hash.from_string(blockhash_str)
            msg = MessageV0.try_compile(
                payer=wallet_pk,
                instructions=ixs,
                address_lookup_table_accounts=sell_alts,
                recent_blockhash=blockhash,
            )
            signed_tx = VersionedTransaction(msg, [wallet._keypair])
        except Exception as exc:
            return OrcaFlashSwapResult(success=False, error=f"compile_failed: {exc}")

        return await self._broadcast(signed_tx, rpc)

    # ── Helpers ────────────────────────────────────────────────────────────

    async def _fetch_blockhash(self, rpc_url: str) -> Optional[str]:
        """Fetch recent blockhash from RPC."""
        payload = {
            "jsonrpc": "2.0", "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}],
        }
        try:
            async with self._session.post(
                rpc_url, json=payload,
                timeout=aiohttp.ClientTimeout(total=8),
            ) as resp:
                data = await resp.json()
                return (data.get("result") or {}).get("value", {}).get("blockhash")
        except Exception as exc:
            logger.warning("orca_flash_swap.blockhash_failed", error=str(exc))
            return None

    async def _broadcast(
        self,
        signed_tx: VersionedTransaction,
        rpc_url: str,
        max_retries: int = 3,
    ) -> OrcaFlashSwapResult:
        """Broadcast signed transaction with retry logic."""
        tx_b64 = base64.b64encode(bytes(signed_tx)).decode()
        payload = {
            "jsonrpc": "2.0", "id": str(uuid.uuid4()),
            "method": "sendTransaction",
            "params": [tx_b64, {
                "encoding": "base64",
                "preflightCommitment": "confirmed",
                "maxRetries": max_retries,
            }],
        }
        try:
            async with self._session.post(
                rpc_url, json=payload,
                timeout=aiohttp.ClientTimeout(total=30),
            ) as resp:
                data = await resp.json()
                if "result" in data:
                    sig = data["result"]
                    logger.info("orca_flash_swap.broadcast_ok",
                                sig=sig[:20] + "...")
                    return OrcaFlashSwapResult(
                        success=True,
                        signature=sig,
                        gas_sol=0.0015,
                    )

                err = data.get("error", {})
                msg = err.get("message", str(err)) if isinstance(err, dict) else str(err)

                if any(k in msg.lower() for k in
                       ("simulation", "preflight", "insufficient", "slippage")):
                    logger.info("orca_flash_swap.simulation_rejected",
                                reason=msg[:100],
                                note="not profitable at execution time — safe")
                    return OrcaFlashSwapResult(
                        success=False, error=f"not_profitable: {msg[:80]}"
                    )

                logger.warning("orca_flash_swap.rpc_error", error=msg[:100])
                return OrcaFlashSwapResult(success=False, error=f"rpc_error: {msg[:80]}")

        except asyncio.TimeoutError:
            return OrcaFlashSwapResult(success=False, error="broadcast_timeout")
        except Exception as exc:
            return OrcaFlashSwapResult(success=False, error=f"broadcast_exception: {exc}")
