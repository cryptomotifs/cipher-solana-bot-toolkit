"""
FlashLoanExecutor — Executes flash-loan-backed atomic arb cycles.

Atomic: borrow -> swap -> repay -> keep profit.
Zero net capital required (only gas fees).
Uses Solend/MarginFi flash loan programs.
"""

from __future__ import annotations

import time
from typing import Optional

import structlog

logger = structlog.get_logger(__name__)


# Flash loan provider configurations
FLASH_LOAN_PROVIDERS = {
    "solend": {
        "program_id": "So1endDq2YkqhipRh3WViPa8hFb7B15cKy3HeRhPJ5t",
        "fee_bps": 30,  # 0.3% flash loan fee
        "max_borrow_usd": 10_000,
        "enabled": True,
    },
    "marginfi": {
        "program_id": "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA",
        "fee_bps": 50,  # 0.5% fee
        "max_borrow_usd": 5_000,
        "enabled": True,
    },
}


class FlashLoanExecutor:
    """
    Executes flash-loan-backed arbitrage cycles.

    Flow:
    1. Check if arb profit > flash loan fee + gas
    2. Build atomic transaction: borrow -> swap_A -> swap_B -> repay
    3. Simulate transaction
    4. Execute if simulation profitable
    """

    def __init__(self, hot_memory=None):
        self._hot = hot_memory
        self._execution_count: int = 0
        self._total_profit_usd: float = 0.0
        self._total_fees_usd: float = 0.0
        self._failed_count: int = 0
        self._simulated_count: int = 0

    def select_provider(self, borrow_amount_usd: float) -> Optional[str]:
        """Select cheapest flash loan provider that supports the borrow amount."""
        best_provider = None
        best_fee = float("inf")

        for name, config in FLASH_LOAN_PROVIDERS.items():
            if not config["enabled"]:
                continue
            if borrow_amount_usd > config["max_borrow_usd"]:
                continue
            if config["fee_bps"] < best_fee:
                best_fee = config["fee_bps"]
                best_provider = name

        return best_provider

    def calculate_profitability(
        self,
        borrow_amount_usd: float,
        expected_profit_bps: float,
        gas_cost_usd: float = 0.005,
    ) -> dict:
        """
        Calculate whether a flash loan arb is profitable after fees.

        Returns:
            dict with keys: profitable, net_profit_usd, fee_usd, gas_usd,
                           gross_profit_usd, provider
        """
        provider = self.select_provider(borrow_amount_usd)
        if not provider:
            return {
                "profitable": False,
                "reason": "no_provider_available",
                "net_profit_usd": 0.0,
            }

        config = FLASH_LOAN_PROVIDERS[provider]
        fee_bps = config["fee_bps"]
        fee_usd = borrow_amount_usd * fee_bps / 10_000
        gross_profit_usd = borrow_amount_usd * expected_profit_bps / 10_000
        net_profit_usd = gross_profit_usd - fee_usd - gas_cost_usd

        return {
            "profitable": net_profit_usd > 0,
            "net_profit_usd": round(net_profit_usd, 6),
            "gross_profit_usd": round(gross_profit_usd, 6),
            "fee_usd": round(fee_usd, 6),
            "gas_usd": gas_cost_usd,
            "provider": provider,
            "fee_bps": fee_bps,
        }

    def build_flash_loan_tx(
        self,
        provider: str,
        borrow_mint: str,
        borrow_amount: float,
        swap_route: list[dict],
    ) -> dict:
        """
        Build a flash loan transaction instruction set.

        Returns a transaction descriptor (not actual Solana tx -- would need
        real SDK integration for that).
        """
        config = FLASH_LOAN_PROVIDERS.get(provider)
        if not config:
            return {"error": "unknown_provider"}

        instructions = []

        # Step 1: Flash loan borrow
        instructions.append({
            "type": "flash_loan_borrow",
            "program_id": config["program_id"],
            "mint": borrow_mint,
            "amount": borrow_amount,
        })

        # Step 2: Execute swap route
        for i, swap in enumerate(swap_route):
            instructions.append({
                "type": "swap",
                "step": i + 1,
                "input_mint": swap.get("input_mint", ""),
                "output_mint": swap.get("output_mint", ""),
                "amount_in": swap.get("amount_in", 0),
                "min_amount_out": swap.get("min_amount_out", 0),
                "dex": swap.get("dex", "jupiter"),
            })

        # Step 3: Flash loan repay
        fee_amount = borrow_amount * config["fee_bps"] / 10_000
        instructions.append({
            "type": "flash_loan_repay",
            "program_id": config["program_id"],
            "mint": borrow_mint,
            "amount": borrow_amount + fee_amount,
        })

        return {
            "provider": provider,
            "instructions": instructions,
            "borrow_amount": borrow_amount,
            "repay_amount": borrow_amount + fee_amount,
            "fee_amount": fee_amount,
            "num_instructions": len(instructions),
        }

    def simulate_execution(self, tx_descriptor: dict) -> dict:
        """
        Simulate flash loan execution.

        Validates instruction structure and computes expected profit.
        If an RPC session is available, could be extended to call
        simulateTransaction for on-chain validation.
        """
        self._simulated_count += 1

        if "error" in tx_descriptor:
            return {"success": False, "error": tx_descriptor["error"]}

        instructions = tx_descriptor.get("instructions", [])
        if len(instructions) < 3:
            return {"success": False, "error": "insufficient_instructions"}

        # Check borrow and repay are present
        has_borrow = any(i["type"] == "flash_loan_borrow" for i in instructions)
        has_repay = any(i["type"] == "flash_loan_repay" for i in instructions)
        has_swap = any(i["type"] == "swap" for i in instructions)

        if not (has_borrow and has_repay and has_swap):
            return {"success": False, "error": "missing_required_instructions"}

        borrow_amount = tx_descriptor.get("borrow_amount", 0)
        repay_amount = tx_descriptor.get("repay_amount", 0)
        fee_amount = tx_descriptor.get("fee_amount", 0)

        # Calculate expected profit from the arb
        swap_instructions = [i for i in instructions if i["type"] == "swap"]
        expected_output = sum(i.get("expected_output_usd", 0) for i in swap_instructions)
        expected_profit = expected_output - repay_amount if expected_output > 0 else 0

        return {
            "success": True,
            "simulated": True,
            "borrow_amount": borrow_amount,
            "repay_amount": repay_amount,
            "fee_amount": fee_amount,
            "expected_profit_usd": round(expected_profit, 6),
            "num_instructions": len(instructions),
            "num_swaps": len(swap_instructions),
        }

    def record_execution(self, profit_usd: float, fees_usd: float, success: bool) -> None:
        """Record flash loan execution result."""
        if success:
            self._execution_count += 1
            self._total_profit_usd += profit_usd
            self._total_fees_usd += fees_usd
        else:
            self._failed_count += 1

        if self._hot:
            self._hot.flash_loan_state = {
                "execution_count": self._execution_count,
                "total_profit_usd": self._total_profit_usd,
                "total_fees_usd": self._total_fees_usd,
                "failed_count": self._failed_count,
                "last_updated": time.monotonic(),
            }

    def get_stats(self) -> dict:
        """Get flash loan execution statistics."""
        return {
            "execution_count": self._execution_count,
            "total_profit_usd": self._total_profit_usd,
            "total_fees_usd": self._total_fees_usd,
            "net_profit_usd": self._total_profit_usd - self._total_fees_usd,
            "failed_count": self._failed_count,
            "simulated_count": self._simulated_count,
            "success_rate": (
                self._execution_count / max(self._execution_count + self._failed_count, 1)
            ),
        }
