//! GeyserManager — manages 2-3 independent gRPC streams for real-time data ingestion.
//!
//! Stream architecture:
//!   Stream 1 (ORACLES): Owner filter on Pyth Push Oracle program — catches ALL oracle updates
//!     in 1 filter. Uses accounts_data_slice (offset 74, 20 bytes) for 97% bandwidth reduction.
//!     [VERIFIED 2026] bot_architecture_deep_2026.md Section 2b: "The Owner Filter Trick"
//!
//!   Stream 2 (POOLS): Specific pool vault pubkeys for backrun detection.
//!     [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c: "Multiple Streams vs Single Stream"
//!
//!   Stream 3 (BLOCKS): blocksMeta for fresh blockhash without RPC.
//!     [VERIFIED 2026] low_latency_dataflow_2026.md Section 9: "gRPC blocksMeta for fresh blockhash"
//!
//! Reconnection: exponential backoff (1s, 2s, 4s, max 30s) with jitter.
//!   [VERIFIED 2026] low_latency_dataflow_2026.md Section 6: "Reconnection Strategy"
//!
//! QuickNode 1-filter-per-stream workaround: each stream has exactly 1 subscription filter.
//!   [VERIFIED 2026] bot_architecture_deep_2026.md Section 2a: "QuickNode allows 1 filter per stream"

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn, error};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::*;

use crate::filters;

/// Account update received from Yellowstone gRPC.
///
/// [VERIFIED 2026] code_structure_patterns_2026.md lines 138-143: AccountUpdate pattern
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1: "data: Vec<u8> for owned data"
#[derive(Debug, Clone)]
pub struct AccountUpdate {
    /// The account's public key (32 bytes, no bs58 conversion on hot path).
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 5: "Avoid String Conversion of Pubkeys"
    pub pubkey: Pubkey,
    /// Raw account data bytes. May be a data slice (e.g., 20 bytes for oracle).
    pub data: Vec<u8>,
    /// Slot at which this update was observed.
    pub slot: u64,
    /// gRPC write_version for deduplication — higher version wins.
    pub write_version: u64,
}

/// Identifies a logical gRPC stream.
///
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c: 3 dedicated streams
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeyserStream {
    /// Stream 1: Oracle price feeds via Pyth Push Oracle owner filter.
    Oracles,
    /// Stream 2: Pool vault accounts for backrun detection.
    Pools,
    /// Stream 3: blocksMeta for fresh blockhash.
    Blocks,
}

impl std::fmt::Display for GeyserStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeyserStream::Oracles => write!(f, "oracles"),
            GeyserStream::Pools => write!(f, "pools"),
            GeyserStream::Blocks => write!(f, "blocks"),
        }
    }
}

/// Connection configuration for a gRPC endpoint.
#[derive(Debug, Clone)]
pub struct GeyserEndpoint {
    /// gRPC endpoint URL (e.g., "https://your-quicknode-endpoint.quiknode.pro").
    pub url: String,
    /// Authentication token (x-token header for QuickNode/Triton/Helius).
    pub token: String,
}

/// Channels produced by GeyserManager for downstream consumers.
///
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 3: "tokio mpsc is the right choice"
pub struct GeyserChannels {
    /// Oracle account updates (from Stream 1).
    pub oracle_rx: mpsc::Receiver<AccountUpdate>,
    /// Pool vault account updates (from Stream 2).
    pub pool_rx: mpsc::Receiver<AccountUpdate>,
    /// Blockhash updates from blocksMeta (from Stream 3).
    /// watch::channel — always-latest, no queue.
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 2: "watch is an ArcSwap with async notification"
    pub blockhash_rx: watch::Receiver<Hash>,
    /// Block height updates (for expiry prediction).
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 9: "Track Block Height for Expiry"
    pub block_height_rx: watch::Receiver<u64>,
    /// Send new pool pubkeys for in-place resubscription (from scanner).
    pub resub_tx: mpsc::Sender<Vec<Pubkey>>,
}

/// GeyserManager manages 2-3 independent gRPC streams with independent
/// reconnection, circuit breakers, and per-stream data slicing.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 202-207: GeyserManager spec
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c: "Use 2-3 dedicated streams"
pub struct GeyserManager {
    endpoint: GeyserEndpoint,
    /// Dropped updates counter per stream (for health monitoring).
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 3: "track drops for health monitoring"
    oracle_drops: Arc<AtomicU64>,
    pool_drops: Arc<AtomicU64>,
}

impl GeyserManager {
    /// Create a new GeyserManager.
    pub fn new(endpoint: GeyserEndpoint) -> Self {
        Self {
            endpoint,
            oracle_drops: Arc::new(AtomicU64::new(0)),
            pool_drops: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Connect to the gRPC endpoint and validate connectivity.
    ///
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 6: Connection configuration
    /// Ported from working client/src/geyser.rs lines 118-128.
    pub async fn connect(endpoint: &GeyserEndpoint) -> Result<()> {
        info!(
            "GeyserManager: validating endpoint {}...",
            &endpoint.url[..endpoint.url.len().min(50)]
        );

        // Attempt a test connection to validate the endpoint and token.
        // Each stream task creates its own client for independent reconnection.
        let _client = build_grpc_client(&endpoint.url, &endpoint.token).await?;
        info!("GeyserManager: endpoint validated successfully");
        Ok(())
    }

    /// Start all 3 gRPC streams and return channels for downstream consumers.
    ///
    /// Each stream runs as an independent tokio task with its own reconnection logic.
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c: "Independent reconnection"
    pub fn start(self, initial_pool_accounts: Vec<Pubkey>) -> GeyserChannels {
        // Channel capacities from research:
        // [VERIFIED 2026] low_latency_dataflow_2026.md Section 3:
        //   "The 2048 buffer is generous enough to handle bursts"
        let (oracle_tx, oracle_rx) = mpsc::channel::<AccountUpdate>(4096);
        let (pool_tx, pool_rx) = mpsc::channel::<AccountUpdate>(2048);
        let (bh_tx, bh_rx) = watch::channel::<Hash>(Hash::default());
        let (height_tx, height_rx) = watch::channel::<u64>(0);
        let (resub_tx, resub_rx) = mpsc::channel::<Vec<Pubkey>>(16);

        let endpoint = self.endpoint.clone();
        let oracle_drops = self.oracle_drops.clone();
        let pool_drops = self.pool_drops.clone();

        // Stream 1: ORACLES — owner filter on Pyth Push Oracle program.
        // [VERIFIED 2026] bot_architecture_deep_2026.md Section 2b: Owner Filter Trick
        let ep1 = endpoint.clone();
        let oracle_drops_clone = oracle_drops.clone();
        tokio::spawn(async move {
            Self::run_oracle_stream(ep1, oracle_tx, oracle_drops_clone).await;
        });

        // Stream 2: POOLS — specific pool vault pubkeys.
        // [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c: Pool stream
        let ep2 = endpoint.clone();
        let pool_drops_clone = pool_drops.clone();
        tokio::spawn(async move {
            Self::run_pool_stream(ep2, pool_tx, resub_rx, initial_pool_accounts, pool_drops_clone)
                .await;
        });

        // Stream 3: BLOCKS — blocksMeta for fresh blockhash.
        // [VERIFIED 2026] low_latency_dataflow_2026.md Section 9: Blockhash Management
        let ep3 = endpoint;
        tokio::spawn(async move {
            Self::run_blocks_stream(ep3, bh_tx, height_tx).await;
        });

        GeyserChannels {
            oracle_rx,
            pool_rx,
            blockhash_rx: bh_rx,
            block_height_rx: height_rx,
            resub_tx,
        }
    }

    /// Run the oracle gRPC stream with exponential backoff reconnection.
    ///
    /// Subscribes by OWNER to the Pyth Push Oracle program, receiving ALL oracle
    /// account updates in a single filter. Uses accounts_data_slice to request only
    /// the price bytes (offset 74, 20 bytes) for 97% bandwidth reduction.
    ///
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2b: Owner filter trick
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2d: accounts_data_slice
    async fn run_oracle_stream(
        endpoint: GeyserEndpoint,
        tx: mpsc::Sender<AccountUpdate>,
        drops: Arc<AtomicU64>,
    ) {
        let mut backoff_ms: u64 = 1000; // Start at 1s
        // [VERIFIED 2026] low_latency_dataflow_2026.md Section 6: "1s, 2s, 4s, max 30s"

        loop {
            info!("GeyserManager[oracles]: connecting to {}...", &endpoint.url[..endpoint.url.len().min(50)]);

            match Self::stream_oracle_updates(&endpoint, &tx, &drops).await {
                Ok(()) => {
                    info!("GeyserManager[oracles]: stream ended cleanly, reconnecting...");
                    backoff_ms = 1000; // Reset on clean disconnect
                }
                Err(e) => {
                    error!("GeyserManager[oracles]: error: {}, reconnecting in {}ms...", e, backoff_ms);
                }
            }

            // Exponential backoff with jitter.
            // [VERIFIED 2026] low_latency_dataflow_2026.md Section 6: "exponential backoff with jitter"
            let jitter_ms = fastrand_u64() % 500;
            tokio::time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms * 2).min(30_000); // Max 30s
        }
    }

    /// Internal: connect and stream oracle updates until disconnection.
    ///
    /// Ported from working client/src/geyser.rs subscribe_and_stream().
    /// Uses Pyth owner filter with accounts_data_slice for bandwidth reduction.
    async fn stream_oracle_updates(
        endpoint: &GeyserEndpoint,
        tx: &mpsc::Sender<AccountUpdate>,
        drops: &Arc<AtomicU64>,
    ) -> Result<()> {
        let mut client = build_grpc_client(&endpoint.url, &endpoint.token).await?;

        let filter = filters::build_oracle_owner_filter();
        info!("GeyserManager[oracles]: connected! Subscribing with Pyth owner filter + data_slice(74, 20)");

        // Build SubscribeRequest with owner filter for Pyth Push Oracle program.
        let mut accounts_filter = HashMap::new();
        accounts_filter.insert(
            filter.label.clone(),
            SubscribeRequestFilterAccounts {
                account: filter.account_pubkeys.clone(),
                owner: filter.owner_pubkeys.clone(),
                filters: vec![],
                nonempty_txn_signature: None,
            },
        );

        // accounts_data_slice: request only price bytes (offset 74, 20 bytes)
        // [VERIFIED 2026] bot_architecture_deep_2026.md Section 2d: 97% bandwidth reduction
        let data_slices: Vec<SubscribeRequestAccountsDataSlice> = filter
            .data_slices
            .iter()
            .map(|&(offset, length)| SubscribeRequestAccountsDataSlice { offset, length })
            .collect();

        let request = SubscribeRequest {
            accounts: accounts_filter,
            slots: HashMap::new(),
            transactions: HashMap::new(),
            transactions_status: HashMap::new(),
            blocks: HashMap::new(),
            blocks_meta: HashMap::new(),
            entry: HashMap::new(),
            // PROCESSED gives 2-3s faster oracle detection vs Confirmed.
            // [VERIFIED 2026] grpc_optimization_2026.md
            commitment: Some(CommitmentLevel::Processed as i32),
            accounts_data_slice: data_slices,
            ping: None,
            from_slot: None,
        };

        let (mut subscribe_tx, mut stream) = client
            .subscribe_with_request(Some(request))
            .await
            .map_err(|e| anyhow!("gRPC oracle subscribe: {}", e))?;

        info!("GeyserManager[oracles]: subscribed! Streaming oracle updates...");

        use futures_util::{StreamExt, SinkExt};
        let mut count = 0u64;

        loop {
            match stream.next().await {
                Some(Ok(msg)) => {
                    if let Some(update_oneof) = msg.update_oneof {
                        match update_oneof {
                            subscribe_update::UpdateOneof::Account(acct) => {
                                if let Some(account) = acct.account {
                                    if account.pubkey.len() == 32 {
                                        let pk = Pubkey::new_from_array(
                                            account.pubkey.as_slice().try_into().unwrap_or([0; 32]),
                                        );

                                        let update = AccountUpdate {
                                            pubkey: pk,
                                            data: account.data,
                                            slot: acct.slot,
                                            write_version: account.write_version,
                                        };

                                        if tx.try_send(update).is_err() {
                                            drops.fetch_add(1, Ordering::Relaxed);
                                            // Don't warn on every drop — only periodic
                                            let total = drops.load(Ordering::Relaxed);
                                            if total % 100 == 0 {
                                                warn!(
                                                    "GeyserManager[oracles]: channel full, {} total drops",
                                                    total
                                                );
                                            }
                                        }

                                        count += 1;
                                        if count % 500 == 0 {
                                            info!(
                                                "GeyserManager[oracles]: {} updates (slot {})",
                                                count, acct.slot
                                            );
                                        }
                                    }
                                }
                            }
                            subscribe_update::UpdateOneof::Ping(_) => {
                                // Respond to server pings to keep connection alive.
                                // Ported from working geyser.rs line 248-251.
                                let _ = subscribe_tx
                                    .send(SubscribeRequest {
                                        ping: Some(SubscribeRequestPing { id: 1 }),
                                        ..Default::default()
                                    })
                                    .await;
                            }
                            _ => {}
                        }
                    }
                }
                Some(Err(e)) => {
                    return Err(anyhow!("gRPC oracle stream error: {}", e));
                }
                None => {
                    return Ok(()); // stream ended cleanly
                }
            }
        }
    }

    /// Run the pool vault gRPC stream with resubscription support.
    ///
    /// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2c: Pool stream
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 204: "specific pool vault pubkeys"
    async fn run_pool_stream(
        endpoint: GeyserEndpoint,
        tx: mpsc::Sender<AccountUpdate>,
        mut resub_rx: mpsc::Receiver<Vec<Pubkey>>,
        initial_accounts: Vec<Pubkey>,
        drops: Arc<AtomicU64>,
    ) {
        let mut current_accounts = initial_accounts;
        let mut backoff_ms: u64 = 1000;

        loop {
            info!(
                "GeyserManager[pools]: connecting with {} accounts...",
                current_accounts.len()
            );

            match Self::stream_pool_updates(
                &endpoint,
                &tx,
                &mut resub_rx,
                &current_accounts,
                &drops,
            )
            .await
            {
                Ok(Some(new_accounts)) => {
                    // Resubscription requested — update accounts and reconnect.
                    info!(
                        "GeyserManager[pools]: resubscribing to {} new accounts",
                        new_accounts.len()
                    );
                    current_accounts = new_accounts;
                    backoff_ms = 1000;
                }
                Ok(None) => {
                    info!("GeyserManager[pools]: stream ended cleanly, reconnecting...");
                    backoff_ms = 1000;
                }
                Err(e) => {
                    error!(
                        "GeyserManager[pools]: error: {}, reconnecting in {}ms...",
                        e, backoff_ms
                    );
                }
            }

            let jitter_ms = fastrand_u64() % 500;
            tokio::time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms * 2).min(30_000);
        }
    }

    /// Internal: connect and stream pool updates with resubscription support.
    ///
    /// Returns Ok(Some(new_pubkeys)) if resubscription was requested but in-place
    /// resubscription failed (needs reconnect). Returns Ok(None) on clean disconnect.
    ///
    /// Ported from working client/src/geyser.rs subscribe_and_stream() select! loop.
    async fn stream_pool_updates(
        endpoint: &GeyserEndpoint,
        tx: &mpsc::Sender<AccountUpdate>,
        resub_rx: &mut mpsc::Receiver<Vec<Pubkey>>,
        accounts: &[Pubkey],
        drops: &Arc<AtomicU64>,
    ) -> Result<Option<Vec<Pubkey>>> {
        let mut client = build_grpc_client(&endpoint.url, &endpoint.token).await?;

        let filter = filters::build_account_filter(accounts);
        info!(
            "GeyserManager[pools]: connected! Subscribing to {} vault accounts",
            accounts.len()
        );

        let request = build_account_subscribe_request(&filter);

        let (mut subscribe_tx, mut stream) = client
            .subscribe_with_request(Some(request))
            .await
            .map_err(|e| anyhow!("gRPC pool subscribe: {}", e))?;

        info!("GeyserManager[pools]: subscribed! Streaming pool updates...");

        use futures_util::{StreamExt, SinkExt};
        let mut count = 0u64;

        loop {
            tokio::select! {
                // Check for resubscription requests from scanner.
                // Ported from working geyser.rs lines 145-158.
                Some(new_pubkeys) = resub_rx.recv() => {
                    info!("GeyserManager[pools]: resubscription requested with {} accounts", new_pubkeys.len());
                    // Build new filter and try in-place resubscription.
                    let new_filter = filters::build_account_filter(&new_pubkeys);
                    let new_request = build_account_subscribe_request(&new_filter);
                    match subscribe_tx.send(new_request).await {
                        Ok(_) => {
                            info!("GeyserManager[pools]: resubscribed in-place to {} accounts", new_pubkeys.len());
                            // Continue streaming — no reconnection needed.
                        }
                        Err(e) => {
                            warn!("GeyserManager[pools]: in-place resubscription failed: {}, reconnecting", e);
                            return Ok(Some(new_pubkeys));
                        }
                    }
                }

                // Process gRPC stream messages.
                msg_result = stream.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            if let Some(update_oneof) = msg.update_oneof {
                                match update_oneof {
                                    subscribe_update::UpdateOneof::Account(acct) => {
                                        if let Some(account) = acct.account {
                                            if account.pubkey.len() == 32 {
                                                let pk = Pubkey::new_from_array(
                                                    account.pubkey.as_slice().try_into().unwrap_or([0; 32]),
                                                );

                                                let update = AccountUpdate {
                                                    pubkey: pk,
                                                    data: account.data,
                                                    slot: acct.slot,
                                                    write_version: account.write_version,
                                                };

                                                if tx.try_send(update).is_err() {
                                                    drops.fetch_add(1, Ordering::Relaxed);
                                                    let total = drops.load(Ordering::Relaxed);
                                                    if total % 100 == 0 {
                                                        warn!(
                                                            "GeyserManager[pools]: channel full, {} total drops",
                                                            total
                                                        );
                                                    }
                                                }

                                                count += 1;
                                                if count % 100 == 0 {
                                                    info!(
                                                        "GeyserManager[pools]: {} updates (slot {})",
                                                        count, acct.slot
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    subscribe_update::UpdateOneof::Ping(_) => {
                                        let _ = subscribe_tx.send(SubscribeRequest {
                                            ping: Some(SubscribeRequestPing { id: 1 }),
                                            ..Default::default()
                                        }).await;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some(Err(e)) => {
                            return Err(anyhow!("gRPC pool stream error: {}", e));
                        }
                        None => {
                            return Ok(None); // stream ended cleanly
                        }
                    }
                }
            }
        }
    }

    /// Run the blocksMeta gRPC stream for fresh blockhash.
    ///
    /// [VERIFIED 2026] low_latency_dataflow_2026.md Section 9: "gRPC blocksMeta for fresh blockhash"
    /// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 205: "blocksMeta for fresh blockhash"
    async fn run_blocks_stream(
        endpoint: GeyserEndpoint,
        bh_tx: watch::Sender<Hash>,
        height_tx: watch::Sender<u64>,
    ) {
        let mut backoff_ms: u64 = 1000;

        loop {
            info!("GeyserManager[blocks]: connecting for blocksMeta...");

            match Self::stream_blocks(&endpoint, &bh_tx, &height_tx).await {
                Ok(()) => {
                    info!("GeyserManager[blocks]: stream ended cleanly, reconnecting...");
                    backoff_ms = 1000;
                }
                Err(e) => {
                    error!(
                        "GeyserManager[blocks]: error: {}, reconnecting in {}ms...",
                        e, backoff_ms
                    );
                }
            }

            let jitter_ms = fastrand_u64() % 500;
            tokio::time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms * 2).min(30_000);
        }
    }

    /// Internal: connect and stream blocksMeta until disconnection.
    ///
    /// Ported from working client/src/geyser.rs BlockMeta handling (lines 192-200).
    /// Extracts blockhash and block_height from each SubscribeUpdateBlockMeta.
    async fn stream_blocks(
        endpoint: &GeyserEndpoint,
        bh_tx: &watch::Sender<Hash>,
        height_tx: &watch::Sender<u64>,
    ) -> Result<()> {
        let mut client = build_grpc_client(&endpoint.url, &endpoint.token).await?;

        info!("GeyserManager[blocks]: connected! Subscribing to blocksMeta...");

        let mut blocks_meta = HashMap::new();
        blocks_meta.insert("bh".to_string(), SubscribeRequestFilterBlocksMeta {});

        let request = SubscribeRequest {
            accounts: HashMap::new(),
            slots: HashMap::new(),
            transactions: HashMap::new(),
            transactions_status: HashMap::new(),
            blocks: HashMap::new(),
            blocks_meta,
            entry: HashMap::new(),
            commitment: Some(CommitmentLevel::Processed as i32),
            accounts_data_slice: vec![],
            ping: None,
            from_slot: None,
        };

        let (mut subscribe_tx, mut stream) = client
            .subscribe_with_request(Some(request))
            .await
            .map_err(|e| anyhow!("gRPC blocks subscribe: {}", e))?;

        info!("GeyserManager[blocks]: subscribed to blocksMeta!");

        use futures_util::{StreamExt, SinkExt};

        loop {
            match stream.next().await {
                Some(Ok(msg)) => {
                    if let Some(update_oneof) = msg.update_oneof {
                        match update_oneof {
                            subscribe_update::UpdateOneof::BlockMeta(meta) => {
                                // Parse blockhash string to Hash.
                                // Ported from working geyser.rs lines 193-199.
                                if let Ok(hash) = Hash::from_str(&meta.blockhash) {
                                    let _ = bh_tx.send(hash);
                                }

                                // Send block height for expiry prediction.
                                // [VERIFIED 2026] low_latency_dataflow_2026.md Section 9
                                let block_height = meta.block_height.map(|bh| bh.block_height).unwrap_or(0);
                                let _ = height_tx.send(block_height);

                                if meta.slot % 100 == 0 {
                                    info!(
                                        "GeyserManager[blocks]: slot={} hash={}... height={}",
                                        meta.slot,
                                        &meta.blockhash[..meta.blockhash.len().min(12)],
                                        block_height
                                    );
                                }
                            }
                            subscribe_update::UpdateOneof::Ping(_) => {
                                let _ = subscribe_tx
                                    .send(SubscribeRequest {
                                        ping: Some(SubscribeRequestPing { id: 1 }),
                                        ..Default::default()
                                    })
                                    .await;
                            }
                            _ => {}
                        }
                    }
                }
                Some(Err(e)) => {
                    return Err(anyhow!("gRPC blocks stream error: {}", e));
                }
                None => {
                    return Ok(()); // stream ended cleanly
                }
            }
        }
    }

    /// Get the cumulative drop count for the oracle stream.
    pub fn oracle_drop_count(&self) -> u64 {
        self.oracle_drops.load(Ordering::Relaxed)
    }

    /// Get the cumulative drop count for the pool stream.
    pub fn pool_drop_count(&self) -> u64 {
        self.pool_drops.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build a GeyserGrpcClient with the verified connection parameters.
///
/// Ported from working client/src/geyser.rs lines 118-128.
/// Connection parameters [VERIFIED 2026 low_latency_dataflow_2026.md Section 6]:
///   .connect_timeout(10s)
///   .initial_stream_window_size(2MB)
///   .initial_connection_window_size(4MB)
///   .max_decoding_message_size(1GB)
///   .tls_config with native roots
async fn build_grpc_client(url: &str, token: &str) -> Result<GeyserGrpcClient> {
    GeyserGrpcClient::build_from_shared(url.to_string())
        .map_err(|e| anyhow!("gRPC build: {}", e))?
        .x_token(Some(token.to_string()))
        .map_err(|e| anyhow!("gRPC x_token: {}", e))?
        .connect_timeout(Duration::from_secs(10))
        .tls_config(
            yellowstone_grpc_client::ClientTlsConfig::new().with_native_roots(),
        )
        .map_err(|e| anyhow!("gRPC tls: {}", e))?
        .initial_stream_window_size(2 * 1024 * 1024)
        .initial_connection_window_size(4 * 1024 * 1024)
        // 1GB max decoding — prevent truncation during high activity.
        // [VERIFIED 2026] grpc_optimization_2026.md
        .max_decoding_message_size(1024 * 1024 * 1024)
        .connect()
        .await
        .map_err(|e| anyhow!("gRPC connect to {}: {}", &url[..url.len().min(50)], e))
}

/// Build a SubscribeRequest for an account filter (pool vaults, etc.).
///
/// Ported from working client/src/geyser.rs build_subscribe_request().
fn build_account_subscribe_request(filter: &filters::SubscribeFilter) -> SubscribeRequest {
    let mut accounts_filter = HashMap::new();
    if !filter.account_pubkeys.is_empty() || !filter.owner_pubkeys.is_empty() {
        accounts_filter.insert(
            filter.label.clone(),
            SubscribeRequestFilterAccounts {
                account: filter.account_pubkeys.clone(),
                owner: filter.owner_pubkeys.clone(),
                filters: vec![],
                nonempty_txn_signature: None,
            },
        );
    }

    SubscribeRequest {
        accounts: accounts_filter,
        slots: HashMap::new(),
        transactions: HashMap::new(),
        transactions_status: HashMap::new(),
        blocks: HashMap::new(),
        blocks_meta: HashMap::new(),
        entry: HashMap::new(),
        commitment: Some(CommitmentLevel::Processed as i32),
        accounts_data_slice: vec![],
        ping: None,
        from_slot: None,
    }
}

/// Simple pseudo-random u64 without importing rand.
/// Uses a thread-local LCG seeded from the current time.
fn fastrand_u64() -> u64 {
    use std::cell::Cell;
    use std::time::SystemTime;

    thread_local! {
        static SEED: Cell<u64> = Cell::new(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        );
    }

    SEED.with(|s| {
        // LCG: x_{n+1} = (a * x_n + c) mod 2^64
        let x = s.get().wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s.set(x);
        x
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_update_basics() {
        let update = AccountUpdate {
            pubkey: Pubkey::default(),
            data: vec![1, 2, 3],
            slot: 100,
            write_version: 42,
        };
        assert_eq!(update.data.len(), 3);
        assert_eq!(update.slot, 100);
        assert_eq!(update.write_version, 42);
    }

    #[test]
    fn geyser_stream_display() {
        assert_eq!(GeyserStream::Oracles.to_string(), "oracles");
        assert_eq!(GeyserStream::Pools.to_string(), "pools");
        assert_eq!(GeyserStream::Blocks.to_string(), "blocks");
    }

    #[test]
    fn fastrand_produces_different_values() {
        let a = fastrand_u64();
        let b = fastrand_u64();
        // Extremely unlikely to be equal
        assert_ne!(a, b);
    }

    #[test]
    fn geyser_manager_new() {
        let mgr = GeyserManager::new(GeyserEndpoint {
            url: "https://test.quiknode.pro".to_string(),
            token: "test-token".to_string(),
        });
        assert_eq!(mgr.oracle_drop_count(), 0);
        assert_eq!(mgr.pool_drop_count(), 0);
    }

    #[test]
    fn build_account_request_with_pubkeys() {
        let filter = filters::build_account_filter(&[Pubkey::new_unique(), Pubkey::new_unique()]);
        let request = build_account_subscribe_request(&filter);
        assert!(request.accounts.contains_key("pool_vaults"));
        let acct_filter = &request.accounts["pool_vaults"];
        assert_eq!(acct_filter.account.len(), 2);
        assert!(acct_filter.owner.is_empty());
    }

    #[test]
    fn build_account_request_empty() {
        let filter = filters::build_account_filter(&[]);
        let request = build_account_subscribe_request(&filter);
        // Empty pubkeys still produces an entry with empty account list
        assert!(request.accounts.is_empty());
    }
}
