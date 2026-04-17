# Setup

Every module in this toolkit is standalone, but they all read the same project-root `.env`. Get it right once and every module works.

## 1. Clone

```bash
git clone https://github.com/cryptomotifs/cipher-solana-bot-toolkit
cd cipher-solana-bot-toolkit
cp .env.example .env
```

## 2. Get a Solana RPC

The public mainnet RPC (`https://api.mainnet-beta.solana.com`) works but is rate-limited. For real use, pick one:

- **Helius** — https://dev.helius.xyz/   — 100K credits/mo free. Set `HELIUS_API_KEY`. Required for copy-trade (WebSocket) and priority-fee estimates.
- **Alchemy** — https://www.alchemy.com/   — 300M compute units/mo free.
- **Chainstack** — https://chainstack.com/   — free tier 5 RPS, good as backup.
- **Ankr** — https://www.ankr.com/   — free tier with rate limit.

Set `RPC_URL` and (optionally) `RPC_ENDPOINTS` (comma-separated for round-robin).

## 3. Set up a wallet

**NEVER put a real private key into a committed file.** Use one of:

### Option A — encrypted keyfile (recommended)

Encrypt your base58 secret with any AES-256-GCM tool. Put the encrypted file at `keys/trading.enc` and set:

```
WALLET_KEYFILE=keys/trading.enc
WALLET_PASSWORD=<your-decrypt-password>
```

### Option B — plain JSON keypair (dev only)

```bash
solana-keygen new -o keys/id.json
```

Set `WALLET_PATH=keys/id.json`. This is the same format `solana-keygen` writes by default.

### Option C — base58 env var (quick start)

```
PRIVATE_KEY=<YOUR_BASE58_SECRET>
```

Fine for dry-run/testing. For live trading, prefer Option A.

## 4. Fund the wallet

Start with 0.1 SOL for gas + small trades. Do NOT use a wallet that holds your main balance.

Security pattern: see [`SECURITY.md`](SECURITY.md) for the 3-tier hot/warm/cold wallet split.

## 5. Per-module setup

Each module's `README.md` lists its specific deps and commands:

- `src/flashloan/README.md` — Python, `pip install ...`
- `src/volume-bot/README.md` — Node, `npm install`
- `src/arb-mev/README.md` — Rust, `cargo build --release`
- `src/memecoin-launcher/README.md` — Rust, `cargo build --release`
- `src/copy-trade/README.md` — Python, `pip install ...`

## 6. Dry run everything first

All modules support `DRY_RUN=true`. Don't skip this. Watch the logs for at least a few hours, confirm the math matches your expectations, then flip to live with small caps.
