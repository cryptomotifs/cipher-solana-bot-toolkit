//! predator-launcher — Memecoin token launcher on pump.fun (and later Raydium LaunchLab).
//!
//! Pipeline: narrative detection → concept generation → image generation → IPFS upload
//!           → PumpPortal token creation → first buyer (Wallet B) → fee collection
//!
//! [VERIFIED 2026] memecoin_launcher_strategy_2026.md — master strategy
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md — create_v2, PumpPortal API
//! [VERIFIED 2026] narrative_detection_twitter_2026.md — trend detection
//! [VERIFIED 2026] memecoin_launch_revenue_model_2026.md — fee tiers, revenue math

pub mod config;
pub mod wallet;
pub mod budget;
pub mod tracker;
pub mod narrative;
pub mod concept;
pub mod image_gen;
pub mod ipfs;
pub mod creator;
pub mod first_buyer;
pub mod sell_monitor;
pub mod fee_collector;
pub mod pipeline;
