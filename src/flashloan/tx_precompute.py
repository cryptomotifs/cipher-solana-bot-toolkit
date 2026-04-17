"""
TxPrecomputeCache — Innovation #29: Transaction Precomputation.

Pre-caches Jupiter quotes for tokens the bot is actively watching, so when
a signal fires we already have a fresh quote ready. Reduces execution latency
from ~500ms (quote + swap) to ~150ms (swap only, quote already cached).

Watches:
- Tokens in open positions (for fast exits)
- Tokens from recent signals (for fast entries)
- Tokens on bonding curves nearing graduation

Refresh interval: 3s for hot tokens, 10s for warm tokens.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field

import aiohttp
import structlog

logger = structlog.get_logger(__name__)

# Jupiter Swap API
JUPITER_QUOTE_URL = "https://api.jup.ag/swap/v1/quote"
JUPITER_SWAP_URL = "https://api.jup.ag/swap/v1/swap"
USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"

# Cache settings
HOT_REFRESH_SEC = 3.0      # Tokens in open positions
WARM_REFRESH_SEC = 10.0    # Recently signaled tokens
MAX_CACHED_TOKENS = 30     # Don't cache more than this (API budget)
QUOTE_MAX_AGE_SEC = 8.0    # Quotes older than this are stale
DEFAULT_SLIPPAGE_BPS = 300  # 3%

# Phase 35: Pre-signed sell transaction cache
PRE_SIGN_ENABLED = True
PRE_SIGN_REFRESH_SEC = 5.0
PRE_SIGN_MAX_AGE_SEC = 6.0


@dataclass
class CachedQuote:
    """A pre-fetched Jupiter quote."""
    mint: str = ""
    direction: str = "buy"   # "buy" or "sell"
    amount: int = 0          # Input amount in smallest units
    quote: dict = field(default_factory=dict)
    fetched_at: float = 0.0
    is_hot: bool = False     # Hot = open position, refreshes faster
    # Innovation #40: Pre-serialized swap transaction
    swap_tx: str = ""        # Serialized transaction (if pre-built)
    swap_tx_fetched_at: float = 0.0
    # Phase 35: Pre-signed transaction bytes
    pre_signed_tx: bytes = b""
    pre_signed_at: float = 0.0
    pre_sign_blockhash: str = ""

    @property
    def age_sec(self) -> float:
        return time.monotonic() - self.fetched_at

    @property
    def is_stale(self) -> bool:
        return self.age_sec > QUOTE_MAX_AGE_SEC

    @property
    def has_fresh_swap_tx(self) -> bool:
        """Innovation #40: Check if pre-serialized tx is still fresh."""
        if not self.swap_tx:
            return False
        return (time.monotonic() - self.swap_tx_fetched_at) < QUOTE_MAX_AGE_SEC

    @property
    def has_fresh_pre_signed(self) -> bool:
        """Check if pre-signed tx is still valid (blockhash not expired)."""
        if not self.pre_signed_tx:
            return False
        return (time.monotonic() - self.pre_signed_at) < PRE_SIGN_MAX_AGE_SEC


class TxPrecomputeCache:
    """Pre-caches Jupiter quotes for fast execution.

    Usage:
        cache = TxPrecomputeCache(jup_session=session)
        cache.set_hot_memory(hot_memory)
        await cache.start()

        # When signal fires:
        quote = cache.get_cached_quote(mint, "buy", amount)
        if quote and not quote.is_stale:
            # Use cached quote — save 200-500ms
        else:
            # Fetch fresh quote (fallback)
    """

    def __init__(
        self,
        jup_session: aiohttp.ClientSession | None = None,
        slippage_bps: int = DEFAULT_SLIPPAGE_BPS,
        wallet_pubkey: str = "",
    ) -> None:
        self._jup_session = jup_session
        self._slippage_bps = slippage_bps
        self._wallet_pubkey = wallet_pubkey
        self._hot_memory = None

        # mint -> CachedQuote (buy direction)
        self._buy_cache: dict[str, CachedQuote] = {}
        # mint -> CachedQuote (sell direction)
        self._sell_cache: dict[str, CachedQuote] = {}

        self._refresh_task: asyncio.Task | None = None
        self._running: bool = False

        # Phase 35: Pre-signed transaction support
        self._wallet = None
        self._blockhash_manager = None

        # Stats
        self._cache_hits: int = 0
        self._cache_misses: int = 0
        self._refreshes: int = 0

        # Route pre-cache: warm tokens from various sources
        # Tokens that are likely to signal soon — pre-fetch routes
        self._warm_tokens: dict[str, float] = {}  # mint → added_time (monotonic)
        self._warm_token_max = 15  # Max warm tokens to track
        self._warm_token_ttl = 120.0  # Warm tokens expire after 2 min

    def set_hot_memory(self, hot_memory) -> None:
        """Inject HotMemory reference."""
        self._hot_memory = hot_memory

    def set_session(self, session: aiohttp.ClientSession) -> None:
        """Inject Jupiter API session."""
        self._jup_session = session

    def set_wallet_pubkey(self, pubkey: str) -> None:
        """Inject wallet pubkey for pre-serialized swap transactions."""
        self._wallet_pubkey = pubkey

    def add_warm_token(self, mint: str) -> None:
        """Add a token to the warm pre-cache queue.

        Called when a token is mentioned in Telegram, a tracked wallet
        transacts it, or it shows volume activity — likely to signal soon.
        """
        if mint in self._warm_tokens:
            return
        # Evict oldest if at capacity
        if len(self._warm_tokens) >= self._warm_token_max:
            oldest = min(self._warm_tokens, key=self._warm_tokens.get)
            del self._warm_tokens[oldest]
        self._warm_tokens[mint] = time.monotonic()

    async def start(self) -> None:
        """Start background quote refresh loop."""
        self._running = True
        self._refresh_task = asyncio.create_task(self._refresh_loop())
        logger.info("tx_precompute.started")

    async def stop(self) -> None:
        """Stop background refresh."""
        self._running = False
        if self._refresh_task:
            self._refresh_task.cancel()
            try:
                await self._refresh_task
            except asyncio.CancelledError:
                pass
        logger.info(
            "tx_precompute.stopped",
            cache_hits=self._cache_hits,
            cache_misses=self._cache_misses,
            refreshes=self._refreshes,
        )

    def get_cached_quote(
        self, mint: str, direction: str, amount: int,
    ) -> CachedQuote | None:
        """Get a cached quote if available and not stale.

        Args:
            mint: Token mint address
            direction: "buy" or "sell"
            amount: Input amount in smallest units (for validation)

        Returns:
            CachedQuote if found and fresh, None otherwise.
        """
        cache = self._buy_cache if direction == "buy" else self._sell_cache
        cached = cache.get(mint)

        if cached and not cached.is_stale:
            self._cache_hits += 1
            return cached

        self._cache_misses += 1
        return None

    async def _refresh_loop(self) -> None:
        """Background loop that refreshes cached quotes."""
        while self._running:
            try:
                await self._refresh_all()
            except asyncio.CancelledError:
                return
            except Exception:
                logger.exception("tx_precompute.refresh_error")
            await asyncio.sleep(HOT_REFRESH_SEC)

    async def _refresh_all(self) -> None:
        """Refresh quotes for all watched tokens."""
        if not self._hot_memory or not self._jup_session:
            return

        now = time.monotonic()
        tasks: list[tuple[str, str, int, bool]] = []  # (mint, direction, amount, is_hot)

        # Hot tokens: open positions need sell quotes for fast exit
        for pos in self._hot_memory.get_open_positions():
            mint = pos.token_mint
            if pos.token_amount_raw > 0:
                sell_cached = self._sell_cache.get(mint)
                if not sell_cached or sell_cached.age_sec > HOT_REFRESH_SEC:
                    tasks.append((mint, "sell", pos.token_amount_raw, True))

        # Warm tokens: recent signals need buy quotes
        recent_mints = set()
        for sig in list(self._hot_memory.recent_signals)[-20:]:
            mint = sig.token_mint
            if mint not in recent_mints:
                recent_mints.add(mint)
                buy_cached = self._buy_cache.get(mint)
                if not buy_cached or buy_cached.age_sec > WARM_REFRESH_SEC:
                    # Default buy amount: $15 worth of USDC (15 * 1e6)
                    tasks.append((mint, "buy", 15_000_000, False))

        # Route pre-cache: warm tokens from Telegram, wallet activity, volume
        now = time.monotonic()
        expired = [m for m, t in self._warm_tokens.items() if now - t > self._warm_token_ttl]
        for m in expired:
            del self._warm_tokens[m]
        for mint in list(self._warm_tokens.keys()):
            if mint not in recent_mints:
                buy_cached = self._buy_cache.get(mint)
                if not buy_cached or buy_cached.age_sec > WARM_REFRESH_SEC:
                    tasks.append((mint, "buy", 15_000_000, False))

        # Cap total tasks to avoid API overload
        tasks = tasks[:MAX_CACHED_TOKENS]

        # Fetch quotes concurrently (batch of 5 at a time)
        for i in range(0, len(tasks), 5):
            batch = tasks[i:i + 5]
            coros = [
                self._fetch_and_cache(mint, direction, amount, is_hot)
                for mint, direction, amount, is_hot in batch
            ]
            await asyncio.gather(*coros, return_exceptions=True)

        self._refreshes += 1

        # Prune stale entries
        self._prune_stale()
        # Enforce max cache size to prevent OOM in long-running sessions
        self._prune_caches()

    async def _fetch_and_cache(
        self, mint: str, direction: str, amount: int, is_hot: bool,
    ) -> None:
        """Fetch a Jupiter quote and cache it."""
        if not self._jup_session:
            return

        if direction == "buy":
            input_mint = USDC_MINT
            output_mint = mint
        else:
            input_mint = mint
            output_mint = USDC_MINT

        params = {
            "inputMint": input_mint,
            "outputMint": output_mint,
            "amount": str(amount),
            "slippageBps": str(self._slippage_bps),
        }

        try:
            async with self._jup_session.get(
                JUPITER_QUOTE_URL,
                params=params,
                timeout=aiohttp.ClientTimeout(total=5),
            ) as resp:
                if resp.status != 200:
                    return
                quote_data = await resp.json()
        except Exception:
            return

        cached = CachedQuote(
            mint=mint,
            direction=direction,
            amount=amount,
            quote=quote_data,
            fetched_at=time.monotonic(),
            is_hot=is_hot,
        )

        # Innovation #40: Pre-build swap transaction for hot tokens
        if is_hot and self._wallet_pubkey and quote_data:
            swap_tx = await self._fetch_swap_tx(quote_data)
            if swap_tx:
                cached.swap_tx = swap_tx
                cached.swap_tx_fetched_at = time.monotonic()

        if direction == "buy":
            self._buy_cache[mint] = cached
        else:
            self._sell_cache[mint] = cached

    async def _fetch_swap_tx(self, quote_data: dict) -> str:
        """Innovation #40: Pre-build a serialized swap transaction.

        Calls Jupiter /swap endpoint with the quote to get a ready-to-sign
        transaction. This saves ~200-300ms at execution time since the
        transaction is already built.

        Returns serialized transaction string or empty string on failure.
        """
        if not self._jup_session or not self._wallet_pubkey:
            return ""

        payload = {
            "quoteResponse": quote_data,
            "userPublicKey": self._wallet_pubkey,
            "wrapAndUnwrapSol": True,
            "dynamicComputeUnitLimit": True,
            "asLegacyTransaction": False,
        }

        try:
            async with self._jup_session.post(
                JUPITER_SWAP_URL,
                json=payload,
                timeout=aiohttp.ClientTimeout(total=5),
            ) as resp:
                if resp.status != 200:
                    return ""
                data = await resp.json()
                return data.get("swapTransaction", "")
        except Exception:
            return ""

    def set_wallet(self, wallet) -> None:
        """Inject wallet for signing pre-built transactions."""
        self._wallet = wallet

    def set_blockhash_manager(self, mgr) -> None:
        """Inject blockhash manager for fresh blockhashes."""
        self._blockhash_manager = mgr

    async def _pre_sign_exit_tx(self, mint: str, quote_data: dict) -> bytes:
        """Pre-sign an exit transaction template for a token.

        Returns raw signed transaction bytes, or b'' on failure.
        """
        if not hasattr(self, '_wallet') or not self._wallet:
            return b""
        if not hasattr(self, '_blockhash_manager') or not self._blockhash_manager:
            return b""
        if not quote_data:
            return b""
        # TX signing requires solders/solana-py SDK (not installed).
        # When available: get fresh blockhash → build TX from quote → sign with wallet.
        # Without the SDK, pre-signing is skipped and transactions are built at execution time.
        logger.debug("tx_precompute.signing_skipped", reason="solders_not_installed")
        return b""

    def get_pre_signed_tx(self, mint: str, direction: str = "sell") -> tuple[bytes, float] | None:
        """Get a pre-signed transaction if available and fresh.

        Returns (tx_bytes, signed_at) or None if not available/stale.
        """
        cache = self._sell_cache if direction == "sell" else self._buy_cache
        cached = cache.get(mint)
        if cached and cached.has_fresh_pre_signed:
            return (cached.pre_signed_tx, cached.pre_signed_at)
        return None

    def _prune_caches(self, max_size: int = 200) -> None:
        """Enforce max size on buy/sell caches to prevent OOM.

        Keeps the most recently fetched entries up to max_size.
        """
        for cache in (self._buy_cache, self._sell_cache):
            if len(cache) <= max_size:
                continue
            # Sort by fetched_at ascending (oldest first), prune excess
            by_age = sorted(cache.items(), key=lambda item: item[1].fetched_at)
            to_remove = len(cache) - max_size
            for mint, _ in by_age[:to_remove]:
                del cache[mint]

    def _prune_stale(self) -> None:
        """Remove quotes older than 30 seconds."""
        cutoff = time.monotonic() - 30.0
        for cache in (self._buy_cache, self._sell_cache):
            stale = [m for m, q in cache.items() if q.fetched_at < cutoff]
            for m in stale:
                del cache[m]

    @property
    def stats(self) -> dict:
        """Return cache statistics."""
        prebuilt_txs = sum(
            1 for c in list(self._buy_cache.values()) + list(self._sell_cache.values())
            if c.swap_tx
        )
        return {
            "buy_cached": len(self._buy_cache),
            "sell_cached": len(self._sell_cache),
            "cache_hits": self._cache_hits,
            "cache_misses": self._cache_misses,
            "refreshes": self._refreshes,
            "prebuilt_swap_txs": prebuilt_txs,
        }

    def get_stats(self) -> dict:
        """Return runtime stats for monitoring."""
        return {
            "warm_token_max": self._warm_token_max,
            "warm_token_ttl": self._warm_token_ttl,
        }
