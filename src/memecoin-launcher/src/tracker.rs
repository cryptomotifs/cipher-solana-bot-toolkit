//! P&L tracker — per-token launch tracking with JSON persistence.
//!
//! [VERIFIED 2026] memecoin_launcher_strategy_2026.md s17: "Track P&L rigorously"

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Status of a launched token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TokenStatus {
    Active,
    Sold,
    Dead,
    Graduated,
}

/// Record of a single token launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub mint: String,
    pub name: String,
    pub symbol: String,
    pub platform: String,
    pub narrative: String,
    pub created_at: String, // ISO8601
    pub creation_cost_lamports: u64,
    pub trader_buy_lamports: u64,
    pub trader_sell_lamports: u64,
    pub fees_collected_lamports: u64,
    pub status: TokenStatus,
    pub creator_tx: String,
    pub buyer_tx: String,
}

/// Aggregate P&L data across all launches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherPnL {
    pub total_spent_lamports: u64,
    pub total_fees_collected_lamports: u64,
    pub total_trade_pnl_lamports: i64,
    pub tokens_launched: u32,
    pub tokens_graduated: u32,
    pub records: Vec<LaunchRecord>,
}

impl Default for LauncherPnL {
    fn default() -> Self {
        Self {
            total_spent_lamports: 0,
            total_fees_collected_lamports: 0,
            total_trade_pnl_lamports: 0,
            tokens_launched: 0,
            tokens_graduated: 0,
            records: Vec::new(),
        }
    }
}

impl LauncherPnL {
    /// Load tracker from JSON file, or create new if not found.
    pub fn load(path: &str) -> Result<Self> {
        if std::path::Path::new(path).exists() {
            let data = std::fs::read_to_string(path)?;
            let pnl: Self = serde_json::from_str(&data)?;
            Ok(pnl)
        } else {
            Ok(Self::default())
        }
    }

    /// Save tracker to JSON file.
    pub fn save(&self, path: &str) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Add a new launch record.
    pub fn add_record(&mut self, record: LaunchRecord) {
        self.total_spent_lamports += record.creation_cost_lamports;
        self.tokens_launched += 1;
        self.records.push(record);
    }

    /// Get total SOL spent today.
    pub fn daily_spend(&self, date: &str) -> u64 {
        self.records
            .iter()
            .filter(|r| r.created_at.starts_with(date))
            .map(|r| r.creation_cost_lamports)
            .sum()
    }

    /// Count tokens launched today.
    pub fn tokens_launched_today(&self, date: &str) -> u32 {
        self.records
            .iter()
            .filter(|r| r.created_at.starts_with(date))
            .count() as u32
    }

    /// Get all active (unsold) positions.
    pub fn active_positions(&self) -> Vec<&LaunchRecord> {
        self.records
            .iter()
            .filter(|r| r.status == TokenStatus::Active)
            .collect()
    }

    /// Check if a token name was recently used (dedup).
    pub fn name_recently_used(&self, name: &str, days: u32) -> bool {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
        let cutoff_str = cutoff.format("%Y-%m-%d").to_string();
        self.records
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(name) && r.created_at >= cutoff_str)
    }

    /// Net P&L in lamports.
    pub fn net_pnl_lamports(&self) -> i64 {
        self.total_fees_collected_lamports as i64
            + self.total_trade_pnl_lamports
            - self.total_spent_lamports as i64
    }
}
