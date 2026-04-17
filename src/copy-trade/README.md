# Copy trader (Python)

Single-file copy-trade engine. Watches a list of target wallet pubkeys via Helius WebSocket, decodes their DEX trades, and mirrors them from your wallet subject to a size cap and delay.

## File

- `copy_trade.py` — strategy implementation. Exports a `CopyTradeStrategy` class.

## How it works

1. Subscribe via Helius WebSocket to `accountSubscribe` / `logsSubscribe` for each target wallet.
2. On tx parse: classify as Jupiter swap / Raydium swap / Orca swap / pump.fun buy/sell.
3. Size the copied trade: `min(COPY_TRADE_MAX_SOL, target_size * ratio)`.
4. Delay: `COPY_TRADE_DELAY_MS` (default 500ms — pure copy, no frontrun).
5. Execute via Jupiter direct swap.

## Env vars

```
HELIUS_API_KEY           (required — for WebSocket)
RPC_URL                  (for Jupiter tx build + send)
COPY_TRADE_TARGETS       (comma-separated base58 pubkeys)
COPY_TRADE_MAX_SOL       (default 0.05)
COPY_TRADE_DELAY_MS      (default 500)
WALLET_KEYFILE/PASSWORD  (or PRIVATE_KEY)
DRY_RUN                  (true/false)
```

## Run

```bash
pip install aiohttp websockets solders solana structlog
DRY_RUN=true python copy_trade.py
```

## Notes

- Extracted from the FlashLoanRouter archive where it lived as `src/strategy/copy_trade.py`. Imports reference old package paths (`src.*`) — fix with a `sed` if you want it to run standalone, or drop into your own tree under `src/strategy/`.
- No sandwich / no frontrun — this is a pure follower. The 500ms delay is intentional.
