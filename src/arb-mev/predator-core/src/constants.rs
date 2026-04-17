//! Constants for the PREDATOR Solana liquidation & arbitrage bot.
//!
//! Every program ID, market address, tip account, and endpoint in this file
//! is sourced from [VERIFIED 2026] research files. The exact research file and
//! section are cited in the doc-comment above each constant group.
//!
//! All addresses are `&str` literals parsed to `Pubkey` at runtime via
//! `Pubkey::from_str()` to keep this crate dependency-light.

// ---------------------------------------------------------------------------
// 1. SYSTEM PROGRAMS
// [scanner_deep_research_2026.md, config.rs — verified on-chain April 2026]
// ---------------------------------------------------------------------------

/// Native System Program.
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

/// SPL Token program (original, non-2022).
pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Associated Token Account program — derives ATAs.
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// Sysvar: Instructions — required by flash-loan programs for introspection.
pub const SYSVAR_INSTRUCTIONS: &str = "Sysvar1nstructions1111111111111111111111111";

/// Sysvar: Rent — needed for account creation size calculations.
pub const SYSVAR_RENT: &str = "SysvarRent111111111111111111111111111111111";

// ---------------------------------------------------------------------------
// 1b. ORACLE PROGRAMS
// [VERIFIED 2026 pyth_post_update_atomic_2026.md s1]
// ---------------------------------------------------------------------------

/// Pyth Receiver program — handles PostUpdateAtomic instruction to update oracle prices on-chain.
/// [VERIFIED 2026 pyth_post_update_atomic_2026.md s1: "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ"]
pub const PYTH_RECEIVER_PROGRAM: &str = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";

/// Pyth Push Oracle program — owns price feed accounts.
/// [VERIFIED 2026 pyth_post_update_atomic_2026.md s1]
pub const PYTH_PUSH_ORACLE_PROGRAM: &str = "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT";

/// Wormhole Core Bridge program — guardian set verification for PostUpdateAtomic.
/// [VERIFIED 2026 pyth_post_update_atomic_2026.md s1: "worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth"]
pub const WORMHOLE_PROGRAM: &str = "worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth";

/// Pyth Hermes API — fetches binary VAA data for PostUpdateAtomic.
/// [VERIFIED 2026 pyth_post_update_atomic_2026.md s10]
pub const HERMES_URL: &str = "https://hermes.pyth.network/v2/updates/price/latest";

/// PostUpdateAtomic Anchor discriminator: sha256("global:post_update_atomic")[..8]
/// [VERIFIED 2026 pyth_post_update_atomic_2026.md s2]
pub const POST_UPDATE_ATOMIC_DISC: [u8; 8] = [0x31, 0xac, 0x54, 0xc0, 0xaf, 0xb4, 0x34, 0xea];

/// Default treasury ID for PostUpdateAtomic (most common).
/// [VERIFIED 2026 pyth_post_update_atomic_2026.md s4]
pub const PYTH_TREASURY_ID: u8 = 0;

/// Compute Budget program — `SetComputeUnitLimit` / `SetComputeUnitPrice`.
pub const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";

// ---------------------------------------------------------------------------
// 2. TOKEN MINTS
// [config.rs, fast_scanner.rs, lst_depeg_arb.rs — all verified on-chain]
// ---------------------------------------------------------------------------

/// Wrapped SOL (native mint).
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// USDC (Circle, SPL).
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// USDT (Tether, SPL).
pub const USDT_MINT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";

/// Jito Staked SOL (jitoSOL).
/// [fast_scanner.rs, lst_depeg_arb.rs — VERIFIED 2026]
pub const JITOSOL_MINT: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";

/// Marinade Staked SOL (mSOL).
/// [fast_scanner.rs, lst_depeg_arb.rs — VERIFIED 2026]
pub const MSOL_MINT: &str = "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So";

/// BlazeStake SOL (bSOL).
/// [fast_scanner.rs, lst_depeg_arb.rs — VERIFIED 2026]
pub const BSOL_MINT: &str = "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1";

/// Jupiter Staked SOL (jupSOL).
/// [flash_arb.rs, lst_depeg_arb.rs — VERIFIED 2026]
pub const JUPSOL_MINT: &str = "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v";

// ---------------------------------------------------------------------------
// 3. LENDING PROTOCOLS — SAVE (formerly Solend)
// [scanner_deep_research_2026.md Section 1, save_market_discovery_2026.md]
// Program ID + 5 open-liquidation markets — VERIFIED 2026
// ---------------------------------------------------------------------------

/// Save (Solend) lending program.
/// [scanner_deep_research_2026.md line 50 — VERIFIED 2026]
pub const SAVE_PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

/// Save Main market — 271,093 obligations, 89 reserves.
/// [save_market_discovery_2026.md line 30 — VERIFIED 2026]
pub const SAVE_MAIN_MARKET: &str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";

/// Save TURBO SOL market — 5,309 obligations, SOL+USDC.
/// [save_market_discovery_2026.md line 31 — VERIFIED 2026]
pub const SAVE_TURBO_MARKET: &str = "7RCz8wb6WXxUhAigok9ttgrVgDFFFbibcirECzWSBauM";

/// Save BONK market — 1,737 obligations.
/// [save_market_discovery_2026.md line 32 — VERIFIED 2026]
pub const SAVE_BONK_MARKET: &str = "GnCfohzaT8uCYsn4ybcsfUZJ8BUuz9F82VivJSW9NMnj";

/// Save Stable market — 1,566 obligations.
/// [save_market_discovery_2026.md line 33 — VERIFIED 2026]
pub const SAVE_STABLE_MARKET: &str = "GktVYgkstojYd8nVXGXKJHi7SstvgZ6pkQqQhUPD7y7Q";

/// Save JLP market — 1,392 obligations.
/// [save_market_discovery_2026.md line 34 — VERIFIED 2026]
pub const SAVE_JLP_MARKET: &str = "7XttJ7hp83u5euzT7ybC5zsjdgKA4WPbQHVS27CATAJH";

/// All 5 Save markets as an array for iteration.
pub const SAVE_MARKETS: [&str; 5] = [
    SAVE_MAIN_MARKET,
    SAVE_TURBO_MARKET,
    SAVE_BONK_MARKET,
    SAVE_STABLE_MARKET,
    SAVE_JLP_MARKET,
];

// ---------------------------------------------------------------------------
// 4. LENDING PROTOCOLS — KAMINO (klend)
// [scanner_deep_research_2026.md Section 2, batch1_gap_resolution_2026.md GAP #1]
// 9 markets total. Only Main + Jito have confirmed on-chain addresses.
// Remaining 7 require on-chain discovery via getProgramAccounts.
// ---------------------------------------------------------------------------

/// Kamino Lend (klend) program.
/// [scanner_deep_research_2026.md line 81 — VERIFIED 2026]
pub const KAMINO_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

/// Kamino Main market — $2.08B supply, $803M debt (Jan 2026).
/// [scanner_deep_research_2026.md line 97 — VERIFIED 2026]
pub const KAMINO_MAIN_MARKET: &str = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";

/// Kamino Jito market — $97M supply, $45M debt. $27.2M whale position.
/// 0% protocol liq fee — FULL bonus to liquidator.
/// [scanner_deep_research_2026.md line 101, config.rs — VERIFIED 2026]
pub const KAMINO_JITO_MARKET: &str = "H6rHXmXoCQvq8Ue81MqNh7ow5ysPa1dSozwW3PU1dDH6";

/// Kamino JLP market — $295M supply (Jan 2026). Address from config.rs.
/// [config.rs line 50 — VERIFIED 2026]
pub const KAMINO_JLP_MARKET: &str = "DxXdAyU3kCjnyggvHmY5nAwg5cRbbmdyX3npfDMjjMek";

/// Kamino Altcoins market — address from config.rs.
/// [config.rs line 51 — VERIFIED 2026]
pub const KAMINO_ALTCOINS_MARKET: &str = "ByYiZxp8QrdN9qbdtaAiePN8AAr3qvTPppNJDpf5DVJ5";

// The following 5 Kamino markets are known to exist from governance reports
// but their on-chain addresses have NOT been discovered yet.
// [batch1_gap_resolution_2026.md GAP #1 — NEEDS ON-CHAIN DISCOVERY]
//
// Market         | Supply   | Debt   | Status
// ---------------+----------+--------+-----------------------
// Prime          | $575M    | $257M  | ADDRESS UNKNOWN
// Maple          | $262M    | $100M  | ADDRESS UNKNOWN
// OnRe           | $71M     | $22M   | ADDRESS UNKNOWN
// Marinade       | $15M     | $7M    | ADDRESS UNKNOWN
// Solstice       | $35M     | $10M   | ADDRESS UNKNOWN
// Superstate     | $45M     | $17M   | ADDRESS UNKNOWN
//
// To discover: `getProgramAccounts` on KAMINO_PROGRAM with LendingMarket
// discriminator filter. One-time RPC call.

/// Known Kamino markets for scanning. Expand as addresses are discovered.
pub const KAMINO_MARKETS: [&str; 4] = [
    KAMINO_MAIN_MARKET,
    KAMINO_JITO_MARKET,
    KAMINO_JLP_MARKET,
    KAMINO_ALTCOINS_MARKET,
];

/// Kamino Farms program — needed for farm accounts in liquidation instructions.
/// [config.rs line 47 — VERIFIED 2026]
pub const KAMINO_FARMS_PROGRAM: &str = "FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr";

// ---------------------------------------------------------------------------
// 5. LENDING PROTOCOLS — MARGINFI / PROJECT 0
// [scanner_deep_research_2026.md Section 4, batch1_gap_resolution_2026.md GAP #3]
// LIVE under Project 0 rebrand. 0% flash loan fee. 2.5% liquidator bonus.
// ---------------------------------------------------------------------------

/// MarginFi v2 program (unchanged under P0 rebrand).
/// [scanner_deep_research_2026.md line 157, batch1_gap_resolution_2026.md — VERIFIED 2026]
pub const MARGINFI_PROGRAM: &str = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";

/// MarginFi main group account.
/// [scanner_deep_research_2026.md line 158 — VERIFIED 2026]
pub const MARGINFI_GROUP: &str = "4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8";

// ---------------------------------------------------------------------------
// 6. LENDING PROTOCOLS — JUPITER LEND
// [scanner_deep_research_2026.md Section 3 — VERIFIED 2026]
// 0.1% liquidation penalty, tick-based health, 0% flash loan fee.
// ---------------------------------------------------------------------------

/// Jupiter Lend Vaults program.
/// [scanner_deep_research_2026.md line 126 — VERIFIED 2026]
pub const JUPLEND_PROGRAM: &str = "jupr81YtYssSyPt8jbnGuiWon5f6x9TcDEFxYe3Bdzi";

/// Jupiter flashloan program (separate from vaults).
/// [config.rs line 8 — VERIFIED 2026]
pub const JUPITER_FLASHLOAN_PROGRAM: &str = "jupgfSgfuAXv4B6R2Uxu85Z1qdzgju79s6MfZekN6XS";

// ---------------------------------------------------------------------------
// 7. DEX PROGRAMS
// [config.rs lines 14-18, dex_token_coverage_2026.md — VERIFIED 2026]
// ---------------------------------------------------------------------------

/// Orca Whirlpool concentrated liquidity AMM.
pub const ORCA_WHIRLPOOL: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

/// Raydium AMM V4 (legacy constant-product).
pub const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// Raydium Concentrated Liquidity Market Maker.
pub const RAYDIUM_CLMM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

/// Raydium Constant Product Market Maker (CPMM).
pub const RAYDIUM_CPMM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";

/// Meteora Dynamic Liquidity Market Maker.
pub const METEORA_DLMM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

/// PumpSwap AMM — Pump.fun's native DEX for graduated tokens.
/// [dex_token_coverage_2026.md line 64, backrun_arb_strategies_2026.md line 283 — VERIFIED 2026]
pub const PUMPSWAP_AMM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// Pump.fun bonding curve program (pre-graduation).
/// [dex_token_coverage_2026.md line 63 — VERIFIED 2026]
pub const PUMPFUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

// ---------------------------------------------------------------------------
// 8. JITO BUNDLE INFRASTRUCTURE
// [config.rs lines 59-73, jito_multiregion_2026.md, jito_bundle_landing_2026.md]
// ---------------------------------------------------------------------------

/// Primary Jito block engine base URL.
pub const JITO_BLOCK_ENGINE: &str = "https://mainnet.block-engine.jito.wtf";

/// Primary Jito bundle submission endpoint (JSON-RPC `sendBundle`).
pub const JITO_BUNDLE_URL: &str = "https://mainnet.block-engine.jito.wtf/api/v1/bundles";

/// Jito tip floor API — returns percentile tip data.
/// [jito_bundle_landing_2026.md line 161 — VERIFIED 2026]
pub const JITO_TIP_FLOOR_URL: &str = "https://bundles-api-rest.jito.wtf/api/v1/bundles/tip_floor";

/// 8 Jito tip accounts — send a SOL transfer to any ONE per bundle.
/// These are the LIVE tip accounts verified from the `getTipAccounts` API on
/// 2026-04-07. Three accounts were corrected from earlier wrong values.
/// [config.rs lines 64-73 — VERIFIED 2026]
pub const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// 6 Jito regional bundle endpoints for concurrent multi-region submission.
/// Same bundle deduplicates by SHA-256 hash — safe to send to all.
/// [jito_multiregion_2026.md Section 1.3, jito_bundle_landing_2026.md Section 4
///  — VERIFIED 2026]
pub const JITO_REGIONAL_ENDPOINTS: [&str; 6] = [
    "https://mainnet.block-engine.jito.wtf/api/v1/bundles",
    "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles",
    "https://slc.mainnet.block-engine.jito.wtf/api/v1/bundles",
    "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1/bundles",
    "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1/bundles",
    "https://tokyo.mainnet.block-engine.jito.wtf/api/v1/bundles",
];

// ---------------------------------------------------------------------------
// 9. NOZOMI (Temporal) — ALTERNATIVE FAST SUBMISSION PATH
// [batch1_gap_resolution_2026.md GAPs #18 & #19 — VERIFIED 2026]
// 17 tip accounts. Use a RANDOM address per tx to avoid write-lock CU exhaustion.
// Minimum tip: 0.001 SOL (100,000 lamports).
// Source: github.com/weeaa/tempgoral/blob/main/constants.go
// ---------------------------------------------------------------------------

/// 17 Nozomi tip accounts — pick one at random per transaction.
/// [batch1_gap_resolution_2026.md lines 293-309 — VERIFIED 2026]
pub const NOZOMI_TIP_ACCOUNTS: [&str; 17] = [
    "TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq",
    "noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4",
    "noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE",
    "noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo",
    "noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ",
    "nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L",
    "nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z",
    "nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu",
    "noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7",
    "nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP",
    "nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P",
    "nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge",
    "nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3",
    "nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ",
    "nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk",
    "nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne",
    "nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb",
];

/// 4 confirmed Nozomi regional HTTP endpoints (append connection param `?c=`).
/// [batch1_gap_resolution_2026.md lines 260-263 — VERIFIED 2026]
pub const NOZOMI_ENDPOINTS: [&str; 4] = [
    "https://pit1.secure.nozomi.temporal.xyz",
    "https://fra2.secure.nozomi.temporal.xyz",
    "https://ewr1.secure.nozomi.temporal.xyz",
    "https://ams1.secure.nozomi.temporal.xyz",
];

/// Nozomi WebSocket tip stream for monitoring competitive tips.
/// [batch1_gap_resolution_2026.md line 276 — VERIFIED 2026]
pub const NOZOMI_TIP_STREAM_WS: &str = "wss://api.nozomi.temporal.xyz/tip_stream";

/// Nozomi minimum tip in lamports (0.001 SOL).
/// [batch1_gap_resolution_2026.md line 312 — VERIFIED 2026]
pub const NOZOMI_MIN_TIP_LAMPORTS: u64 = 100_000;

// ---------------------------------------------------------------------------
// 10. JUPITER V2 API
// [batch1_gap_resolution_2026.md GAP #16, clmm_swap_fix_2026.md — VERIFIED 2026]
// V2 /build returns raw swap instructions for custom tx assembly (flash loans).
// V2 /quote is the standard quote endpoint.
// Requires `x-api-key` header from dev.jup.ag dashboard.
// ---------------------------------------------------------------------------

/// Jupiter V2 quote endpoint.
/// [execution_pipeline_deep_2026.md — VERIFIED 2026]
pub const JUPITER_V2_QUOTE_URL: &str = "https://api.jup.ag/swap/v2/quote";

/// Jupiter V2 /build endpoint — returns raw instructions for custom tx assembly.
/// Replaces the old 2-step /quote + /swap-instructions flow.
/// [batch1_gap_resolution_2026.md GAP #16 — VERIFIED 2026]
pub const JUPITER_V2_BUILD_URL: &str = "https://api.jup.ag/swap/v2/build";

/// Jupiter V1 quote endpoint (legacy Metis, still functional).
pub const JUPITER_V1_QUOTE_URL: &str = "https://api.jup.ag/swap/v1/quote";

/// Jupiter V1 swap instructions endpoint (legacy).
pub const JUPITER_V1_SWAP_IX_URL: &str = "https://api.jup.ag/swap/v1/swap-instructions";

// ---------------------------------------------------------------------------
// 11. COMPUTE UNIT LIMITS
// [config.rs lines 102-109 — based on on-chain observation, VERIFIED 2026]
// ---------------------------------------------------------------------------

/// CU budget for flash-loan-wrapped liquidation (Jito bundle):
/// flash_borrow + liquidate + swap + flash_repay = ~800K-1.2M CU.
/// [config.rs line 107 — VERIFIED 2026]
pub const CU_FLASH_LIQ: u32 = 1_200_000;

/// CU budget for Save liquidation:
/// flash_borrow + N*refresh_reserve + refresh_obligation + liquidate + swap + flash_repay.
/// [config.rs line 105 — VERIFIED 2026]
pub const CU_SAVE_LIQ: u32 = 1_000_000;

/// CU budget for Kamino liquidation. Same structure as Save but larger obligation.
/// Kamino obligation is 3344 bytes; refresh is heavier. Budget conservatively.
pub const CU_KAMINO_LIQ: u32 = 1_200_000;

/// CU budget for MarginFi liquidation:
/// start_liq + withdraw + repay + end_liq = ~300-400K CU.
/// [config.rs line 103 — VERIFIED 2026]
pub const CU_MARGINFI_LIQ: u32 = 400_000;

/// Default priority fee in micro-lamports per CU.
/// Overridden at runtime by dynamic fee from QuickNode Priority Fee API.
/// [config.rs line 109 — VERIFIED 2026]
pub const DEFAULT_PRIORITY_FEE_MICRO_LAMPORTS: u64 = 50_000;

// ---------------------------------------------------------------------------
// 12. FEE & PROFIT CONSTANTS
// [config.rs lines 83-99 — VERIFIED 2026]
// ---------------------------------------------------------------------------

/// Jito tip as a fraction of profit (50%).
/// [config.rs line 86 — VERIFIED 2026]
pub const JITO_TIP_PERCENT: f64 = 0.50;

/// Jito minimum tip in lamports. 50K lamports — below this, bundles never win.
/// [config.rs line 87 — VERIFIED 2026]
pub const JITO_MIN_TIP_LAMPORTS: u64 = 50_000;

/// Minimum profit (in lamports) to execute any transaction.
/// TX cost ~ 5000 base + (CU * priority_fee / 1e6) + Jito tip.
/// At 50K priority, 1.2M CU: cost ~ 5000 + 60000 + tip = ~70K lamports.
/// [config.rs line 99 — VERIFIED 2026]
pub const MIN_PROFIT_LAMPORTS: u64 = 100_000;

/// Minimum liquidation profit threshold in lamports (0.00001 SOL).
/// [config.rs line 91 — VERIFIED 2026]
pub const MIN_LIQUIDATION_PROFIT_LAMPORTS: u64 = 10_000;

/// Minimum profit in basis points for flash arb opportunities.
/// 0.1% — even tiny profits work with 0% flash fee.
/// [config.rs line 83 — VERIFIED 2026]
pub const MIN_PROFIT_BPS: f64 = 1.0;

// ---------------------------------------------------------------------------
// 13. ACCOUNT SIZES (bytes) — for gRPC dataSize filters & buffer allocation
// [scanner_deep_research_2026.md Sections 1-4 — VERIFIED 2026]
// ---------------------------------------------------------------------------

/// Save (Solend) obligation account size.
/// [scanner_deep_research_2026.md line 57 — VERIFIED 2026]
pub const SAVE_OBLIGATION_SIZE: usize = 1300;

/// Save LendingMarket account size.
/// [save_market_discovery_2026.md line 4 — VERIFIED 2026]
pub const SAVE_LENDING_MARKET_SIZE: usize = 290;

/// Kamino (klend) obligation account size.
/// [scanner_deep_research_2026.md line 87 — VERIFIED 2026]
pub const KAMINO_OBLIGATION_SIZE: usize = 3344;

/// Kamino obligation discriminator (first 8 bytes).
/// [scanner_deep_research_2026.md line 88 — VERIFIED 2026]
pub const KAMINO_OBLIGATION_DISCRIMINATOR: [u8; 8] = [168, 206, 141, 106, 88, 76, 172, 167];

/// MarginFi account size (2304 body + 8 discriminator).
/// [scanner_deep_research_2026.md line 163 — VERIFIED 2026]
pub const MARGINFI_ACCOUNT_SIZE: usize = 2312;

/// MarginFi balance slot size (16 slots per account, 104 bytes each).
/// [scanner_deep_research_2026.md line 164 — VERIFIED 2026]
pub const MARGINFI_BALANCE_SLOT_SIZE: usize = 104;

/// Jupiter Lend position size (8 discriminator + 63 data).
/// [scanner_deep_research_2026.md line 131 — VERIFIED 2026]
pub const JUPLEND_POSITION_SIZE: usize = 71;

/// Jupiter Lend position discriminator.
/// [scanner_deep_research_2026.md line 132 — VERIFIED 2026]
pub const JUPLEND_POSITION_DISCRIMINATOR: [u8; 8] = [170, 188, 143, 228, 122, 64, 247, 208];

// ---------------------------------------------------------------------------
// 14. HEALTH CHECK OFFSETS — byte offsets within obligation/position accounts
// [scanner_deep_research_2026.md Sections 1-2 — VERIFIED 2026]
// ---------------------------------------------------------------------------

/// Save: borrowed_value field offset in obligation (Decimal u128).
/// [scanner_deep_research_2026.md line 58 — VERIFIED 2026]
pub const SAVE_BORROWED_VALUE_OFFSET: usize = 90;

/// Save: unhealthy_borrow_value field offset in obligation (Decimal u128).
/// [scanner_deep_research_2026.md line 58 — VERIFIED 2026]
pub const SAVE_UNHEALTHY_VALUE_OFFSET: usize = 122;

/// Kamino: debt_value_sf field offset in obligation (u128 scaled fraction).
/// [scanner_deep_research_2026.md line 89 — VERIFIED 2026]
pub const KAMINO_DEBT_VALUE_OFFSET: usize = 2208;

/// Kamino: unhealthy_value_sf field offset in obligation (u128 scaled fraction).
/// [scanner_deep_research_2026.md line 89 — VERIFIED 2026]
pub const KAMINO_UNHEALTHY_VALUE_OFFSET: usize = 2256;

// ---------------------------------------------------------------------------
// 15. PROTOCOL PARAMETERS — liquidation bonuses, close factors, fees
// [batch1_gap_resolution_2026.md GAPs #2, #6, #20 — VERIFIED 2026]
// ---------------------------------------------------------------------------

/// Save liquidation bonus: 5% gross, ~3.5% net after protocol fee.
/// Highest on Solana for standard lending.
/// [scanner_deep_research_2026.md line 52 — VERIFIED 2026]
pub const SAVE_LIQUIDATION_BONUS_BPS: u16 = 500;

/// Save close factor: 20% of debt per liquidation.
/// [scanner_deep_research_2026.md line 54 — VERIFIED 2026]
pub const SAVE_CLOSE_FACTOR_PCT: u8 = 20;

/// Save flash loan fee: 5 bps (0.05%). Was 30 bps pre-Dec 2023.
/// [batch1_gap_resolution_2026.md GAP #20 — VERIFIED 2026]
pub const SAVE_FLASH_LOAN_FEE_BPS: u16 = 5;

/// Kamino liquidation bonus: 2% minimum, 10% maximum (sliding scale based on health).
/// bonus_bps = min + (max - min) * (1 - health_factor).
/// [batch1_gap_resolution_2026.md GAP #2 — VERIFIED 2026]
pub const KAMINO_MIN_LIQUIDATION_BONUS_BPS: u16 = 200;
pub const KAMINO_MAX_LIQUIDATION_BONUS_BPS: u16 = 1000;

/// Kamino close factor: 50% max debt repayable per liquidation.
/// The "10% increments" is an unwinding parameter, NOT the close factor.
/// [batch1_gap_resolution_2026.md GAP #6 — VERIFIED 2026]
pub const KAMINO_CLOSE_FACTOR_PCT: u8 = 50;

/// Kamino flash loan fee: 0.001% (1 bp = 0.01%, so this is 0.1 bps).
/// [scanner_deep_research_2026.md line 86 — VERIFIED 2026]
pub const KAMINO_FLASH_LOAN_FEE_BPS: f64 = 0.1;

/// MarginFi liquidation bonus: 5% total (2.5% to liquidator, 2.5% to insurance fund).
/// [scanner_deep_research_2026.md line 160, batch1_gap_resolution_2026.md — VERIFIED 2026]
pub const MARGINFI_LIQUIDATION_BONUS_BPS: u16 = 500;
pub const MARGINFI_LIQUIDATOR_BONUS_BPS: u16 = 250;

/// MarginFi flash loan fee: 0% (free).
/// [batch1_gap_resolution_2026.md GAP #23 — VERIFIED 2026]
pub const MARGINFI_FLASH_LOAN_FEE_BPS: u16 = 0;

/// Jupiter Lend liquidation penalty: 0.1% (1 bp).
/// [scanner_deep_research_2026.md line 128 — VERIFIED 2026]
pub const JUPLEND_LIQUIDATION_PENALTY_BPS: u16 = 10;

/// Jupiter Lend flash loan fee: 0%.
/// [batch1_gap_resolution_2026.md — VERIFIED 2026]
pub const JUPLEND_FLASH_LOAN_FEE_BPS: u16 = 0;

// ---------------------------------------------------------------------------
// 16. SCANNING INTERVALS
// [config.rs line 90 — VERIFIED 2026]
// ---------------------------------------------------------------------------

/// How often to re-scan all obligations (seconds).
/// 97K obligations takes ~9s to scan.
/// [config.rs line 90 — VERIFIED 2026]
pub const LIQUIDATION_SCAN_INTERVAL_SECS: u64 = 15;

// ---------------------------------------------------------------------------
// 17. TRANSACTION LIMITS
// ---------------------------------------------------------------------------

/// Solana transaction size limit (bytes). 1232 bytes for legacy, 1644 for v0.
pub const MAX_TX_SIZE_LEGACY: usize = 1232;
pub const MAX_TX_SIZE_V0: usize = 1644;

/// Maximum number of accounts per transaction (64 with address lookup tables).
pub const MAX_ACCOUNTS_PER_TX: usize = 64;

/// Jito bundle maximum: 5 transactions per bundle.
pub const MAX_BUNDLE_SIZE: usize = 5;
