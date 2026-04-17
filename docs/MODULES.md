# Modules — what each does and when to use it

## `src/flashloan/` — Flash loan router (Python)

**Use when**: you have a detected cross-DEX price discrepancy and want zero-capital atomic execution.

**Provider chain** (tries in order): MarginFi (true zero-capital) → Kamino → Orca Whirlpool flash swap → Raydium CLMM flash swap → Jupiter direct.

**Why router instead of single provider**: MarginFi requires a one-time PDA init. While uninitialized, Orca/Raydium are available immediately with minimal setup. The router keeps you live through the cold-start phase.

**Doesn't include**: opportunity detection. The router executes — you feed it `buy_quote` + `sell_quote` + `loan_usdc`.

## `src/volume-bot/` — PumpSwap + Jito volume (Node)

**Use when**: you own a pump.fun-migrated token and want to generate real on-chain volume ≥ the Dexscreener/Birdeye minimum thresholds for trending.

**Not for**: wash-trading a token you don't own, or pre-migration (pump.fun bonding curve) volume. Pre-migration volume has different economics — use `memecoin-launcher` for the initial buy, not this.

**Jito bundle semantics** mean the 4-wallet-buy-sell pattern is atomic — if any leg fails, the whole bundle is dropped, so you don't get half-filled state.

## `src/arb-mev/` — Predator arb + backrun (Rust)

**Use when**: you want a Geyser-fed MEV-style arb bot. Five crates:

- **`predator-geyser`** — Yellowstone gRPC subscriber. Filters + decodes on-chain events.
- **`predator-strategies`** — 6 strategies (backrun, flash-arb, lst-arb, migration-snipe, liquidation, scanner).
- **`predator-protocols`** — flash loan providers, Jupiter quote, Pyth oracle.
- **`predator-execution`** — Jito tx builder, simulator, confirmer, ALT manager.
- **`predator-core`** — shared config/types/events.

**Not for**: beginners. You need to understand Anchor, CPIs, address lookup tables, and Jito bundle mechanics to drive this safely.

## `src/memecoin-launcher/` — pump.fun launcher (Rust)

**Use when**: you want to programmatically launch tokens on pump.fun with narrative → concept → image → IPFS → create_v2 → first-buy → monitor → exit pipeline.

**Pipeline stages** are pluggable — swap out `narrative.rs` with your own trend detector, `image_gen.rs` with a real image generator, etc.

## `src/copy-trade/` — Copy trader (Python)

**Use when**: you've identified a set of "alpha" wallets and want to mirror their DEX trades subject to a size cap.

**Not frontrun**: intentional ~500ms delay after target's fill. If you want frontrun / sandwich, use `predator-strategies::backrun` instead.

## What's NOT shipped

From the private archives, we excluded:

- Orchestrator / dashboard UIs — too much project-local wiring to be reusable.
- Test fixtures with real historical data — large, project-specific.
- Research / backtesting scripts — better as separate repos.
- `bot.log` / telemetry databases — not reproducible.
- `.env` / wallet files — obviously.

If you want one of these ported, open an issue.
