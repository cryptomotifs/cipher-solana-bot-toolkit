//! AccountDecoder — zero-copy parsing of gRPC account data on the hot path.
//!
//! All parsing uses raw byte slicing (0 allocations, ~50ns per field extraction).
//! NO Borsh, NO String conversion, NO bs58 encoding on the hot path.
//!
//! [VERIFIED 2026] low_latency_dataflow_2026.md Section 1:
//!   "Raw Byte Slicing (Fastest — 0 allocations). Direct field extraction from raw
//!    byte slices. Cost: ~50ns per field extraction. No allocation. No copy."
//!
//! [VERIFIED 2026] low_latency_dataflow_2026.md Section 1 (Production Bot Pattern):
//!   "Borsh and Anchor frameworks removed — Standard frameworks add significant overhead.
//!    Using bytemuck for zero-copy transmutation. Microsecond-level deserialization
//!    vs milliseconds with Borsh/Anchor."
//!
//! [VERIFIED 2026] low_latency_dataflow_2026.md Section 5:
//!   "Zero-Allocation Hot Path Checklist — no Vec::new(), no String::from(),
//!    no .clone() on Vec/String, no format!(), no to_string()"

use predator_core::{DexType, OraclePrice, OracleSource, Slot};

// ---------------------------------------------------------------------------
// Oracle price decoding
// ---------------------------------------------------------------------------

/// Decode a Pyth oracle price from raw account data bytes.
///
/// Expected input: 20-byte data slice from gRPC accounts_data_slice
/// (offset 74, length 20 in the full Pyth price feed account).
///
/// Layout (within the 20-byte slice):
///   bytes 0..8:   i64 price (little-endian)
///   bytes 8..16:  i64 confidence (little-endian, unsigned width)
///   bytes 16..20: i32 exponent (little-endian)
///
/// [VERIFIED 2026] bot_architecture_deep_2026.md Section 2d:
///   "Pyth price accounts: price is at offset 74, 20 bytes (i64 price + i64 conf + i32 expo)"
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1:
///   "Extract u64 from account data at known offset — ZERO allocation"
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 1.2:
///   "price.price, price.conf, price.expo from parsed data"
#[inline]
pub fn decode_oracle_price(data: &[u8], slot: u64) -> Option<OraclePrice> {
    // Need exactly 20 bytes: i64(8) + i64(8) + i32(4)
    if data.len() < 20 {
        return None;
    }

    // Zero-copy extraction — no heap allocation.
    // [VERIFIED 2026] low_latency_dataflow_2026.md Section 1: "~50ns per field extraction"
    let raw_price = i64::from_le_bytes(read_8(data, 0));
    let raw_conf = i64::from_le_bytes(read_8(data, 8));
    let expo = i32::from_le_bytes(read_4(data, 16));

    // Convert to f64 using the exponent.
    // price_f64 = raw_price * 10^expo
    let multiplier = 10f64.powi(expo);
    let price_f64 = raw_price as f64 * multiplier;
    let confidence = raw_conf.unsigned_abs() as f64 * multiplier;

    // Sanity check: price must be positive.
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

/// Decode a Switchboard V2 oracle price from raw account data.
///
/// Layout: AggregatorAccountData
///   offset 366: i128 mantissa (16 bytes, little-endian)
///   offset 382: u32 scale (4 bytes, little-endian)
///   price = mantissa * 10^(-scale)
///
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.2:
///   "V2 parsing (offset 366 for SwitchboardDecimal)"
#[inline]
pub fn decode_switchboard_v2_price(data: &[u8], slot: u64) -> Option<OraclePrice> {
    if data.len() < 386 {
        return None;
    }

    let mantissa = i128::from_le_bytes(read_16(data, 366));
    let scale = u32::from_le_bytes(read_4(data, 382));
    let price_f64 = mantissa as f64 * 10f64.powi(-(scale as i32));

    if price_f64 <= 0.0 {
        return None;
    }

    Some(OraclePrice {
        price_f64,
        confidence: 0.0, // Switchboard V2 doesn't expose confidence the same way
        expo: -(scale as i32),
        slot: Slot(slot),
        source: OracleSource::SwitchboardV2,
    })
}

/// Decode a Switchboard On-Demand oracle price from raw account data.
///
/// Layout: PullFeedAccountData
///   offset 2261: i128 value (16 bytes, little-endian, 18 decimal precision)
///   price = value / 1e18
///
/// [VERIFIED 2026] oracle_monitoring_deep_2026.md Section 2.3:
///   "On-Demand parsing (offset 2261 for CurrentResult.value)"
#[inline]
pub fn decode_switchboard_ondemand_price(data: &[u8], slot: u64) -> Option<OraclePrice> {
    if data.len() < 2277 {
        return None;
    }

    let value = i128::from_le_bytes(read_16(data, 2261));
    let price_f64 = value as f64 / 1e18;

    if price_f64 <= 0.0 {
        return None;
    }

    Some(OraclePrice {
        price_f64,
        confidence: 0.0,
        expo: -18,
        slot: Slot(slot),
        source: OracleSource::SwitchboardOnDemand,
    })
}

// ---------------------------------------------------------------------------
// Pool state decoding
// ---------------------------------------------------------------------------

/// Decoded pool price information.
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 214:
///   "PoolPrice struct { price_f64, tick_current, liquidity }"
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PoolPrice {
    /// Current price as f64 (derived from sqrtPriceX64 or reserves).
    pub price_f64: f64,
    /// Current tick index (for concentrated liquidity pools). 0 for constant-product.
    pub tick_current: i32,
    /// Current pool liquidity. Interpretation varies by DEX type.
    pub liquidity: u128,
}

/// Decode pool state from raw account data based on DEX type.
///
/// Extracts sqrtPriceX64 for CLMM pools and reserve ratios for constant-product pools.
/// All extraction is zero-copy byte slicing — no Borsh, no allocation.
///
/// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1:
///   "bytemuck zero-copy transmutation" for pool states
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 212:
///   "decode_pool_state(data, dex: DexType) -> Option<PoolPrice> — sqrtPriceX64 extraction"
#[inline]
pub fn decode_pool_state(data: &[u8], dex: DexType) -> Option<PoolPrice> {
    match dex {
        DexType::RaydiumClmm => decode_raydium_clmm(data),
        DexType::OrcaWhirlpool => decode_orca_whirlpool(data),
        DexType::RaydiumV4 => decode_raydium_v4(data),
        DexType::RaydiumCpmm => decode_raydium_cpmm(data),
        DexType::MeteoraDlmm => decode_meteora_dlmm(data),
        DexType::PumpSwap => decode_pumpswap(data),
    }
}

/// Decode Raydium CLMM pool state.
///
/// Layout (after 8-byte discriminator):
///   offset 253: u128 sqrtPriceX64 (16 bytes, LE)
///   offset 269: i32 tick_current (4 bytes, LE)
///   offset 281: u128 liquidity (16 bytes, LE)
///
/// sqrtPriceX64 to price: price = (sqrtPriceX64 / 2^64)^2
///
/// [VERIFIED 2026] dex_token_coverage_2026.md: Raydium CLMM pool layout
fn decode_raydium_clmm(data: &[u8]) -> Option<PoolPrice> {
    if data.len() < 297 {
        return None;
    }
    let sqrt_price_x64 = u128::from_le_bytes(read_16(data, 253));
    let tick_current = i32::from_le_bytes(read_4(data, 269));
    let liquidity = u128::from_le_bytes(read_16(data, 281));

    let price_f64 = sqrt_price_x64_to_price(sqrt_price_x64);
    Some(PoolPrice {
        price_f64,
        tick_current,
        liquidity,
    })
}

/// Decode Orca Whirlpool state.
///
/// Layout (after 8-byte discriminator):
///   offset 65: u128 sqrtPrice (16 bytes, LE) — same X64 format as Raydium CLMM
///   offset 81: i32 tickCurrentIndex (4 bytes, LE)
///   offset 89: u128 liquidity (16 bytes, LE)
///
/// [VERIFIED 2026] dex_token_coverage_2026.md: Orca Whirlpool layout
fn decode_orca_whirlpool(data: &[u8]) -> Option<PoolPrice> {
    if data.len() < 105 {
        return None;
    }
    let sqrt_price_x64 = u128::from_le_bytes(read_16(data, 65));
    let tick_current = i32::from_le_bytes(read_4(data, 81));
    let liquidity = u128::from_le_bytes(read_16(data, 89));

    let price_f64 = sqrt_price_x64_to_price(sqrt_price_x64);
    Some(PoolPrice {
        price_f64,
        tick_current,
        liquidity,
    })
}

/// Decode Raydium V4 AMM (constant-product).
///
/// Uses token reserve amounts to compute price = reserve_b / reserve_a.
/// Layout (after 8-byte discriminator):
///   offset 224: u64 pool_coin_token_account_amount (reserve_a)
///   offset 232: u64 pool_pc_token_account_amount (reserve_b)
///
/// [VERIFIED 2026] bot_architecture_deep_2026.md line 422: Raydium V4 pool
fn decode_raydium_v4(data: &[u8]) -> Option<PoolPrice> {
    if data.len() < 240 {
        return None;
    }
    let reserve_a = u64::from_le_bytes(read_8(data, 224));
    let reserve_b = u64::from_le_bytes(read_8(data, 232));

    if reserve_a == 0 {
        return None;
    }

    let price_f64 = reserve_b as f64 / reserve_a as f64;
    Some(PoolPrice {
        price_f64,
        tick_current: 0,
        liquidity: reserve_a as u128 + reserve_b as u128,
    })
}

/// Decode Raydium CPMM (newer constant-product).
///
/// Layout (after 8-byte discriminator):
///   offset 168: u64 token_0_vault_amount
///   offset 176: u64 token_1_vault_amount
fn decode_raydium_cpmm(data: &[u8]) -> Option<PoolPrice> {
    if data.len() < 184 {
        return None;
    }
    let reserve_a = u64::from_le_bytes(read_8(data, 168));
    let reserve_b = u64::from_le_bytes(read_8(data, 176));

    if reserve_a == 0 {
        return None;
    }

    let price_f64 = reserve_b as f64 / reserve_a as f64;
    Some(PoolPrice {
        price_f64,
        tick_current: 0,
        liquidity: reserve_a as u128 + reserve_b as u128,
    })
}

/// Decode Meteora DLMM pool state.
///
/// Layout (after 8-byte discriminator):
///   offset 104: i32 active_id (current bin)
///   offset 108: u16 bin_step
///
/// Price from active bin: price = (1 + bin_step / 10000) ^ (active_id - 2^23)
/// Simplified: uses the bin_step-based formula.
fn decode_meteora_dlmm(data: &[u8]) -> Option<PoolPrice> {
    if data.len() < 110 {
        return None;
    }
    let active_id = i32::from_le_bytes(read_4(data, 104));
    let bin_step = u16::from_le_bytes(read_2(data, 108));

    if bin_step == 0 {
        return None;
    }

    // Price formula: (1 + bin_step/10000)^(active_id - 8388608)
    // 8388608 = 2^23 (Meteora's zero-point offset)
    let base = 1.0 + (bin_step as f64 / 10_000.0);
    let exponent = active_id as f64 - 8_388_608.0;
    let price_f64 = base.powf(exponent);

    Some(PoolPrice {
        price_f64,
        tick_current: active_id,
        liquidity: 0, // DLMM doesn't have a single liquidity value
    })
}

/// Decode PumpSwap AMM (graduated pump.fun token pools).
///
/// Layout (after 8-byte discriminator):
///   offset 193: u64 pool_base_token_reserves
///   offset 201: u64 pool_quote_token_reserves
///
/// [VERIFIED 2026] PREDATOR_ARCHITECTURE_2026.md line 154: PumpSwap AMM
fn decode_pumpswap(data: &[u8]) -> Option<PoolPrice> {
    if data.len() < 209 {
        return None;
    }
    let reserve_base = u64::from_le_bytes(read_8(data, 193));
    let reserve_quote = u64::from_le_bytes(read_8(data, 201));

    if reserve_base == 0 {
        return None;
    }

    let price_f64 = reserve_quote as f64 / reserve_base as f64;
    Some(PoolPrice {
        price_f64,
        tick_current: 0,
        liquidity: reserve_base as u128 + reserve_quote as u128,
    })
}

// ---------------------------------------------------------------------------
// Helper: sqrtPriceX64 -> price conversion
// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1: zero-copy math
// ---------------------------------------------------------------------------

/// Convert sqrtPriceX64 (Q64.64 fixed-point) to a float price.
///
/// sqrtPriceX64 = sqrt(price) * 2^64
/// price = (sqrtPriceX64 / 2^64)^2
///
/// Used by Raydium CLMM and Orca Whirlpool.
#[inline]
fn sqrt_price_x64_to_price(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    sqrt_price * sqrt_price
}

// ---------------------------------------------------------------------------
// Byte reading helpers — zero allocation, bounds checked at call site
// [VERIFIED 2026] low_latency_dataflow_2026.md Section 1:
//   "fn read_u64(data: &[u8], offset: usize) -> u64"
// ---------------------------------------------------------------------------

#[inline(always)]
fn read_2(data: &[u8], offset: usize) -> [u8; 2] {
    let mut buf = [0u8; 2];
    buf.copy_from_slice(&data[offset..offset + 2]);
    buf
}

#[inline(always)]
fn read_4(data: &[u8], offset: usize) -> [u8; 4] {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&data[offset..offset + 4]);
    buf
}

#[inline(always)]
fn read_8(data: &[u8], offset: usize) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[offset..offset + 8]);
    buf
}

#[inline(always)]
fn read_16(data: &[u8], offset: usize) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&data[offset..offset + 16]);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_pyth_price_valid() {
        // Construct a 20-byte Pyth price slice:
        // price=8050000000 (i64), conf=5000000 (i64), expo=-8 (i32)
        // Expected price_f64 = 8050000000 * 10^-8 = 80.50
        let mut data = [0u8; 20];
        data[0..8].copy_from_slice(&8050000000i64.to_le_bytes());
        data[8..16].copy_from_slice(&5000000i64.to_le_bytes());
        data[16..20].copy_from_slice(&(-8i32).to_le_bytes());

        let price = decode_oracle_price(&data, 100).unwrap();
        assert!((price.price_f64 - 80.50).abs() < 0.01);
        assert!((price.confidence - 0.05).abs() < 0.01);
        assert_eq!(price.expo, -8);
        assert_eq!(price.slot.0, 100);
        assert_eq!(price.source, OracleSource::PythOnChain);
    }

    #[test]
    fn decode_pyth_price_too_short() {
        let data = [0u8; 19]; // 1 byte too short
        assert!(decode_oracle_price(&data, 0).is_none());
    }

    #[test]
    fn decode_pyth_price_zero_rejected() {
        let data = [0u8; 20]; // All zeros = price 0 -> rejected
        assert!(decode_oracle_price(&data, 0).is_none());
    }

    #[test]
    fn decode_pyth_price_negative_rejected() {
        let mut data = [0u8; 20];
        data[0..8].copy_from_slice(&(-1i64).to_le_bytes());
        data[16..20].copy_from_slice(&0i32.to_le_bytes());
        assert!(decode_oracle_price(&data, 0).is_none());
    }

    #[test]
    fn sqrt_price_x64_conversion() {
        // sqrtPriceX64 for price ~1.0: sqrt(1) * 2^64 = 2^64
        let sqrt = 1u128 << 64;
        let price = sqrt_price_x64_to_price(sqrt);
        assert!((price - 1.0).abs() < 0.0001);

        // sqrtPriceX64 for price ~4.0: sqrt(4) * 2^64 = 2 * 2^64
        let sqrt4 = 2u128 << 64;
        let price4 = sqrt_price_x64_to_price(sqrt4);
        assert!((price4 - 4.0).abs() < 0.0001);
    }

    #[test]
    fn decode_pool_raydium_v4() {
        let mut data = vec![0u8; 240];
        // reserve_a at offset 224, reserve_b at offset 232
        data[224..232].copy_from_slice(&1_000_000u64.to_le_bytes()); // 1M base
        data[232..240].copy_from_slice(&80_000_000u64.to_le_bytes()); // 80M quote

        let pool = decode_pool_state(&data, DexType::RaydiumV4).unwrap();
        assert!((pool.price_f64 - 80.0).abs() < 0.01);
        assert_eq!(pool.tick_current, 0); // constant-product, no tick
    }

    #[test]
    fn decode_pool_too_short() {
        let data = vec![0u8; 10];
        assert!(decode_pool_state(&data, DexType::RaydiumV4).is_none());
        assert!(decode_pool_state(&data, DexType::OrcaWhirlpool).is_none());
        assert!(decode_pool_state(&data, DexType::RaydiumClmm).is_none());
    }

    #[test]
    fn decode_pool_zero_reserve_rejected() {
        let data = vec![0u8; 240];
        // reserve_a = 0 -> division by zero -> None
        assert!(decode_pool_state(&data, DexType::RaydiumV4).is_none());
    }

    #[test]
    fn pool_price_struct() {
        let pp = PoolPrice {
            price_f64: 80.5,
            tick_current: -12345,
            liquidity: 1_000_000,
        };
        assert!((pp.price_f64 - 80.5).abs() < f64::EPSILON);
        assert_eq!(pp.tick_current, -12345);
        assert_eq!(pp.liquidity, 1_000_000);
    }

    #[test]
    fn decode_switchboard_v2_valid() {
        let mut data = vec![0u8; 386];
        // mantissa at offset 366 (i128), scale at offset 382 (u32)
        // mantissa = 8050 (i128), scale = 2 -> price = 8050 * 10^-2 = 80.50
        data[366..382].copy_from_slice(&8050i128.to_le_bytes());
        data[382..386].copy_from_slice(&2u32.to_le_bytes());

        let price = decode_switchboard_v2_price(&data, 200).unwrap();
        assert!((price.price_f64 - 80.50).abs() < 0.01);
        assert_eq!(price.source, OracleSource::SwitchboardV2);
    }

    #[test]
    fn decode_switchboard_v2_too_short() {
        let data = vec![0u8; 385];
        assert!(decode_switchboard_v2_price(&data, 0).is_none());
    }

    #[test]
    fn decode_switchboard_ondemand_valid() {
        let mut data = vec![0u8; 2277];
        // value at offset 2261 (i128, 18 decimal precision)
        // value = 80_500_000_000_000_000_000 (80.5 * 1e18)
        let value: i128 = 80_500_000_000_000_000_000;
        data[2261..2277].copy_from_slice(&value.to_le_bytes());

        let price = decode_switchboard_ondemand_price(&data, 300).unwrap();
        assert!((price.price_f64 - 80.50).abs() < 0.01);
        assert_eq!(price.source, OracleSource::SwitchboardOnDemand);
    }
}
