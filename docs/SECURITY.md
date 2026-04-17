# Security

## The 3-tier wallet pattern

Never trade from a wallet that holds your main stack. Use three wallets:

| Tier     | Purpose                          | Balance              | Key storage                          |
|----------|----------------------------------|----------------------|--------------------------------------|
| **Cold** | Long-term storage                | 80%+ of your stack   | Hardware wallet (Ledger / Trezor)    |
| **Warm** | Trading float — tops up Hot      | 2-10% of your stack  | Separate hardware or encrypted key   |
| **Hot**  | The bot wallet                   | 0.05–1 SOL           | Encrypted keyfile (AES-256-GCM)      |

Cold → Warm → Hot flows are **one-way**: Hot can pay back to Warm via a manual step, never with a hardcoded auto-sweep.

## Never commit plaintext keys

The source archives this toolkit was extracted from **did** have plaintext keys in them. We stripped everything before publishing. To verify:

```bash
# Base58 Solana secrets are 87-88 chars; EVM hex keys are 0x + 64 hex
grep -rE '\b[1-9A-HJ-NP-Za-km-z]{87,88}\b' src/ || echo "CLEAN"
grep -rE '0x[a-fA-F0-9]{64}' src/ || echo "CLEAN"
```

Before committing ANY change, run [cipher-solana-wallet-audit](https://github.com/cryptomotifs/cipher-solana-wallet-audit) as a pre-commit hook or GitHub Action. It catches:

- Base58 secrets 87–88 chars
- EVM 0x hex 64 chars
- BIP39 mnemonics
- `keystore.json` / `keypair.json` / `wallet.json` file commits
- `.env` commits

## Before going live

1. Dry-run for at least 48h on a fresh hot wallet with ≤0.1 SOL.
2. Set **per-trade caps** and **daily-loss caps** in your strategy config.
3. Have a **kill switch** — a keyboard interrupt path that halts new entries and closes open positions.
4. Run your own node or dedicated RPC — the public RPC WILL throttle you and cost you fills.
5. Monitor **off-chain**: Telegram bot for trade alerts + daily PnL summary. See `.env.example` for `TELEGRAM_*` vars.

## Wallet compromise recovery

If you suspect a key leak (plaintext in a cloud backup, a git commit, a screenshot, a malicious dep):

1. **Sweep funds immediately** — even before rotating keys, move everything to a fresh wallet funded through a CEX hop.
2. Generate the new wallet on an **offline machine** if possible.
3. Rotate ALL keys that touched the same machine, not just the leaked one.
4. Review every `.env`, `.bashrc`, `~/Downloads/`, `~/Documents/` for other secrets.
5. Revoke exposed API keys (Helius, Alchemy, etc).

## Disclaimer

This is free software provided under the MIT license. **None of this is financial advice.** You are solely responsible for any capital you put through these bots. Paper-trade first. Read the code before running it live.
