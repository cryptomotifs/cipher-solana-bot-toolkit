"""
FlashLoanCascader (Agent #130) — NODE D: Multi-protocol flash loan chaining.

Chains flash loans across multiple DeFi protocols for multi-hop profit
extraction. Atomic: either ALL hops succeed and we profit, or the entire
cascade reverts with ZERO cost (only failed gas).

Supports: Solend, MarginFi, Kamino, Port Finance (simulated).
"""

from __future__ import annotations

import time
import uuid
from collections import deque
from typing import Optional

import structlog

from src.core.agents.base import BaseAgent
from src.core.event_bus import Event, DummyEventBus

logger = structlog.get_logger(__name__)


# Supported flash loan protocols
PROTOCOLS = {
    "solend": {
        "name": "Solend",
        "fee_pct": 0.003,       # 0.3%
        "max_amount_usd": 10_000,
        "priority": 1,           # Lower = preferred
        "enabled": True,
    },
    "marginfi": {
        "name": "MarginFi",
        "fee_pct": 0.005,       # 0.5%
        "max_amount_usd": 5_000,
        "priority": 2,
        "enabled": True,
    },
    "kamino": {
        "name": "Kamino",
        "fee_pct": 0.004,       # 0.4%
        "max_amount_usd": 8_000,
        "priority": 3,
        "enabled": True,
    },
    "port_finance": {
        "name": "Port Finance",
        "fee_pct": 0.006,       # 0.6%
        "max_amount_usd": 3_000,
        "priority": 4,
        "enabled": True,
    },
}

MAX_HOPS = 5
MAX_CASCADE_HISTORY = 200
MIN_PROFIT_USD = 0.01
GAS_COST_PER_HOP_USD = 0.0005


class FlashLoanCascader(BaseAgent):
    """
    Chains flash loans across multiple protocols for multi-hop arbitrage.

    Flow:
    1. Evaluate opportunity: check if route is profitable after all fees.
    2. Build cascade: select cheapest protocol, construct hop sequence.
    3. Simulate cascade: verify atomicity and profitability.
    4. Execute or discard based on simulation result.

    All cascades are atomic: success means profit, failure means zero cost.
    """

    def __init__(self, event_bus=None, hot_memory=None):
        eb = event_bus or DummyEventBus()
        super().__init__(name="flash_loan_cascader", team="execution", event_bus=eb)
        self._hot = hot_memory

        # Protocol state
        self._protocols = {k: dict(v) for k, v in PROTOCOLS.items()}

        # Active and historical cascades
        self._active_cascades: dict[str, dict] = {}
        self._cascade_history: deque = deque(maxlen=MAX_CASCADE_HISTORY)

        # Counters
        self._cascades_evaluated: int = 0
        self._cascades_built: int = 0
        self._cascades_simulated: int = 0
        self._cascades_executed: int = 0
        self._cascades_reverted: int = 0
        self._total_profit_usd: float = 0.0

    # ---- Protocol selection ----

    def _select_protocol(self, amount_usd: float) -> Optional[str]:
        """
        Select the cheapest enabled protocol that supports the amount.

        Args:
            amount_usd: Borrow amount in USD.

        Returns:
            Protocol key or None if no protocol supports the amount.
        """
        candidates = []
        for key, proto in self._protocols.items():
            if not proto.get("enabled", False):
                continue
            if amount_usd > proto["max_amount_usd"]:
                continue
            candidates.append((key, proto["fee_pct"], proto["priority"]))

        if not candidates:
            return None

        # Sort by fee first, then priority
        candidates.sort(key=lambda x: (x[1], x[2]))
        return candidates[0][0]

    def _calculate_total_fee(self, protocol_key: str, amount_usd: float, hops: int) -> float:
        """Calculate total fees for a cascade."""
        proto = self._protocols.get(protocol_key)
        if not proto:
            return float("inf")

        flash_fee = amount_usd * proto["fee_pct"]
        gas_cost = GAS_COST_PER_HOP_USD * hops
        return round(flash_fee + gas_cost, 6)

    # ---- Evaluation ----

    def evaluate_opportunity(self, route: dict) -> dict:
        """
        Evaluate whether a multi-hop route is profitable via flash loan.

        Args:
            route: Dict with amount_usd, hops (list of hop dicts),
                   expected_gross_profit_usd.

        Returns:
            Dict with is_profitable, expected_profit, fee_cost, hops.
        """
        self._cascades_evaluated += 1

        amount_usd = route.get("amount_usd", 0.0)
        hops = route.get("hops", [])
        gross_profit = route.get("expected_gross_profit_usd", 0.0)

        if not hops or amount_usd <= 0:
            return {
                "is_profitable": False,
                "expected_profit": 0.0,
                "fee_cost": 0.0,
                "hops": 0,
                "reason": "invalid_route",
            }

        if len(hops) > MAX_HOPS:
            return {
                "is_profitable": False,
                "expected_profit": 0.0,
                "fee_cost": 0.0,
                "hops": len(hops),
                "reason": f"too_many_hops_{len(hops)}_max_{MAX_HOPS}",
            }

        protocol = self._select_protocol(amount_usd)
        if not protocol:
            return {
                "is_profitable": False,
                "expected_profit": 0.0,
                "fee_cost": 0.0,
                "hops": len(hops),
                "reason": "no_protocol_available",
            }

        fee_cost = self._calculate_total_fee(protocol, amount_usd, len(hops))
        net_profit = gross_profit - fee_cost

        is_profitable = net_profit >= MIN_PROFIT_USD

        return {
            "is_profitable": is_profitable,
            "expected_profit": round(net_profit, 6),
            "fee_cost": round(fee_cost, 6),
            "hops": len(hops),
            "protocol": protocol,
            "gross_profit": round(gross_profit, 6),
            "amount_usd": amount_usd,
        }

    # ---- Building ----

    def build_cascade(self, route: dict) -> dict:
        """
        Build a flash loan cascade from a profitable route.

        Args:
            route: Dict with amount_usd, hops, expected_gross_profit_usd.

        Returns:
            Cascade dict with cascade_id, borrow_protocol, hops,
            total_fee, expected_profit.
        """
        evaluation = self.evaluate_opportunity(route)
        # Undo the double-count from evaluate_opportunity
        self._cascades_evaluated -= 1

        if not evaluation.get("is_profitable"):
            return {
                "cascade_id": "",
                "error": evaluation.get("reason", "not_profitable"),
                "borrow_protocol": "",
                "hops": [],
                "total_fee": 0.0,
                "expected_profit": 0.0,
            }

        protocol = evaluation["protocol"]
        amount_usd = route.get("amount_usd", 0.0)
        hops = route.get("hops", [])

        cascade_id = uuid.uuid4().hex[:16]

        # Build hop sequence
        hop_sequence = []
        for i, hop in enumerate(hops):
            hop_sequence.append({
                "order": i,
                "from_token": hop.get("from_token", ""),
                "to_token": hop.get("to_token", ""),
                "dex": hop.get("dex", "unknown"),
                "amount_usd": hop.get("amount_usd", amount_usd),
                "expected_output_usd": hop.get("expected_output_usd", 0.0),
            })

        cascade = {
            "cascade_id": cascade_id,
            "borrow_protocol": protocol,
            "borrow_amount_usd": amount_usd,
            "hops": hop_sequence,
            "total_fee": evaluation["fee_cost"],
            "expected_profit": evaluation["expected_profit"],
            "created_at": time.monotonic(),
            "status": "built",
            "is_atomic": True,
        }

        self._cascades_built += 1
        self._active_cascades[cascade_id] = cascade

        return cascade

    # ---- Simulation ----

    def simulate_cascade(self, cascade: dict) -> dict:
        """
        Simulate a cascade to verify profitability and atomicity.

        Args:
            cascade: Cascade dict from build_cascade().

        Returns:
            Dict with success, simulated_profit, gas_cost.
        """
        self._cascades_simulated += 1
        cascade_id = cascade.get("cascade_id", "")

        if not cascade_id or cascade.get("error"):
            return {
                "success": False,
                "simulated_profit": 0.0,
                "gas_cost": 0.0,
                "reason": "invalid_cascade",
            }

        hops = cascade.get("hops", [])
        expected_profit = cascade.get("expected_profit", 0.0)
        gas_cost = GAS_COST_PER_HOP_USD * len(hops)

        # Simulated slippage: reduce profit by 5% per hop
        slippage_factor = 0.95 ** len(hops)
        simulated_profit = expected_profit * slippage_factor

        success = simulated_profit > MIN_PROFIT_USD

        if cascade_id in self._active_cascades:
            self._active_cascades[cascade_id]["status"] = (
                "simulated_ok" if success else "simulated_fail"
            )
            self._active_cascades[cascade_id]["simulated_profit"] = round(
                simulated_profit, 6
            )

        return {
            "success": success,
            "simulated_profit": round(simulated_profit, 6),
            "gas_cost": round(gas_cost, 6),
            "slippage_factor": round(slippage_factor, 4),
            "hop_count": len(hops),
            "cascade_id": cascade_id,
        }

    # ---- Execution (simulated) ----

    def execute_cascade(self, cascade_id: str) -> dict:
        """
        Execute a simulated cascade.

        Args:
            cascade_id: ID of a built and simulated cascade.

        Returns:
            Execution result.
        """
        cascade = self._active_cascades.get(cascade_id)
        if not cascade:
            return {"success": False, "reason": "cascade_not_found"}

        if cascade.get("status") != "simulated_ok":
            return {"success": False, "reason": "not_simulated_or_failed"}

        profit = cascade.get("simulated_profit", cascade.get("expected_profit", 0.0))

        cascade["status"] = "executed"
        cascade["executed_at"] = time.monotonic()
        cascade["final_profit"] = profit

        self._cascades_executed += 1
        self._total_profit_usd += profit

        # Move to history
        self._cascade_history.append(cascade)
        del self._active_cascades[cascade_id]

        return {
            "success": True,
            "cascade_id": cascade_id,
            "profit_usd": round(profit, 6),
            "protocol": cascade.get("borrow_protocol", ""),
        }

    # ---- Query ----

    def get_active_cascades(self) -> list[dict]:
        """Get currently active (in-flight) cascades."""
        return list(self._active_cascades.values())

    def get_stats(self) -> dict:
        """Get cascader statistics."""
        return {
            "cascades_evaluated": self._cascades_evaluated,
            "cascades_built": self._cascades_built,
            "cascades_simulated": self._cascades_simulated,
            "cascades_executed": self._cascades_executed,
            "cascades_reverted": self._cascades_reverted,
            "total_profit_usd": round(self._total_profit_usd, 6),
            "active_cascades": len(self._active_cascades),
            "protocols_enabled": sum(
                1 for p in self._protocols.values() if p.get("enabled")
            ),
        }

    # ---- Event handler ----

    async def analyze(self, event: Event) -> Optional[Event]:
        """
        Handle arb.opportunity events.

        Expected event data:
        - route: dict with amount_usd, hops, expected_gross_profit_usd
        - (optional) auto_execute: bool
        """
        if event.event_type != "arb.opportunity":
            return None

        data = event.data if isinstance(event.data, dict) else {}
        route = data.get("route", data)

        evaluation = self.evaluate_opportunity(route)

        if not evaluation.get("is_profitable"):
            return None

        cascade = self.build_cascade(route)
        if cascade.get("error"):
            return None

        sim = self.simulate_cascade(cascade)

        result = {
            "evaluation": evaluation,
            "cascade": cascade,
            "simulation": sim,
        }

        # Auto-execute if requested and simulation passed
        if data.get("auto_execute") and sim.get("success"):
            exec_result = self.execute_cascade(cascade["cascade_id"])
            result["execution"] = exec_result

        if self._hot and hasattr(self._hot, "cascade_stats"):
            self._hot.cascade_stats = self.get_stats()

        return Event(
            event_type="flash_loan.cascade_result",
            source="flash_loan_cascader",
            data=result,
        )
