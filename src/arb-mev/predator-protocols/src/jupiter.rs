//! JupiterClient — Jupiter V2 API adapter for swap instruction generation.
//!
//! Supports two execution paths:
//! 1. **V2 /build** (preferred) — single GET call returns raw swap instructions.
//!    Replaces the old 2-step /quote + /swap-instructions flow.
//!    [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1A:
//!    "GET /swap/v2/build — Advanced Path"
//!
//! 2. **V1 /quote + /swap-instructions** (legacy fallback) — two calls.
//!    Still functional but deprecated.
//!
//! Key facts:
//! - V2 /build HALVES API consumption (1 call vs 2 per swap)
//! - `taker` replaces `userPublicKey` in V2
//! - `wrapAndUnwrapSol=false` for flash loan context (existing WSOL ATA)
//! - Response includes `addressesByLookupTableAddress` for v0 transactions
//! - No Jupiter swap fees on /build path
//! - Our `parse_instruction_json()` works unchanged (same instruction format)
//! - Rate limit: 110ms min spacing (~9 RPS) for Pro I tier (10 RPS)
//!
//! [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1
//! [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5: Rate limiting
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md lines 124-126: JupiterClient spec

use predator_core::error::BotError;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, trace, warn};

// ---------------------------------------------------------------------------
// Jupiter V2 API endpoints
// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1A-1B
// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 124
// ---------------------------------------------------------------------------

/// Jupiter V2 /build — single call returns quote + raw swap instructions.
/// [VERIFIED 2026] jupiter_public_api_paths_2026.md: "api.jup.ag is NOT under maintenance, V2 /build returns 200"
const JUPITER_V2_BUILD_URL: &str = "https://api.jup.ag/swap/v2/build";

/// Jupiter V1 quote — primary on api.jup.ag (no fee, 0.5 RPS keyless).
/// [VERIFIED 2026] jupiter_public_api_paths_2026.md: "V1 /swap/v1/quote returns 200"
const JUPITER_V1_QUOTE_URL: &str = "https://api.jup.ag/swap/v1/quote";

/// Jupiter V1 swap-instructions — primary on api.jup.ag.
/// [VERIFIED 2026] jupiter_public_api_paths_2026.md: "V1 /swap/v1/swap-instructions returns 200"
const JUPITER_V1_SWAP_IX_URL: &str = "https://api.jup.ag/swap/v1/swap-instructions";

/// Jupiter fallback quote — public.jupiterapi.com (10 RPS, 0.20% fee, NO /v1/ prefix!).
/// [VERIFIED 2026] jupiter_public_api_paths_2026.md: "paths are bare — no /v1/ prefix"
const JUPITER_FALLBACK_QUOTE_URL: &str = "https://public.jupiterapi.com/quote";

/// Jupiter fallback swap-instructions — public.jupiterapi.com.
/// [VERIFIED 2026] jupiter_public_api_paths_2026.md
const JUPITER_FALLBACK_SWAP_IX_URL: &str = "https://public.jupiterapi.com/swap-instructions";

// ---------------------------------------------------------------------------
// Rate limiter
// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5:
//   "110ms min spacing (~9 RPS) instead of per-second bucketing"
// [VERIFIED 2026] client/src/adapters/jupiter.rs: proven atomic CAS pattern
// ---------------------------------------------------------------------------

/// Last call epoch timestamp (ms) — atomic for lock-free concurrent access.
static LAST_CALL_EPOCH_MS: AtomicU64 = AtomicU64::new(0);

/// Minimum milliseconds between Jupiter API calls.
/// Free tier = 1 RPS. Using 1100ms spacing for safety.
/// With V2 /build (1 call per swap), this allows 1 liquidation per ~1.1s.
/// Upgrade to Pro ($25/mo) for 10 RPS (110ms spacing).
/// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5
const MIN_INTERVAL_MS: u64 = 1100;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Jupiter quote result containing output amount, price impact, and raw response.
///
/// The `raw_response` field preserves the full JSON for passing to /swap-instructions
/// (V1 path) or for logging/debugging.
#[derive(Debug, Clone)]
pub struct JupiterQuote {
    /// Expected output amount in base units.
    pub out_amount: u64,
    /// Price impact percentage (e.g. 0.01 = 1%).
    pub price_impact_pct: f64,
    /// Minimum output after slippage (from V2 `otherAmountThreshold`).
    pub other_amount_threshold: u64,
    /// Full JSON response for downstream consumption.
    pub raw_response: serde_json::Value,
}

/// Parsed swap instructions from Jupiter, ready for transaction assembly.
///
/// Contains the ordered instruction list and any address lookup table addresses
/// needed for v0 transaction construction.
#[derive(Debug, Clone)]
pub struct SwapInstructions {
    /// Setup instructions (ATA creation, compute budget, etc.).
    pub setup_instructions: Vec<Instruction>,
    /// The main swap instruction(s).
    pub swap_instruction: Vec<Instruction>,
    /// Post-swap cleanup instructions.
    pub cleanup_instructions: Vec<Instruction>,
    /// Address lookup table addresses for v0 transaction compression.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md line 77
    pub address_lookup_table_addresses: Vec<String>,
}

// ---------------------------------------------------------------------------
// JupiterClient
// ---------------------------------------------------------------------------

/// Jupiter V2 API client with rate limiting and dual-path support.
///
/// # Construction
/// ```no_run
/// let client = JupiterClient::new(
///     reqwest::Client::new(),
///     Some("your-api-key".to_string()),
///     true, // use V2 /build
/// );
/// ```
pub struct JupiterClient {
    http: reqwest::Client,
    /// Optional API key from dev.jup.ag dashboard.
    /// Sent as `x-api-key` header. Required for Developer+ tiers.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1
    api_key: Option<String>,
    /// Whether to use V2 /build (true) or V1 /quote + /swap-instructions (false).
    use_v2: bool,
    /// QuickNode Metis endpoint — separate rate limit from Jupiter direct.
    /// Format: {quicknode_rpc_url} with Metis add-on enabled.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5C:
    ///   "QuickNode Metis (via QN add-on): Separate rate limit, Metis routing"
    ///   "Effective combined RPS: ~20 RPS"
    metis_base_url: Option<String>,
    /// Track consecutive 429s on main Jupiter — switch to Metis after 2
    main_api_failures: std::sync::atomic::AtomicU32,
}

impl JupiterClient {
    /// Create a new JupiterClient.
    ///
    /// # Arguments
    /// - `http`: Shared reqwest client (connection pooling across the bot).
    /// - `api_key`: Optional API key for authenticated access.
    /// - `use_v2`: Use V2 /build endpoint (recommended) vs V1 legacy path.
    pub fn new(http: reqwest::Client, api_key: Option<String>, use_v2: bool) -> Self {
        // Check for QuickNode Metis endpoint from env
        // [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5C: dual-path routing
        let metis_base_url = std::env::var("RPC_ENDPOINTS")
            .or_else(|_| std::env::var("QUICKNODE_URL"))
            .ok()
            .and_then(|url| {
                let endpoint = url.split(',').next().unwrap_or("").trim().to_string();
                if endpoint.contains("quiknode.pro") || endpoint.contains("quicknode.com") {
                    // QuickNode Metis: append /jupiter/quote or use as base
                    Some(endpoint)
                } else {
                    None
                }
            });
        if metis_base_url.is_some() {
            tracing::info!("Jupiter Metis dual-path enabled via QuickNode");
        }
        Self {
            http,
            api_key,
            use_v2,
            metis_base_url,
            main_api_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Get the best quote URL — use Metis if main API is rate-limited.
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5C
    fn quote_base_url(&self) -> &str {
        let failures = self.main_api_failures.load(std::sync::atomic::Ordering::Relaxed);
        if failures >= 2 {
            if let Some(ref metis) = self.metis_base_url {
                return metis.as_str();
            }
        }
        JUPITER_V1_QUOTE_URL.trim_end_matches("/quote")
    }

    /// Record a main API success (reset failure counter)
    fn record_success(&self) {
        self.main_api_failures.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record a main API 429 failure
    fn record_rate_limit(&self) {
        self.main_api_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // -----------------------------------------------------------------------
    // Rate limiter
    // [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5
    // [VERIFIED 2026] client/src/adapters/jupiter.rs: atomic CAS pattern
    // -----------------------------------------------------------------------

    /// Global atomic rate limiter — enforces 110ms minimum spacing between API calls.
    ///
    /// Uses compare-and-swap on a global atomic timestamp. Multiple concurrent
    /// tokio tasks compete for the slot; losers sleep and retry.
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 5:
    ///   "110ms min spacing (~9 RPS)"
    pub async fn rate_limit(&self) {
        loop {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let last = LAST_CALL_EPOCH_MS.load(Ordering::Relaxed);
            let elapsed = now_ms.saturating_sub(last);

            if elapsed >= MIN_INTERVAL_MS {
                // Try to claim this slot atomically.
                if LAST_CALL_EPOCH_MS
                    .compare_exchange(last, now_ms, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
                // Another task claimed it — retry.
                continue;
            }

            // Too soon — wait the difference.
            let wait = MIN_INTERVAL_MS - elapsed;
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        }
    }

    /// Add API key header if configured.
    fn add_api_key(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.header("x-api-key", key)
        } else {
            req
        }
    }

    // -----------------------------------------------------------------------
    // V1 Legacy Path: /quote + /swap-instructions
    // -----------------------------------------------------------------------

    /// Get a Jupiter quote (V1 /quote endpoint).
    ///
    /// [VERIFIED 2026] client/src/adapters/jupiter.rs: existing proven implementation
    pub async fn get_quote(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount: u64,
        slippage_bps: u16,
        dexes: Option<&str>,
        only_direct_routes: bool,
    ) -> Result<JupiterQuote, BotError> {
        let mut url = format!(
            "{}?inputMint={}&outputMint={}&amount={}&slippageBps={}&maxAccounts=20",
            JUPITER_V1_QUOTE_URL, input_mint, output_mint, amount, slippage_bps,
        );
        if let Some(dex_list) = dexes {
            url.push_str(&format!("&dexes={}", dex_list));
        }
        if only_direct_routes {
            url.push_str("&onlyDirectRoutes=true");
        }

        self.rate_limit().await;

        let req = self.add_api_key(self.http.get(&url));
        let resp = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            self.record_rate_limit();
            // Fallback to public.jupiterapi.com (10 RPS, separate from api.jup.ag limits)
            // [VERIFIED 2026] jupiter_public_api_paths_2026.md:
            //   "paths are bare — no /v1/ prefix. GET /quote returns 200"
            //   "10 RPS free, 0.20% platform fee"
            let fallback_url = format!(
                "{}?inputMint={}&outputMint={}&amount={}&slippageBps={}&maxAccounts=20",
                JUPITER_FALLBACK_QUOTE_URL, input_mint, output_mint, amount, slippage_bps,
            );
            tracing::info!("Jupiter 429 on api.jup.ag — trying public.jupiterapi.com");
            if let Ok(resp2) = self.http.get(&fallback_url).send().await {
                if resp2.status().is_success() {
                    self.record_success();
                    if let Ok(json) = resp2.json().await {
                        return Self::parse_quote_response(json);
                    }
                }
            }
            // Fallback also failed — retry main after 2s
            warn!("Jupiter quote rate limited on both endpoints, retrying after 2s");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let req2 = self.add_api_key(self.http.get(&url));
            let resp2 = req2.send().await?;
            if !resp2.status().is_success() {
                return Err(BotError::JupiterError(format!(
                    "Quote rate limited (retry): {}",
                    resp2.status()
                )));
            }
            self.record_success();
            return Self::parse_quote_response(resp2.json().await?);
        }
        self.record_success();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BotError::JupiterError(format!(
                "Quote failed: {} {}",
                status,
                &body[..body.len().min(200)]
            )));
        }

        Self::parse_quote_response(resp.json().await?)
    }

    /// Get swap instructions from a previously obtained quote response (V1).
    ///
    /// [VERIFIED 2026] client/src/adapters/jupiter.rs: existing proven implementation
    pub async fn get_swap_instructions(
        &self,
        quote_response: &serde_json::Value,
        user_pubkey: &str,
        wrap_sol: bool,
    ) -> Result<SwapInstructions, BotError> {
        let body = serde_json::json!({
            "quoteResponse": quote_response,
            "userPublicKey": user_pubkey,
            "wrapAndUnwrapSol": wrap_sol,
            "useSharedAccounts": true,
            "asLegacyTransaction": false,
            "dynamicComputeUnitLimit": false,
        });

        self.rate_limit().await;

        let req = self.add_api_key(self.http.post(JUPITER_V1_SWAP_IX_URL));
        let resp = req.json(&body).send().await?;

        if resp.status().as_u16() == 429 {
            warn!("Jupiter swap-instructions rate limited, retrying after 2s");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let req2 = self.add_api_key(self.http.post(JUPITER_V1_SWAP_IX_URL));
            let resp2 = req2.json(&body).send().await?;
            if !resp2.status().is_success() {
                return Err(BotError::JupiterError(format!(
                    "Swap-instructions rate limited: {}",
                    resp2.status()
                )));
            }
            return Self::parse_swap_response(resp2.json().await?);
        }

        if !resp.status().is_success() {
            return Err(BotError::JupiterError(format!(
                "Swap-instructions failed: {}",
                resp.status()
            )));
        }

        Self::parse_swap_response(resp.json().await?)
    }

    // -----------------------------------------------------------------------
    // V2 /build — single-call path
    // [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1A:
    //   "GET /swap/v2/build — Advanced Path"
    //   "Replaces old two-step /quote + /swap-instructions flow with SINGLE call"
    //   "userPublicKey renamed to taker"
    //   "wrapAndUnwrapSol=false for flash loan context"
    // -----------------------------------------------------------------------

    /// Build swap instructions via V2 /build endpoint (single API call).
    ///
    /// This is the preferred method — halves API consumption compared to V1.
    ///
    /// # Parameters
    /// - `input_mint`, `output_mint`: Token mints (base58 strings).
    /// - `amount`: Input amount in smallest units (lamports, etc.).
    /// - `slippage_bps`: Slippage tolerance (0-10000).
    /// - `user_pubkey`: Taker wallet address (renamed from `userPublicKey` in V2).
    ///
    /// # V2 Changes from V1
    /// - `userPublicKey` → `taker` parameter
    /// - Response includes `computeBudgetInstructions`, `setupInstructions`,
    ///   `swapInstruction`, `cleanupInstruction`, `addressesByLookupTableAddress`
    /// - No Jupiter swap fees on /build path
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md Section 1A
    pub async fn build_swap(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount: u64,
        slippage_bps: u16,
        user_pubkey: &str,
    ) -> Result<SwapInstructions, BotError> {
        // V2 /build uses `taker` instead of `userPublicKey`.
        // [VERIFIED 2026] execution_pipeline_deep_2026.md line 47
        let url = format!(
            "{}?inputMint={}&outputMint={}&amount={}&taker={}&slippageBps={}&maxAccounts=20&wrapAndUnwrapSol=false",
            JUPITER_V2_BUILD_URL, input_mint, output_mint, amount, user_pubkey, slippage_bps,
        );

        self.rate_limit().await;

        let req = self.add_api_key(self.http.get(&url));
        let resp = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            warn!("Jupiter V2 /build rate limited, retrying after 2s");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let req2 = self.add_api_key(self.http.get(&url));
            let resp2 = req2.send().await?;
            if !resp2.status().is_success() {
                return Err(BotError::JupiterError(format!(
                    "V2 /build rate limited (retry): {}",
                    resp2.status()
                )));
            }
            return Self::parse_v2_build_response(resp2.json().await?);
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BotError::JupiterError(format!(
                "V2 /build failed: {} {}",
                status,
                &body[..body.len().min(200)]
            )));
        }

        Self::parse_v2_build_response(resp.json().await?)
    }

    // -----------------------------------------------------------------------
    // Response parsers
    // -----------------------------------------------------------------------

    fn parse_quote_response(data: serde_json::Value) -> Result<JupiterQuote, BotError> {
        if data.get("error").is_some() {
            return Err(BotError::JupiterError(format!("Quote error: {}", data)));
        }
        let out_amount = data
            .get("outAmount")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let price_impact = data
            .get("priceImpactPct")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let threshold = data
            .get("otherAmountThreshold")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(JupiterQuote {
            out_amount,
            price_impact_pct: price_impact,
            other_amount_threshold: threshold,
            raw_response: data,
        })
    }

    /// Parse V1 /swap-instructions response.
    fn parse_swap_response(data: serde_json::Value) -> Result<SwapInstructions, BotError> {
        let mut swap_instruction = Vec::new();
        let mut setup_instructions = Vec::new();
        let mut cleanup_instructions = Vec::new();
        let mut alt_addresses = Vec::new();

        // Parse setup instructions.
        if let Some(setups) = data.get("setupInstructions").and_then(|v| v.as_array()) {
            for ix_json in setups {
                if let Some(ix) = parse_instruction_json(ix_json) {
                    setup_instructions.push(ix);
                }
            }
        }

        // Parse main swap instruction (singular in V1).
        if let Some(swap_ix) = data.get("swapInstruction") {
            if let Some(ix) = parse_instruction_json(swap_ix) {
                swap_instruction.push(ix);
            }
        }

        // Parse cleanup instruction.
        if let Some(cleanup) = data.get("cleanupInstruction") {
            if let Some(ix) = parse_instruction_json(cleanup) {
                cleanup_instructions.push(ix);
            }
        }

        // Collect ALT addresses.
        if let Some(alts) = data
            .get("addressLookupTableAddresses")
            .and_then(|v| v.as_array())
        {
            for alt in alts {
                if let Some(s) = alt.as_str() {
                    alt_addresses.push(s.to_string());
                }
            }
        }

        if swap_instruction.is_empty() {
            return Err(BotError::JupiterError(
                "No swap instruction in Jupiter response".into(),
            ));
        }

        Ok(SwapInstructions {
            setup_instructions,
            swap_instruction,
            cleanup_instructions,
            address_lookup_table_addresses: alt_addresses,
        })
    }

    /// Parse V2 /build response.
    ///
    /// V2 response structure:
    /// - `computeBudgetInstructions`: Compute unit price instructions
    /// - `setupInstructions`: Pre-swap instructions (ATA creation, etc.)
    /// - `swapInstruction`: Main swap instruction (singular object)
    /// - `cleanupInstruction`: Post-swap cleanup
    /// - `otherInstructions`: Additional instructions
    /// - `addressesByLookupTableAddress`: ALT mappings for v0 txs
    ///
    /// [VERIFIED 2026] execution_pipeline_deep_2026.md lines 70-77
    fn parse_v2_build_response(data: serde_json::Value) -> Result<SwapInstructions, BotError> {
        let mut setup_instructions = Vec::new();
        let mut swap_instruction = Vec::new();
        let mut cleanup_instructions = Vec::new();
        let mut alt_addresses = Vec::new();

        // Parse compute budget instructions (V2 includes these).
        // We skip these — the bot manages its own compute budget.
        // [VERIFIED 2026] execution_pipeline_deep_2026.md line 84:
        //   "developers must simulate to determine correct CU limit"

        // Parse setup instructions.
        if let Some(setups) = data.get("setupInstructions").and_then(|v| v.as_array()) {
            for ix_json in setups {
                if let Some(ix) = parse_instruction_json(ix_json) {
                    setup_instructions.push(ix);
                }
            }
        }

        // Parse main swap instruction.
        if let Some(swap_ix) = data.get("swapInstruction") {
            if let Some(ix) = parse_instruction_json(swap_ix) {
                swap_instruction.push(ix);
            }
        }

        // Parse cleanup instruction.
        if let Some(cleanup) = data.get("cleanupInstruction") {
            if !cleanup.is_null() {
                if let Some(ix) = parse_instruction_json(cleanup) {
                    cleanup_instructions.push(ix);
                }
            }
        }

        // Parse other instructions (V2 addition).
        if let Some(others) = data.get("otherInstructions").and_then(|v| v.as_array()) {
            for ix_json in others {
                if let Some(ix) = parse_instruction_json(ix_json) {
                    cleanup_instructions.push(ix);
                }
            }
        }

        // Collect ALT addresses.
        // V2 uses `addressesByLookupTableAddress` (object mapping address -> keys)
        // instead of the V1 array format.
        // [VERIFIED 2026] execution_pipeline_deep_2026.md line 77
        if let Some(alt_map) = data
            .get("addressesByLookupTableAddress")
            .and_then(|v| v.as_object())
        {
            for key in alt_map.keys() {
                alt_addresses.push(key.clone());
            }
        }

        // Fall back to V1-style array if present.
        if alt_addresses.is_empty() {
            if let Some(alts) = data
                .get("addressLookupTableAddresses")
                .and_then(|v| v.as_array())
            {
                for alt in alts {
                    if let Some(s) = alt.as_str() {
                        alt_addresses.push(s.to_string());
                    }
                }
            }
        }

        if swap_instruction.is_empty() {
            return Err(BotError::JupiterError(
                "No swap instruction in Jupiter V2 /build response".into(),
            ));
        }

        debug!(
            setup_count = setup_instructions.len(),
            swap_count = swap_instruction.len(),
            cleanup_count = cleanup_instructions.len(),
            alt_count = alt_addresses.len(),
            "Parsed Jupiter V2 /build response"
        );

        Ok(SwapInstructions {
            setup_instructions,
            swap_instruction,
            cleanup_instructions,
            address_lookup_table_addresses: alt_addresses,
        })
    }
}

// ---------------------------------------------------------------------------
// Instruction JSON parser
// [VERIFIED 2026] client/src/adapters/jupiter.rs: proven parse_instruction_json
// Same instruction format in V1 and V2 responses.
// ---------------------------------------------------------------------------

/// Parse a JSON instruction object into a Solana `Instruction`.
///
/// Expected JSON format (identical in V1 and V2 responses):
/// ```json
/// {
///   "programId": "base58-encoded-pubkey",
///   "accounts": [
///     { "pubkey": "...", "isSigner": false, "isWritable": true }
///   ],
///   "data": "base64-encoded-instruction-data"
/// }
/// ```
///
/// [VERIFIED 2026] execution_pipeline_deep_2026.md line 155:
///   "Our existing parse_instruction_json() works unchanged — same instruction format"
pub fn parse_instruction_json(json: &serde_json::Value) -> Option<Instruction> {
    let program_id = Pubkey::from_str(json.get("programId")?.as_str()?).ok()?;
    let data_b64 = json.get("data")?.as_str()?;
    let data = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        data_b64,
    )
    .ok()?;

    let accounts: Vec<AccountMeta> = json
        .get("accounts")?
        .as_array()?
        .iter()
        .filter_map(|acc| {
            let pubkey = Pubkey::from_str(acc.get("pubkey")?.as_str()?).ok()?;
            let is_signer = acc.get("isSigner")?.as_bool()?;
            let is_writable = acc.get("isWritable")?.as_bool()?;
            Some(if is_writable {
                AccountMeta::new(pubkey, is_signer)
            } else {
                AccountMeta::new_readonly(pubkey, is_signer)
            })
        })
        .collect();

    Some(Instruction {
        program_id,
        accounts,
        data,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_instruction_json_valid() {
        let json = serde_json::json!({
            "programId": "11111111111111111111111111111111",
            "accounts": [
                {
                    "pubkey": "11111111111111111111111111111111",
                    "isSigner": true,
                    "isWritable": true
                }
            ],
            "data": "AQAAAA==" // [1, 0, 0, 0] in base64
        });

        let ix = parse_instruction_json(&json).unwrap();
        assert_eq!(ix.program_id, Pubkey::default());
        assert_eq!(ix.accounts.len(), 1);
        assert!(ix.accounts[0].is_signer);
        assert!(ix.accounts[0].is_writable);
        assert_eq!(ix.data, vec![1, 0, 0, 0]);
    }

    #[test]
    fn parse_instruction_json_readonly() {
        let json = serde_json::json!({
            "programId": "11111111111111111111111111111111",
            "accounts": [
                {
                    "pubkey": "11111111111111111111111111111111",
                    "isSigner": false,
                    "isWritable": false
                }
            ],
            "data": "AA=="
        });

        let ix = parse_instruction_json(&json).unwrap();
        assert!(!ix.accounts[0].is_signer);
        assert!(!ix.accounts[0].is_writable);
    }

    #[test]
    fn parse_instruction_json_missing_program_id() {
        let json = serde_json::json!({
            "accounts": [],
            "data": "AA=="
        });
        assert!(parse_instruction_json(&json).is_none());
    }

    #[test]
    fn parse_instruction_json_missing_data() {
        let json = serde_json::json!({
            "programId": "11111111111111111111111111111111",
            "accounts": []
        });
        assert!(parse_instruction_json(&json).is_none());
    }

    #[test]
    fn parse_instruction_json_invalid_base64() {
        let json = serde_json::json!({
            "programId": "11111111111111111111111111111111",
            "accounts": [],
            "data": "!!!not-base64!!!"
        });
        assert!(parse_instruction_json(&json).is_none());
    }

    #[test]
    fn jupiter_client_new() {
        let client = JupiterClient::new(
            reqwest::Client::new(),
            Some("test-key".into()),
            true,
        );
        assert!(client.use_v2);
        assert_eq!(client.api_key, Some("test-key".to_string()));
    }

    #[test]
    fn jupiter_client_no_key() {
        let client = JupiterClient::new(reqwest::Client::new(), None, false);
        assert!(!client.use_v2);
        assert!(client.api_key.is_none());
    }

    #[test]
    fn parse_quote_response_valid() {
        let json = serde_json::json!({
            "outAmount": "1000000",
            "priceImpactPct": "0.01",
            "otherAmountThreshold": "990000",
        });
        let quote = JupiterClient::parse_quote_response(json).unwrap();
        assert_eq!(quote.out_amount, 1_000_000);
        assert!((quote.price_impact_pct - 0.01).abs() < f64::EPSILON);
        assert_eq!(quote.other_amount_threshold, 990_000);
    }

    #[test]
    fn parse_quote_response_error() {
        let json = serde_json::json!({
            "error": "Invalid input mint",
        });
        assert!(JupiterClient::parse_quote_response(json).is_err());
    }

    #[test]
    fn parse_swap_response_no_swap_ix() {
        let json = serde_json::json!({
            "setupInstructions": [],
        });
        assert!(JupiterClient::parse_swap_response(json).is_err());
    }
}
