"""
Phase 169: Jito Bundle Coordinator (#627).

Coordinates all Jito bundle operations: adaptive tip calculation, multi-trade
bundling, bundle inclusion tracking, regional endpoint racing, and backrun
detection. Ties together BundleBuilder, InclusionProbability, MEVShield,
LeaderSchedule, and JitoTipMonitor into a unified bundle strategy.

All transactions should go through Jito bundles for MEV protection.
"""

from __future__ import annotations

import time
from collections import defaultdict, deque

from src.core.agents.base import BaseAgent
from src.core.event_bus import Event

# ── Constants ──────────────────────────────────────────────────
# Tip calculation
BASE_TIP_LAMPORTS = 1_000          # 0.001 SOL base
MIN_TIP_LAMPORTS = 500             # 0.0005 SOL floor
MAX_TIP_LAMPORTS = 50_000          # 0.05 SOL ceiling
TIP_MULTIPLIER_HIGH_MEV = 2.0      # Double tip when MEV risk high
TIP_MULTIPLIER_CONGESTED = 1.5     # 1.5x when network congested
TIP_MULTIPLIER_JITO_LEADER = 0.8   # 0.8x when leader is Jito-connected

# Bundle composition
MAX_TXS_PER_BUNDLE = 5            # Jito hard limit
BUNDLE_WINDOW_SEC = 2.0           # Collect trades for bundling
MIN_BUNDLE_PROFIT_LAMPORTS = 500  # Min expected profit to justify bundling

# Inclusion tracking
INCLUSION_HISTORY_SIZE = 200      # Track last N bundle submissions
INCLUSION_TIMEOUT_SEC = 30.0      # Max wait for bundle landing
TIP_HISTORY_SIZE = 500            # Track tip outcomes

# Regional endpoints
JITO_REGIONS = ["mainnet", "amsterdam", "ny"]
REGION_RACE_TIMEOUT_SEC = 5.0     # Max wait for first region response

# Backrun
BACKRUN_MIN_PROFIT_USD = 0.10     # Min profit to attempt backrun
BACKRUN_OPPORTUNITY_WINDOW = 100  # Track last N backrun opportunities

# Report
REPORT_INTERVAL_SEC = 60.0


class JitoBundleCoordinator(BaseAgent):
    """Coordinates all Jito bundle operations for MEV protection."""

    def __init__(self, event_bus, hot_memory):
        super().__init__(
            name="jito_bundle_coordinator",
            team="execution",
            event_bus=event_bus,
        )
        self._hot = hot_memory

        # Tip tracking
        self._tip_history: deque = deque(maxlen=TIP_HISTORY_SIZE)
        self._tip_outcomes: dict[str, dict] = {}  # bundle_id -> outcome
        self._total_tips_paid = 0
        self._total_tips_saved = 0  # vs naive fixed tip

        # Bundle composition queue
        self._pending_trades: deque = deque(maxlen=MAX_TXS_PER_BUNDLE * 5)
        self._bundles_submitted = 0
        self._bundles_landed = 0

        # Inclusion tracking
        self._inclusion_history: deque = deque(maxlen=INCLUSION_HISTORY_SIZE)
        self._pending_confirmations: dict[str, dict] = {}

        # Regional endpoint stats
        self._region_stats: dict[str, dict] = {
            region: {"submitted": 0, "landed": 0, "avg_latency_ms": 0.0,
                     "latency_sum": 0.0}
            for region in JITO_REGIONS
        }

        # Backrun detection
        self._backrun_opportunities: deque = deque(maxlen=BACKRUN_OPPORTUNITY_WINDOW)
        self._backruns_attempted = 0
        self._backruns_profitable = 0

        # Adaptive state
        self._congestion_level = 0.0  # 0-1
        self._current_tip_floor = BASE_TIP_LAMPORTS
        self._jito_leader_active = False

        # Reporting
        self._last_report_time = 0.0
        self._reports = 0

    # ── Main dispatch ─────────────────────────────────────────

    async def analyze(self, event):
        etype = event.event_type
        data = event.data if isinstance(event.data, dict) else {}

        if etype == "execution.confirmed":
            await self._handle_execution_confirmed(data)
        elif etype == "execution.failed":
            await self._handle_execution_failed(data)
        elif etype == "jito.tip_received":
            self._handle_tip_received(data)
        elif etype == "intelligence.leader_info":
            self._handle_leader_info(data)
        elif etype == "market.price_update":
            self._handle_price_update(data)
        elif etype == "risk.decision":
            await self._handle_risk_decision(data)
        elif etype == "chain.wallet_swap":
            await self._handle_wallet_swap(data)

        await self._maybe_report()

    # ── Adaptive Tip Calculation ──────────────────────────────

    def calculate_optimal_tip(self, mev_risk_bps: float = 0.0,
                              trade_size_usd: float = 0.0,
                              urgency: str = "normal") -> int:
        """Calculate optimal Jito tip based on conditions.

        Returns tip in lamports.
        """
        base = self._current_tip_floor

        # Urgency multiplier
        urgency_mult = {
            "low": 0.5,
            "normal": 1.0,
            "high": 1.5,
            "critical": 3.0,
            "emergency": 5.0,
        }.get(urgency, 1.0)

        # MEV risk multiplier
        mev_mult = 1.0
        if mev_risk_bps >= 100:
            mev_mult = TIP_MULTIPLIER_HIGH_MEV
        elif mev_risk_bps >= 50:
            mev_mult = 1.5

        # Congestion multiplier
        congestion_mult = 1.0
        if self._congestion_level > 0.7:
            congestion_mult = TIP_MULTIPLIER_CONGESTED
        elif self._congestion_level > 0.5:
            congestion_mult = 1.2

        # Leader discount
        leader_mult = TIP_MULTIPLIER_JITO_LEADER if self._jito_leader_active else 1.0

        # Trade size factor (larger trades justify higher tips)
        size_mult = 1.0
        if trade_size_usd > 100:
            size_mult = 1.3
        elif trade_size_usd > 50:
            size_mult = 1.1

        tip = int(base * urgency_mult * mev_mult * congestion_mult
                  * leader_mult * size_mult)

        # Clamp
        tip = max(MIN_TIP_LAMPORTS, min(tip, MAX_TIP_LAMPORTS))

        # Track savings vs naive
        naive_tip = BASE_TIP_LAMPORTS
        if tip < naive_tip:
            self._total_tips_saved += (naive_tip - tip)

        return tip

    # ── Bundle Composition ────────────────────────────────────

    def should_bundle(self, trade_data: dict) -> bool:
        """Determine if a trade should be bundled vs sent directly."""
        # Always bundle sells (MEV protection)
        if trade_data.get("direction") == "sell":
            return True
        # Bundle high-value trades
        if trade_data.get("usd_value", 0) > 10:
            return True
        # Bundle when congestion is high
        if self._congestion_level > 0.5:
            return True
        # Bundle when MEV risk is elevated
        if trade_data.get("mev_risk_bps", 0) > 30:
            return True
        return False

    def get_best_region(self) -> str:
        """Get the best Jito region based on recent inclusion rates."""
        best_region = "mainnet"
        best_rate = -1.0

        for region, stats in self._region_stats.items():
            if stats["submitted"] == 0:
                continue
            rate = stats["landed"] / stats["submitted"]
            if rate > best_rate:
                best_rate = rate
                best_region = region

        return best_region

    def record_bundle_submission(self, bundle_id: str, region: str,
                                 tip_lamports: int, tx_count: int) -> None:
        """Record a bundle submission for tracking."""
        now = time.time()
        self._bundles_submitted += 1
        self._total_tips_paid += tip_lamports

        record = {
            "bundle_id": bundle_id,
            "region": region,
            "tip_lamports": tip_lamports,
            "tx_count": tx_count,
            "submitted_at": now,
            "landed": False,
            "landed_at": 0.0,
        }
        self._inclusion_history.append(record)
        self._pending_confirmations[bundle_id] = record

        self._tip_history.append({
            "tip": tip_lamports,
            "ts": now,
            "included": None,  # Unknown yet
        })

        if region in self._region_stats:
            self._region_stats[region]["submitted"] += 1

    def record_bundle_landed(self, bundle_id: str) -> None:
        """Record that a bundle successfully landed on-chain."""
        record = self._pending_confirmations.pop(bundle_id, None)
        if record:
            record["landed"] = True
            record["landed_at"] = time.time()
            self._bundles_landed += 1

            region = record.get("region", "mainnet")
            if region in self._region_stats:
                self._region_stats[region]["landed"] += 1

    # ── Backrun Detection ─────────────────────────────────────

    async def _handle_wallet_swap(self, data: dict) -> None:
        """Detect potential backrun opportunities from large swaps."""
        amount_usd = data.get("amount_usd", 0.0)
        mint = data.get("mint", "")
        action = data.get("action", "")

        if amount_usd < 50 or not mint:
            return

        # Large swap detected — potential backrun opportunity
        price_impact_pct = data.get("price_impact_pct", 0.0)
        if price_impact_pct <= 0:
            return

        # Estimate backrun profit
        estimated_profit_usd = amount_usd * abs(price_impact_pct) / 100 * 0.3

        if estimated_profit_usd >= BACKRUN_MIN_PROFIT_USD:
            opportunity = {
                "mint": mint,
                "trigger_action": action,
                "trigger_amount_usd": amount_usd,
                "price_impact_pct": price_impact_pct,
                "estimated_profit_usd": estimated_profit_usd,
                "detected_at": time.time(),
            }
            self._backrun_opportunities.append(opportunity)

            await self._event_bus.publish(Event(
                event_type="jito.backrun_opportunity",
                source=self.name,
                data=opportunity,
            ))

    # ── Event Handlers ────────────────────────────────────────

    async def _handle_execution_confirmed(self, data: dict) -> None:
        """Track bundle inclusion from execution confirmations."""
        bundle_id = data.get("bundle_id", "")
        if bundle_id and bundle_id in self._pending_confirmations:
            self.record_bundle_landed(bundle_id)

        # Update congestion estimate from confirmation latency
        latency_ms = data.get("latency_ms", 0)
        if latency_ms > 0:
            # High latency = congestion
            self._congestion_level = min(1.0, latency_ms / 5000.0)

    async def _handle_execution_failed(self, data: dict) -> None:
        """Track bundle failures."""
        bundle_id = data.get("bundle_id", "")
        if bundle_id and bundle_id in self._pending_confirmations:
            record = self._pending_confirmations.pop(bundle_id, None)
            if record:
                record["landed"] = False

    async def _handle_risk_decision(self, data: dict) -> None:
        """When risk approves a trade, recommend bundle strategy."""
        mint = data.get("mint", "")
        direction = data.get("direction", "buy")
        usd_value = data.get("position_size_usd", 0.0)
        mev_risk = data.get("mev_risk_bps", 0.0)

        tip = self.calculate_optimal_tip(
            mev_risk_bps=mev_risk,
            trade_size_usd=usd_value,
            urgency="high" if direction == "sell" else "normal",
        )

        region = self.get_best_region()

        await self._event_bus.publish(Event(
            event_type="jito.bundle_recommendation",
            source=self.name,
            data={
                "mint": mint,
                "direction": direction,
                "recommended_tip_lamports": tip,
                "recommended_region": region,
                "should_bundle": self.should_bundle(data),
                "congestion": self._congestion_level,
                "jito_leader": self._jito_leader_active,
            },
        ))

    def _handle_tip_received(self, data: dict) -> None:
        """Update tip floor from Jito tip stream."""
        tip_floor = data.get("tip_floor_lamports", 0)
        if tip_floor > 0:
            self._current_tip_floor = tip_floor

        # Percentile data
        p50 = data.get("p50", 0)
        p75 = data.get("p75", 0)
        if p50 > 0:
            self._current_tip_floor = p50
        elif p75 > 0:
            self._current_tip_floor = int(p75 * 0.8)

    def _handle_leader_info(self, data: dict) -> None:
        """Update leader status for tip calculation."""
        self._jito_leader_active = data.get("is_jito_validator", False)

    def _handle_price_update(self, data: dict) -> None:
        """Track market conditions for congestion/urgency."""
        volume = data.get("volume", 0.0)
        # High volume = likely congestion
        if volume > 0:
            self._congestion_level = min(1.0, max(
                self._congestion_level * 0.95,
                volume / 100000.0,
            ))

    # ── Reporting ─────────────────────────────────────────────

    async def _maybe_report(self) -> None:
        now = time.time()
        if now - self._last_report_time < REPORT_INTERVAL_SEC:
            return
        if self._bundles_submitted == 0:
            return

        self._last_report_time = now
        self._reports += 1

        inclusion_rate = (
            self._bundles_landed / max(self._bundles_submitted, 1)
        ) * 100

        # Clean up stale pending confirmations
        stale_cutoff = now - INCLUSION_TIMEOUT_SEC
        stale_ids = [
            bid for bid, rec in self._pending_confirmations.items()
            if rec.get("submitted_at", 0) < stale_cutoff
        ]
        for bid in stale_ids:
            self._pending_confirmations.pop(bid, None)

        await self._event_bus.publish(Event(
            event_type="jito.bundle_stats",
            source=self.name,
            data={
                "bundles_submitted": self._bundles_submitted,
                "bundles_landed": self._bundles_landed,
                "inclusion_rate_pct": round(inclusion_rate, 1),
                "total_tips_paid_lamports": self._total_tips_paid,
                "total_tips_saved_lamports": self._total_tips_saved,
                "current_tip_floor": self._current_tip_floor,
                "congestion": round(self._congestion_level, 3),
                "backrun_opportunities": len(self._backrun_opportunities),
                "best_region": self.get_best_region(),
            },
        ))

        # Update HotMemory
        if hasattr(self._hot, "jito_bundle_coordinator_state"):
            self._hot.jito_bundle_coordinator_state = {
                "inclusion_rate": round(inclusion_rate, 1),
                "total_bundles": self._bundles_submitted,
                "total_tips_lamports": self._total_tips_paid,
                "congestion": self._congestion_level,
                "best_region": self.get_best_region(),
            }

    # ── Public API ────────────────────────────────────────────

    def get_inclusion_rate(self) -> float:
        """Returns bundle inclusion rate as percentage."""
        if self._bundles_submitted == 0:
            return 0.0
        return (self._bundles_landed / self._bundles_submitted) * 100

    def get_region_stats(self) -> dict:
        return dict(self._region_stats)

    def get_stats(self):
        base = super().get_stats()
        base.update({
            "bundles_submitted": self._bundles_submitted,
            "bundles_landed": self._bundles_landed,
            "inclusion_rate_pct": round(self.get_inclusion_rate(), 1),
            "total_tips_paid": self._total_tips_paid,
            "total_tips_saved": self._total_tips_saved,
            "current_tip_floor": self._current_tip_floor,
            "congestion": self._congestion_level,
            "backrun_opportunities": len(self._backrun_opportunities),
            "backruns_attempted": self._backruns_attempted,
            "best_region": self.get_best_region(),
            "reports": self._reports,
        })
        return base
