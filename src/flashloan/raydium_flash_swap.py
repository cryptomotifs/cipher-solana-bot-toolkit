"""
RaydiumFlashSwap — Raydium CLMM (Concentrated Liquidity) flash swap executor.

Raydium CLMM is Raydium's Uniswap-v3-style pool. Like Orca Whirlpool,
it holds token reserves in vaults. A swap instruction atomically:
  - Moves input tokens from trader ATA → pool vault
  - Moves output tokens from pool vault → trader ATA
Within a single transaction, if you compose the right instructions,
you can do buy-on-Raydium + sell-on-Orca (or vice versa) atomically.

NO protocol account required — only standard ATAs.

Key advantages over Orca for arb:
  - Raydium CLMM pools often have tighter spreads (more efficient)
  - Raydium processes more volume → more arb opportunities
  - Different liquidity depth = different arb windows than Orca

Program:  CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK
Fee tiers: 100 (0.01%), 500 (0.05%), 3000 (0.30%), 10000 (1%)

Usage:
  raydium = RaydiumFlashSwap(rpc_url, session)
  await raydium.init()
  pool = await raydium.fetch_pool(pool_address)
  result = await raydium.execute_two_pool_arb(buy_pool, sell_pool, wallet, amount)
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import struct
import uuid
from dataclasses import dataclass
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
from src.execution.orca_flash_swap import (
    ASSOCIATED_TOKEN_PROGRAM, COMPUTE_BUDGET, SYSTEM_PROGRAM,
    FLASH_SWAP_COMPUTE_UNITS, FLASH_SWAP_PRIORITY_FEE,
    build_create_ata_if_needed_ix, build_compute_budget_ix, build_priority_fee_ix,
    derive_ata,
)

logger = structlog.get_logger(__name__)

# ── Program constants ──────────────────────────────────────────────────────

RAYDIUM_CLMM_PROGRAM  = Pubkey.from_string("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK")
RAYDIUM_AMM_V4        = Pubkey.from_string("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8")
TOKEN_PROGRAM_2022     = Pubkey.from_string("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")

# Discriminators — sha256("global:{name}")[:8]
def _disc(name: str) -> bytes:
    return hashlib.sha256(f"global:{name}".encode()).digest()[:8]

DISC_SWAP_V2          = _disc("swap_v2")         # Raydium CLMM swap instruction
DISC_TWO_HOP_SWAP_V2  = _disc("two_hop_swap_v2")  # two-pool atomic swap

# Known high-liquidity Raydium CLMM pools (token_mint, pool_address, fee_bps)
# These have USDC as one of the pair tokens.
KNOWN_RAYDIUM_USDC_POOLS: list[tuple[str, str, int]] = [
    # SOL/USDC — highest volume pool
    ("So11111111111111111111111111111111111111112",
     "2QdhepnKRTLjjSqPL1PtKNwqrUkoLee5Gqs8bvZhRdMv", 500),
    # BONK/USDC
    ("DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
     "9iBLMtKmtXC6CQjSWS3EXi5kPJBj5T5ngLZPsRpXkrpE", 3000),
    # WIF/USDC
    ("EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm",
     "FpCMFDFGYotvufJ7HrFHsWEiiQCGbkLCtwHiDnh7o28Q", 3000),
    # JTO/USDC
    ("jtojtomepa8bdnE4UGmkKwvgM9Kv2baMQYMnWDxjFLY",
     "9MW7xhF7cADKVgk4cNqiQvM7rGjSp8tqJpMEMsB4RoLp", 3000),
    # RAY/USDC
    ("4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R",
     "G7dxovEzEjPqFejamkxMG6ujfFY7aPJTPjCMj8GzqLrk", 3000),
]


@dataclass
class RaydiumPoolInfo:
    """Metadata for a Raydium CLMM pool."""
    address: Pubkey
    token_mint_0: Pubkey          # token_0 in Raydium layout
    token_mint_1: Pubkey          # token_1
    token_vault_0: Pubkey
    token_vault_1: Pubkey
    tick_array_bitmap_ext: Pubkey  # tick array bitmap extension PDA
    observation_key: Pubkey        # price oracle observation account
    fee_rate: int                  # fee in hundredths of a bip (100 = 0.01%)
    sqrt_price_x64: int            # current sqrt price Q64.64
    current_tick: int
    usdc_is_token0: bool           # True if USDC is token_0, False if token_1


@dataclass
class RaydiumFlashSwapResult:
    success: bool
    signature: str = ""
    profit_usdc_lamports: int = 0
    gas_sol: float = 0.0
    error: str = ""
    provider: str = "raydium"

    @property
    def profit_usd(self) -> float:
        return self.profit_usdc_lamports / 1_000_000

    @property
    def net_profit_usd(self) -> float:
        return self.profit_usd - self.gas_sol


# ── PDA helpers ───────────────────────────────────────────────────────────

def derive_raydium_tick_array(
    pool: Pubkey, start_index: int
) -> Pubkey:
    """Derive tick array PDA for a Raydium CLMM pool."""
    addr, _ = Pubkey.find_program_address(
        [b"tick_array", bytes(pool),
         start_index.to_bytes(4, "little", signed=True)],
        RAYDIUM_CLMM_PROGRAM,
    )
    return addr


def derive_raydium_protocol_position(
    pool: Pubkey, tick_lower: int, tick_upper: int
) -> Pubkey:
    """Derive protocol position PDA (used internally by pool, needed as remaining account)."""
    addr, _ = Pubkey.find_program_address(
        [b"position",
         bytes(pool),
         tick_lower.to_bytes(4, "little", signed=True),
         tick_upper.to_bytes(4, "little", signed=True)],
        RAYDIUM_CLMM_PROGRAM,
    )
    return addr


def derive_raydium_observation(pool: Pubkey) -> Pubkey:
    """Derive observation account PDA for price oracle."""
    addr, _ = Pubkey.find_program_address(
        [b"observation", bytes(pool)],
        RAYDIUM_CLMM_PROGRAM,
    )
    return addr


def derive_raydium_tick_bitmap_ext(pool: Pubkey) -> Pubkey:
    """Derive tick array bitmap extension PDA."""
    addr, _ = Pubkey.find_program_address(
        [b"pool_tick_array_bitmap_extension", bytes(pool)],
        RAYDIUM_CLMM_PROGRAM,
    )
    return addr


def tick_to_array_start_raydium(tick: int, tick_spacing: int) -> int:
    """Snap tick index to Raydium tick array start (60 ticks per array)."""
    ticks_per_array = 60 * tick_spacing
    if tick < 0:
        return ((tick - ticks_per_array + 1) // ticks_per_array) * ticks_per_array
    return (tick // ticks_per_array) * ticks_per_array


# ── Instruction builder ────────────────────────────────────────────────────

def build_raydium_swap_v2_ix(
    pool: RaydiumPoolInfo,
    wallet_pk: Pubkey,
    input_token_ata: Pubkey,
    output_token_ata: Pubkey,
    input_vault: Pubkey,
    output_vault: Pubkey,
    input_token_mint: Pubkey,
    output_token_mint: Pubkey,
    tick_arrays: list[Pubkey],      # 3 tick arrays around current price
    observation: Pubkey,
    amount: int,
    amount_limit: int,              # min out (exact in) or max in (exact out)
    zero_for_one: bool,             # True: token_0→token_1, False: token_1→token_0
    is_base_input: bool = True,     # True: exact input, False: exact output
) -> Instruction:
    """
    Build Raydium CLMM swapV2 instruction.

    Data layout (from Raydium CLMM IDL):
      discriminator:        [u8; 8]   = DISC_SWAP_V2
      amount:               u64       = input/output amount
      other_amount_threshold: u64     = slippage limit
      sqrt_price_limit_x64: u128      = price limit (0 = none)
      is_base_input:        bool      = exact input or exact output
    """
    # Price limit: if zero_for_one (price going down), use minimum; else maximum
    sqrt_limit: int = (
        4_295_048_016 if zero_for_one
        else 79_226_673_515_401_279_992_447_902_215
    )

    data = (
        DISC_SWAP_V2
        + struct.pack("<Q", amount)
        + struct.pack("<Q", amount_limit)
        + struct.pack("<QQ",
                      sqrt_limit & 0xFFFFFFFFFFFFFFFF,
                      (sqrt_limit >> 64) & 0xFFFFFFFFFFFFFFFF)
        + struct.pack("<?", is_base_input)
    )

    # Raydium CLMM swapV2 account order (from IDL):
    accounts = [
        AccountMeta(wallet_pk,              is_signer=True,  is_writable=False),
        AccountMeta(pool.address,           is_signer=False, is_writable=True),
        AccountMeta(input_token_ata,        is_signer=False, is_writable=True),
        AccountMeta(output_token_ata,       is_signer=False, is_writable=True),
        AccountMeta(input_vault,            is_signer=False, is_writable=True),
        AccountMeta(output_vault,           is_signer=False, is_writable=True),
        AccountMeta(observation,            is_signer=False, is_writable=True),
        AccountMeta(TOKEN_PROGRAM,          is_signer=False, is_writable=False),
        AccountMeta(TOKEN_PROGRAM_2022,     is_signer=False, is_writable=False),
        AccountMeta(input_token_mint,       is_signer=False, is_writable=False),
        AccountMeta(output_token_mint,      is_signer=False, is_writable=False),
        # Tick arrays as remaining accounts (3 required)
        *[AccountMeta(ta, is_signer=False, is_writable=True) for ta in tick_arrays[:3]],
    ]

    return Instruction(
        program_id=RAYDIUM_CLMM_PROGRAM,
        accounts=accounts,
        data=data,
    )


# ── Main executor ─────────────────────────────────────────────────────────

class RaydiumFlashSwap:
    """
    Execute atomic arb using Raydium CLMM pools.

    NO protocol account required — only standard ATAs.
    Supports:
      - Two-pool Raydium arb (buy on pool_A, sell on pool_B)
      - Cross-DEX arb (buy on Raydium, sell via Jupiter/Orca instructions)

    For zero-capital, wrap with MarginFi (AtomicArbBuilder).
    For capital-available arb, this works standalone.
    """

    def __init__(
        self,
        rpc_url: str,
        session: Optional[aiohttp.ClientSession] = None,
    ) -> None:
        self._rpc_url = rpc_url
        self._session = session
        self._own_session = session is None
        self._pools: dict[str, RaydiumPoolInfo] = {}

    async def init(self) -> bool:
        if self._own_session:
            self._session = aiohttp.ClientSession()
        asyncio.create_task(self._warm_pool_cache())
        logger.info("raydium_flash_swap.initialized")
        return True

    async def close(self) -> None:
        if self._own_session and self._session and not self._session.closed:
            await self._session.close()

    async def _warm_pool_cache(self) -> None:
        for token_mint, pool_addr, _ in KNOWN_RAYDIUM_USDC_POOLS:
            try:
                pool = await self.fetch_pool(pool_addr)
                if pool:
                    self._pools[token_mint] = pool
            except Exception:
                pass
        logger.debug("raydium_flash_swap.pool_cache_warmed",
                     cached=len(self._pools))

    async def fetch_pool(self, pool_address: str) -> Optional[RaydiumPoolInfo]:
        """
        Fetch and parse Raydium CLMM pool account.

        Raydium CLMM PoolState layout (from Raydium IDL):
          discriminator:          [u8; 8]
          bump:                   u8
          amm_config:             Pubkey (32)   offset 9
          owner:                  Pubkey (32)   offset 41
          token_mint_0:           Pubkey (32)   offset 73
          token_mint_1:           Pubkey (32)   offset 105
          token_vault_0:          Pubkey (32)   offset 137
          token_vault_1:          Pubkey (32)   offset 169
          observation_key:        Pubkey (32)   offset 201
          mint_decimals_0:        u8            offset 233
          mint_decimals_1:        u8            offset 234
          tick_spacing:           u16           offset 235
          liquidity:              u128          offset 237
          sqrt_price_x64:         u128          offset 253
          tick_current:           i32           offset 269
          ...
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
                if len(raw) < 280:
                    return None

                token_mint_0  = Pubkey.from_bytes(raw[73:105])
                token_mint_1  = Pubkey.from_bytes(raw[105:137])
                token_vault_0 = Pubkey.from_bytes(raw[137:169])
                token_vault_1 = Pubkey.from_bytes(raw[169:201])
                obs_key       = Pubkey.from_bytes(raw[201:233])
                tick_spacing  = struct.unpack_from("<H", raw, 235)[0]
                sqrt_price    = int.from_bytes(raw[253:269], "little")
                current_tick  = struct.unpack_from("<i", raw, 269)[0]

                pool_pk = Pubkey.from_string(pool_address)
                usdc_pk = Pubkey.from_string(USDC_MINT)

                return RaydiumPoolInfo(
                    address=pool_pk,
                    token_mint_0=token_mint_0,
                    token_mint_1=token_mint_1,
                    token_vault_0=token_vault_0,
                    token_vault_1=token_vault_1,
                    tick_array_bitmap_ext=derive_raydium_tick_bitmap_ext(pool_pk),
                    observation_key=obs_key,
                    fee_rate=0,
                    sqrt_price_x64=sqrt_price,
                    current_tick=current_tick,
                    usdc_is_token0=(token_mint_0 == usdc_pk),
                )
        except Exception as exc:
            logger.warning("raydium_flash_swap.fetch_pool_failed",
                           pool=pool_address[:16], error=str(exc))
            return None

    def get_pool(self, token_mint: str) -> Optional[RaydiumPoolInfo]:
        return self._pools.get(token_mint)

    async def execute_two_pool_arb(
        self,
        buy_pool: RaydiumPoolInfo,
        sell_pool: RaydiumPoolInfo,
        wallet,
        amount_usdc: int,
        min_profit_usdc: int = 100,
        rpc_url: Optional[str] = None,
    ) -> RaydiumFlashSwapResult:
        """
        Atomic two-pool arb: buy on buy_pool (cheap), sell on sell_pool (expensive).
        Both swaps in one VersionedTransaction — no protocol accounts needed.
        Requires wallet USDC.
        """
        rpc = rpc_url or self._rpc_url
        if not self._session or not wallet or not getattr(wallet, "_keypair", None):
            return RaydiumFlashSwapResult(success=False, error="wallet_or_session_missing")

        wallet_pk  = Pubkey.from_string(
            getattr(wallet, "public_key", "") or getattr(wallet, "pubkey", "")
        )
        usdc_pk    = Pubkey.from_string(USDC_MINT)
        token_mint = (buy_pool.token_mint_0
                      if not buy_pool.usdc_is_token0
                      else buy_pool.token_mint_1)

        usdc_ata   = derive_ata(wallet_pk, usdc_pk)
        token_ata  = derive_ata(wallet_pk, token_mint)

        # Derive tick arrays for both pools
        ts0 = tick_to_array_start_raydium(buy_pool.current_tick, 60)  # default spacing
        buy_tick_arrays  = [derive_raydium_tick_array(buy_pool.address,  ts0 + i * 60 * 60)
                            for i in range(3)]
        sell_tick_arrays = [derive_raydium_tick_array(sell_pool.address, ts0 + i * 60 * 60)
                            for i in range(3)]
        buy_obs  = derive_raydium_observation(buy_pool.address)
        sell_obs = derive_raydium_observation(sell_pool.address)

        # ── Buy leg: USDC → token (zero_for_one depends on which token is 0) ──
        buy_zero_for_one = buy_pool.usdc_is_token0   # USDC→token: if USDC is 0, go 0→1
        buy_ix = build_raydium_swap_v2_ix(
            pool=buy_pool,
            wallet_pk=wallet_pk,
            input_token_ata=usdc_ata,
            output_token_ata=token_ata,
            input_vault=buy_pool.token_vault_0 if buy_zero_for_one else buy_pool.token_vault_1,
            output_vault=buy_pool.token_vault_1 if buy_zero_for_one else buy_pool.token_vault_0,
            input_token_mint=usdc_pk,
            output_token_mint=token_mint,
            tick_arrays=buy_tick_arrays,
            observation=buy_obs,
            amount=amount_usdc,
            amount_limit=0,
            zero_for_one=buy_zero_for_one,
            is_base_input=True,
        )

        # ── Sell leg: token → USDC ────────────────────────────────────────
        sell_zero_for_one = not sell_pool.usdc_is_token0  # token→USDC: if USDC is 1, go 0→1
        sell_ix = build_raydium_swap_v2_ix(
            pool=sell_pool,
            wallet_pk=wallet_pk,
            input_token_ata=token_ata,
            output_token_ata=usdc_ata,
            input_vault=sell_pool.token_vault_0 if sell_zero_for_one else sell_pool.token_vault_1,
            output_vault=sell_pool.token_vault_1 if sell_zero_for_one else sell_pool.token_vault_0,
            input_token_mint=token_mint,
            output_token_mint=usdc_pk,
            tick_arrays=sell_tick_arrays,
            observation=sell_obs,
            amount=0,  # exact output mode: specify min USDC we need back
            amount_limit=amount_usdc + min_profit_usdc,
            zero_for_one=sell_zero_for_one,
            is_base_input=False,  # exact output — we want at least amount_usdc + profit
        )

        ixs = [
            build_priority_fee_ix(FLASH_SWAP_PRIORITY_FEE),
            build_compute_budget_ix(FLASH_SWAP_COMPUTE_UNITS),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, usdc_pk,   usdc_ata),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, token_mint, token_ata),
            buy_ix,
            sell_ix,
        ]

        blockhash_str = await self._fetch_blockhash(rpc)
        if not blockhash_str:
            return RaydiumFlashSwapResult(success=False, error="blockhash_failed")

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
            return RaydiumFlashSwapResult(success=False, error=f"compile_failed: {exc}")

        return await self._broadcast(signed_tx, rpc)

    async def execute_cross_dex_arb(
        self,
        buy_pool: RaydiumPoolInfo,
        sell_swap_ixs: list[Instruction],
        sell_alts: list[AddressLookupTableAccount],
        wallet,
        amount_usdc: int,
        rpc_url: Optional[str] = None,
    ) -> RaydiumFlashSwapResult:
        """
        Cross-DEX arb: buy on Raydium CLMM, sell anywhere (Orca, Jupiter, etc.).
        sell_swap_ixs are pre-built instructions for the sell leg.
        """
        rpc = rpc_url or self._rpc_url
        if not self._session or not wallet or not getattr(wallet, "_keypair", None):
            return RaydiumFlashSwapResult(success=False, error="wallet_or_session_missing")

        wallet_pk  = Pubkey.from_string(
            getattr(wallet, "public_key", "") or getattr(wallet, "pubkey", "")
        )
        usdc_pk    = Pubkey.from_string(USDC_MINT)
        token_mint = (buy_pool.token_mint_0
                      if not buy_pool.usdc_is_token0
                      else buy_pool.token_mint_1)

        usdc_ata  = derive_ata(wallet_pk, usdc_pk)
        token_ata = derive_ata(wallet_pk, token_mint)

        ts0 = tick_to_array_start_raydium(buy_pool.current_tick, 60)
        buy_tick_arrays = [derive_raydium_tick_array(buy_pool.address, ts0 + i * 60 * 60)
                           for i in range(3)]
        buy_obs = derive_raydium_observation(buy_pool.address)

        buy_zero_for_one = buy_pool.usdc_is_token0
        buy_ix = build_raydium_swap_v2_ix(
            pool=buy_pool,
            wallet_pk=wallet_pk,
            input_token_ata=usdc_ata,
            output_token_ata=token_ata,
            input_vault=buy_pool.token_vault_0 if buy_zero_for_one else buy_pool.token_vault_1,
            output_vault=buy_pool.token_vault_1 if buy_zero_for_one else buy_pool.token_vault_0,
            input_token_mint=usdc_pk,
            output_token_mint=token_mint,
            tick_arrays=buy_tick_arrays,
            observation=buy_obs,
            amount=amount_usdc,
            amount_limit=0,
            zero_for_one=buy_zero_for_one,
            is_base_input=True,
        )

        ixs = [
            build_priority_fee_ix(FLASH_SWAP_PRIORITY_FEE),
            build_compute_budget_ix(FLASH_SWAP_COMPUTE_UNITS),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, usdc_pk,    usdc_ata),
            build_create_ata_if_needed_ix(wallet_pk, wallet_pk, token_mint, token_ata),
            buy_ix,
            *sell_swap_ixs,
        ]

        blockhash_str = await self._fetch_blockhash(rpc)
        if not blockhash_str:
            return RaydiumFlashSwapResult(success=False, error="blockhash_failed")

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
            return RaydiumFlashSwapResult(success=False, error=f"compile_failed: {exc}")

        return await self._broadcast(signed_tx, rpc)

    async def _fetch_blockhash(self, rpc_url: str) -> Optional[str]:
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
            logger.warning("raydium_flash_swap.blockhash_failed", error=str(exc))
            return None

    async def _broadcast(
        self,
        signed_tx: VersionedTransaction,
        rpc_url: str,
    ) -> RaydiumFlashSwapResult:
        tx_b64 = base64.b64encode(bytes(signed_tx)).decode()
        payload = {
            "jsonrpc": "2.0", "id": str(uuid.uuid4()),
            "method": "sendTransaction",
            "params": [tx_b64, {
                "encoding": "base64",
                "preflightCommitment": "confirmed",
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
                    logger.info("raydium_flash_swap.broadcast_ok", sig=sig[:20] + "...")
                    return RaydiumFlashSwapResult(
                        success=True, signature=sig, gas_sol=0.0015
                    )
                err = data.get("error", {})
                msg = err.get("message", str(err)) if isinstance(err, dict) else str(err)
                if any(k in msg.lower() for k in
                       ("simulation", "preflight", "insufficient", "slippage")):
                    return RaydiumFlashSwapResult(
                        success=False, error=f"not_profitable: {msg[:80]}"
                    )
                logger.warning("raydium_flash_swap.rpc_error", error=msg[:80])
                return RaydiumFlashSwapResult(success=False, error=f"rpc_error: {msg[:80]}")
        except asyncio.TimeoutError:
            return RaydiumFlashSwapResult(success=False, error="broadcast_timeout")
        except Exception as exc:
            return RaydiumFlashSwapResult(success=False, error=f"broadcast_exception: {exc}")
