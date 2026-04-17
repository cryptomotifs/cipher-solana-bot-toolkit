# Arb / backrun MEV (Rust)

Predator-style MEV toolkit — 5 Rust crates extracted from a working Geyser-driven arb bot.

## Crates

- **`predator-core`** — config, constants, types, state, error, events, actions. Shared across the stack.
- **`predator-execution`** — transaction builder, ALT management, ATA setup, Jito client, submitter, confirmer, simulator.
- **`predator-protocols`** — flash loan (MarginFi/Kamino/Solend/Save), Jupiter, oracle (Pyth), traits.
- **`predator-strategies`** — backrun, flash-arb, LST arb, migration snipe, liquidation scanner, priority scoring, traits.
- **`predator-geyser`** — Yellowstone Geyser gRPC subscriber, filter config, decoder, oracle engine (Pyth SSE), router.

## Stack shape

```
geyser stream
   ↓
[decoder/filters]
   ↓
strategies (backrun / flash_arb / lst_arb / migration_snipe / liquidation)
   ↓
execution (builder → simulator → jito/submitter → confirmer)
   ↓
protocols (flash_loan / jupiter quote / oracle)
```

## Build

```bash
cd src/arb-mev
# Top-level Cargo workspace is intentionally omitted; each crate builds independently.
cd predator-strategies
cargo build --release
```

If you want a workspace, add a root `Cargo.toml`:

```toml
[workspace]
members = [
  "predator-core",
  "predator-execution",
  "predator-protocols",
  "predator-strategies",
  "predator-geyser",
]
resolver = "2"
```

## Key deps

- `solana-sdk` / `solana-client` 2.2+
- `tokio` 1.x
- `reqwest` 0.12 (for Jupiter quote API, oracle REST)
- `yellowstone-grpc-client` (for Geyser)
- `jito-sdk` (bundle submission)

## Env vars read

```
RPC_URL / RPC_ENDPOINTS
HELIUS_API_KEY
JITO_ENDPOINT
MARGINFI_ENABLED / KAMINO_ENABLED / ...
DRY_RUN
```

## Notes

- `predator-bot/` (orchestrator) and `predator-dashboard/` (web UI) are intentionally NOT shipped — too much project-local wiring. The 5 crates above compile as libraries and the strategies each expose a trait-based entry point you can drive from your own `main.rs`.
- Strategy traits live in `predator-strategies/src/traits.rs`.
