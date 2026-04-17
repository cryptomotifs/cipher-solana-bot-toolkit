//! Pinata IPFS upload — image + metadata JSON.
//!
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md lines 362-404
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md s6: "Pinata free tier 1GB"

use anyhow::{Result, anyhow};
use reqwest::multipart;

use crate::concept::TokenConcept;
use crate::config::LauncherConfig;

/// Upload token image + metadata to Pinata IPFS, returning the metadata URI.
///
/// Two-step process:
/// 1. Upload PNG image → get image CID
/// 2. Upload metadata JSON (referencing image CID) → get metadata CID
///
/// Returns: `https://ipfs.io/ipfs/{metadata_cid}`
pub async fn upload_to_pinata(
    http: &reqwest::Client,
    config: &LauncherConfig,
    concept: &TokenConcept,
    image_path: &str,
) -> Result<String> {
    if config.pinata_jwt.is_empty() {
        return Err(anyhow!("PINATA_JWT not set — cannot upload to IPFS"));
    }

    // Step 1: Upload image
    // [VERIFIED 2026] pumpfun_token_creation_technical_2026.md lines 362-377
    let image_bytes = std::fs::read(image_path)
        .map_err(|e| anyhow!("Cannot read image {}: {}", image_path, e))?;

    let image_filename = format!("{}_logo.png", concept.symbol.to_lowercase());
    let image_part = multipart::Part::bytes(image_bytes)
        .file_name(image_filename.clone())
        .mime_str("image/png")?;

    let form = multipart::Form::new()
        .part("file", image_part)
        .text("network", "public")
        .text("name", image_filename);

    let resp = http
        .post(predator_core::constants::PINATA_UPLOAD_URL)
        .header("Authorization", format!("Bearer {}", config.pinata_jwt))
        .multipart(form)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Pinata image upload failed ({}): {}", status, err));
    }

    let body: serde_json::Value = resp.json().await?;
    let image_cid = body["data"]["cid"]
        .as_str()
        .ok_or_else(|| anyhow!("No CID in Pinata response: {}", body))?;
    let image_uri = format!("https://ipfs.io/ipfs/{}", image_cid);
    tracing::info!("Image uploaded: {}", image_uri);

    // Step 2: Upload metadata JSON
    // [VERIFIED 2026] pumpfun_token_creation_technical_2026.md lines 322-335
    let metadata = serde_json::json!({
        "name": concept.name,
        "symbol": concept.symbol,
        "description": concept.description,
        "image": image_uri,
        "showName": true,
        "createdOn": "https://pump.fun"
    });

    let meta_json = serde_json::to_vec(&metadata)?;
    let meta_filename = format!("{}_metadata.json", concept.symbol.to_lowercase());
    let meta_part = multipart::Part::bytes(meta_json)
        .file_name(meta_filename.clone())
        .mime_str("application/json")?;

    let form = multipart::Form::new()
        .part("file", meta_part)
        .text("network", "public")
        .text("name", meta_filename);

    let resp = http
        .post(predator_core::constants::PINATA_UPLOAD_URL)
        .header("Authorization", format!("Bearer {}", config.pinata_jwt))
        .multipart(form)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Pinata metadata upload failed ({}): {}", status, err));
    }

    let body: serde_json::Value = resp.json().await?;
    let meta_cid = body["data"]["cid"]
        .as_str()
        .ok_or_else(|| anyhow!("No CID in Pinata metadata response: {}", body))?;
    let metadata_uri = format!("https://ipfs.io/ipfs/{}", meta_cid);
    tracing::info!("Metadata uploaded: {}", metadata_uri);

    Ok(metadata_uri)
}
