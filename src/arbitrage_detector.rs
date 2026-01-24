use crate::monitor::MarketSnapshot;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::{HashSet, HashMap};

/// Arbitrage detector that finds price discrepancies (like target address strategy)
/// Buys cheaper tokens when price gaps appear, allows multiple entries per period
pub struct ArbitrageDetector {
    min_price_gap: f64,           // Minimum price gap to trigger buy (e.g., 0.05 = 5¢)
    max_entry_price: f64,         // Maximum price to buy at (e.g., 0.50 = 50¢)
    min_profit_target: f64,       // Minimum profit target before selling (e.g., 0.10 = 10¢)
    entries_per_period: usize,     // Max entries per period (0 = unlimited)
    reentry_price_movement: f64,   // Minimum price movement (%) required for re-entry (e.g., 0.05 = 5%)
    min_remaining_time_seconds: u64, // Minimum remaining time (seconds) required before buying
    current_period_entries: Arc<Mutex<HashSet<String>>>, // Track unique token entries (for re-entry tracking)
    last_entry_prices: Arc<Mutex<HashMap<String, f64>>>, // Track last entry price per token_id+period
    total_entries_this_period: Arc<Mutex<usize>>, // Track total trades this period (for limit check)
    failed_attempts: Arc<Mutex<HashMap<String, (u64, std::time::Instant)>>>, // Track failed attempts: (count, last_attempt_time)
}

#[derive(Debug, Clone)]
pub struct ArbitrageOpportunity {
    pub up_token_id: String,           // Up token ID
    pub down_token_id: String,         // Down token ID
    pub up_token_name: String,         // "BTC Up" or "ETH Up"
    pub down_token_name: String,       // "BTC Down" or "ETH Down"
    pub condition_id: String,          // Market condition ID
    pub up_bid_price: f64,             // Up token BID price (for detection)
    pub down_bid_price: f64,           // Down token BID price (for detection)
    pub price_gap: f64,                 // Price gap between Up and Down tokens
    pub period_timestamp: u64,
    pub time_remaining_seconds: u64,
    pub market_type: MarketType,        // BTC or ETH
    pub market_duration: u64,           // Market duration in seconds (900 for 15m, 3600 for 1h)
    pub min_profit_target: f64,        // Minimum profit target for selling
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarketType {
    BTC,
    ETH,
}

impl ArbitrageDetector {
    pub fn new(
        min_price_gap: f64,
        max_entry_price: f64,
        min_profit_target: f64,
        entries_per_period: usize,
        reentry_price_movement: f64,
        min_remaining_time_seconds: u64,
    ) -> Self {
        Self {
            min_price_gap,
            max_entry_price,
            min_profit_target,
            entries_per_period,
            reentry_price_movement,
            min_remaining_time_seconds,
            current_period_entries: Arc::new(Mutex::new(HashSet::new())),
            last_entry_prices: Arc::new(Mutex::new(HashMap::new())),
            total_entries_this_period: Arc::new(Mutex::new(0)),
            failed_attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Detect arbitrage opportunities in all 4 markets (ETH 15m, ETH 1h, BTC 15m, BTC 1h)
    /// Returns opportunities for cheaper tokens when price gaps exist
    pub async fn detect_opportunities(&self, snapshot: &MarketSnapshot) -> Vec<ArbitrageOpportunity> {
        let mut opportunities = Vec::new();

        // Check ETH 15m market
        if snapshot.eth_15m_time_remaining > 0 {
            if let Some(opp) = self.check_market(
                &snapshot.eth_market_15m,
                MarketType::ETH,
                snapshot.eth_15m_period_timestamp,
                snapshot.eth_15m_time_remaining,
                900, // 15 minutes
            ).await {
                opportunities.push(opp);
            }
        }

        // Check ETH 1h market
        if snapshot.eth_1h_time_remaining > 0 {
            if let Some(opp) = self.check_market(
                &snapshot.eth_market_1h,
                MarketType::ETH,
                snapshot.eth_1h_period_timestamp,
                snapshot.eth_1h_time_remaining,
                3600, // 1 hour
            ).await {
                opportunities.push(opp);
            }
        }

        // Check BTC 15m market
        if snapshot.btc_15m_time_remaining > 0 {
            if let Some(opp) = self.check_market(
                &snapshot.btc_market_15m,
                MarketType::BTC,
                snapshot.btc_15m_period_timestamp,
                snapshot.btc_15m_time_remaining,
                900, // 15 minutes
            ).await {
                opportunities.push(opp);
            }
        }

        // Check BTC 1h market
        if snapshot.btc_1h_time_remaining > 0 {
            if let Some(opp) = self.check_market(
                &snapshot.btc_market_1h,
                MarketType::BTC,
                snapshot.btc_1h_period_timestamp,
                snapshot.btc_1h_time_remaining,
                3600, // 1 hour
            ).await {
                opportunities.push(opp);
            }
        }

        opportunities
    }

    /// Check a single market (BTC or ETH) for arbitrage opportunities
    async fn check_market(
        &self,
        market: &crate::models::MarketData,
        market_type: MarketType,
        period_timestamp: u64,
        time_remaining: u64,
        market_duration: u64,
    ) -> Option<ArbitrageOpportunity> {
        let up_token = market.up_token.as_ref()?;
        let down_token = market.down_token.as_ref()?;

        // Get BID prices for opportunity detection (comparing which token is cheaper)
        // Note: BID = what buyers are willing to pay (lower)
        //       ASK = what sellers are asking (higher)
        // When we actually buy, we'll pay ASK price (higher than BID)
        // When we actually sell, we'll receive BID price (lower than ASK)
        let up_bid = decimal_to_f64(up_token.bid.unwrap_or(Decimal::ZERO));
        let down_bid = decimal_to_f64(down_token.bid.unwrap_or(Decimal::ZERO));

        // Skip if prices are invalid
        if up_bid <= 0.001 || down_bid <= 0.001 {
            return None;
        }

        // Calculate price gap using BID prices
        let price_gap = (up_bid - down_bid).abs();

        // Check if gap is large enough
        if price_gap < self.min_price_gap {
            return None;
        }

        // Check minimum remaining time - need enough time to profit
        // For 15-minute markets: use configured minimum (default 120s = 2 minutes)
        // For 1-hour markets: use 5 minutes (300s) minimum, or configured value if higher
        let required_min_time = if market_duration == 900 {
            // 15-minute market: use configured minimum (default 120s)
            self.min_remaining_time_seconds
        } else {
            // 1-hour market: require at least 5 minutes (300s), or configured value if higher
            std::cmp::max(300, self.min_remaining_time_seconds)
        };
        
        if time_remaining < required_min_time {
            return None; // Not enough time remaining to profit
        }
        
        // Market-neutral strategy: Buy BOTH tokens when both are in valid price range
        // Check if both tokens are within acceptable price range
        let min_entry_price = 0.1; // Minimum price to buy (configurable, but default to $0.10)
        let max_entry_price = self.max_entry_price;
        
        // Both tokens must be in valid range for market-neutral strategy
        let up_valid = up_bid >= min_entry_price && (max_entry_price == 0.0 || up_bid <= max_entry_price);
        let down_valid = down_bid >= min_entry_price && (max_entry_price == 0.0 || down_bid <= max_entry_price);
        
        if !up_valid || !down_valid {
            return None; // Skip if either token is outside valid range
        }

        // Get token names
        let up_name = match market_type {
            MarketType::BTC => "BTC Up",
            MarketType::ETH => "ETH Up",
        };
        let down_name = match market_type {
            MarketType::BTC => "BTC Down",
            MarketType::ETH => "ETH Down",
        };

        // Check if we can re-enter this market (allow multiple entries if price moved enough)
        // Use condition_id as key since we're buying both tokens together
        let entry_key = format!("{}_{}", market.condition_id, period_timestamp);
        let mut last_prices = self.last_entry_prices.lock().await;
        if let Some(&last_price) = last_prices.get(&entry_key) {
            // Calculate price movement since last entry (use average of both tokens)
            let avg_price = (up_bid + down_bid) / 2.0;
            let price_movement = (avg_price - last_price).abs() / last_price;
            
            // Only allow re-entry if price moved by required percentage
            if price_movement < self.reentry_price_movement {
                drop(last_prices);
                return None; // Price hasn't moved enough for re-entry
            }
        }
        drop(last_prices);
        
        // Check if we recently failed to buy in this market (prevent spam attempts)
        let attempt_key = format!("{}_{}", market.condition_id, period_timestamp);
        let mut failed = self.failed_attempts.lock().await;
        if let Some((count, last_time)) = failed.get(&attempt_key) {
            // If we failed 3+ times in the last 30 seconds, skip this opportunity
            if *count >= 3 && last_time.elapsed().as_secs() < 30 {
                drop(failed);
                return None; // Too many recent failures, skip
            }
            // Clear old failures (older than 60 seconds)
            if last_time.elapsed().as_secs() > 60 {
                failed.remove(&attempt_key);
            }
        }
        drop(failed);
        
        // Check entry limit (total trades across all markets)
        // Note: Buying both tokens counts as 1 entry (1 market position)
        if self.entries_per_period > 0 {
            let total_entries = *self.total_entries_this_period.lock().await;
            if total_entries >= self.entries_per_period {
                return None;
            }
        }

        Some(ArbitrageOpportunity {
            up_token_id: up_token.token_id.clone(),
            down_token_id: down_token.token_id.clone(),
            up_token_name: up_name.to_string(),
            down_token_name: down_name.to_string(),
            condition_id: market.condition_id.clone(),
            up_bid_price: up_bid,
            down_bid_price: down_bid,
            price_gap,
            period_timestamp,
            time_remaining_seconds: time_remaining,
            market_type,
            market_duration,
            min_profit_target: self.min_profit_target,
        })
    }

    /// Mark that we entered a position for this market in this period
    /// This counts as a new entry for the limit check (buying both tokens = 1 entry)
    pub async fn mark_entry(&self, condition_id: &str, period_timestamp: u64, avg_entry_price: f64) {
        let entry_key = format!("{}_{}", condition_id, period_timestamp);
        
        // Track last entry price for re-entry checks (use average of both tokens)
        let mut last_prices = self.last_entry_prices.lock().await;
        last_prices.insert(entry_key.clone(), avg_entry_price);
        drop(last_prices);
        
        // Track unique market entries (for reference, not for limit)
        let mut entries = self.current_period_entries.lock().await;
        entries.insert(entry_key);
        drop(entries);
        
        // Increment total trade counter (this is what limits total trades)
        // Buying both tokens counts as 1 entry
        let mut total = self.total_entries_this_period.lock().await;
        *total += 1;
    }

    /// Mark a failed attempt (called when order fails)
    pub async fn mark_failed_attempt(&self, condition_id: &str, period_timestamp: u64) {
        let attempt_key = format!("{}_{}", condition_id, period_timestamp);
        let mut failed = self.failed_attempts.lock().await;
        let entry = failed.entry(attempt_key).or_insert((0, std::time::Instant::now()));
        entry.0 += 1;
        entry.1 = std::time::Instant::now();
    }
    
    /// Reset entries when new period starts
    pub async fn reset_period(&self) {
        let mut entries = self.current_period_entries.lock().await;
        entries.clear();
        drop(entries);
        
        let mut last_prices = self.last_entry_prices.lock().await;
        last_prices.clear();
        drop(last_prices);
        
        // Reset total trade counter
        let mut total = self.total_entries_this_period.lock().await;
        *total = 0;
        drop(total);
        
        // Clear failed attempts
        let mut failed = self.failed_attempts.lock().await;
        failed.clear();
    }
}

// Helper function for Decimal to f64 conversion
fn decimal_to_f64(d: Decimal) -> f64 {
    d.to_string().parse().unwrap_or(0.0)
}
