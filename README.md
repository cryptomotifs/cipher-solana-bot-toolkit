# cipher-solana-bot-toolkit

A free + MIT toolkit of production-grade Solana bot primitives — flash loan router, volume bot, arb/MEV, memecoin launcher, copy trader. Built in solo mode over months of iteration; all personal data stripped before publication.

**None of this is financial advice. Paper-trade first. See [docs/SECURITY.md](docs/SECURITY.md).**

## Modules

- **[Flash loan router](src/flashloan/)** — provider-agnostic flash arb with automatic fallback across MarginFi (true zero-capital), Orca Whirlpool, Raydium CLMM, and Jupiter direct. Python / asyncio.
- **[Volume bot](src/volume-bot/)** — direct PumpSwap + Jito bundle, multi-wallet, atomic fund+swap. Node.js / web3.js v1.
- **[Arb + backrun MEV](src/arb-mev/)** — predator-style backrun scanner, flash arb, LST arb, migration snipe, liquidation. Rust.
- **[Memecoin launcher](src/memecoin-launcher/)** — pump.fun launcher via PumpPortal, narrative → concept → creator → first-buyer → sell-monitor pipeline. Rust.
- **[Copy trader](src/copy-trade/)** — wallet-following trader with configurable targets, max position size, and delay. Python.

## Run it yourself

1. Clone: `git clone https://github.com/cryptomotifs/cipher-solana-bot-toolkit`
2. Read [docs/SETUP.md](docs/SETUP.md) — API keys to get, wallet setup, env vars.
3. `cp .env.example .env` and fill in your own values.
4. Per-module build + run instructions in each module's README.

## Tech matrix

| Module           | Language | Runtime      | Key deps                                          |
|------------------|----------|--------------|---------------------------------------------------|
| flashloan        | Python   | asyncio      | aiohttp, structlog, solders, solana-py            |
| volume-bot       | Node     | ESM / Node20 | @solana/web3.js, @pump-fun/pump-swap-sdk, bs58    |
| arb-mev          | Rust     | tokio        | solana-sdk, solana-client, jito-sdk               |
| memecoin-launcher| Rust     | tokio        | solana-sdk, reqwest (PumpPortal), image crate     |
| copy-trade       | Python   | asyncio      | aiohttp, solana-py, websockets                    |

## Related

[![MCPize — cipher-x402-mcp](https://img.shields.io/badge/MCPize-cipher--x402--mcp%20%240%20%2F%20%249%20%2F%20%2429%20%2F%20%2499-00d084)](https://mcpize.com/mcp/cipher-x402-mcp)

- **[cipher-starter](https://github.com/cryptomotifs/cipher-starter)** — solo-dev Solana quant playbook (MIT).
- **[cipher-solana-wallet-audit](https://github.com/cryptomotifs/cipher-solana-wallet-audit)** — free GitHub Action that fails CI on plaintext Solana private keys. Use it on this repo (it catches the exact class of leak this toolkit's source archive had before scrubbing).
- **[cipher-x402-mcp](https://github.com/cryptomotifs/cipher-x402-mcp)** — free MCP server exposing Solana + macro tools via x402 USDC payments. Managed hosted plans ($0/$9/$29/$99) on **[MCPize](https://mcpize.com/mcp/cipher-x402-mcp)**.

## Status

Alpha. Extracted and scrubbed from working private source. Each module compiles standalone; cross-module integration glue is intentionally minimal. PRs welcome.

## Support

Free + MIT. If it saved you time, tips in SOL are appreciated:

`cR9KrbsLVJvir5rY9cfY3WeNoxMwUGofzpCoVyobryy`

Also good: star the repo, share it, open a PR.
