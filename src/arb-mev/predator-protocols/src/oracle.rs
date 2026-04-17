//! OracleParser — parse oracle account data from Pyth and Switchboard into uniform OraclePrice.
//!
//! Supports three oracle formats:
//! 1. **Pyth PriceUpdateV2 / PriceFeedAccount** — price at offset 74 (i64), conf at 82 (u64),
//!    exponent at 90 (i32). Magic identified by account owner (rec5EK... or pythWSn...).
//!    [VERIFIED 2026] solana_oracle_systems_complete_2026.md Section 2.2-2.3:
//!    PriceFeedMessage sub-layout at offset 42 within PriceUpdateV2
//!
//! 2. **Switchboard V2 AggregatorAccountData** — SwitchboardDecimal at offset 366:
//!    mantissa i128 at [366..382], scale u32 at [382..386]. price = mantissa * 10^(-scale).
//!    [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.2
//!
//! 3. **Switchboard On-Demand PullFeedAccountData** — CurrentResult.value at offset 2261:
//!    i128 with 18 decimal precision. price = value / 1e18.
//!    [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.3
//!
//! Auto-detection works by data length heuristics when account owner is not available.
//! When owner is known, use `parse_by_owner()` for definitive routing.
//!
//! [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.4: owner-aware parsing
//! [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 76-78: OracleParser spec

use predator_core::{OraclePrice, OracleSource, Slot};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use tracing::trace;

// ---------------------------------------------------------------------------
// Program IDs for owner-based oracle detection
// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.4
// [VERIFIED 2026] solana_oracle_systems_complete_2026.md Section 2.1
// ---------------------------------------------------------------------------

/// Pyth Receiver program — owns PriceUpdateV2 accounts (pull oracle).
/// [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 55
const PYTH_RECEIVER_PROGRAM: &str = "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ";

/// Pyth Price Feed program — owns PriceFeedAccount accounts (push oracle, shard 0).
/// Same internal layout as PriceUpdateV2.
/// [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 56
const PYTH_PRICE_FEED_PROGRAM: &str = "pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT";

/// Switchboard V2 program — owns AggregatorAccountData.
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.1
const SWITCHBOARD_V2_PROGRAM: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";

/// Switchboard On-Demand program — owns PullFeedAccountData.
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.1
const SWITCHBOARD_ONDEMAND_PROGRAM: &str = "SBondMDrcV3K4kxZR1HNVT7osZxAHVHgYXL5Ze1oMUv";

// ---------------------------------------------------------------------------
// Pyth PriceUpdateV2 / PriceFeedAccount offsets
// [VERIFIED 2026] solana_oracle_systems_complete_2026.md Section 2.2:
//   "PriceFeedMessage sub-layout (at offset 42)"
//   Offset 42+32=74: price (i64 LE)
//   Offset 42+40=82: conf  (u64 LE)
//   Offset 42+48=90: exponent (i32 LE)
// Total PriceUpdateV2 size: 134 bytes
// ---------------------------------------------------------------------------

/// Offset of the i64 aggregate price within Pyth PriceUpdateV2 account.
/// This is `PriceFeedMessage.price` at byte 74 (offset 42 + 32 for feed_id).
/// [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 98
const PYTH_PRICE_OFFSET: usize = 74;

/// Offset of the u64 confidence interval.
/// [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 99
const PYTH_CONF_OFFSET: usize = 82;

/// Offset of the i32 exponent (power of 10).
/// [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 100
const PYTH_EXPO_OFFSET: usize = 90;

/// Minimum data length for a valid Pyth PriceUpdateV2 account.
/// Total: 8 (disc) + 32 (write_auth) + 2 (verification) + 84 (PriceFeedMessage) + 8 (posted_slot) = 134.
/// [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 69
const PYTH_MIN_LEN: usize = 134;

// ---------------------------------------------------------------------------
// Switchboard V2 offsets
// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.2:
//   "V2 uses AggregatorAccountData"
//   Offset 366: latest_confirmed_round.result.mantissa (i128 LE)
//   Offset 382: latest_confirmed_round.result.scale    (u32 LE)
// ---------------------------------------------------------------------------

/// Offset of SwitchboardDecimal mantissa (i128) in V2 AggregatorAccountData.
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md line 242
const SB_V2_MANTISSA_OFFSET: usize = 366;

/// Offset of SwitchboardDecimal scale (u32).
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md line 243
const SB_V2_SCALE_OFFSET: usize = 382;

/// Minimum data length for V2 parsing (need mantissa + scale = 366 + 20).
const SB_V2_MIN_LEN: usize = 386;

// ---------------------------------------------------------------------------
// Switchboard On-Demand offsets
// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.3:
//   "On-Demand uses PullFeedAccountData"
//   Offset 2261: result.value (CurrentResult) i128 LE, 18 decimal precision
// ---------------------------------------------------------------------------

/// Offset of CurrentResult.value (i128) in On-Demand PullFeedAccountData.
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md line 265
const SB_OD_VALUE_OFFSET: usize = 2261;

/// Minimum data length for On-Demand parsing (need value = 2261 + 16).
const SB_OD_MIN_LEN: usize = 2277;

/// 18-decimal precision divisor for Switchboard On-Demand values.
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md line 267
const SB_OD_PRECISION: f64 = 1e18;

// ---------------------------------------------------------------------------
// OracleParser
// ---------------------------------------------------------------------------

/// Stateless oracle parser that converts raw account bytes into `OraclePrice`.
///
/// All parsing is zero-allocation (reads from byte slices directly) and designed
/// for the gRPC hot path where thousands of oracle updates arrive per second.
pub struct OracleParser;

impl OracleParser {
    /// Parse a Pyth PriceUpdateV2 or PriceFeedAccount from raw account data.
    ///
    /// Layout (total 134 bytes):
    /// ```text
    /// [0..8]    Anchor discriminator
    /// [8..40]   write_authority (Pubkey)
    /// [40..42]  verification_level (Borsh enum)
    /// [42..126] PriceFeedMessage:
    ///   [42..74]   feed_id ([u8; 32])
    ///   [74..82]   price (i64 LE)       <-- extracted
    ///   [82..90]   conf  (u64 LE)       <-- extracted
    ///   [90..94]   exponent (i32 LE)    <-- extracted
    ///   [94..102]  publish_time (i64 LE)
    ///   [102..110] prev_publish_time (i64 LE)
    ///   [110..118] ema_price (i64 LE)
    ///   [118..126] ema_conf  (u64 LE)
    /// [126..134] posted_slot (u64 LE)
    /// ```
    ///
    /// [VERIFIED 2026] solana_oracle_systems_complete_2026.md Section 2.2
    /// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.4
    pub fn parse_pyth_price(data: &[u8]) -> Option<OraclePrice> {
        if data.len() < PYTH_MIN_LEN {
            trace!(
                len = data.len(),
                "Pyth data too short (need {} bytes)",
                PYTH_MIN_LEN
            );
            return None;
        }

        // Extract i64 price at offset 74.
        // [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 98
        let price_raw = i64::from_le_bytes(
            data[PYTH_PRICE_OFFSET..PYTH_PRICE_OFFSET + 8]
                .try_into()
                .ok()?,
        );

        // Extract u64 confidence at offset 82.
        // [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 99
        let conf_raw = u64::from_le_bytes(
            data[PYTH_CONF_OFFSET..PYTH_CONF_OFFSET + 8]
                .try_into()
                .ok()?,
        );

        // Extract i32 exponent at offset 90.
        // [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 100
        let expo = i32::from_le_bytes(
            data[PYTH_EXPO_OFFSET..PYTH_EXPO_OFFSET + 4]
                .try_into()
                .ok()?,
        );

        // Convert: price_usd = price_raw * 10^expo
        // Example: 8036292213 * 10^(-8) = $80.36
        // [VERIFIED 2026] solana_oracle_systems_complete_2026.md line 121-123
        let scale = 10f64.powi(expo);
        let price_f64 = price_raw as f64 * scale;
        let confidence = conf_raw as f64 * scale;

        // Extract posted_slot at offset 126.
        let slot = if data.len() >= 134 {
            u64::from_le_bytes(data[126..134].try_into().unwrap_or([0; 8]))
        } else {
            0
        };

        // Reject zero/negative prices as invalid.
        if price_f64 <= 0.0 {
            return None;
        }

        Some(OraclePrice {
            price_f64,
            confidence,
            expo,
            slot: Slot(slot),
            source: OracleSource::PythOnChain,
        })
    }

    /// Parse a Switchboard V2 AggregatorAccountData from raw account data.
    ///
    /// V2 stores the latest confirmed result as a SwitchboardDecimal:
    /// ```text
    /// Offset 366: mantissa (i128 LE, 16 bytes)
    /// Offset 382: scale    (u32 LE, 4 bytes)
    /// ```
    /// Price = mantissa * 10^(-scale)
    ///
    /// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.2
    /// Program owner: SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f
    pub fn parse_switchboard_v2(data: &[u8]) -> Option<OraclePrice> {
        if data.len() < SB_V2_MIN_LEN {
            trace!(
                len = data.len(),
                "Switchboard V2 data too short (need {} bytes)",
                SB_V2_MIN_LEN
            );
            return None;
        }

        // Extract i128 mantissa at offset 366.
        // [VERIFIED 2026] oracle_monitoring_deep_2026.md line 242
        let mantissa = i128::from_le_bytes(
            data[SB_V2_MANTISSA_OFFSET..SB_V2_MANTISSA_OFFSET + 16]
                .try_into()
                .ok()?,
        );

        // Extract u32 scale at offset 382.
        // [VERIFIED 2026] oracle_monitoring_deep_2026.md line 243
        let scale = u32::from_le_bytes(
            data[SB_V2_SCALE_OFFSET..SB_V2_SCALE_OFFSET + 4]
                .try_into()
                .ok()?,
        );

        // Price = mantissa * 10^(-scale)
        // [VERIFIED 2026] oracle_monitoring_deep_2026.md line 245
        let price_f64 = mantissa as f64 * 10f64.powi(-(scale as i32));

        if price_f64 <= 0.0 {
            return None;
        }

        Some(OraclePrice {
            price_f64,
            confidence: 0.0, // V2 does not expose confidence in a simple field
            expo: -(scale as i32),
            slot: Slot(0), // V2 AggregatorAccountData doesn't have a simple slot field
            source: OracleSource::SwitchboardV2,
        })
    }

    /// Parse a Switchboard On-Demand PullFeedAccountData from raw account data.
    ///
    /// On-Demand stores CurrentResult.value as i128 with 18-decimal precision:
    /// ```text
    /// Offset 2261: value (i128 LE, 16 bytes)
    /// ```
    /// Price = value / 1e18
    ///
    /// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.3
    /// Program owner: SBondMDrcV3K4kxZR1HNVT7osZxAHVHgYXL5Ze1oMUv
    pub fn parse_switchboard_ondemand(data: &[u8]) -> Option<OraclePrice> {
        if data.len() < SB_OD_MIN_LEN {
            trace!(
                len = data.len(),
                "Switchboard On-Demand data too short (need {} bytes)",
                SB_OD_MIN_LEN
            );
            return None;
        }

        // Extract i128 value at offset 2261.
        // [VERIFIED 2026] oracle_monitoring_deep_2026.md line 265
        let value = i128::from_le_bytes(
            data[SB_OD_VALUE_OFFSET..SB_OD_VALUE_OFFSET + 16]
                .try_into()
                .ok()?,
        );

        // Price = value / 1e18
        // [VERIFIED 2026] oracle_monitoring_deep_2026.md line 267
        let price_f64 = value as f64 / SB_OD_PRECISION;

        if price_f64 <= 0.0 {
            return None;
        }

        Some(OraclePrice {
            price_f64,
            confidence: 0.0, // On-Demand exposes stddev, not simple confidence
            expo: -18,
            slot: Slot(0),
            source: OracleSource::SwitchboardOnDemand,
        })
    }

    /// Auto-detect oracle type from raw data and parse accordingly.
    ///
    /// Detection heuristics (when account owner is not available):
    /// 1. Length >= 2277 and non-zero at offset 2261 → Switchboard On-Demand
    /// 2. Length >= 386 and non-zero at offset 366 → Switchboard V2
    /// 3. Length >= 134 → Pyth PriceUpdateV2
    ///
    /// Prefer `parse_by_owner()` when the account owner pubkey is known, as it
    /// is definitive rather than heuristic-based.
    pub fn detect_and_parse(data: &[u8]) -> Option<OraclePrice> {
        // Try Switchboard On-Demand first (largest, most distinctive).
        if data.len() >= SB_OD_MIN_LEN {
            // Check if value at offset 2261 is non-zero.
            let value_bytes = &data[SB_OD_VALUE_OFFSET..SB_OD_VALUE_OFFSET + 16];
            if value_bytes.iter().any(|&b| b != 0) {
                if let Some(price) = Self::parse_switchboard_ondemand(data) {
                    return Some(price);
                }
            }
        }

        // Try Switchboard V2.
        if data.len() >= SB_V2_MIN_LEN {
            let mantissa_bytes = &data[SB_V2_MANTISSA_OFFSET..SB_V2_MANTISSA_OFFSET + 16];
            if mantissa_bytes.iter().any(|&b| b != 0) {
                if let Some(price) = Self::parse_switchboard_v2(data) {
                    return Some(price);
                }
            }
        }

        // Try Pyth PriceUpdateV2.
        if data.len() >= PYTH_MIN_LEN {
            if let Some(price) = Self::parse_pyth_price(data) {
                return Some(price);
            }
        }

        None
    }

    /// Parse oracle price using the account owner for definitive type detection.
    ///
    /// This is the preferred method when processing gRPC account updates, which
    /// always include the account owner.
    ///
    /// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.4
    pub fn parse_by_owner(data: &[u8], owner: &Pubkey) -> Option<OraclePrice> {
        let pyth_receiver =
            Pubkey::from_str(PYTH_RECEIVER_PROGRAM).ok()?;
        let pyth_feed =
            Pubkey::from_str(PYTH_PRICE_FEED_PROGRAM).ok()?;
        let sb_v2 =
            Pubkey::from_str(SWITCHBOARD_V2_PROGRAM).ok()?;
        let sb_od =
            Pubkey::from_str(SWITCHBOARD_ONDEMAND_PROGRAM).ok()?;

        if *owner == pyth_receiver || *owner == pyth_feed {
            Self::parse_pyth_price(data)
        } else if *owner == sb_v2 {
            Self::parse_switchboard_v2(data)
        } else if *owner == sb_od {
            Self::parse_switchboard_ondemand(data)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build synthetic Pyth PriceUpdateV2 data (134 bytes).
    fn make_pyth_data(price: i64, conf: u64, expo: i32, slot: u64) -> Vec<u8> {
        let mut data = vec![0u8; 134];
        // Discriminator at [0..8] — doesn't matter for parsing
        // write_authority at [8..40]
        // verification_level at [40..42]
        // PriceFeedMessage starts at 42:
        //   feed_id [42..74] — zeros is fine
        //   price [74..82]
        data[74..82].copy_from_slice(&price.to_le_bytes());
        //   conf [82..90]
        data[82..90].copy_from_slice(&conf.to_le_bytes());
        //   expo [90..94]
        data[90..94].copy_from_slice(&expo.to_le_bytes());
        // posted_slot [126..134]
        data[126..134].copy_from_slice(&slot.to_le_bytes());
        data
    }

    #[test]
    fn pyth_sol_usd_price() {
        // SOL at $80.36 with expo=-8
        // price_raw = 8_036_000_000, conf = 5_000_000
        let data = make_pyth_data(8_036_000_000, 5_000_000, -8, 300_000_000);
        let price = OracleParser::parse_pyth_price(&data).unwrap();

        assert!((price.price_f64 - 80.36).abs() < 0.01);
        assert!((price.confidence - 0.05).abs() < 0.001);
        assert_eq!(price.expo, -8);
        assert_eq!(price.slot.0, 300_000_000);
        assert_eq!(price.source, OracleSource::PythOnChain);
    }

    #[test]
    fn pyth_too_short() {
        let data = vec![0u8; 50]; // too short
        assert!(OracleParser::parse_pyth_price(&data).is_none());
    }

    #[test]
    fn pyth_zero_price_rejected() {
        let data = make_pyth_data(0, 0, -8, 100);
        assert!(OracleParser::parse_pyth_price(&data).is_none());
    }

    #[test]
    fn pyth_negative_price_rejected() {
        let data = make_pyth_data(-100, 0, -8, 100);
        assert!(OracleParser::parse_pyth_price(&data).is_none());
    }

    /// Build synthetic Switchboard V2 AggregatorAccountData.
    fn make_sb_v2_data(mantissa: i128, scale: u32) -> Vec<u8> {
        let mut data = vec![0u8; 400];
        data[SB_V2_MANTISSA_OFFSET..SB_V2_MANTISSA_OFFSET + 16]
            .copy_from_slice(&mantissa.to_le_bytes());
        data[SB_V2_SCALE_OFFSET..SB_V2_SCALE_OFFSET + 4]
            .copy_from_slice(&scale.to_le_bytes());
        data
    }

    #[test]
    fn switchboard_v2_sol_price() {
        // SOL at $80.50, mantissa=8050, scale=2 → 8050 * 10^(-2) = 80.50
        let data = make_sb_v2_data(8050, 2);
        let price = OracleParser::parse_switchboard_v2(&data).unwrap();

        assert!((price.price_f64 - 80.50).abs() < 0.01);
        assert_eq!(price.source, OracleSource::SwitchboardV2);
    }

    #[test]
    fn switchboard_v2_too_short() {
        let data = vec![0u8; 300];
        assert!(OracleParser::parse_switchboard_v2(&data).is_none());
    }

    /// Build synthetic Switchboard On-Demand PullFeedAccountData.
    fn make_sb_od_data(value: i128) -> Vec<u8> {
        let mut data = vec![0u8; 2300];
        data[SB_OD_VALUE_OFFSET..SB_OD_VALUE_OFFSET + 16]
            .copy_from_slice(&value.to_le_bytes());
        data
    }

    #[test]
    fn switchboard_ondemand_sol_price() {
        // SOL at $80.50: value = 80_500_000_000_000_000_000 (80.5 * 1e18)
        let value: i128 = 80_500_000_000_000_000_000;
        let data = make_sb_od_data(value);
        let price = OracleParser::parse_switchboard_ondemand(&data).unwrap();

        assert!((price.price_f64 - 80.50).abs() < 0.01);
        assert_eq!(price.source, OracleSource::SwitchboardOnDemand);
    }

    #[test]
    fn switchboard_ondemand_too_short() {
        let data = vec![0u8; 2000];
        assert!(OracleParser::parse_switchboard_ondemand(&data).is_none());
    }

    #[test]
    fn detect_pyth() {
        let data = make_pyth_data(8_036_000_000, 5_000_000, -8, 100);
        let price = OracleParser::detect_and_parse(&data).unwrap();
        assert_eq!(price.source, OracleSource::PythOnChain);
        assert!((price.price_f64 - 80.36).abs() < 0.01);
    }

    #[test]
    fn detect_switchboard_v2() {
        let data = make_sb_v2_data(8050, 2);
        let price = OracleParser::detect_and_parse(&data).unwrap();
        assert_eq!(price.source, OracleSource::SwitchboardV2);
    }

    #[test]
    fn detect_switchboard_ondemand() {
        let value: i128 = 80_500_000_000_000_000_000;
        let data = make_sb_od_data(value);
        let price = OracleParser::detect_and_parse(&data).unwrap();
        assert_eq!(price.source, OracleSource::SwitchboardOnDemand);
    }

    #[test]
    fn parse_by_owner_pyth_receiver() {
        let owner = Pubkey::from_str(PYTH_RECEIVER_PROGRAM).unwrap();
        let data = make_pyth_data(8_036_000_000, 5_000_000, -8, 100);
        let price = OracleParser::parse_by_owner(&data, &owner).unwrap();
        assert_eq!(price.source, OracleSource::PythOnChain);
    }

    #[test]
    fn parse_by_owner_pyth_feed() {
        let owner = Pubkey::from_str(PYTH_PRICE_FEED_PROGRAM).unwrap();
        let data = make_pyth_data(8_036_000_000, 5_000_000, -8, 100);
        let price = OracleParser::parse_by_owner(&data, &owner).unwrap();
        assert_eq!(price.source, OracleSource::PythOnChain);
    }

    #[test]
    fn parse_by_owner_switchboard_v2() {
        let owner = Pubkey::from_str(SWITCHBOARD_V2_PROGRAM).unwrap();
        let data = make_sb_v2_data(8050, 2);
        let price = OracleParser::parse_by_owner(&data, &owner).unwrap();
        assert_eq!(price.source, OracleSource::SwitchboardV2);
    }

    #[test]
    fn parse_by_owner_switchboard_ondemand() {
        let owner = Pubkey::from_str(SWITCHBOARD_ONDEMAND_PROGRAM).unwrap();
        let value: i128 = 80_500_000_000_000_000_000;
        let data = make_sb_od_data(value);
        let price = OracleParser::parse_by_owner(&data, &owner).unwrap();
        assert_eq!(price.source, OracleSource::SwitchboardOnDemand);
    }

    #[test]
    fn parse_by_owner_unknown() {
        let owner = Pubkey::default();
        let data = make_pyth_data(8_036_000_000, 5_000_000, -8, 100);
        assert!(OracleParser::parse_by_owner(&data, &owner).is_none());
    }

    #[test]
    fn detect_empty_data() {
        assert!(OracleParser::detect_and_parse(&[]).is_none());
    }
}
