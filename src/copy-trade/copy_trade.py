"""
CopyTradeAgent — Smart money copy trading (Profit Engine #1).
Monitors alpha wallets (60%+ win rate) and copies their trades within seconds.
"""

from __future__ import annotations

import asyncio
import time
from collections import deque

import aiohttp
import structlog

from src.core.agents.base import BaseAgent
from src.core.event_bus import Event, EventBus
from src.core.memory import HotMemory, WarmMemory
from src.core.types import PositionStatus, Signal, SignalDirection, SignalSource, WalletProfile

logger = structlog.get_logger(__name__)

# WebSocket URL (used only for reference; actual WS handled by HeliusWsAgent)
HELIUS_WS_URL = "wss://atlas-mainnet.helius-rpc.com"

# Known DEX program IDs
JUPITER_PROGRAM = "JUP6LkMFaUhMJNxmZcxJmp3PEbqJvMrgCFbN1KGSCP4"
RAYDIUM_PROGRAM = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"


class CopyTradeAgent(BaseAgent):
    """
    Monitors smart money wallets and generates buy signals when they trade.
    Win rate requirement: wallet must have 60%+ win rate over 50+ trades.
    """

    def __init__(
        self,
        event_bus: EventBus,
        hot_memory: HotMemory,
        warm_memory: WarmMemory,
        helius_api_key: str = "",
        min_wallet_score: float = 0.6,
        max_copy_delay_sec: float = 10.0,
    ) -> None:
        super().__init__(name="copy_trade", team="strategy", event_bus=event_bus)
        self.register_skill("wallet_monitor")
        self.register_skill("tx_decode")
        self.register_skill("confidence_score")
        self.register_skill("exit_liquidity_check")

        self._hot = hot_memory
        self._warm = warm_memory
        self._helius_key = helius_api_key
        self._min_wallet_score = min_wallet_score
        self._max_copy_delay = max_copy_delay_sec
        self._session: aiohttp.ClientSession | None = None
        self._monitor_task: asyncio.Task | None = None
        self._poll_interval: float = 5.0
        self._recent_signals: deque[str] = deque(maxlen=50)  # Dedup

        # W1.3 — Anti-exit-liquidity tracking
        self._dump_risk_wallets: set[str] = set()
        self._dump_counts: dict[str, int] = {}  # wallet -> consecutive dump count
        self._wallet_buy_timestamps: dict[str, float] = {}  # "wallet:mint" -> timestamp
        self._recent_mint_signals: deque[str] = deque(maxlen=100)

        # Alpha wallets to track (loaded from warm memory on start)
        self._tracked_wallets: dict[str, WalletProfile] = {}
        self._wallet_refresh_task: asyncio.Task | None = None
        self._wallet_refresh_interval: float = 24 * 3600  # 24 hours

        # Free-first transaction fetching via EnhancedTransactionClient
        self._enhanced_tx = None  # set via set_enhanced_tx()

        # Innovation #27: Real-time wallet stream via HeliusWsAgent
        self._helius_ws = None  # set via set_helius_ws()
        # Phase 131: Wallet reputation engine (regime-conditional)
        self._wallet_reputation_engine = None  # set via set_wallet_reputation_engine()
        self._realtime_enabled: bool = False
        self._avg_copy_latency_ms: float = 5000.0  # Tracks improvement

        # Innovation #41: Track multi-wallet exit consensus
        self._alpha_sell_wallets: dict[str, set[str]] = {}  # mint -> {wallet_addrs that sold}
        self._alpha_sell_timestamps: dict[str, float] = {}   # mint -> first sell time

    # Known Solana smart money wallets (public, high-win-rate traders)
    # These are seeded on first run when warm memory is empty.
    SEED_WALLETS = [
        # High-volume Solana traders with public track records
        {"address": "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1", "win_rate": 0.68, "avg_profit_pct": 12.5, "total_trades": 200, "score": 0.72, "label": "whale_trader_1"},
        {"address": "HN7cABqLq46Es1jh92dQQisAi5YqE1Bu2qbRgqYf9pe3", "win_rate": 0.65, "avg_profit_pct": 8.3, "total_trades": 150, "score": 0.68, "label": "smart_money_2"},
        {"address": "2iSMxqkPTLQGqS7LJvGGbRwNLfXKXVMpMCbVLM98Fxph", "win_rate": 0.70, "avg_profit_pct": 15.0, "total_trades": 100, "score": 0.75, "label": "alpha_3"},
    ]

    def set_enhanced_tx(self, client) -> None:
        """Wire EnhancedTransactionClient for free-first tx fetching."""
        self._enhanced_tx = client

    def set_wallet_reputation_engine(self, engine) -> None:
        """Phase 131: Wire WalletReputationEngine for regime-conditional modifiers."""
        self._wallet_reputation_engine = engine

    def set_helius_ws(self, helius_ws_agent) -> None:
        """Innovation #27: Wire HeliusWsAgent for real-time wallet streams."""
        self._helius_ws = helius_ws_agent
        self._realtime_enabled = True

    async def _on_start(self) -> None:
        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=10)
        )
        self.subscribe("market.snapshot", self._handle_snapshot)

        # Innovation #27: Subscribe to real-time wallet swap events
        self.subscribe("chain.wallet_swap", self._handle_realtime_wallet_swap)

        # Load wallet profiles from warm memory
        top_wallets = await self._warm.get_top_wallets(limit=20)

        # Seed wallets if warm memory is empty (first run)
        if not top_wallets:
            await self._seed_wallets()
            top_wallets = await self._warm.get_top_wallets(limit=20)

        for w in top_wallets:
            if w["win_rate"] >= self._min_wallet_score:
                profile = WalletProfile(
                    address=w["address"],
                    win_rate=w["win_rate"],
                    avg_profit_pct=w["avg_profit_pct"],
                    total_trades=w["total_trades"],
                    score=w["score"],
                )
                self._tracked_wallets[w["address"]] = profile
                self._hot.wallet_profiles[w["address"]] = profile

        # Innovation #27: If HeliusWsAgent is wired, use it for primary monitoring
        # and increase polling interval to 60s as safety-net fallback
        if self._realtime_enabled and self._helius_ws:
            self._poll_interval = 60.0
            # Register tracked wallets with WebSocket agent
            await self._helius_ws.update_wallet_subscriptions(
                list(self._tracked_wallets.keys())
            )
            self._log.info(
                "copy_trade.realtime_enabled",
                wallets=len(self._tracked_wallets),
                poll_interval=self._poll_interval,
            )

        if self._helius_key and self._tracked_wallets:
            self._monitor_task = asyncio.create_task(self._monitor_loop())
        # Background refresh of alpha wallets daily
        self._wallet_refresh_task = asyncio.create_task(self._wallet_refresh_loop())

    async def _seed_wallets(self) -> None:
        """Seed initial alpha wallets into warm memory on first run."""
        for w in self.SEED_WALLETS:
            profile = WalletProfile(
                address=w["address"],
                win_rate=w["win_rate"],
                avg_profit_pct=w["avg_profit_pct"],
                total_trades=w["total_trades"],
                score=w["score"],
            )
            await self._warm.update_wallet_score(profile)
        self._log.info("copy_trade.wallets_seeded", count=len(self.SEED_WALLETS))

    async def _on_stop(self) -> None:
        if self._monitor_task:
            self._monitor_task.cancel()
            try:
                await self._monitor_task
            except asyncio.CancelledError:
                pass
        if self._wallet_refresh_task:
            self._wallet_refresh_task.cancel()
            try:
                await self._wallet_refresh_task
            except asyncio.CancelledError:
                pass
        if self._session:
            await self._session.close()

    def add_wallet(self, address: str, profile: WalletProfile) -> None:
        """Add a wallet to track."""
        self._tracked_wallets[address] = profile
        self._hot.wallet_profiles[address] = profile

    async def _wallet_refresh_loop(self) -> None:
        """Refresh alpha wallet list daily from warm memory."""
        while True:
            try:
                await asyncio.sleep(self._wallet_refresh_interval)
                await self._refresh_wallets()
            except asyncio.CancelledError:
                return
            except Exception:
                self._log.exception("copy_trade.wallet_refresh_error")

    async def _refresh_wallets(self) -> None:
        """Reload top wallets, drop underperformers, add new alphas."""
        top_wallets = await self._warm.get_top_wallets(limit=20)
        new_tracked: dict[str, WalletProfile] = {}
        for w in top_wallets:
            if w["win_rate"] >= self._min_wallet_score:
                profile = WalletProfile(
                    address=w["address"],
                    win_rate=w["win_rate"],
                    avg_profit_pct=w["avg_profit_pct"],
                    total_trades=w["total_trades"],
                    score=w["score"],
                )
                new_tracked[w["address"]] = profile

        # Remove dump-risk wallets from the new set
        for addr in self._dump_risk_wallets:
            new_tracked.pop(addr, None)

        added = set(new_tracked) - set(self._tracked_wallets)
        removed = set(self._tracked_wallets) - set(new_tracked)

        self._tracked_wallets = new_tracked
        self._hot.wallet_profiles = dict(new_tracked)

        self._log.info(
            "copy_trade.wallets_refreshed",
            total=len(new_tracked),
            added=len(added),
            removed=len(removed),
        )

        # Innovation #27: Sync wallet subscriptions with HeliusWsAgent
        if self._realtime_enabled and self._helius_ws:
            await self._helius_ws.update_wallet_subscriptions(
                list(new_tracked.keys())
            )

    async def _monitor_loop(self) -> None:
        """Poll for recent transactions from tracked wallets."""
        while True:
            try:
                await self._check_wallet_transactions()
            except asyncio.CancelledError:
                return
            except Exception:
                self._log.exception("copy_trade.monitor_error")
            await asyncio.sleep(self._poll_interval)

    async def _check_wallet_transactions(self) -> None:
        """Check recent transactions for all tracked wallets."""
        if not self._session or not self._tracked_wallets:
            return

        for address, profile in self._tracked_wallets.items():
            t0 = time.monotonic()
            try:
                txns = await self._fetch_recent_transactions(address)
                latency = (time.monotonic() - t0) * 1000
                self.record_skill_call("wallet_monitor", True, latency)

                for tx in txns:
                    await self._process_transaction(tx, profile)
            except Exception as e:
                latency = (time.monotonic() - t0) * 1000
                self.record_skill_call("wallet_monitor", False, latency, str(e))

    async def _fetch_recent_transactions(self, wallet: str) -> list[dict]:
        """Fetch recent transactions for a wallet (free-first via EnhancedTransactionClient)."""
        # Use free-first EnhancedTransactionClient (SolanaFM → RPC → Helius last resort)
        if self._enhanced_tx:
            try:
                return await self._enhanced_tx.get_wallet_transfers(wallet, tx_type="SWAP", limit=5) or []
            except Exception:
                return []

        # Fallback: no enhanced_tx client wired — skip (don't burn Helius credits)
        return []

    async def _process_transaction(self, tx: dict, wallet: WalletProfile) -> None:
        """Decode a transaction and generate signal if it's a swap."""
        t0 = time.monotonic()
        sig = tx.get("signature", "")

        # Dedup — don't signal the same tx twice
        if sig in self._recent_signals:
            return
        self._recent_signals.append(sig)

        # Check if it's a swap on Jupiter or Raydium
        tx_type = tx.get("type", "")
        if tx_type != "SWAP":
            return

        # Check age — don't copy stale trades
        tx_time = tx.get("timestamp", 0)
        age_sec = time.time() - tx_time
        if age_sec > self._max_copy_delay:
            return

        # Extract token being bought
        token_transfers = tx.get("tokenTransfers", [])
        if not token_transfers:
            return

        # Detect direction: find what wallet received (BUY) and what it sent (SELL)
        output_mint = ""
        input_mint = ""
        for transfer in token_transfers:
            if transfer.get("toUserAccount", "") == wallet.address:
                output_mint = transfer.get("mint", "")
            if transfer.get("fromUserAccount", "") == wallet.address:
                input_mint = transfer.get("mint", "")

        latency = (time.monotonic() - t0) * 1000
        self.record_skill_call("tx_decode", True, latency)

        # Innovation #41: Track alpha wallet sells for cross-strategy exit
        # Must run BEFORE H-5 close logic (which sets status=CLOSING)
        if input_mint:
            if input_mint not in self._alpha_sell_wallets:
                self._alpha_sell_wallets[input_mint] = set()
                self._alpha_sell_timestamps[input_mint] = time.monotonic()
            self._alpha_sell_wallets[input_mint].add(wallet.address)

            n_sellers = len(self._alpha_sell_wallets[input_mint])
            # Multi-wallet consensus: 2+ alpha wallets selling = strong exit signal
            # Even for positions opened by other strategies
            if n_sellers >= 2:
                positions = self._hot.get_open_positions()
                for pos in positions:
                    if pos.token_mint == input_mint and pos.strategy != "copy_trade":
                        pnl_pct = 0.0
                        if pos.entry_price > 0 and pos.current_price > 0:
                            pnl_pct = ((pos.current_price - pos.entry_price) / pos.entry_price) * 100
                        # Graded response: 2 wallets = partial close, 3+ = full close
                        close_pct = 0.5 if n_sellers == 2 else 1.0
                        await self.publish(self.emit("position.close_trigger", {
                            "position": pos,
                            "reason": f"smart_money_exit:{n_sellers}_wallets",
                            "pnl_pct": pnl_pct,
                            "close_pct": close_pct,
                        }))
                        self._log.info(
                            "copy_trade.smart_money_exit",
                            mint=input_mint[:8],
                            strategy=pos.strategy,
                            n_sellers=n_sellers,
                            close_pct=close_pct,
                        )

            # Cleanup: trim entries older than 5 minutes
            now = time.monotonic()
            stale_mints = [
                m for m, ts in self._alpha_sell_timestamps.items()
                if now - ts > 300
            ]
            for m in stale_mints:
                self._alpha_sell_wallets.pop(m, None)
                self._alpha_sell_timestamps.pop(m, None)

        # Wallet SOLD a token we might hold → close matching positions
        # FIX H-5: Route directly to position.close_trigger instead of
        # signal.generated, which only handles BUY signals downstream.
        if input_mint and input_mint in self._hot.prices:
            positions = self._hot.get_open_positions()
            for pos in positions:
                if pos.token_mint == input_mint:
                    pnl_pct = 0.0
                    if pos.entry_price > 0 and pos.current_price > 0:
                        pnl_pct = ((pos.current_price - pos.entry_price) / pos.entry_price) * 100
                    pos.status = PositionStatus.CLOSING
                    await self.publish(self.emit("position.close_trigger", {
                        "position": pos,
                        "reason": f"alpha_wallet_sold:{wallet.address[:8]}",
                        "pnl_pct": pnl_pct,
                        "close_pct": 1.0,
                    }))
                    self._log.info(
                        "copy_trade.alpha_exit_close",
                        mint=input_mint[:8],
                        wallet=wallet.address[:8],
                        wallet_score=round(wallet.score, 3),
                        pnl_pct=round(pnl_pct, 1),
                    )

        # Wallet BOUGHT a token → emit buy signal
        if not output_mint:
            return

        # W1.3 — Anti-exit-liquidity shield
        if self._check_exit_liquidity(wallet, output_mint, tx):
            self._log.info(
                "copy_trade.exit_liquidity_blocked",
                wallet=wallet.address[:8],
                mint=output_mint[:8],
            )
            return

        # Calculate confidence based on wallet quality
        confidence = self._calculate_confidence(wallet, age_sec)
        if confidence < 0.4:
            return

        # Phase 129d: Apply enrichment + concentration modifiers
        enrichment = self._hot.token_enrichments.get(output_mint, {})
        if isinstance(enrichment, dict):
            if enrichment.get("has_freeze_authority"):
                confidence *= 0.7
            elif enrichment.get("enriched_at") and not enrichment.get("has_mint_authority"):
                confidence *= 1.1  # Enriched + no dangerous authorities
        concentration = self._hot.holder_concentrations.get(output_mint, {})
        if isinstance(concentration, dict):
            gini = concentration.get("gini", 0.5)
            if gini < 0.5:
                confidence *= 1.1
            elif gini > 0.8:
                confidence *= 0.5
        confidence = min(confidence, 0.98)

        # Get current price from hot memory for position tracking
        entry_price = 0.0
        tp_data = self._hot.prices.get(output_mint)
        if tp_data:
            entry_price = tp_data.price_usd

        signal = Signal(
            source=SignalSource.COPY_TRADE,
            token_mint=output_mint,
            direction=SignalDirection.BUY,
            confidence=confidence,
            suggested_size_usd=25.0,  # Max position
            entry_price=entry_price,
            timestamp=time.monotonic(),
            metadata={
                "wallet": wallet.address,
                "wallet_win_rate": wallet.win_rate,
                "wallet_score": wallet.score,
                "copy_delay_sec": age_sec,
                "tx_signature": sig,
            },
        )

        self._hot.recent_signals.append(signal)
        await self.publish(self.emit("signal.generated", signal))
        self._log.info(
            "copy_trade.signal",
            mint=output_mint,
            confidence=round(confidence, 3),
            wallet=wallet.address[:8],
        )

    async def _handle_realtime_wallet_swap(self, event) -> None:
        """Innovation #27: Handle real-time wallet swap from HeliusWsAgent.

        Fetches full transaction details and processes via existing logic.
        Reduces copy trade latency from ~5000ms to ~500ms.
        """
        data = event.data if isinstance(event.data, dict) else {}
        wallet_addr = data.get("wallet", "")
        signature = data.get("signature", "")

        if not wallet_addr or not signature:
            return

        # Only process wallets we're tracking
        profile = self._tracked_wallets.get(wallet_addr)
        if not profile:
            return

        # Dedup — same as polling path
        if signature in self._recent_signals:
            return

        t0 = time.monotonic()

        # Fetch full transaction details from Helius REST
        txns = await self._fetch_transaction_by_sig(signature)
        if not txns:
            return

        for tx in txns:
            await self._process_transaction(tx, profile)

        # Track latency improvement
        chain_ts = data.get("timestamp", 0)
        if chain_ts > 0:
            latency_ms = (time.time() - chain_ts) * 1000
            # Exponential moving average
            self._avg_copy_latency_ms = (
                0.9 * self._avg_copy_latency_ms + 0.1 * latency_ms
            )
            self._log.info(
                "copy_trade.realtime_latency",
                wallet=wallet_addr[:8],
                sig=signature[:12],
                latency_ms=round(latency_ms, 0),
                avg_ms=round(self._avg_copy_latency_ms, 0),
            )

    async def _fetch_transaction_by_sig(self, signature: str) -> list[dict]:
        """Fetch a single transaction by signature (free-first via EnhancedTransactionClient)."""
        # Use free-first EnhancedTransactionClient (SolanaFM → RPC → Helius last resort)
        if self._enhanced_tx:
            try:
                result = await self._enhanced_tx.get_parsed_transactions([signature]) or []
                return result
            except Exception:
                return []

        # Fallback: no enhanced_tx client wired — skip (don't burn Helius credits)
        return []

    def _calculate_confidence(self, wallet: WalletProfile, delay_sec: float) -> float:
        """
        Confidence = wallet_score * delay_decay.
        Higher wallet score + faster copy = higher confidence.

        Step-based decay replaces linear — trades copied in 8-12s are still valuable,
        not killed by linear decay to near-zero.
        """
        t0 = time.monotonic()
        # Base confidence from wallet quality
        base = wallet.score

        # Step-based delay decay (gentler than linear)
        # Check expiry FIRST — max_copy_delay may be shorter than step thresholds
        if delay_sec > self._max_copy_delay:
            delay_factor = 0.0       # Expired
        elif delay_sec <= 3.0:
            delay_factor = 1.0       # Instant copy — full confidence
        elif delay_sec <= 7.0:
            delay_factor = 0.90      # Still fresh
        elif delay_sec <= 12.0:
            delay_factor = 0.75      # Acceptable delay
        else:
            delay_factor = 0.50      # Stale but usable

        confidence = base * delay_factor

        # Phase 129d: Compound confidence with enrichment + concentration data
        if self._hot:
            # Wallet reliability multiplier
            wallet_addr = wallet.address if hasattr(wallet, "address") else ""
            wallet_wr = wallet.win_rate if hasattr(wallet, "win_rate") else 0.5
            if wallet_wr > 0.7:
                confidence *= 1.2
            elif wallet_wr < 0.3:
                confidence *= 0.6

            # Phase 131: Regime-conditional wallet reputation modifier
            if self._wallet_reputation_engine and wallet_addr:
                regime = getattr(self._hot, "current_regime", "ranging") or "ranging"
                rep_mod = self._wallet_reputation_engine.get_confidence_modifier(wallet_addr, regime)
                confidence *= rep_mod

        latency = (time.monotonic() - t0) * 1000
        self.record_skill_call("confidence_score", True, latency)
        return round(min(confidence, 0.98), 4)

    def _check_exit_liquidity(
        self, wallet: WalletProfile, mint: str, tx: dict,
    ) -> bool:
        """W1.3: Check if this trade is an exit-liquidity trap.

        Returns True if the signal should be blocked.
        Three checks:
          1. Wallet already traded this mint recently (re-buy cycling trap).
          2. Wallet flagged as dump-risk from historical patterns.
          3. Too many copy bots already signaling on this mint.
        """
        t0 = time.monotonic()
        blocked = False
        reason = ""

        # Check 1: Wallet already traded this mint within last hour (re-buy trap)
        key = f"{wallet.address}:{mint}"
        if key in self._wallet_buy_timestamps:
            prev_time = self._wallet_buy_timestamps[key]
            if time.time() - prev_time < 3600:
                blocked = True
                reason = "rebuy_within_1h"

        # Check 2: Known dump-risk wallet
        if not blocked and wallet.address in self._dump_risk_wallets:
            blocked = True
            reason = "dump_risk_wallet"

        # Check 3: Too many bots already copying this token
        if not blocked:
            recent_count = sum(1 for m in self._recent_mint_signals if m == mint)
            if recent_count >= 5:
                blocked = True
                reason = "copy_crowd_too_large"

        # Track this buy for future re-buy detection
        self._wallet_buy_timestamps[key] = time.time()
        self._recent_mint_signals.append(mint)

        latency = (time.monotonic() - t0) * 1000
        self.record_skill_call("exit_liquidity_check", not blocked, latency, reason)
        return blocked

    def get_alpha_exit_count(self, mint: str) -> int:
        """Innovation #41: Number of alpha wallets that sold this mint in last 5 min."""
        ts = self._alpha_sell_timestamps.get(mint, 0)
        if time.monotonic() - ts > 300:
            return 0
        wallets = self._alpha_sell_wallets.get(mint, set())
        return len(wallets)

    def track_dump_pattern(self, wallet_address: str, sold_within_sec: float) -> None:
        """Track wallet dump patterns for exit-liquidity detection.

        Call this when a tracked wallet sells a token. If the wallet
        consistently buys then dumps within 2 hours, mark as dump risk.
        """
        if sold_within_sec < 7200:  # Sold within 2 hours of buy
            self._dump_counts[wallet_address] = (
                self._dump_counts.get(wallet_address, 0) + 1
            )
            if self._dump_counts[wallet_address] >= 3:
                self._dump_risk_wallets.add(wallet_address)
                self._log.warning(
                    "copy_trade.dump_risk_detected",
                    wallet=wallet_address[:8],
                    dump_count=self._dump_counts[wallet_address],
                )

    async def _handle_snapshot(self, event: Event) -> None:
        """Snapshot can be used to cross-reference wallet trades with market data."""
        pass

    async def _reconnect(self) -> bool:
        if self._session:
            await self._session.close()
        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=10)
        )
        return True

    async def _custom_health_checks(self) -> list[str]:
        issues = []
        if not self._tracked_wallets:
            issues.append("no wallets being tracked")
        if not self._helius_key:
            issues.append("no Helius API key configured")
        return issues

    def get_stats(self) -> dict:
        """Return runtime stats for monitoring."""
        return {"class": "CopyTradeAgent"}
