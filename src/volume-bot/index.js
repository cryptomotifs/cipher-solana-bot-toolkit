/**
 * Sol Volume Bot v3 — Direct PumpSwap + Jito Bundles + Multi-Wallet
 *
 * Architecture (S-tier, verified from research):
 * - Jito bundles execute SEQUENTIALLY — TX2 sees TX1's state
 * - So fund+swap can be in the SAME bundle — no need to confirm fund first
 * - 4 wallets per bundle, 1 tip, atomic execution
 *
 * Bundle structure (up to 5 TXs):
 * TX1: Main wallet funds all ephemeral wallets
 * TX2: Wallet A buy+sell (same tx)
 * TX3: Wallet B buy+sell (same tx)
 * TX4: Wallet C buy+sell (same tx)
 * TX5: Wallet D buy+sell + return all SOL + Jito tip
 *
 * Uses @pump-fun/pump-swap-sdk for correct instruction building
 */

import {
  Keypair, PublicKey, SystemProgram, Transaction, TransactionMessage,
  VersionedTransaction, Connection, LAMPORTS_PER_SOL, ComputeBudgetProgram,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID, ASSOCIATED_TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync, createAssociatedTokenAccountIdempotentInstruction,
  createCloseAccountInstruction, createSyncNativeInstruction,
} from '@solana/spl-token';
import { OnlinePumpAmmSdk, PumpAmmSdk, canonicalPumpPoolPda, PUMP_AMM_PROGRAM_ID } from '@pump-fun/pump-swap-sdk';
import BN from 'bn.js';
import bs58 from 'bs58';
import fs from 'fs';
import dotenv from 'dotenv';
import axios from 'axios';
import express from 'express';
import { WebSocketServer } from 'ws';
import http from 'http';
import { createInterface } from 'readline';

dotenv.config();

const WSOL = new PublicKey('So11111111111111111111111111111111111111112');
const JITO_ENDPOINTS = [
  'https://mainnet.block-engine.jito.wtf',
  'https://amsterdam.mainnet.block-engine.jito.wtf',
  'https://frankfurt.mainnet.block-engine.jito.wtf',
  'https://ny.mainnet.block-engine.jito.wtf',
  'https://tokyo.mainnet.block-engine.jito.wtf',
];
const JITO_TIP_ACCOUNTS = [
  '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5',
  'HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe',
  'Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY',
  'ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49',
];

// ===== SETUP =====
const connection = new Connection(process.env.RPC_URL || 'https://api.mainnet-beta.solana.com', 'confirmed');
let mainKeypair;

// Load wallet
const privKey = process.env.PRIVATE_KEY;
if (privKey) {
  try {
    mainKeypair = Keypair.fromSecretKey(bs58.default ? bs58.default.decode(privKey) : bs58.decode(privKey));
  } catch {
    // Try as JSON array
    const bytes = JSON.parse(fs.readFileSync(privKey, 'utf8'));
    mainKeypair = Keypair.fromSecretKey(Uint8Array.from(bytes));
  }
} else if (process.env.WALLET_PATH) {
  const bytes = JSON.parse(fs.readFileSync(process.env.WALLET_PATH, 'utf8'));
  mainKeypair = Keypair.fromSecretKey(Uint8Array.from(bytes));
}

if (!mainKeypair) {
  console.error('Set PRIVATE_KEY or WALLET_PATH in .env');
  process.exit(1);
}
console.log(`Wallet: ${mainKeypair.publicKey.toBase58()}`);

// Initialize PumpSwap SDK (Online version fetches state from chain)
const pumpSdk = new PumpAmmSdk();
const onlineSdk = new OnlinePumpAmmSdk(connection);

// Ledger
const LEDGER_FILE = 'wallet_ledger.json';
let ledger = [];
try { ledger = JSON.parse(fs.readFileSync(LEDGER_FILE, 'utf8')); } catch {}
function saveLedger() { fs.writeFileSync(LEDGER_FILE, JSON.stringify(ledger, null, 2)); }
function logWallet(entry) { ledger.push({ ...entry, ts: new Date().toISOString() }); saveLedger(); }

// ===== POOL DISCOVERY =====
async function findPumpSwapPool(tokenMint) {
  const baseMint = new PublicKey(tokenMint);

  // Try canonical pool PDA first
  let poolAddress;
  try {
    const [pda] = canonicalPumpPoolPda(baseMint, WSOL, PUMP_AMM_PROGRAM_ID);
    poolAddress = pda;
  } catch {
    // Fallback: get from DexScreener
    const resp = await axios.get(`https://api.dexscreener.com/latest/dex/tokens/${tokenMint}`, { timeout: 10000 });
    const pair = resp.data.pairs?.find(p => p.dexId === 'pumpswap');
    if (!pair) throw new Error('No PumpSwap pool found');
    poolAddress = new PublicKey(pair.pairAddress);
  }

  // Verify pool exists on-chain
  const poolAccount = await connection.getAccountInfo(poolAddress);
  if (!poolAccount) throw new Error('Pool account not found on-chain');

  // Get liquidity info from DexScreener
  let liquidity = null;
  try {
    const resp = await axios.get(`https://api.dexscreener.com/latest/dex/tokens/${tokenMint}`, { timeout: 5000 });
    liquidity = resp.data.pairs?.find(p => p.dexId === 'pumpswap')?.liquidity;
  } catch {}

  return { poolAddress, baseMint, quoteMint: WSOL, liquidity };
}

// ===== BUILD SWAP INSTRUCTIONS =====

/**
 * Build buy+sell instructions for one ephemeral wallet using PumpSwap SDK
 * Uses OnlinePumpAmmSdk.swapSolanaState to get the full state needed
 */
async function buildSwapInstructions(pool, ephemeralPubkey, solAmountLamports, slippageBps) {
  const slippage = slippageBps / 10000; // convert bps to decimal

  // Get swap state from online SDK — this fetches all accounts, reserves, configs
  const swapState = await onlineSdk.swapSolanaState(pool.poolAddress, ephemeralPubkey);

  const ixs = [];

  // The SDK's buyQuoteInput/sellBaseInput return instructions WITH the needed
  // account setup (WSOL wrap, ATA creation) via withWsolAccounts internally.

  // 1. BUY: spend exact SOL amount, get tokens
  const quoteAmount = new BN(solAmountLamports.toString());
  const buyResult = await pumpSdk.buyQuoteInput(swapState, quoteAmount, slippage);
  const buyIxs = Array.isArray(buyResult) ? buyResult :
    buyResult?.instructions ? buyResult.instructions : [buyResult];
  ixs.push(...buyIxs.filter(Boolean));

  // 2. SELL: estimate tokens from constant product formula, sell (slightly less than) all back.
  // PumpSwap takes a ~1% fee on the quote (SOL) input to the buy, so the pool's k math sees
  // only ~99% of quoteAmount. We also apply an extra 3% safety discount so rounding and
  // protocol-fee variance never leave us selling more tokens than we actually hold
  // (that would fail the sell's token transfer with Custom:1 / insufficient balance).
  const poolBase = new BN(swapState.poolBaseAmount.toString());
  const poolQuote = new BN(swapState.poolQuoteAmount.toString());
  const netQuote = quoteAmount.muln(99).divn(100); // 1% pump fee
  const estimatedTokensGross = poolBase.mul(netQuote).div(poolQuote.add(netQuote));
  const estimatedTokens = estimatedTokensGross.muln(95).divn(100); // 5% safety discount
  const sellAmount = estimatedTokens.gtn(0) ? estimatedTokens : new BN(1);

  const sellResult = await pumpSdk.sellBaseInput(swapState, sellAmount, slippage);
  const sellIxs = Array.isArray(sellResult) ? sellResult :
    sellResult?.instructions ? sellResult.instructions : [sellResult];
  ixs.push(...sellIxs.filter(Boolean));

  // 3. Close the user_volume_accumulator PDA to reclaim its rent (~1,844,400 lamports).
  // Verified against node_modules/@pump-fun/pump-swap-sdk/dist/sdk/offlinePumpAmm.d.ts:48
  // (PumpAmmSdk.closeUserVolumeAccumulator(user: PublicKey): Promise<TransactionInstruction>)
  // and the IDL in dist/esm/sdk/offlinePumpAmm.js lines 3573-3630 (accounts: user[w,s] +
  // user_volume_accumulator PDA + event_authority, empty args). Anchor's close constraint
  // refunds the PDA rent to `user` in the same TX, after which our post-cycle sweep
  // recovers it to the main wallet. SDK method returns a single instruction, not an array.
  try {
    const closeAccumIx = await pumpSdk.closeUserVolumeAccumulator(ephemeralPubkey);
    if (closeAccumIx) ixs.push(closeAccumIx);
  } catch (e) {
    // If the SDK refuses (unexpected account state), skip the optimization rather than
    // failing the whole swap — we'd just leave the 1.844M lamports stranded as before.
    console.warn(`  [closeUserVolumeAccumulator skipped] ${e.message}`);
  }

  return ixs;
}

// ===== JITO BUNDLE =====

const b58encode = (bytes) => (bs58.default ? bs58.default.encode(bytes) : bs58.encode(bytes));
const b58decode = (str) => (bs58.default ? bs58.default.decode(str) : bs58.decode(str));

async function sendJitoBundle(transactions) {
  const encoded = transactions.map(tx => b58encode(tx.serialize()));
  const body = { jsonrpc: '2.0', id: Date.now(), method: 'sendBundle', params: [encoded] };

  const errors = [];
  for (const ep of JITO_ENDPOINTS) {
    try {
      const resp = await axios.post(`${ep}/api/v1/bundles`, body, { timeout: 15000 });
      if (resp.data.error) {
        errors.push(`${ep.replace('https://', '')}: ${resp.data.error.message}`);
        continue;
      }
      return { success: true, bundleId: resp.data.result, endpoint: ep };
    } catch (e) {
      const msg = e.response?.data?.error?.message || e.code || e.message;
      errors.push(`${ep.replace('https://', '')}: ${msg}`);
    }
  }
  return { success: false, error: errors.join(' | ') };
}

async function getBundleStatus(bundleId, endpoint) {
  try {
    const resp = await axios.post(`${endpoint}/api/v1/getBundleStatuses`, {
      jsonrpc: '2.0', id: 1, method: 'getBundleStatuses', params: [[bundleId]],
    }, { timeout: 5000 });
    const v = resp.data?.result?.value?.[0];
    return v || null;
  } catch { return null; }
}

async function waitForBundleLanding(bundleId, endpoint, signatures, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const status = await getBundleStatus(bundleId, endpoint);
    if (status && (status.confirmation_status === 'confirmed' || status.confirmation_status === 'finalized')) {
      return { landed: true, via: 'bundle-status', slot: status.slot };
    }
    try {
      const sigStatuses = await connection.getSignatureStatuses(signatures);
      const allConfirmed = sigStatuses.value.every(s =>
        s && (s.confirmationStatus === 'confirmed' || s.confirmationStatus === 'finalized')
      );
      if (allConfirmed) return { landed: true, via: 'sig-status' };
    } catch {}
    await new Promise(r => setTimeout(r, 1000));
  }
  return { landed: false };
}

// ===== VOLUME CYCLE =====

/**
 * Run one volume cycle with N wallets in a single Jito bundle
 *
 * TX1: Fund all wallets from main
 * TX2-N: Each wallet does buy+sell+close+return
 * Last TX includes Jito tip
 */
async function runVolumeCycle(pool, config) {
  const { walletsPerBundle, solAmountLamports, slippageBps, tipLamports, priorityMicroLamports } = config;
  const numWallets = Math.min(walletsPerBundle, 4); // max 4 (bundle limit is 5 TXs: 1 fund + 4 swaps)

  // Generate ephemeral wallets
  const wallets = Array.from({ length: numWallets }, () => Keypair.generate());

  // Log all wallets BEFORE any SOL moves
  for (const w of wallets) {
    logWallet({
      pubkey: w.publicKey.toBase58(),
      secretKey: b58encode(w.secretKey),
      tokenMint: pool.baseMint.toBase58(),
      status: 'created',
    });
  }

  // Per-wallet budget: swap SOL + headroom for fees/rent transients + (tip on last wallet).
  // During a first-ever swap on PumpSwap, a fresh wallet pays (transient peak):
  //   - WSOL ATA rent                 ~2,039,280 (refunded when WSOL closes)
  //   - base token ATA rent           ~2,039,280 (not closed -> stranded)
  //   - volume-accumulator PDA rent   ~1,844,400 (permanent protocol rent)
  //   - wrap SOL for buy              = solAmount
  //   - tx fees + priority fees       ~10,000
  // Peak ≈ solAmount + 5,933,000. Buffer 6.0M covers it.
  const baseBuffer = 6_000_000;
  const tipBudget = (tipLamports || 0) + 5_000;
  const fundPerWallet = solAmountLamports + baseBuffer;
  const fundLast = fundPerWallet + tipBudget;

  // Guard: main wallet must stay rent-exempt after funding all wallets + fee.
  const totalFundOut = fundPerWallet * (wallets.length - 1) + fundLast + 10_000;
  const mainBal = await connection.getBalance(mainKeypair.publicKey);
  const RENT_EXEMPT_MIN = 890_880;
  if (mainBal - totalFundOut < RENT_EXEMPT_MIN) {
    return {
      success: false,
      error: `Main wallet would drop below rent-exempt min. Have ${mainBal}, need ${totalFundOut + RENT_EXEMPT_MIN}`,
    };
  }
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash('confirmed');

  // === TX1: Fund all wallets ===
  const fundIxs = [
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: priorityMicroLamports || 1000 }),
  ];
  for (let i = 0; i < wallets.length; i++) {
    const isLast = i === wallets.length - 1;
    fundIxs.push(SystemProgram.transfer({
      fromPubkey: mainKeypair.publicKey,
      toPubkey: wallets[i].publicKey,
      lamports: isLast ? fundLast : fundPerWallet,
    }));
  }

  const fundMsg = new TransactionMessage({
    payerKey: mainKeypair.publicKey,
    recentBlockhash: blockhash,
    instructions: fundIxs,
  }).compileToV0Message();
  const fundTx = new VersionedTransaction(fundMsg);
  fundTx.sign([mainKeypair]);

  // === TX2-N: Each wallet swaps ===
  const swapTxs = [];
  for (let i = 0; i < wallets.length; i++) {
    const w = wallets[i];
    try {
      const swapIxs = await buildSwapInstructions(pool, w.publicKey, solAmountLamports, slippageBps);

      // Jito tip goes on the LAST wallet's swap TX (Jito requires a tip somewhere in the bundle).
      // NOTE: no intra-TX "return SOL" — the exact post-swap balance depends on slippage/fees,
      // so we sweep residuals in a second step after the cycle lands.
      if (i === wallets.length - 1) {
        const tipAccount = new PublicKey(JITO_TIP_ACCOUNTS[Math.floor(Math.random() * JITO_TIP_ACCOUNTS.length)]);
        swapIxs.push(SystemProgram.transfer({
          fromPubkey: w.publicKey,
          toPubkey: tipAccount,
          lamports: Math.max(tipLamports || 10_000, 1000),
        }));
      }

      // Prepend compute budget
      swapIxs.unshift(ComputeBudgetProgram.setComputeUnitLimit({ units: 400_000 }));
      swapIxs.unshift(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: priorityMicroLamports || 1000 }));

      const swapMsg = new TransactionMessage({
        payerKey: w.publicKey,
        recentBlockhash: blockhash,
        instructions: swapIxs,
      }).compileToV0Message();
      const swapTx = new VersionedTransaction(swapMsg);
      swapTx.sign([w]);
      swapTxs.push(swapTx);
    } catch (e) {
      console.error(`  Wallet ${i} swap build failed: ${e.message}`);
    }
  }

  if (swapTxs.length === 0) {
    return { success: false, error: 'All swap builds failed' };
  }

  const allTxs = [fundTx, ...swapTxs];
  const allSigs = allTxs.map(tx => b58encode(tx.signatures[0]));

  let landed = false;
  let bundleId = null;
  let deliveryPath = null;
  let lastErr = null;

  // === PATH 1: Jito bundle (primary) ===
  console.log(`  [JITO] Sending bundle: 1 fund + ${swapTxs.length} swaps (${allTxs.length} TXs)...`);
  const jitoResp = await sendJitoBundle(allTxs);
  if (jitoResp.success) {
    console.log(`  [JITO] Accepted: ${jitoResp.bundleId} via ${jitoResp.endpoint}`);
    const landing = await waitForBundleLanding(jitoResp.bundleId, jitoResp.endpoint, allSigs, 20000);
    if (landing.landed) {
      console.log(`  [JITO] Landed (${landing.via}${landing.slot ? ` slot ${landing.slot}` : ''})`);
      landed = true;
      bundleId = jitoResp.bundleId;
      deliveryPath = 'jito';
    } else {
      console.log(`  [JITO] Did not land in 20s — falling back to RPC`);
      lastErr = 'jito bundle did not land';
    }
  } else {
    console.log(`  [JITO] Rejected: ${jitoResp.error} — falling back to RPC`);
    lastErr = jitoResp.error;
  }

  // === PATH 2: Sequential RPC (fallback) ===
  if (!landed) {
    // The fund TX may already be on-chain from the Jito attempt; getSignatureStatuses tells us.
    try {
      const fundStatus = (await connection.getSignatureStatuses([allSigs[0]])).value[0];
      const fundAlreadyLanded = fundStatus && (fundStatus.confirmationStatus === 'confirmed' || fundStatus.confirmationStatus === 'finalized');

      if (!fundAlreadyLanded) {
        const fundSig = await connection.sendRawTransaction(fundTx.serialize(), { skipPreflight: false, maxRetries: 3 });
        console.log(`  [RPC FUND] ${fundSig}`);
        const conf = await connection.confirmTransaction({ signature: fundSig, blockhash, lastValidBlockHeight }, 'confirmed');
        if (conf.value?.err) throw new Error(`fund tx on-chain error: ${JSON.stringify(conf.value.err)}`);
        console.log(`  [RPC FUND] Confirmed`);
      } else {
        console.log(`  [RPC FUND] Already on-chain from Jito attempt, skipping`);
      }

      let anySwapLanded = false;
      for (let i = 0; i < swapTxs.length; i++) {
        try {
          const existing = (await connection.getSignatureStatuses([allSigs[i + 1]])).value[0];
          if (existing && (existing.confirmationStatus === 'confirmed' || existing.confirmationStatus === 'finalized')) {
            console.log(`  [RPC SWAP ${i}] Already on-chain, skipping`);
            anySwapLanded = true;
            continue;
          }
          const swapSig = await connection.sendRawTransaction(swapTxs[i].serialize(), { skipPreflight: true, maxRetries: 3 });
          console.log(`  [RPC SWAP ${i}] ${swapSig}`);
          const sconf = await connection.confirmTransaction({ signature: swapSig, blockhash, lastValidBlockHeight }, 'confirmed');
          if (sconf.value?.err) throw new Error(`swap tx on-chain error: ${JSON.stringify(sconf.value.err)}`);
          console.log(`  [RPC SWAP ${i}] Confirmed`);
          anySwapLanded = true;
        } catch (e) {
          console.error(`  [RPC SWAP ${i}] FAIL: ${e.message}`);
          lastErr = e.message;
        }
      }
      landed = anySwapLanded;
      deliveryPath = landed ? 'rpc' : null;
      bundleId = 'rpc-direct';
    } catch (e) {
      console.error(`  [RPC] FAIL: ${e.message}`);
      lastErr = e.message;
    }
  }

  // === Sweep residual SOL from ephemeral wallets (always try, regardless of path) ===
  let swept = 0;
  for (const w of wallets) {
    try {
      const bal = await connection.getBalance(w.publicKey);
      if (bal <= 5000) {
        logWallet({ pubkey: w.publicKey.toBase58(), status: 'swept' });
        continue;
      }
      const { blockhash: bh } = await connection.getLatestBlockhash('confirmed');
      const retMsg = new TransactionMessage({
        payerKey: w.publicKey,
        recentBlockhash: bh,
        instructions: [SystemProgram.transfer({
          fromPubkey: w.publicKey, toPubkey: mainKeypair.publicKey, lamports: bal - 5000,
        })],
      }).compileToV0Message();
      const retTx = new VersionedTransaction(retMsg);
      retTx.sign([w]);
      await connection.sendRawTransaction(retTx.serialize(), { skipPreflight: true });
      swept += bal - 5000;
      logWallet({ pubkey: w.publicKey.toBase58(), status: 'swept' });
    } catch (e) {
      console.warn(`  [SWEEP] ${w.publicKey.toBase58().slice(0, 8)}: ${e.message}`);
    }
  }
  if (swept > 0) console.log(`  [SWEEP] Recovered ${(swept / LAMPORTS_PER_SOL).toFixed(6)} SOL`);

  return {
    success: landed,
    bundleId,
    deliveryPath,
    walletsUsed: wallets.length,
    volume: landed ? solAmountLamports * 2 * wallets.length : 0,
    error: landed ? null : lastErr,
  };
}

// ===== SWEEP =====
async function sweepAll() {
  const unsettled = ledger.filter(e => e.status !== 'returned' && e.status !== 'swept' && e.secretKey);
  if (!unsettled.length) { console.log('Nothing to sweep'); return; }

  console.log(`Sweeping ${unsettled.length} wallets...`);
  let recovered = 0;
  for (const entry of unsettled) {
    try {
      const kp = Keypair.fromSecretKey(
        bs58.default ? bs58.default.decode(entry.secretKey) : bs58.decode(entry.secretKey)
      );
      const bal = await connection.getBalance(kp.publicKey);
      if (bal <= 5000) { entry.status = 'swept'; continue; }

      const tx = new Transaction().add(SystemProgram.transfer({
        fromPubkey: kp.publicKey, toPubkey: mainKeypair.publicKey, lamports: bal - 5000,
      }));
      tx.recentBlockhash = (await connection.getLatestBlockhash()).blockhash;
      tx.feePayer = kp.publicKey;
      tx.sign(kp);
      const sig = await connection.sendRawTransaction(tx.serialize(), { skipPreflight: true });
      console.log(`  Swept ${entry.pubkey.slice(0,8)}...: ${((bal-5000)/LAMPORTS_PER_SOL).toFixed(6)} SOL`);
      entry.status = 'swept';
      recovered += bal - 5000;
    } catch (e) { console.warn(`  Sweep failed ${entry.pubkey.slice(0,8)}: ${e.message}`); }
  }
  saveLedger();
  console.log(`Recovered: ${(recovered/LAMPORTS_PER_SOL).toFixed(6)} SOL`);
}

// ===== CLI INTERFACE =====
async function runCLI() {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const ask = (q) => new Promise(r => rl.question(q, r));

  console.log('\n=== Sol Volume Bot v3 ===');
  console.log(`Wallet: ${mainKeypair.publicKey.toBase58()}`);
  const bal = await connection.getBalance(mainKeypair.publicKey);
  console.log(`Balance: ${(bal/LAMPORTS_PER_SOL).toFixed(4)} SOL\n`);

  const tokenMint = await ask('Token mint: ');
  console.log('Finding pool...');
  const pool = await findPumpSwapPool(tokenMint);
  console.log(`Pool: ${pool.poolAddress.toBase58()} | Liq: $${pool.liquidity?.usd?.toFixed(0) || '?'}\n`);

  const walletsPerBundle = parseInt(await ask('Wallets per bundle (1-4): ')) || 1;
  const numBundles = parseInt(await ask('Number of bundles: ')) || 1;
  const solAmount = parseFloat(await ask('SOL per swap (e.g. 0.001): ')) || 0.001;
  const slippage = parseInt(await ask('Slippage bps (e.g. 1500): ')) || 1500;
  const delay = parseInt(await ask('Delay between bundles (sec): ')) || 3;
  const tip = parseFloat(await ask('Jito tip SOL (e.g. 0.001): ')) || 0.001;

  const totalCost = walletsPerBundle * numBundles * (solAmount + 0.005) + tip * numBundles;
  console.log(`\nEstimated cost: ${totalCost.toFixed(4)} SOL`);
  console.log(`Estimated volume: ${(walletsPerBundle * numBundles * solAmount * 2).toFixed(4)} SOL`);
  console.log(`Unique wallets: ${walletsPerBundle * numBundles}`);
  const confirm = await ask('Continue? (y/n): ');
  if (confirm.toLowerCase() !== 'y') { rl.close(); return; }

  console.log('\nStarting...\n');
  let totalVolume = 0;

  for (let i = 0; i < numBundles; i++) {
    console.log(`\n--- Bundle ${i+1}/${numBundles} ---`);
    try {
      const result = await runVolumeCycle(pool, {
        walletsPerBundle,
        solAmountLamports: Math.floor(solAmount * LAMPORTS_PER_SOL),
        slippageBps: slippage,
        tipLamports: Math.floor(tip * LAMPORTS_PER_SOL),
        priorityMicroLamports: 5000,
      });

      if (result.success) {
        totalVolume += result.volume;
        console.log(`  Volume: +${(result.volume/LAMPORTS_PER_SOL).toFixed(4)} SOL | Total: ${(totalVolume/LAMPORTS_PER_SOL).toFixed(4)} SOL`);
      }
    } catch (e) {
      console.error(`  Error: ${e.message}`);
    }

    if (i < numBundles - 1) {
      console.log(`  Waiting ${delay}s...`);
      await new Promise(r => setTimeout(r, delay * 1000));
    }
  }

  console.log(`\nDone! Total volume: ${(totalVolume/LAMPORTS_PER_SOL).toFixed(4)} SOL`);
  const newBal = await connection.getBalance(mainKeypair.publicKey);
  console.log(`Balance: ${(newBal/LAMPORTS_PER_SOL).toFixed(4)} SOL (cost: ${((bal-newBal)/LAMPORTS_PER_SOL).toFixed(4)} SOL)`);

  const doSweep = await ask('\nSweep unsettled wallets? (y/n): ');
  if (doSweep.toLowerCase() === 'y') await sweepAll();

  rl.close();
}

async function runNonInteractive() {
  const tokenMint = process.env.BOT_TOKEN_MINT;
  const walletsPerBundle = parseInt(process.env.BOT_WALLETS || '1');
  const numBundles = parseInt(process.env.BOT_BUNDLES || '1');
  const solAmount = parseFloat(process.env.BOT_SOL || '0.001');
  const slippage = parseInt(process.env.BOT_SLIPPAGE || '1500');
  const delay = parseInt(process.env.BOT_DELAY || '3');
  const tip = parseFloat(process.env.BOT_TIP || '0.0001');

  console.log('\n=== Sol Volume Bot v3 (non-interactive) ===');
  console.log(`Wallet: ${mainKeypair.publicKey.toBase58()}`);
  const bal = await connection.getBalance(mainKeypair.publicKey);
  console.log(`Balance: ${(bal / LAMPORTS_PER_SOL).toFixed(4)} SOL`);
  console.log(`Params: token=${tokenMint} wallets=${walletsPerBundle} bundles=${numBundles} sol=${solAmount} slippage=${slippage}bps tip=${tip}`);

  console.log('Finding pool...');
  const pool = await findPumpSwapPool(tokenMint);
  console.log(`Pool: ${pool.poolAddress.toBase58()} | Liq: $${pool.liquidity?.usd?.toFixed(0) || '?'}\n`);

  let totalVolume = 0;
  for (let i = 0; i < numBundles; i++) {
    console.log(`\n--- Bundle ${i + 1}/${numBundles} ---`);
    try {
      const result = await runVolumeCycle(pool, {
        walletsPerBundle,
        solAmountLamports: Math.floor(solAmount * LAMPORTS_PER_SOL),
        slippageBps: slippage,
        tipLamports: Math.floor(tip * LAMPORTS_PER_SOL),
        priorityMicroLamports: 5000,
      });
      console.log(`  Result: success=${result.success} path=${result.deliveryPath} bundle=${result.bundleId} error=${result.error || 'none'}`);
      if (result.success) {
        totalVolume += result.volume;
        console.log(`  Volume: +${(result.volume / LAMPORTS_PER_SOL).toFixed(4)} SOL | Total: ${(totalVolume / LAMPORTS_PER_SOL).toFixed(4)} SOL`);
      }
    } catch (e) {
      console.error(`  Error: ${e.message}`);
      console.error(e.stack);
    }
    if (i < numBundles - 1) {
      console.log(`  Waiting ${delay}s...`);
      await new Promise(r => setTimeout(r, delay * 1000));
    }
  }

  console.log(`\nDone! Total volume: ${(totalVolume / LAMPORTS_PER_SOL).toFixed(4)} SOL`);
  const newBal = await connection.getBalance(mainKeypair.publicKey);
  console.log(`Balance: ${(newBal / LAMPORTS_PER_SOL).toFixed(4)} SOL (cost: ${((bal - newBal) / LAMPORTS_PER_SOL).toFixed(4)} SOL)`);

  await sweepAll();
}

// Run
const entryPoint = process.env.BOT_AUTORUN === '1' ? runNonInteractive : runCLI;
entryPoint().catch(e => { console.error('Fatal:', e.message); console.error(e.stack); process.exit(1); });
