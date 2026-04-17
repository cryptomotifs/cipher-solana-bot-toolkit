"""
PriorityFeeOptimizer — Innovation #28: Dynamic priority fee selection.

Calls Helius getPriorityFeeEstimate API before each trade to get optimal
priority fees based on current network congestion. Caches results for 3
seconds to avoid redundant API calls.

Replaces static MAX_PRIORITY_FEE_LAMPORTS with dynamic, data-driven fees
that save money during quiet periods and outbid competitors during congestion.
"""

from __future__ import annotations

import asyncio
import time

import aiohttp
import structlog

logger = structlog.get_logger(__name__)

# Fee percentiles returned by Helius
PERCENTILE_LOW = "low"          # ~25th percentile
PERCENTILE_MEDIUM = "medium"    # ~50th percentile
PERCENTILE_HIGH = "high"        # ~75th percentile
PERCENTILE_VERY_HIGH = "veryHigh"  # ~95th percentile

# Hard cap — never pay more than 0.001 SOL regardless of API suggestion
MAX_FEE_LAMPORTS = 1_000_000

# Cache TTL — re-fetch after this many seconds
CACHE_TTL_SEC = 3.0

# Default fees when API is unavailable (conservative)
# Phase 35: Solana RPC-based fee estimation
SOLANA_FEE_CACHE_TTL = 10.0
FEE_PERCENTILE_TARGETS = {"low": 25, "medium": 50, "high": 75, "veryHigh": 95}

DEFAULT_FEES = {
    PERCENTILE_LOW: 1_000,
    PERCENTILE_MEDIUM: 10_000,
    PERCENTILE_HIGH: 50_000,
    PERCENTILE_VERY_HIGH: 100_000,
}

# Urgency → Helius percentile mapping
URGENCY_MAP = {
    "low": PERCENTILE_LOW,
    "medium": PERCENTILE_MEDIUM,
    "high": PERCENTILE_HIGH,
    "veryHigh": PERCENTILE_VERY_HIGH,
}


class PriorityFeeOptimizer:
    """Fetches and caches optimal priority fees from Helius.

    Usage:
        optimizer = PriorityFeeOptimizer(helius_api_key="xxx")
        optimizer.set_session(session)
        fee = await optimizer.get_optimal_fee(urgency="high")
    """

    def __init__(
        self,
        helius_api_key: str = "",
        cache_ttl_sec: float = CACHE_TTL_SEC,
        max_fee_lamports: int = MAX_FEE_LAMPORTS,
    ) -> None:
        self._api_key = helius_api_key
        self._rpc_url = (
            f"https://mainnet.helius-rpc.com/?api-key={helius_api_key}"
            if helius_api_key else ""
        )
        self._cache_ttl = cache_ttl_sec
        self._max_fee = max_fee_lamports

        self._session: aiohttp.ClientSession | None = None
        self._cached_fees: dict[str, int] = dict(DEFAULT_FEES)
        self._cache_time: float = 0.0
        self._fetch_lock = asyncio.Lock()

        # Multi-provider: injected by main.py
        self._rpc_manager = None

        # Stats
        self._api_calls: int = 0
        self._cache_hits: int = 0
        self._api_errors: int = 0

    def set_session(self, session: aiohttp.ClientSession) -> None:
        """Inject aiohttp session (shared with engine)."""
        self._session = session

    def set_rpc_manager(self, rpc_manager) -> None:
        """Inject RPCManagerAgent for multi-provider fallback."""
        self._rpc_manager = rpc_manager

    async def get_optimal_fee(
        self,
        urgency: str = "medium",
        account_keys: list[str] | None = None,
    ) -> int:
        """Get optimal priority fee in lamports for the given urgency level.

        Args:
            urgency: "low", "medium", "high", or "veryHigh"
            account_keys: Optional list of account addresses for fee estimation.
                         If provided, Helius returns fees specific to those accounts.

        Returns:
            Priority fee in lamports, capped at max_fee_lamports.
        """
        # Refresh cache if stale
        if time.monotonic() - self._cache_time > self._cache_ttl:
            await self._refresh_fees(account_keys)

        percentile = URGENCY_MAP.get(urgency, PERCENTILE_MEDIUM)
        fee = self._cached_fees.get(percentile, DEFAULT_FEES.get(percentile, 10_000))

        # Apply hard cap
        fee = min(fee, self._max_fee)
        return fee

    async def get_fee_for_signal(
        self,
        signal_source: str,
        account_keys: list[str] | None = None,
    ) -> int:
        """Map signal source to urgency and return optimal fee.

        Args:
            signal_source: "copy_trade", "sniper", "volume_spike", "mean_reversion", "arb"
            account_keys: Optional accounts for more accurate estimation.

        Returns:
            Priority fee in lamports.
        """
        urgency_map = {
            "copy_trade": "veryHigh",    # Time-critical — must land fast
            "sniper": "veryHigh",        # Graduation timing is critical
            "arb": "veryHigh",           # Arb windows close in seconds
            "volume_spike": "high",      # Momentum — moderate urgency
            "mean_reversion": "medium",  # Less time-sensitive
        }
        urgency = urgency_map.get(signal_source, "medium")
        return await self.get_optimal_fee(urgency, account_keys)

    async def _refresh_fees(
        self, account_keys: list[str] | None = None,
    ) -> None:
        """Fetch fresh priority fees with multi-provider fallback.

        1. Try Helius getPriorityFeeEstimate (best, proprietary)
        2. Fallback: standard getRecentPrioritizationFees via any RPC
        3. If all fail: keep cached defaults
        """
        if not self._session:
            self._cache_time = time.monotonic()
            return

        async with self._fetch_lock:
            if time.monotonic() - self._cache_time < self._cache_ttl:
                self._cache_hits += 1
                return

            # Try FREE standard RPC first (getRecentPrioritizationFees)
            fallback_url = ""
            if self._rpc_manager:
                fallback_url = self._rpc_manager.get_next_read_url()

            if fallback_url:
                success = await self._try_standard_fee_estimate(fallback_url)
                if success:
                    return

            # Fallback: Helius proprietary getPriorityFeeEstimate (paid — last resort)
            if self._rpc_url:
                success = await self._try_helius_fee_estimate(account_keys)
                if success:
                    return

            # All providers failed — keep defaults, don't retry immediately
            self._cache_time = time.monotonic()

    async def _try_helius_fee_estimate(
        self, account_keys: list[str] | None = None,
    ) -> bool:
        """Try Helius getPriorityFeeEstimate. Returns True on success."""
        if not self._session or not self._rpc_url:
            return False

        payload = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getPriorityFeeEstimate",
            "params": [{}],
        }
        if account_keys:
            payload["params"] = [{"accountKeys": account_keys}]

        try:
            async with self._session.post(
                self._rpc_url,
                json=payload,
                timeout=aiohttp.ClientTimeout(total=5),
            ) as resp:
                self._api_calls += 1
                if resp.status != 200:
                    self._api_errors += 1
                    return False

                data = await resp.json()

                # Check for RPC error (method not found on non-Helius providers)
                if "error" in data:
                    return False

                result = data.get("result", {})
                priority_levels = result.get("priorityFeeLevels", result)

                if priority_levels and isinstance(priority_levels, dict):
                    for level in (
                        PERCENTILE_LOW, PERCENTILE_MEDIUM,
                        PERCENTILE_HIGH, PERCENTILE_VERY_HIGH,
                    ):
                        if level in priority_levels:
                            self._cached_fees[level] = int(priority_levels[level])

                    self._cache_time = time.monotonic()
                    logger.debug(
                        "priority_fee.refreshed",
                        source="helius",
                        low=self._cached_fees.get(PERCENTILE_LOW),
                        medium=self._cached_fees.get(PERCENTILE_MEDIUM),
                        high=self._cached_fees.get(PERCENTILE_HIGH),
                        very_high=self._cached_fees.get(PERCENTILE_VERY_HIGH),
                    )
                    return True
                return False

        except Exception as e:
            self._api_errors += 1
            logger.warning("priority_fee.helius_error", error=str(e))
            return False

    async def _try_standard_fee_estimate(self, rpc_url: str) -> bool:
        """Fallback: use standard getRecentPrioritizationFees.

        This RPC method returns recent prioritization fees per slot.
        We compute percentiles from the raw data.
        """
        if not self._session:
            return False

        payload = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getRecentPrioritizationFees",
            "params": [],
        }

        try:
            async with self._session.post(
                rpc_url,
                json=payload,
                timeout=aiohttp.ClientTimeout(total=5),
            ) as resp:
                self._api_calls += 1
                if resp.status != 200:
                    self._api_errors += 1
                    return False

                data = await resp.json()

                if "error" in data:
                    return False

                fees_list = data.get("result", [])
                if not fees_list:
                    return False

                # Extract prioritization fees and sort
                raw_fees = sorted(
                    [int(f.get("prioritizationFee", 0)) for f in fees_list if f.get("prioritizationFee", 0) > 0]
                )

                if not raw_fees:
                    # All zero = quiet network
                    self._cached_fees[PERCENTILE_LOW] = 100
                    self._cached_fees[PERCENTILE_MEDIUM] = 1_000
                    self._cached_fees[PERCENTILE_HIGH] = 10_000
                    self._cached_fees[PERCENTILE_VERY_HIGH] = 50_000
                    self._cache_time = time.monotonic()
                    return True

                # Compute percentiles
                n = len(raw_fees)
                self._cached_fees[PERCENTILE_LOW] = raw_fees[int(n * 0.25)] if n > 4 else raw_fees[0]
                self._cached_fees[PERCENTILE_MEDIUM] = raw_fees[int(n * 0.50)] if n > 2 else raw_fees[0]
                self._cached_fees[PERCENTILE_HIGH] = raw_fees[int(n * 0.75)] if n > 4 else raw_fees[-1]
                self._cached_fees[PERCENTILE_VERY_HIGH] = raw_fees[int(n * 0.95)] if n > 20 else raw_fees[-1]

                self._cache_time = time.monotonic()
                logger.debug(
                    "priority_fee.refreshed",
                    source="standard_rpc",
                    low=self._cached_fees.get(PERCENTILE_LOW),
                    medium=self._cached_fees.get(PERCENTILE_MEDIUM),
                    high=self._cached_fees.get(PERCENTILE_HIGH),
                    very_high=self._cached_fees.get(PERCENTILE_VERY_HIGH),
                )
                return True

        except Exception as e:
            self._api_errors += 1
            logger.warning("priority_fee.standard_rpc_error", error=str(e))
            return False

    async def _fetch_solana_recent_fees(self) -> list[int]:
        """Fetch recent prioritization fees from Solana RPC (free).

        Uses getRecentPrioritizationFees RPC method.
        Returns list of fee values in micro-lamports.
        """
        if not hasattr(self, '_rpc_url') or not self._rpc_url:
            return []
        try:
            import aiohttp
            payload = {
                "jsonrpc": "2.0", "id": 1,
                "method": "getRecentPrioritizationFees",
                "params": [],
            }
            async with aiohttp.ClientSession() as session:
                async with session.post(
                    self._rpc_url, json=payload,
                    timeout=aiohttp.ClientTimeout(total=5),
                ) as resp:
                    if resp.status == 200:
                        data = await resp.json()
                        result = data.get("result", [])
                        return [int(entry.get("prioritizationFee", 0)) for entry in result if entry.get("prioritizationFee", 0) > 0]
        except Exception as exc:
            logger.warning("priorityfeeoptimizer.result_failed", error=str(exc))
        return []

    @staticmethod
    def _compute_percentile_fee(fees: list[int], percentile: int) -> int:
        """Compute percentile from sorted fee list."""
        if not fees:
            return 0
        sorted_fees = sorted(fees)
        idx = int(len(sorted_fees) * percentile / 100)
        idx = min(idx, len(sorted_fees) - 1)
        return sorted_fees[idx]

    async def get_optimal_fee_v2(self, urgency: str = "medium") -> int:
        """Enhanced fee optimization: Helius -> Solana RPC -> defaults.

        Args:
            urgency: "low", "medium", "high", or "veryHigh"

        Returns:
            Optimal priority fee in micro-lamports.
        """
        # Try existing Helius-based fee first
        try:
            existing = await self.get_optimal_fee(urgency=urgency)
            if existing and existing > 0:
                return existing
        except Exception as exc:
            logger.warning("priorityfeeoptimizer.get_optimal_fee_v2.get_optimal_fee_failed", error=str(exc))
        fees = await self._fetch_solana_recent_fees()
        if fees:
            percentile = FEE_PERCENTILE_TARGETS.get(urgency, 50)
            return self._compute_percentile_fee(fees, percentile)

        # Final fallback: conservative defaults
        defaults = {"low": 1000, "medium": 5000, "high": 20000, "veryHigh": 100000}
        return defaults.get(urgency, 5000)

    def set_rpc_url(self, url: str) -> None:
        """Set RPC URL for Solana fee queries."""
        self._rpc_url = url

    @property
    def stats(self) -> dict:
        """Return fee optimizer statistics."""
        return {
            "api_calls": self._api_calls,
            "cache_hits": self._cache_hits,
            "api_errors": self._api_errors,
            "current_fees": dict(self._cached_fees),
        }

    def get_stats(self) -> dict:
        """Return runtime stats for monitoring."""
        return {"class": "PriorityFeeOptimizer"}
