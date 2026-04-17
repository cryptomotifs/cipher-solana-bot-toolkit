"""
FlashLoanAmplifier — Innovation #31: Flash Loan Position Amplifier.

Uses Solana flash loans (Flash Loan Mastery program, 0.095% fee) to amplify
position sizes on high-confidence (A-grade, confidence > 0.75) trades.

Example: $25 position + $50 flash loan = $75 effective position.
If trade makes 10% = $7.50 profit instead of $2.50, minus $0.0475 fee.
3x amplified return for 0.095% cost.

Safety constraints:
- Only amplify A-grade signals (conviction >= 0.75)
- Max amplification: 3x position size
- Only for BUY orders (never amplify sells)
- Flash loan + repay must be atomic (single transaction)
- Abort if estimated slippage on amplified size > 2%
"""

from __future__ import annotations

from dataclasses import dataclass

import structlog

logger = structlog.get_logger(__name__)

# Flash Loan Mastery program on Solana
FLASH_LOAN_PROGRAM = "F1aShdFVv5WNEzqYMFBpEMg2QiLivkHGQzPMehGgCeFg"

# Flash loan fee: 0.095% (9.5 bps)
FLASH_LOAN_FEE_BPS = 9.5

# Minimum confidence to enable amplification
MIN_CONFIDENCE_FOR_AMPLIFICATION = 0.75

# Maximum amplification multiplier (position_size * multiplier)
MAX_AMPLIFICATION_MULTIPLIER = 3.0

# Don't amplify positions smaller than this (fees eat into profit)
MIN_POSITION_FOR_AMPLIFICATION_USD = 10.0

# Max estimated slippage on amplified size — abort if exceeded
MAX_AMPLIFIED_SLIPPAGE_PCT = 2.0

# Minimum expected profit to justify the flash loan fee
MIN_PROFIT_TO_FEE_RATIO = 3.0


@dataclass
class AmplificationDecision:
    """Result of flash loan amplification analysis."""
    should_amplify: bool = False
    original_size_usd: float = 0.0
    loan_amount_usd: float = 0.0
    total_size_usd: float = 0.0
    multiplier: float = 1.0
    fee_usd: float = 0.0
    expected_profit_usd: float = 0.0
    rejection_reason: str = ""


class FlashLoanAmplifier:
    """Decides whether and how much to amplify trades with flash loans.

    This class does NOT execute flash loans directly — it provides the
    decision logic and loan parameters. The actual flash loan instruction
    building requires integration with the Flash Loan Mastery SDK.

    Usage:
        amplifier = FlashLoanAmplifier()
        decision = amplifier.evaluate(
            position_size_usd=25.0,
            confidence=0.82,
            conviction_grade="A",
            estimated_profit_pct=8.0,
            liquidity_usd=50000.0,
        )
        if decision.should_amplify:
            # Build flash loan + swap + repay transaction
            total_size = decision.total_size_usd
    """

    def __init__(
        self,
        max_multiplier: float = MAX_AMPLIFICATION_MULTIPLIER,
        min_confidence: float = MIN_CONFIDENCE_FOR_AMPLIFICATION,
        min_position_usd: float = MIN_POSITION_FOR_AMPLIFICATION_USD,
    ) -> None:
        self._max_multiplier = max_multiplier
        self._min_confidence = min_confidence
        self._min_position = min_position_usd

        # Stats
        self._evaluations: int = 0
        self._amplified: int = 0
        self._rejected: int = 0
        self._total_loan_volume_usd: float = 0.0

    def evaluate(
        self,
        position_size_usd: float,
        confidence: float,
        conviction_grade: str = "",
        estimated_profit_pct: float = 0.0,
        liquidity_usd: float = 0.0,
        is_buy: bool = True,
    ) -> AmplificationDecision:
        """Evaluate whether a trade should be amplified with a flash loan.

        Args:
            position_size_usd: Original position size
            confidence: Signal confidence (0-1)
            conviction_grade: "A", "B", "C", "D"
            estimated_profit_pct: Expected profit % (from take-profit level)
            liquidity_usd: Pool liquidity for slippage estimation
            is_buy: Only BUY orders can be amplified

        Returns:
            AmplificationDecision with amplification parameters.
        """
        self._evaluations += 1
        decision = AmplificationDecision(original_size_usd=position_size_usd)

        # Gate 1: Only BUY orders
        if not is_buy:
            decision.rejection_reason = "sell_order"
            self._rejected += 1
            return decision

        # Gate 2: Minimum confidence
        if confidence < self._min_confidence:
            decision.rejection_reason = f"low_confidence_{confidence:.2f}"
            self._rejected += 1
            return decision

        # Gate 3: Conviction grade must be A
        if conviction_grade and conviction_grade != "A":
            decision.rejection_reason = f"grade_{conviction_grade}_not_A"
            self._rejected += 1
            return decision

        # Gate 4: Minimum position size (below this, fees eat profit)
        if position_size_usd < self._min_position:
            decision.rejection_reason = f"position_too_small_{position_size_usd:.0f}"
            self._rejected += 1
            return decision

        # Gate 5: Need estimated profit to calculate fee viability
        if estimated_profit_pct <= 0:
            decision.rejection_reason = "no_profit_estimate"
            self._rejected += 1
            return decision

        # Calculate optimal amplification
        multiplier = self._calculate_multiplier(
            confidence, position_size_usd, liquidity_usd,
        )

        loan_amount = position_size_usd * (multiplier - 1.0)
        total_size = position_size_usd + loan_amount
        fee_usd = loan_amount * (FLASH_LOAN_FEE_BPS / 10_000)

        # Expected profit on the amplified portion
        expected_profit = loan_amount * (estimated_profit_pct / 100)

        # Gate 6: Profit must justify the fee (minimum 3x)
        if expected_profit < fee_usd * MIN_PROFIT_TO_FEE_RATIO:
            decision.rejection_reason = (
                f"fee_ratio_{expected_profit:.2f}/{fee_usd:.4f}"
            )
            self._rejected += 1
            return decision

        # Gate 7: Slippage check — amplified size vs liquidity
        if liquidity_usd > 0:
            size_to_liquidity = total_size / liquidity_usd
            estimated_slippage = size_to_liquidity * 100  # rough estimate
            if estimated_slippage > MAX_AMPLIFIED_SLIPPAGE_PCT:
                decision.rejection_reason = (
                    f"slippage_{estimated_slippage:.1f}pct"
                )
                self._rejected += 1
                return decision

        # All gates passed — amplify
        decision.should_amplify = True
        decision.loan_amount_usd = round(loan_amount, 2)
        decision.total_size_usd = round(total_size, 2)
        decision.multiplier = round(multiplier, 2)
        decision.fee_usd = round(fee_usd, 4)
        decision.expected_profit_usd = round(expected_profit, 2)

        self._amplified += 1
        self._total_loan_volume_usd += loan_amount

        logger.info(
            "flash_loan.amplify_approved",
            original_usd=position_size_usd,
            loan_usd=decision.loan_amount_usd,
            total_usd=decision.total_size_usd,
            multiplier=decision.multiplier,
            fee_usd=decision.fee_usd,
            confidence=confidence,
        )

        return decision

    def _calculate_multiplier(
        self,
        confidence: float,
        position_usd: float,
        liquidity_usd: float,
    ) -> float:
        """Calculate optimal amplification multiplier.

        Higher confidence = higher multiplier, constrained by liquidity.
        """
        # Base multiplier from confidence (0.75→1.5x, 0.85→2.0x, 0.95→3.0x)
        if confidence >= 0.90:
            base = 3.0
        elif confidence >= 0.85:
            base = 2.5
        elif confidence >= 0.80:
            base = 2.0
        else:
            base = 1.5

        # Reduce multiplier if liquidity is thin
        if liquidity_usd > 0:
            max_safe_size = liquidity_usd * 0.02  # 2% of liquidity
            max_from_liquidity = max_safe_size / max(position_usd, 1.0)
            base = min(base, max_from_liquidity)

        # Apply hard cap
        return max(1.0, min(base, self._max_multiplier))

    @property
    def stats(self) -> dict:
        """Return amplifier statistics."""
        return {
            "evaluations": self._evaluations,
            "amplified": self._amplified,
            "rejected": self._rejected,
            "total_loan_volume_usd": round(self._total_loan_volume_usd, 2),
        }

    def get_stats(self) -> dict:
        """Return runtime stats for monitoring."""
        return {"class": "FlashLoanAmplifier"}
