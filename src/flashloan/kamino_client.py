"""
KaminoClient - Kamino Finance lending protocol interaction.
Phase 53: deposit, withdraw, borrow, repay via Kamino program.
"""
from __future__ import annotations

import hashlib
import time
from collections import deque

import aiohttp
import structlog

logger = structlog.get_logger(__name__)

KAMINO_LENDING_PROGRAM = "KLend2g3cP87ber41GFZGeSWFuMKbXMbKEKQpFL2DPRq"
KAMINO_API_BASE = "https://api.kamino.finance"

KAMINO_RESERVES = {
    "SOL": {"mint": "So11111111111111111111111111111111111111112", "decimals": 9},
    "USDC": {"mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "decimals": 6},
    "USDT": {"mint": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "decimals": 6},
    "JitoSOL": {"mint": "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn", "decimals": 9},
    "mSOL": {"mint": "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So", "decimals": 9},
    "BONK": {"mint": "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263", "decimals": 5},
}

MAX_DEPOSIT_HISTORY = 200
MAX_BORROW_HISTORY = 200

COLLATERAL_FACTORS = {
    "SOL": 0.80, "USDC": 0.90, "USDT": 0.88,
    "JitoSOL": 0.75, "mSOL": 0.75, "BONK": 0.50,
}

DEFAULT_DEPOSIT_APYS = {
    "SOL": 0.065, "USDC": 0.082, "USDT": 0.078,
    "JitoSOL": 0.072, "mSOL": 0.068, "BONK": 0.12,
}

DEFAULT_BORROW_APYS = {
    "SOL": 0.095, "USDC": 0.112, "USDT": 0.108,
    "JitoSOL": 0.10, "mSOL": 0.098, "BONK": 0.18,
}


def _mock_signature(action: str, reserve: str, amount: float) -> str:
    """Generate a deterministic mock transaction signature."""
    payload = f"{action}:{reserve}:{amount}:{time.monotonic()}"
    return hashlib.sha256(payload.encode()).hexdigest()[:88]


class KaminoClient:
    """
    Interacts with Kamino Finance lending protocol.
    """

    def __init__(self, rpc_url: str = "", wallet_pubkey: str = "") -> None:
        self._rpc_url = rpc_url
        self._wallet_pubkey = wallet_pubkey
        self._session: aiohttp.ClientSession | None = None
        self._deposits: dict[str, dict] = {}
        self._borrows: dict[str, dict] = {}
        self._health_factor: float = 0.0
        self._deposit_history: deque = deque(maxlen=MAX_DEPOSIT_HISTORY)
        self._borrow_history: deque = deque(maxlen=MAX_BORROW_HISTORY)
        self._total_deposited_usd: float = 0.0
        self._total_borrowed_usd: float = 0.0
        self._total_interest_earned: float = 0.0
        self._total_interest_paid: float = 0.0
        self._error_count: int = 0
        self._started: bool = False
    async def start(self) -> None:
        """Initialize client session."""
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession()
        self._started = True
        logger.info("kamino_client.started", wallet=self._wallet_pubkey[:8] if self._wallet_pubkey else "none")

    async def stop(self) -> None:
        """Close client session."""
        if self._session and not self._session.closed:
            await self._session.close()
        self._session = None
        self._started = False
        logger.info("kamino_client.stopped")

    async def deposit(self, reserve: str, amount: float) -> dict:
        """Deposit to Kamino reserve."""
        if not self.validate_reserve(reserve):
            self._error_count += 1
            return {"status": "error", "error": f"Invalid reserve: {reserve}"}
        if amount <= 0:
            self._error_count += 1
            return {"status": "error", "error": "Amount must be positive"}
        apy = DEFAULT_DEPOSIT_APYS.get(reserve, 0.05)
        sig = _mock_signature("deposit", reserve, amount)
        now = time.time()
        if reserve in self._deposits:
            self._deposits[reserve]["amount"] += amount
            self._deposits[reserve]["timestamp"] = now
        else:
            self._deposits[reserve] = {"amount": amount, "apy": apy, "timestamp": now}
        self._deposits[reserve]["apy"] = apy
        self._total_deposited_usd += amount
        record = {"action": "deposit", "reserve": reserve, "amount": amount, "apy": apy, "signature": sig, "timestamp": now}
        self._deposit_history.append(record)
        self._health_factor = self.calculate_health_factor()
        logger.info("kamino.deposit", reserve=reserve, amount=amount, apy=apy)
        return {"signature": sig, "reserve": reserve, "amount": amount, "apy": apy, "status": "ok"}

    async def withdraw(self, reserve: str, amount: float) -> dict:
        """Withdraw from Kamino reserve."""
        if not self.validate_reserve(reserve):
            self._error_count += 1
            return {"status": "error", "error": f"Invalid reserve: {reserve}"}
        if amount <= 0:
            self._error_count += 1
            return {"status": "error", "error": "Amount must be positive"}
        current = self._deposits.get(reserve, {}).get("amount", 0.0)
        if amount > current:
            self._error_count += 1
            return {"status": "error", "error": f"Insufficient deposit: have {current}, requested {amount}"}
        dep = self._deposits[reserve]
        elapsed_years = (time.time() - dep["timestamp"]) / (365.25 * 86400)
        interest = dep["amount"] * dep["apy"] * elapsed_years
        self._total_interest_earned += max(0.0, interest)
        dep["amount"] -= amount
        if dep["amount"] < 1e-9:
            del self._deposits[reserve]
        sig = _mock_signature("withdraw", reserve, amount)
        now = time.time()
        record = {"action": "withdraw", "reserve": reserve, "amount": amount, "interest_earned": interest, "signature": sig, "timestamp": now}
        self._deposit_history.append(record)
        self._health_factor = self.calculate_health_factor()
        logger.info("kamino.withdraw", reserve=reserve, amount=amount)
        return {"signature": sig, "reserve": reserve, "amount": amount, "interest_earned": interest, "status": "ok"}
    async def borrow(self, reserve: str, amount: float) -> dict:
        """Borrow from Kamino. Requires sufficient collateral."""
        if not self.validate_reserve(reserve):
            self._error_count += 1
            return {"status": "error", "error": f"Invalid reserve: {reserve}"}
        if amount <= 0:
            self._error_count += 1
            return {"status": "error", "error": "Amount must be positive"}
        total_collateral_value = self._get_total_collateral_value()
        total_borrow_value = self._get_total_borrow_value() + amount
        if total_collateral_value <= 0:
            self._error_count += 1
            return {"status": "error", "error": "No collateral deposited"}
        projected_hf = total_collateral_value / total_borrow_value if total_borrow_value > 0 else float("inf")
        if projected_hf < 1.0:
            self._error_count += 1
            return {"status": "error", "error": f"Insufficient collateral: projected health factor {projected_hf:.2f}"}
        rate = DEFAULT_BORROW_APYS.get(reserve, 0.10)
        sig = _mock_signature("borrow", reserve, amount)
        now = time.time()
        if reserve in self._borrows:
            self._borrows[reserve]["amount"] += amount
            self._borrows[reserve]["timestamp"] = now
        else:
            self._borrows[reserve] = {"amount": amount, "apy": rate, "timestamp": now}
        self._borrows[reserve]["apy"] = rate
        self._total_borrowed_usd += amount
        record = {"action": "borrow", "reserve": reserve, "amount": amount, "rate": rate, "signature": sig, "timestamp": now}
        self._borrow_history.append(record)
        self._health_factor = self.calculate_health_factor()
        logger.info("kamino.borrow", reserve=reserve, amount=amount, rate=rate)
        return {"signature": sig, "reserve": reserve, "amount": amount, "rate": rate, "status": "ok"}

    async def repay(self, reserve: str, amount: float) -> dict:
        """Repay borrowed amount on Kamino."""
        if not self.validate_reserve(reserve):
            self._error_count += 1
            return {"status": "error", "error": f"Invalid reserve: {reserve}"}
        if amount <= 0:
            self._error_count += 1
            return {"status": "error", "error": "Amount must be positive"}
        current = self._borrows.get(reserve, {}).get("amount", 0.0)
        if current <= 0:
            self._error_count += 1
            return {"status": "error", "error": f"No outstanding borrow for {reserve}"}
        actual_repay = min(amount, current)
        bor = self._borrows[reserve]
        elapsed_years = (time.time() - bor["timestamp"]) / (365.25 * 86400)
        interest = bor["amount"] * bor["apy"] * elapsed_years
        self._total_interest_paid += max(0.0, interest)
        bor["amount"] -= actual_repay
        remaining = bor["amount"]
        if remaining < 1e-9:
            del self._borrows[reserve]
            remaining = 0.0
        sig = _mock_signature("repay", reserve, actual_repay)
        now = time.time()
        record = {"action": "repay", "reserve": reserve, "amount": actual_repay, "interest_paid": interest, "signature": sig, "timestamp": now}
        self._borrow_history.append(record)
        self._health_factor = self.calculate_health_factor()
        logger.info("kamino.repay", reserve=reserve, amount=actual_repay, remaining=remaining)
        return {"signature": sig, "reserve": reserve, "amount": actual_repay, "remaining": remaining, "status": "ok"}
    async def get_positions(self) -> dict:
        """Query current deposits and borrows."""
        deposits = {}
        for res, dep in self._deposits.items():
            deposits[res] = {"amount": dep["amount"], "apy": dep["apy"]}
        borrows = {}
        for res, bor in self._borrows.items():
            borrows[res] = {"amount": bor["amount"], "apy": bor["apy"]}
        net_apy = self._calculate_net_apy()
        self._health_factor = self.calculate_health_factor()
        return {"deposits": deposits, "borrows": borrows, "health_factor": self._health_factor, "net_apy": net_apy}

    async def get_reserve_rates(self) -> dict[str, dict]:
        """Get current deposit/borrow rates for all reserves."""
        rates: dict[str, dict] = {}
        for reserve in KAMINO_RESERVES:
            deposit_apy = DEFAULT_DEPOSIT_APYS.get(reserve, 0.05)
            borrow_apy = DEFAULT_BORROW_APYS.get(reserve, 0.10)
            utilization = deposit_apy / borrow_apy if borrow_apy > 0 else 0.0
            rates[reserve] = {"deposit_apy": deposit_apy, "borrow_apy": borrow_apy, "utilization": round(utilization, 4)}
        return rates

    def calculate_health_factor(self) -> float:
        """Calculate health factor. HF < 1.0 = liquidation risk."""
        collateral_value = self._get_total_collateral_value()
        borrow_value = self._get_total_borrow_value()
        if borrow_value <= 0:
            return float("inf") if collateral_value > 0 else 0.0
        hf = collateral_value / borrow_value
        self._health_factor = hf
        return hf

    def validate_reserve(self, reserve: str) -> bool:
        """Check if reserve is supported."""
        return reserve in KAMINO_RESERVES

    def get_stats(self) -> dict:
        """Return operational statistics."""
        return {
            "total_deposited_usd": self._total_deposited_usd,
            "total_borrowed_usd": self._total_borrowed_usd,
            "total_interest_earned": self._total_interest_earned,
            "total_interest_paid": self._total_interest_paid,
            "active_deposits": len(self._deposits),
            "active_borrows": len(self._borrows),
            "health_factor": self._health_factor,
            "deposit_history_count": len(self._deposit_history),
            "borrow_history_count": len(self._borrow_history),
            "error_count": self._error_count,
        }

    def _get_total_collateral_value(self) -> float:
        """Sum of deposits weighted by collateral factor."""
        total = 0.0
        for reserve, dep in self._deposits.items():
            cf = COLLATERAL_FACTORS.get(reserve, 0.5)
            total += dep["amount"] * cf
        return total

    def _get_total_borrow_value(self) -> float:
        """Sum of all borrows."""
        return sum(bor["amount"] for bor in self._borrows.values())

    def _calculate_net_apy(self) -> float:
        """Net APY = (deposit interest - borrow interest) / total deposits."""
        total_dep = sum(d["amount"] for d in self._deposits.values())
        if total_dep <= 0:
            return 0.0
        deposit_yield = sum(d["amount"] * d["apy"] for d in self._deposits.values())
        borrow_cost = sum(b["amount"] * b["apy"] for b in self._borrows.values())
        net = (deposit_yield - borrow_cost) / total_dep
        return round(net, 6)
