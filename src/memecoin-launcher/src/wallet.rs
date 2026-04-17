//! Wallet keypair loading for transaction signing.
//!
//! Supports:
//!   1. AES-256-GCM encrypted keyfile (SBOT format from Python bot)
//!   2. Solana CLI JSON keypair file
//!   3. Base58 private key from env var
//!
//! Copied from client/src/wallet.rs (binary crate, not importable).

use anyhow::{Result, anyhow};
use solana_sdk::signature::Keypair;
use std::path::Path;

use crate::config::LauncherConfig;

/// Load Wallet A (creator) keypair. Tries in order:
/// 1. WALLET_KEYFILE + WALLET_PASSWORD env vars (AES-256-GCM encrypted)
/// 2. WALLET_KEYPAIR_PATH env var (Solana CLI JSON format)
/// 3. WALLET_PRIVATE_KEY env var (base58 encoded)
/// 4. Default Solana CLI path
pub fn load_wallet() -> Result<Keypair> {
    // Try encrypted keyfile FIRST (SBOT format — this is the trading wallet)
    if let (Ok(keyfile), Ok(password)) = (
        std::env::var("WALLET_KEYFILE"),
        std::env::var("WALLET_PASSWORD"),
    ) {
        let keyfile = keyfile.trim().trim_end_matches('\r').to_string();
        let password = password.trim().trim_end_matches('\r').to_string();
        if !keyfile.is_empty() && !password.is_empty() {
            let paths = [
                keyfile.clone(),
                format!("C:/Users/s_amr/Downloads/solana-arb-bot/{}", keyfile),
                format!("/mnt/c/Users/s_amr/Downloads/solana-arb-bot/{}", keyfile),
                format!("../{}", keyfile),
            ];
            for path in &paths {
                if Path::new(path).exists() {
                    match load_encrypted_keyfile(path, &password) {
                        Ok(kp) => {
                            tracing::info!("Loaded encrypted Wallet A from {}", path);
                            return Ok(kp);
                        }
                        Err(e) => tracing::warn!("Encrypted keyfile {}: {}", path, e),
                    }
                }
            }
        }
    }

    // Fallback: JSON keypair file
    if let Ok(path) = std::env::var("WALLET_KEYPAIR_PATH") {
        let path = path.trim().trim_end_matches('\r').to_string();
        let try_paths = [
            path.clone(),
            format!("../{}", path),
            format!("C:/Users/s_amr/Downloads/solana-arb-bot/{}", path),
            format!("/mnt/c/Users/s_amr/Downloads/solana-arb-bot/{}", path),
        ];
        for p in &try_paths {
            if Path::new(p).exists() {
                return load_keypair_from_file(p);
            }
        }
    }

    // Try base58 private key from env
    if let Ok(key) = std::env::var("WALLET_PRIVATE_KEY") {
        let key = key.trim().trim_end_matches('\r');
        let bytes = bs58::decode(key).into_vec()
            .map_err(|e| anyhow!("Invalid base58 private key: {}", e))?;
        return Keypair::try_from(bytes.as_slice())
            .map_err(|e| anyhow!("Invalid keypair bytes: {}", e));
    }

    // Try default paths
    let default_paths = [
        "wallet.json".to_string(),
        format!("{}/.config/solana/id.json",
            std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default()),
    ];
    for path in &default_paths {
        if Path::new(path).exists() {
            return load_keypair_from_file(path);
        }
    }

    Err(anyhow!(
        "No wallet found. Set WALLET_KEYFILE+WALLET_PASSWORD or WALLET_KEYPAIR_PATH"
    ))
}

/// Load Wallet B (trader) — separate wallet for first-buyer strategy.
/// Uses TRADER_WALLET_KEYFILE + TRADER_WALLET_PASSWORD, or TRADER_PRIVATE_KEY.
/// [VERIFIED 2026] gmgn_wallet_reputation_flywheel_2026.md: "separate wallet, no on-chain link"
pub fn load_trader_wallet(config: &LauncherConfig) -> Result<Keypair> {
    // Try dedicated trader wallet env vars
    if let (Ok(keyfile), Ok(password)) = (
        std::env::var("TRADER_WALLET_KEYFILE"),
        std::env::var("TRADER_WALLET_PASSWORD"),
    ) {
        let keyfile = keyfile.trim().trim_end_matches('\r').to_string();
        let password = password.trim().trim_end_matches('\r').to_string();
        if !keyfile.is_empty() && !password.is_empty() {
            let paths = [
                keyfile.clone(),
                format!("C:/Users/s_amr/Downloads/solana-arb-bot/{}", keyfile),
                format!("/mnt/c/Users/s_amr/Downloads/solana-arb-bot/{}", keyfile),
            ];
            for path in &paths {
                if Path::new(path).exists() {
                    match load_encrypted_keyfile(path, &password) {
                        Ok(kp) => {
                            tracing::info!("Loaded encrypted Wallet B from {}", path);
                            return Ok(kp);
                        }
                        Err(e) => tracing::warn!("Trader keyfile {}: {}", path, e),
                    }
                }
            }
        }
    }

    // Try base58 trader private key
    if let Ok(key) = std::env::var("TRADER_PRIVATE_KEY") {
        let key = key.trim().trim_end_matches('\r');
        let bytes = bs58::decode(key).into_vec()
            .map_err(|e| anyhow!("Invalid trader base58 key: {}", e))?;
        return Keypair::try_from(bytes.as_slice())
            .map_err(|e| anyhow!("Invalid trader keypair: {}", e));
    }

    // Try config path
    if !config.trader_wallet_path.is_empty() && Path::new(&config.trader_wallet_path).exists() {
        return load_keypair_from_file(&config.trader_wallet_path);
    }

    // Fallback: same as Wallet A (not ideal but allows testing)
    tracing::warn!("No separate trader wallet configured — using Wallet A (NOT recommended for GMGN)");
    load_wallet()
}

/// Decrypt AES-256-GCM encrypted keyfile (SBOT format).
/// Format: MAGIC(4) + VERSION(1) + SALT(32) + NONCE(12) + CIPHERTEXT+TAG
/// Key derivation: scrypt(password, salt, N=16384, r=8, p=2, dklen=32)
fn load_encrypted_keyfile(path: &str, password: &str) -> Result<Keypair> {
    use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
    use aes_gcm::aead::generic_array::GenericArray;

    let data = std::fs::read(path)
        .map_err(|e| anyhow!("Cannot read keyfile {}: {}", path, e))?;

    if data.len() < 49 + 16 {
        return Err(anyhow!("Keyfile too small: {} bytes", data.len()));
    }
    if &data[..4] != b"SBOT" {
        return Err(anyhow!("Invalid magic bytes"));
    }
    if data[4] != 1 {
        return Err(anyhow!("Unsupported version: {}", data[4]));
    }

    let salt = &data[5..37];
    let nonce = &data[37..49];
    let ciphertext = &data[49..];

    let params = scrypt::Params::new(14, 8, 2, 32)
        .map_err(|e| anyhow!("scrypt params: {}", e))?;
    let mut derived_key = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut derived_key)
        .map_err(|e| anyhow!("scrypt: {}", e))?;

    let cipher = Aes256Gcm::new(GenericArray::from_slice(&derived_key));
    let nonce = GenericArray::from_slice(nonce);

    use aes_gcm::aead::Payload;
    let payload = Payload { msg: ciphertext, aad: salt };
    let plaintext = cipher.decrypt(nonce, payload)
        .map_err(|_| anyhow!("Decryption failed — wrong password?"))?;

    if plaintext.len() != 64 && plaintext.len() != 32 {
        return Err(anyhow!("Invalid key length: {} bytes", plaintext.len()));
    }

    Keypair::try_from(plaintext.as_slice())
        .map_err(|e| anyhow!("Invalid keypair: {}", e))
}

/// Load keypair from Solana CLI JSON format (array of 64 u8 values).
fn load_keypair_from_file(path: &str) -> Result<Keypair> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("Cannot read keypair file {}: {}", path, e))?;
    let bytes: Vec<u8> = serde_json::from_str(&data)
        .map_err(|e| anyhow!("Invalid keypair JSON in {}: {}", path, e))?;
    Keypair::try_from(bytes.as_slice())
        .map_err(|e| anyhow!("Invalid keypair bytes in {}: {}", path, e))
}
