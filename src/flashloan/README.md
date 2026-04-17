# Flash loan router (Python)

Provider-agnostic flash arb execution with automatic fallback across:

1. **MarginFi** — true zero-capital flash loan. Needs a `marginfi_account` PDA (auto-created on first trade). Fee: 0.09% of loan + gas.
2. **Kamino** — lending market flash loans, similar fee structure.
3. **Orca Whirlpool flash swap** — no protocol account needed, but needs USDC in wallet for buy leg. Fee: pool fee (0.01–1%) + gas.
4. **Raydium CLMM flash swap** — same as Orca — needs USDC in wallet. Fee: pool fee + gas.
5. **Jupiter direct swap** — no flash loan, just wallet USDC. Jupiter routing fees + gas.

The router tries the cheapest profitable provider first and falls back when a provider isn't initialized or fails. Returns a unified `RouterResult` with provider name, signature, profit, and gas.

## Files

- `flash_loan_router.py` — main router with provider fallback chain.
- `flash_loan.py`, `flash_loan_executor.py`, `flash_loan_bridge.py`, `flash_loan_cascader.py` — core flash loan primitives.
- `marginfi_flash.py` — MarginFi integration.
- `kamino_client.py` — Kamino lending client.
- `orca_flash_swap.py` — Orca Whirlpool flash swap builder.
- `raydium_flash_swap.py` — Raydium CLMM flash swap builder.
- `atomic_arb_builder.py` — atomic arb tx builder (single-tx buy+sell + flash repay).
- `priority_fee.py` — Helius-based priority fee estimator.
- `tx_precompute.py` — tx size estimation, CU budget math.
- `jito_bundle_coordinator.py` — Jito bundle submission + confirmation.

## Dependencies

```
aiohttp>=3.10
structlog>=24
solders>=0.23
solana>=0.35
based58>=0.1
```

## Run

```bash
pip install aiohttp structlog solders solana based58
python -c "
import asyncio
from flash_loan_router import FlashLoanRouter

async def main():
    router = FlashLoanRouter(rpc_url='https://api.mainnet-beta.solana.com', wallet=..., session=...)
    await router.init()
    result = await router.execute_arb(buy_quote, sell_quote, loan_usdc=10_000_000_000)
    print(result)

asyncio.run(main())
"
```

## Env vars read

```
RPC_URL / RPC_ENDPOINTS
WALLET_KEYFILE, WALLET_PASSWORD  (or WALLET_PATH / PRIVATE_KEY)
HELIUS_API_KEY   (for priority fee lookups)
MARGINFI_ENABLED, KAMINO_ENABLED, ORCA_ENABLED, RAYDIUM_ENABLED
DRY_RUN
```

## Notes

Imports reference `src.execution.*` from the original project layout — if you rewire for standalone use, `sed -i 's|src\.execution\.|flashloan.|g' *.py` across the module.
