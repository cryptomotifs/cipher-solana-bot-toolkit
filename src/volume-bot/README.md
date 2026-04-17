# Volume bot — direct PumpSwap + Jito bundles (Node.js)

Runs atomic volume on a migrated pump.fun (PumpSwap) token using Jito bundles with 4 ephemeral wallets per bundle + main wallet funder + Jito tip.

Jito bundles execute **sequentially within a block**, so TX2 sees TX1's state. This means the fund tx and the swap txs can be in the **same bundle** — no wait for fund confirmation required. Net effect: 4 wallets buy-then-sell atomically in one bundle, one block.

Bundle structure (up to 5 TXs per Jito bundle):

- TX1: main wallet funds 4 ephemeral wallets.
- TX2: wallet A buy + sell (same tx).
- TX3: wallet B buy + sell (same tx).
- TX4: wallet C buy + sell (same tx).
- TX5: wallet D buy + sell, return leftover SOL to main, Jito tip.

Uses [`@pump-fun/pump-swap-sdk`](https://www.npmjs.com/package/@pump-fun/pump-swap-sdk) for correct instruction building.

## Files

- `index.js` — single-file bot with HTTP dashboard + WebSocket telemetry.
- `package.json` — dependencies.

## Install

```bash
cd src/volume-bot
npm install
```

## Configure

Create `.env` in the module dir (or rely on project-root `.env`):

```
PRIVATE_KEY=<YOUR_BASE58_SECRET>
RPC_URL=https://api.mainnet-beta.solana.com
JITO_ENDPOINT=https://mainnet.block-engine.jito.wtf
VOLUME_TOKEN_MINT=<the pump.fun-migrated mint>
```

## Run

```bash
node index.js
```

Open http://localhost:3000 for live dashboard.

## Jito endpoints

Round-robins across 5 regional block engines (mainnet, Amsterdam, Frankfurt, NY, Tokyo). Tip goes to one of the 4 standard Jito tip accounts.

## Safety

- Bot auto-sweeps ephemeral wallets back to main on shutdown.
- `wallet_ledger.json` is written locally (in `.gitignore`; contains ephemeral secrets and **must never leave your machine**).
- Start with `VOLUME_TRADE_SOL=0.001` to dry-run on mainnet cheaply.
