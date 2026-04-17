//! Error types for the PREDATOR bot.
//!
//! Uses `thiserror` 2.x for ergonomic, typed error handling as recommended in
//! [rust_solana_patterns_2026.md Section 8]: "thiserror for library crates,
//! anyhow for the top-level binary."
//!
//! Each variant covers a distinct failure domain so callers can pattern-match
//! on the error kind without parsing strings.

use thiserror::Error;

/// Canonical error type for all PREDATOR bot operations.
///
/// Converts to `anyhow::Error` at crate boundaries via the blanket `From` impl
/// that `thiserror` provides. Strategy crates should define their own error
/// enums with `#[from] BotError` for transparent propagation.
#[derive(Debug, Error)]
pub enum BotError {
    // ------------------------------------------------------------------
    // Network / RPC
    // ------------------------------------------------------------------

    /// An RPC call to a Solana validator or RPC provider failed.
    /// Includes the raw error message from `solana-client` or `reqwest`.
    #[error("RPC error: {0}")]
    RpcError(String),

    /// Jupiter Swap API (V1 or V2) returned an error.
    /// Covers /quote, /build, /swap-instructions, and /order endpoints.
    #[error("Jupiter API error: {0}")]
    JupiterError(String),

    /// Jito block engine rejected a bundle or returned an unexpected response.
    /// Covers `sendBundle`, `getBundleStatuses`, and tip-related failures.
    #[error("Jito error: {0}")]
    JitoError(String),

    // ------------------------------------------------------------------
    // Protocol / DeFi
    // ------------------------------------------------------------------

    /// A lending protocol (Save, Kamino, MarginFi, JupLend) returned an
    /// on-chain error or produced unexpected account data.
    #[error("Protocol error [{protocol}]: {detail}")]
    ProtocolError {
        /// Which protocol: "save", "kamino", "marginfi", "juplend".
        protocol: String,
        /// Human-readable detail (e.g. instruction error code, account parse failure).
        detail: String,
    },

    /// Flash loan borrow or repay failed.
    /// Covers Kamino (0.001%), MarginFi (0%), JupLend (0%), and Save (5 bps).
    #[error("Flash loan error: {0}")]
    FlashLoanError(String),

    // ------------------------------------------------------------------
    // Transaction building / simulation
    // ------------------------------------------------------------------

    /// `simulateTransaction` returned an error.
    /// Includes the error message and optionally the CU consumed before failure,
    /// which is useful for right-sizing the compute budget on retry.
    #[error("Simulation failed: {error} (CU consumed: {cu_consumed:?})")]
    SimulationFailed {
        /// The simulation error message from the RPC.
        error: String,
        /// Compute units consumed before the failure, if reported.
        cu_consumed: Option<u64>,
    },

    /// The assembled transaction exceeds the Solana size limit.
    /// `size` is the actual serialized byte count; `max` is the applicable
    /// limit (1232 for legacy, 1644 for versioned v0).
    #[error("Transaction too large: {size} bytes (max {max})")]
    TxTooLarge {
        /// Actual serialized transaction size in bytes.
        size: usize,
        /// Maximum allowed size for the transaction type.
        max: usize,
    },

    // ------------------------------------------------------------------
    // Resource constraints
    // ------------------------------------------------------------------

    /// The bot's wallet does not have enough SOL/tokens to cover the
    /// transaction fee, tip, or ATA rent-exemption.
    #[error("Insufficient balance: required {required} lamports, available {available}")]
    InsufficientBalance {
        /// Lamports needed to proceed.
        required: u64,
        /// Lamports currently available.
        available: u64,
    },

    /// An API endpoint (RPC, Jupiter, Jito, Nozomi) returned HTTP 429.
    /// `retry_after_ms` is the suggested backoff period; callers should
    /// exponentially back off if this is zero.
    #[error("Rate limited by {endpoint} (retry after {retry_after_ms}ms)")]
    RateLimited {
        /// The endpoint or provider that rate-limited us (e.g. "jupiter", "quicknode").
        endpoint: String,
        /// Milliseconds to wait before retrying (0 if not specified by the server).
        retry_after_ms: u64,
    },

    // ------------------------------------------------------------------
    // Configuration
    // ------------------------------------------------------------------

    /// Missing or invalid configuration (env vars, config.toml, CLI args).
    #[error("Config error: {0}")]
    ConfigError(String),

    // ------------------------------------------------------------------
    // Timeouts
    // ------------------------------------------------------------------

    /// An async operation exceeded its deadline.
    /// `operation` identifies what timed out (e.g. "rpc_get_account",
    /// "jupiter_quote", "jito_bundle_status").
    #[error("Timeout: {operation} exceeded {elapsed_ms}ms")]
    Timeout {
        /// Name of the operation that timed out.
        operation: String,
        /// Milliseconds elapsed before the timeout fired.
        elapsed_ms: u64,
    },
}

/// Convenience alias used throughout the PREDATOR crate.
pub type Result<T> = std::result::Result<T, BotError>;

// ---------------------------------------------------------------------------
// Conversions from common external error types
// ---------------------------------------------------------------------------

impl From<reqwest::Error> for BotError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            BotError::Timeout {
                operation: format!("HTTP request to {}", err.url().map_or("unknown".into(), |u| u.to_string())),
                elapsed_ms: 0, // reqwest doesn't expose the exact elapsed time
            }
        } else if err.status().map_or(false, |s| s.as_u16() == 429) {
            BotError::RateLimited {
                endpoint: err.url().map_or("unknown".into(), |u| u.host_str().unwrap_or("unknown").to_string()),
                retry_after_ms: 0,
            }
        } else {
            BotError::RpcError(err.to_string())
        }
    }
}

impl From<serde_json::Error> for BotError {
    fn from(err: serde_json::Error) -> Self {
        BotError::ConfigError(format!("JSON parse error: {err}"))
    }
}

impl From<std::io::Error> for BotError {
    fn from(err: std::io::Error) -> Self {
        BotError::ConfigError(format!("IO error: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_rpc_error() {
        let err = BotError::RpcError("connection refused".into());
        assert_eq!(err.to_string(), "RPC error: connection refused");
    }

    #[test]
    fn display_protocol_error() {
        let err = BotError::ProtocolError {
            protocol: "kamino".into(),
            detail: "obligation not found".into(),
        };
        assert_eq!(
            err.to_string(),
            "Protocol error [kamino]: obligation not found"
        );
    }

    #[test]
    fn display_simulation_failed() {
        let err = BotError::SimulationFailed {
            error: "custom program error: 0x1771".into(),
            cu_consumed: Some(450_000),
        };
        assert!(err.to_string().contains("0x1771"));
        assert!(err.to_string().contains("450000"));
    }

    #[test]
    fn display_tx_too_large() {
        let err = BotError::TxTooLarge {
            size: 1700,
            max: 1644,
        };
        assert!(err.to_string().contains("1700"));
        assert!(err.to_string().contains("1644"));
    }

    #[test]
    fn display_insufficient_balance() {
        let err = BotError::InsufficientBalance {
            required: 500_000,
            available: 100_000,
        };
        assert!(err.to_string().contains("500000"));
        assert!(err.to_string().contains("100000"));
    }

    #[test]
    fn display_rate_limited() {
        let err = BotError::RateLimited {
            endpoint: "jupiter".into(),
            retry_after_ms: 1000,
        };
        assert!(err.to_string().contains("jupiter"));
        assert!(err.to_string().contains("1000ms"));
    }

    #[test]
    fn display_timeout() {
        let err = BotError::Timeout {
            operation: "rpc_get_account".into(),
            elapsed_ms: 5000,
        };
        assert!(err.to_string().contains("rpc_get_account"));
        assert!(err.to_string().contains("5000ms"));
    }

    #[test]
    fn result_alias_works() {
        fn fallible() -> Result<u64> {
            Err(BotError::ConfigError("missing key".into()))
        }
        assert!(fallible().is_err());
    }
}
