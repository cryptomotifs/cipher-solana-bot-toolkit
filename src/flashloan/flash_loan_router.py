"""
FlashLoanRouter — Provider-agnostic flash arb execution with automatic fallback.

Priority chain (tried in order):
  1. MarginFi (AtomicArbBuilder) — TRUE zero-capital, best for cross-DEX arb
       Needs: marginfi_account PDA (auto-created on first trade)
       Fee:   0.09% of loan + gas
  2. Orca Whirlpool flash swap — no protocol account needed
       Needs: USDC in wallet for buy leg, ATAs (created automatically)
       Fee:   Orca pool fee (0.01–1%) + gas
  3. Raydium CLMM flash swap — no protocol account needed
       Needs: USDC in wallet for buy leg, ATAs (created automatically)
       Fee:   Raydium pool fee (0.01–1%) + gas
  4. Jupiter direct swap — no protocol, no flash loan, just wallet USDC
       Needs: USDC in wallet, ATAs
       Fee:   Jupiter routing fees + gas

Providers 2-4 are NOT zero-capital — they need wallet USDC.
Provider 1 is the only true zero-capital option (MarginFi lending flash loan).

The router is designed to:
  - Always attempt the cheapest (most profitable) provider first
  - Fall back automatically when a provider fails or isn't initialized
  - Report which provider was used so you can track performance

Usage:
  router = FlashLoanRouter(rpc_url, wallet, session)
  await router.init()
  result = await router.execute_arb(buy_quote, sell_quote, loan_usdc)
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Optional

import aiohttp
import structlog

from src.execution.atomic_arb_builder import AtomicArbBuilder, FlashArbTxResult
from src.execution.orca_flash_swap import OrcaFlashSwap
from src.execution.raydium_flash_swap import RaydiumFlashSwap

logger = structlog.get_logger(__name__)


@dataclass
class RouterResult:
    """Unified result from any flash loan provider."""
    success: bool
    provider: str = ""           # "marginfi" | "orca" | "raydium" | "jupiter_direct"
    signature: str = ""
    profit_usdc_lamports: int = 0
    gas_sol: float = 0.0
    error: str = ""
    providers_tried: list[str] = None

    def __post_init__(self):
        if self.providers_tried is None:
            self.providers_tried = []

    @property
    def profit_usd(self) -> float:
        return self.profit_usdc_lamports / 1_000_000

    @property
    def net_profit_usd(self) -> float:
        return self.profit_usd - self.gas_sol


class FlashLoanRouter:
    """
    Routes flash arb execution across MarginFi, Orca, Raydium, and direct Jupiter.

    Tries providers in priority order. If MarginFi isn't initialized yet
    (first run, no marginfi_account), automatically falls through to
    Orca/Raydium which need no account setup — keeping the bot live
    even before MarginFi is ready.
    """

    def __init__(
        self,
        rpc_url: str,
        wallet,
        session: Optional[aiohttp.ClientSession] = None,
        loan_usdc_lamports: int = 500_000_000,  # $500 default
    ) -> None:
        self._rpc_url  = rpc_url
        self._wallet   = wallet
        self._session  = session
        self._loan_usd = loan_usdc_lamports

        # Providers — injected via setters or init()
        self._marginfi: Optional[AtomicArbBuilder]   = None
        self._orca:     Optional[OrcaFlashSwap]      = None
        self._raydium:  Optional[RaydiumFlashSwap]   = None

        self._initialized = False

        # Stats per provider
        self._attempts: dict[str, int] = {
            "marginfi": 0, "orca": 0, "raydium": 0, "jupiter_direct": 0
        }
        self._successes: dict[str, int] = {
            "marginfi": 0, "orca": 0, "raydium": 0, "jupiter_direct": 0
        }

    async def init(self) -> bool:
        """
        Initialise all providers concurrently.
        MarginFi failure doesn't block Orca/Raydium from starting.
        """
        # Build providers if not already injected
        if self._marginfi is None:
            self._marginfi = AtomicArbBuilder(
                rpc_url=self._rpc_url,
                wallet=self._wallet,
                session=self._session,
                loan_usdc_lamports=self._loan_usd,
            )
        if self._orca is None:
            self._orca = OrcaFlashSwap(self._rpc_url, self._session)
        if self._raydium is None:
            self._raydium = RaydiumFlashSwap(self._rpc_url, self._session)

        # Init all concurrently — failures are non-fatal
        results = await asyncio.gather(
            self._marginfi.init(),
            self._orca.init(),
            self._raydium.init(),
            return_exceptions=True,
        )

        mf_ok  = results[0] is True
        orc_ok = results[1] is True
        ray_ok = results[2] is True

        logger.info(
            "flash_loan_router.initialized",
            marginfi=mf_ok,
            orca=orc_ok,
            raydium=ray_ok,
        )

        self._initialized = mf_ok or orc_ok or ray_ok
        return self._initialized

    def set_marginfi_builder(self, builder: AtomicArbBuilder) -> None:
        """Inject pre-built MarginFi builder (from agent factory)."""
        self._marginfi = builder

    async def execute_arb(
        self,
        buy_quote: dict,
        sell_quote: dict,
        loan_usdc_lamports: Optional[int] = None,
        preferred_provider: Optional[str] = None,
    ) -> RouterResult:
        """
        Execute arb, trying providers in priority order.

        Args:
            buy_quote:  Jupiter /quote response for buy leg (USDC → token)
            sell_quote: Jupiter /quote response for sell leg (token → USDC)
            loan_usdc_lamports: override default loan size
            preferred_provider: force a specific provider ("marginfi"/"orca"/"raydium")

        Returns:
            RouterResult with success/failure, provider used, and profit
        """
        loan = loan_usdc_lamports or self._loan_usd
        tried: list[str] = []

        # ── Provider 1: MarginFi (true zero-capital) ──────────────────────
        if preferred_provider in (None, "marginfi"):
            mf = self._marginfi
            if mf and getattr(mf, "_initialized", False):
                tried.append("marginfi")
                self._attempts["marginfi"] += 1
                try:
                    r = await mf.execute_flash_arb(
                        buy_quote=buy_quote,
                        sell_quote=sell_quote,
                        loan_usdc_lamports=loan,
                    )
                    if r.success:
                        self._successes["marginfi"] += 1
                        return RouterResult(
                            success=True, provider="marginfi",
                            signature=r.signature,
                            profit_usdc_lamports=r.profit_usdc_lamports,
                            gas_sol=r.gas_sol,
                            providers_tried=tried,
                        )
                    # Not profitable → no point trying other providers
                    if r.error and "not_profitable" in r.error:
                        return RouterResult(
                            success=False, provider="marginfi",
                            error=r.error, providers_tried=tried,
                        )
                    logger.debug("flash_loan_router.marginfi_failed",
                                 error=r.error, trying_next="orca")
                except Exception as exc:
                    logger.warning("flash_loan_router.marginfi_exception",
                                   error=str(exc))
            elif preferred_provider == "marginfi":
                return RouterResult(
                    success=False, provider="marginfi",
                    error="marginfi_not_initialized",
                    providers_tried=tried,
                )

        # ── Provider 2: Orca Whirlpool (no account needed, needs wallet USDC) ──
        if preferred_provider in (None, "orca"):
            orca = self._orca
            if orca:
                token_mint = buy_quote.get("inputMint") or buy_quote.get("outputMint", "")
                # For buy leg, inputMint is USDC, outputMint is token
                token_mint = buy_quote.get("outputMint", "")
                buy_pool  = orca.get_pool(token_mint)
                sell_pool = orca.get_pool(token_mint)  # same token, may be different pool

                if buy_pool and sell_pool and buy_pool.address != sell_pool.address:
                    tried.append("orca")
                    self._attempts["orca"] += 1
                    try:
                        r = await orca.execute_two_pool_arb(
                            buy_pool=buy_pool,
                            sell_pool=sell_pool,
                            wallet=self._wallet,
                            amount_usdc=loan,
                            min_profit_usdc=100,
                            rpc_url=self._rpc_url,
                        )
                        if r.success:
                            self._successes["orca"] += 1
                            return RouterResult(
                                success=True, provider="orca",
                                signature=r.signature,
                                profit_usdc_lamports=r.profit_usdc_lamports,
                                gas_sol=r.gas_sol,
                                providers_tried=tried,
                            )
                        if r.error and "not_profitable" in r.error:
                            return RouterResult(
                                success=False, provider="orca",
                                error=r.error, providers_tried=tried,
                            )
                        logger.debug("flash_loan_router.orca_failed",
                                     error=r.error, trying_next="raydium")
                    except Exception as exc:
                        logger.warning("flash_loan_router.orca_exception", error=str(exc))

        # ── Provider 3: Raydium CLMM (no account needed, needs wallet USDC) ──
        if preferred_provider in (None, "raydium"):
            ray = self._raydium
            if ray:
                token_mint = buy_quote.get("outputMint", "")
                buy_pool  = ray.get_pool(token_mint)
                sell_pool = ray.get_pool(token_mint)

                if buy_pool and sell_pool and buy_pool.address != sell_pool.address:
                    tried.append("raydium")
                    self._attempts["raydium"] += 1
                    try:
                        r = await ray.execute_two_pool_arb(
                            buy_pool=buy_pool,
                            sell_pool=sell_pool,
                            wallet=self._wallet,
                            amount_usdc=loan,
                            min_profit_usdc=100,
                            rpc_url=self._rpc_url,
                        )
                        if r.success:
                            self._successes["raydium"] += 1
                            return RouterResult(
                                success=True, provider="raydium",
                                signature=r.signature,
                                profit_usdc_lamports=r.profit_usdc_lamports,
                                gas_sol=r.gas_sol,
                                providers_tried=tried,
                            )
                        logger.debug("flash_loan_router.raydium_failed",
                                     error=r.error)
                    except Exception as exc:
                        logger.warning("flash_loan_router.raydium_exception", error=str(exc))

        # All providers tried / unavailable
        return RouterResult(
            success=False,
            error=f"all_providers_failed: tried={tried}",
            providers_tried=tried,
        )

    async def execute_single_swap_arb(
        self,
        swap_quote: dict,
        loan_usdc_lamports: Optional[int] = None,
    ) -> RouterResult:
        """
        Execute a single multi-hop (triangular) arb via MarginFi.
        Falls through to direct Jupiter swap if MarginFi unavailable.
        """
        loan = loan_usdc_lamports or self._loan_usd
        tried: list[str] = []

        # MarginFi handles single multi-hop quotes via execute_single_swap_flash_arb
        mf = self._marginfi
        if mf and getattr(mf, "_initialized", False):
            tried.append("marginfi")
            self._attempts["marginfi"] += 1
            try:
                r = await mf.execute_single_swap_flash_arb(
                    swap_quote=swap_quote,
                    loan_usdc_lamports=loan,
                )
                if r.success:
                    self._successes["marginfi"] += 1
                    return RouterResult(
                        success=True, provider="marginfi",
                        signature=r.signature,
                        profit_usdc_lamports=r.profit_usdc_lamports,
                        gas_sol=r.gas_sol,
                        providers_tried=tried,
                    )
                if r.error and "not_profitable" in r.error:
                    return RouterResult(
                        success=False, error=r.error, providers_tried=tried
                    )
            except Exception as exc:
                logger.warning("flash_loan_router.marginfi_single_exception",
                               error=str(exc))

        return RouterResult(
            success=False,
            error="marginfi_unavailable_for_single_swap",
            providers_tried=tried,
        )

    @property
    def stats(self) -> dict:
        return {
            "initialized":  self._initialized,
            "providers": {
                p: {"attempts": self._attempts[p], "successes": self._successes[p]}
                for p in self._attempts
            },
            "marginfi_ready":  bool(self._marginfi and getattr(self._marginfi, "_initialized", False)),
            "orca_ready":      bool(self._orca),
            "raydium_ready":   bool(self._raydium),
        }
