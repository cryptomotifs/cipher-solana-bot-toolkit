# Memecoin launcher — pump.fun via PumpPortal (Rust)

End-to-end pump.fun token launcher. Pipeline stages:

1. **narrative** — pull trending narratives (input hook for your own scanner).
2. **concept** — generate name / ticker / description from the narrative.
3. **image_gen** — create token image (pluggable generator; default is local/static).
4. **ipfs** — upload metadata + image to IPFS (pump.fun's endpoint is free).
5. **creator** — sign + send `create_v2` via PumpPortal.
6. **first_buyer** — optional same-block first buy (bonding curve entry).
7. **sell_monitor** — tracks position, exit triggers.
8. **fee_collector** — sweeps creator fees periodically.
9. **tracker** — PnL + launch history.

## Files

- `src/main.rs`, `src/lib.rs` — binary entrypoint + public API.
- `src/pipeline.rs` — top-level orchestrator.
- `src/{narrative,concept,creator,first_buyer,sell_monitor,fee_collector,tracker,wallet,budget,config,image_gen,ipfs}.rs` — pipeline stages.

## Build

```bash
cd src/memecoin-launcher
cargo build --release
```

## Env vars read

```
RPC_URL
PUMPPORTAL_API_KEY
WALLET_KEYFILE + WALLET_PASSWORD  (or WALLET_PRIVATE_KEY env var)
TRADER_PRIVATE_KEY                (separate wallet for the first buy, optional)
```

## Run

```bash
./target/release/predator-launcher --dry-run --budget 0.5
```

`--budget` is SOL ceiling per launch (initial buy + priority fees). `--dry-run` simulates without sending.

## Legal

Memecoin launches have compliance implications (securities, consumer protection) depending on jurisdiction. This toolkit is for **research and personal use**. You are solely responsible for how you use it.
