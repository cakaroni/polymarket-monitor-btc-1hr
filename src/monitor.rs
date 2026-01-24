use crate::api::PolymarketApi;
use crate::models::*;
use anyhow::Result;
use log::warn;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use chrono::{TimeZone, Utc, Datelike, Timelike};
use chrono_tz::America::New_York;

pub struct MarketMonitor {
    api: Arc<PolymarketApi>,
    btc_market_1h: Arc<tokio::sync::Mutex<crate::models::Market>>,
    check_interval: Duration,
    // Cached token IDs from getMarket() - refreshed once per period
    btc_1h_up_token_id: Arc<tokio::sync::Mutex<Option<String>>>,
    btc_1h_down_token_id: Arc<tokio::sync::Mutex<Option<String>>>,
    last_market_refresh: Arc<tokio::sync::Mutex<Option<std::time::Instant>>>,
    current_period_timestamp: Arc<tokio::sync::Mutex<u64>>, // Track current period
}

#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub btc_market_1h: MarketData,
    pub timestamp: std::time::Instant,
    pub btc_1h_time_remaining: u64,
    pub btc_1h_period_timestamp: u64,
}

impl MarketMonitor {
    pub fn new(
        api: Arc<PolymarketApi>,
        btc_market_1h: crate::models::Market,
        check_interval_ms: u64,
    ) -> Self {
        // Calculate current period timestamp (1 hour = 3600 seconds)
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let current_period = (current_time / 3600) * 3600; // Round to nearest hour
        
        Self {
            api,
            btc_market_1h: Arc::new(tokio::sync::Mutex::new(btc_market_1h)),
            check_interval: Duration::from_millis(check_interval_ms),
            btc_1h_up_token_id: Arc::new(tokio::sync::Mutex::new(None)),
            btc_1h_down_token_id: Arc::new(tokio::sync::Mutex::new(None)),
            last_market_refresh: Arc::new(tokio::sync::Mutex::new(None)),
            current_period_timestamp: Arc::new(tokio::sync::Mutex::new(current_period)),
        }
    }

    /// Update market when a new period starts
    pub async fn update_market(&self, btc_market_1h: crate::models::Market) -> Result<()> {
        eprintln!("🔄 Updating BTC 1h market...");
        eprintln!("New BTC 1h Market: {} ({})", btc_market_1h.slug, btc_market_1h.condition_id);
        
        *self.btc_market_1h.lock().await = btc_market_1h;
        
        // Reset token IDs - will be refreshed on next fetch
        *self.btc_1h_up_token_id.lock().await = None;
        *self.btc_1h_down_token_id.lock().await = None;
        *self.last_market_refresh.lock().await = None;
        
        // Update current period timestamp
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let new_period = (current_time / 3600) * 3600; // Round to nearest hour
        *self.current_period_timestamp.lock().await = new_period;
        
        Ok(())
    }

    /// Refresh market data once per period to get token IDs
    async fn refresh_market_tokens(&self) -> Result<()> {
        // Check if we need to refresh (every hour = 3600 seconds)
        let should_refresh = {
            let last_refresh = self.last_market_refresh.lock().await;
            last_refresh
                .map(|last| last.elapsed().as_secs() >= 3600)
                .unwrap_or(true)
        };

        if !should_refresh {
            return Ok(());
        }

        let btc_1h_id = self.btc_market_1h.lock().await.condition_id.clone();

        // Get BTC 1h market details
        if let Ok(btc_1h_details) = self.api.get_market(&btc_1h_id).await {
            for token in &btc_1h_details.tokens {
                let outcome_upper = token.outcome.to_uppercase();
                if outcome_upper.contains("UP") || outcome_upper == "1" {
                    *self.btc_1h_up_token_id.lock().await = Some(token.token_id.clone());
                    eprintln!("BTC 1h Up token_id: {}", token.token_id);
                } else if outcome_upper.contains("DOWN") || outcome_upper == "0" {
                    *self.btc_1h_down_token_id.lock().await = Some(token.token_id.clone());
                    eprintln!("BTC 1h Down token_id: {}", token.token_id);
                }
            }
        }

        *self.last_market_refresh.lock().await = Some(std::time::Instant::now());
        Ok(())
    }

    /// Fetch current market data for BTC 1h market
    /// Uses get_price() endpoint continuously for real-time prices
    pub async fn fetch_market_data(&self) -> Result<MarketSnapshot> {
        // Refresh token IDs if needed
        self.refresh_market_tokens().await?;

        // Get market slug to extract timestamp
        let btc_1h_guard = self.btc_market_1h.lock().await;
        let btc_1h_slug = btc_1h_guard.slug.clone();
        let btc_1h_id = btc_1h_guard.condition_id.clone();
        drop(btc_1h_guard);
        
        // Extract period start timestamp from 1-hour market slug
        let btc_1h_timestamp = Self::extract_timestamp_from_1h_slug(&btc_1h_slug);
        
        // Get current timestamp
        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        // 1-hour markets: calculate from period start timestamp + 3600 seconds (1 hour)
        let btc_1h_period_end = btc_1h_timestamp + 3600; // 1 hour = 3600 seconds
        
        let btc_1h_remaining = if btc_1h_period_end > current_timestamp {
            btc_1h_period_end - current_timestamp
        } else { 0 };
        
        // Fetch prices for BTC 1h tokens
        let btc_1h_up_token_id = self.btc_1h_up_token_id.lock().await.clone();
        let btc_1h_down_token_id = self.btc_1h_down_token_id.lock().await.clone();
        
        let (btc_1h_up_price, btc_1h_down_price) = if btc_1h_remaining > 0 {
            tokio::join!(
                self.fetch_token_price(&btc_1h_up_token_id, "BTC 1h", "Up"),
                self.fetch_token_price(&btc_1h_down_token_id, "BTC 1h", "Down"),
            )
        } else {
            (None, None)
        };
        
        // Format remaining time as "Xm Ys"
        let format_remaining_time = |secs: u64| -> String {
            if secs == 0 {
                "0s".to_string()
            } else {
                let minutes = secs / 60;
                let seconds = secs % 60;
                if minutes > 0 {
                    format!("{}m {}s", minutes, seconds)
                } else {
                    format!("{}s", seconds)
                }
            }
        };
        
        let btc_1h_remaining_str = format_remaining_time(btc_1h_remaining);

        // Helper function to format price with both BID and ASK
        let format_price_with_both = |p: &TokenPrice| -> String {
            let bid = p.bid.unwrap_or(rust_decimal::Decimal::ZERO);
            let ask = p.ask.unwrap_or(rust_decimal::Decimal::ZERO);
            let bid_f64: f64 = bid.to_string().parse().unwrap_or(0.0);
            let ask_f64: f64 = ask.to_string().parse().unwrap_or(0.0);
            format!("BID:${:.2} ASK:${:.2}", bid_f64, ask_f64)
        };

        // Log prices for BTC 1h market
        let btc_1h_up_str = btc_1h_up_price.as_ref()
            .map(format_price_with_both)
            .unwrap_or_else(|| "N/A".to_string());
        let btc_1h_down_str = btc_1h_down_price.as_ref()
            .map(format_price_with_both)
            .unwrap_or_else(|| "N/A".to_string());

        // Print BTC 1h market stats
        let message = format!(
            "BTC 1h Up Token {} Down Token {} remaining time:{}\n",
            btc_1h_up_str, btc_1h_down_str, btc_1h_remaining_str
        );
        crate::log_to_history(&message);
        
        crate::log_to_history("\n"); // Empty line for readability

        let btc_1h_market_data = MarketData {
            condition_id: btc_1h_id,
            market_name: "BTC 1h".to_string(),
            up_token: btc_1h_up_price,
            down_token: btc_1h_down_price,
        };

        Ok(MarketSnapshot {
            btc_market_1h: btc_1h_market_data,
            timestamp: std::time::Instant::now(),
            btc_1h_time_remaining: btc_1h_remaining,
            btc_1h_period_timestamp: btc_1h_timestamp,
        })
    }

    async fn fetch_token_price(
        &self,
        token_id: &Option<String>,
        market_name: &str,
        outcome: &str,
    ) -> Option<TokenPrice> {
        let token_id = token_id.as_ref()?;

        // Get BUY price (BID price - what we pay to buy, higher)
        // get_price(token_id, "BUY") returns the BID price (what we pay to buy)
        let buy_price = match self.api.get_price(token_id, "BUY").await {
            Ok(price) => Some(price),
            Err(e) => {
                warn!("Failed to fetch {} {} BUY price: {}", market_name, outcome, e);
                None
            }
        };

        // Get SELL price (ASK price - what we receive when selling, lower)
        // get_price(token_id, "SELL") returns the ASK price (what we receive when selling)
        let sell_price = match self.api.get_price(token_id, "SELL").await {
            Ok(price) => Some(price),
            Err(e) => {
                warn!("Failed to fetch {} {} SELL price: {}", market_name, outcome, e);
                None
            }
        };

        if buy_price.is_some() || sell_price.is_some() {
            Some(TokenPrice {
                token_id: token_id.clone(),
                bid: buy_price,  // BID = BUY price (what we pay to buy, higher)
                ask: sell_price, // ASK = SELL price (what we receive when selling, lower)
            })
        } else {
            None
        }
    }

    /// Extract timestamp from market slug (e.g., "eth-updown-15m-1767796200" -> 1767796200)
    pub fn extract_timestamp_from_slug(slug: &str) -> u64 {
        // Slug format: {asset}-updown-{timeframe}-{timestamp}
        // Try to extract the timestamp (last number after the last dash)
        if let Some(last_dash) = slug.rfind('-') {
            if let Ok(timestamp) = slug[last_dash + 1..].parse::<u64>() {
                return timestamp;
            }
        }
        // Fallback: return 0 if we can't parse
        0
    }
    
    /// Extract duration in seconds from market slug (e.g., "eth-updown-15m-..." -> 900, "eth-updown-1h-..." -> 3600)
    pub fn extract_duration_from_slug(slug: &str) -> u64 {
        // Slug format: {asset}-updown-{timeframe}-{timestamp}
        // Look for timeframe pattern: -15m- or -1h-
        if slug.contains("-15m-") {
            900 // 15 minutes
        } else if slug.contains("-1h-") {
            3600 // 1 hour
        } else {
            900 // Default to 15 minutes if unknown
        }
    }

    /// Extract timestamp from 1-hour market slug (e.g., "ethereum-up-or-down-january-19-10am-et")
    /// Converts the date/time string to a Unix timestamp
    pub fn extract_timestamp_from_1h_slug(slug: &str) -> u64 {
        
        // Parse date/time from slug format: ethereum-up-or-down-january-19-10am-et
        // Extract: january-19-10am
        let month_names = ["january", "february", "march", "april", "may", "june",
                          "july", "august", "september", "october", "november", "december"];
        
        let slug_lower = slug.to_lowercase();
        for (idx, month) in month_names.iter().enumerate() {
            if slug_lower.contains(month) {
                // Try to extract day and hour
                let parts: Vec<&str> = slug_lower.split('-').collect();
                for (i, part) in parts.iter().enumerate() {
                    if part == month {
                        if i + 2 < parts.len() {
                            // Next part should be day, then hour+am/pm
                            if let Ok(day) = parts[i + 1].parse::<u32>() {
                                let hour_part = parts[i + 2];
                                if let Some(hour_str) = hour_part.strip_suffix("am").or_else(|| hour_part.strip_suffix("pm")) {
                                    if let Ok(mut hour) = hour_str.parse::<u32>() {
                                        let is_pm = hour_part.ends_with("pm");
                                        if is_pm && hour != 12 {
                                            hour += 12;
                                        } else if !is_pm && hour == 12 {
                                            hour = 0;
                                        }
                                        
                                        // Get current year (assume current year for the market)
                                        let now = Utc::now();
                                        let year = now.year();
                                        
                                        // Create datetime in ET timezone (slug uses ET time)
                                        // Then convert to UTC for timestamp calculation
                                        if let Some(dt_et) = New_York.with_ymd_and_hms(year, (idx + 1) as u32, day, hour, 0, 0).single() {
                                            let dt_utc = dt_et.with_timezone(&Utc);
                                            return dt_utc.timestamp() as u64;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        0 // Fallback
    }


    /// Start monitoring BTC 1h market continuously
    pub async fn start_monitoring(&self) {
        eprintln!("Starting BTC 1h market monitoring...");
        
        let mut last_market_condition_id: Option<String> = None;
        let mut consecutive_closed_count = 0;
        let mut last_discovery_attempt: Option<std::time::Instant> = None;
        
        loop {
            match self.fetch_market_data().await {
                Ok(snapshot) => {
                    // Get current market condition ID
                    let current_condition_id = snapshot.btc_market_1h.condition_id.clone();
                    
                    // Check if market has closed (remaining time = 0)
                    if snapshot.btc_1h_time_remaining == 0 {
                        consecutive_closed_count += 1;
                        
                        // If market has been closed for a few checks, try to discover new market
                        // But only if we haven't tried recently (avoid spam)
                        let should_try_discovery = if let Some(last_attempt) = last_discovery_attempt {
                            last_attempt.elapsed().as_secs() >= 10 // Wait at least 10 seconds between attempts
                        } else {
                            true
                        };
                        
                        if consecutive_closed_count >= 3 && should_try_discovery {
                            eprintln!("🔄 Market closed, attempting to discover new BTC 1h market...");
                            last_discovery_attempt = Some(std::time::Instant::now());
                            
                            // Try to discover new market
                            match self.discover_new_market().await {
                                Ok(new_market) => {
                                    // Only update if it's a different market
                                    if new_market.condition_id != current_condition_id {
                                        eprintln!("✅ Discovered new BTC 1h market: {} ({})", 
                                            new_market.slug, new_market.condition_id);
                                        
                                        if let Err(e) = self.update_market(new_market).await {
                                            warn!("Failed to update market: {}", e);
                                        } else {
                                            consecutive_closed_count = 0;
                                            last_market_condition_id = Some(current_condition_id);
                                        }
                                    } else {
                                        eprintln!("   (Same market, waiting for new market to become available...)");
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to discover new BTC 1h market: {}", e);
                                }
                            }
                        }
                    } else {
                        // Market is still active, reset counter and update last known market
                        if consecutive_closed_count > 0 {
                            consecutive_closed_count = 0;
                        }
                        last_market_condition_id = Some(current_condition_id);
                    }
                }
                Err(e) => {
                    warn!("Error fetching market data: {}", e);
                    consecutive_closed_count += 1;
                }
            }
            
            sleep(self.check_interval).await;
        }
    }
    
    /// Discover a new BTC 1h market
    async fn discover_new_market(&self) -> Result<crate::models::Market> {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        // 1-hour markets use format: bitcoin-up-or-down-{date-time}
        // Use slug-based approach with ET timezone
        use chrono_tz::America::New_York;
        
        // Helper function to convert UTC timestamp to ET date/time format: january-19-9am-et
        let timestamp_to_slug_date_et = |utc_ts: u64| -> String {
            // Convert UTC timestamp to ET (Eastern Time)
            let dt_utc = chrono::Utc.timestamp_opt(utc_ts as i64, 0).unwrap();
            let dt_et = dt_utc.with_timezone(&New_York); // Convert to ET
            
            let month_names = ["january", "february", "march", "april", "may", "june",
                              "july", "august", "september", "october", "november", "december"];
            let month = month_names[(dt_et.month() as usize) - 1];
            let day = dt_et.day();
            let hour = dt_et.hour();
            let hour_12 = if hour == 0 { 12 } else if hour > 12 { hour - 12 } else { hour };
            let am_pm = if hour < 12 { "am" } else { "pm" };
            format!("{}-{}-{}{}-et", month, day, hour_12, am_pm)
        };
        
        // Try current hour in ET, and a few hours around it
        // Since future markets are also "active", try current hour and next few hours
        for hour_offset in 0..=5 {
            // hour_offset 0: current hour in ET
            // hour_offset 1-2: previous hours (in case we're at the start of an hour)
            // hour_offset 3-5: future hours (future markets are also "active")
            let try_time_utc = if hour_offset <= 2 {
                current_time - (hour_offset * 3600) // Go back
            } else {
                current_time + ((hour_offset - 2) * 3600) // Go forward
            };
            let date_time_str = timestamp_to_slug_date_et(try_time_utc);
            let slug = format!("bitcoin-up-or-down-{}", date_time_str);
            
            eprintln!("Trying BTC 1h market by slug: {}", slug);
            match self.api.get_market_by_slug(&slug).await {
                Ok(market) => {
                    eprintln!("Found market {}: active={}, closed={}, condition_id={}", market.slug, market.active, market.closed, market.condition_id);
                    
                    // For 1-hour markets, if active=true and closed=false, accept it
                    if market.active && !market.closed {
                        eprintln!("Found BTC 1h market by slug: {} | Condition ID: {}", market.slug, market.condition_id);
                        return Ok(market);
                    } else {
                        eprintln!("Market {} is inactive or closed (active: {}, closed: {}), skipping", market.slug, market.active, market.closed);
                    }
                }
                Err(e) => {
                    // Silently continue - market doesn't exist with this slug
                    // Only log if it's not a 404/not found error
                    if !e.to_string().contains("404") && !e.to_string().contains("not found") && !e.to_string().contains("Failed to fetch market") {
                        eprintln!("Error fetching market by slug {}: {}", slug, e);
                    }
                }
            }
        }
        
        anyhow::bail!("Could not find active BTC 1h up/down market")
    }
}

