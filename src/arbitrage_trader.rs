use crate::api::PolymarketApi;
use crate::models::*;
use crate::arbitrage_detector::ArbitrageOpportunity;
use crate::config::TradingConfig;
use anyhow::Result;
use log::warn;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;

/// Trader for arbitrage strategy (like target address)
/// Handles multiple entries, progressive selling based on price movements
pub struct ArbitrageTrader {
    api: Arc<PolymarketApi>,
    config: TradingConfig,
    simulation_mode: bool,
    total_profit: Arc<Mutex<f64>>, // Cumulative profit across all periods (never resets)
    period_profit: Arc<Mutex<f64>>, // Profit for current period (resets each period)
    trades_executed: Arc<Mutex<u64>>,
    pending_trades: Arc<Mutex<HashMap<String, ArbitrageTrade>>>, // Key: token_id+period_timestamp
    position_sizes: Arc<Mutex<HashMap<String, f64>>>, // Track total position per token
    pending_sell_attempts: Arc<Mutex<HashMap<String, std::time::Instant>>>, // Track pending sell attempts: token_id -> last attempt time
}

impl ArbitrageTrader {
    pub fn new(api: Arc<PolymarketApi>, config: TradingConfig, simulation_mode: bool) -> Self {
        Self {
            api,
            config,
            simulation_mode,
            total_profit: Arc::new(Mutex::new(0.0)), // Cumulative (never resets)
            period_profit: Arc::new(Mutex::new(0.0)), // Current period (resets each period)
            trades_executed: Arc::new(Mutex::new(0)),
            pending_trades: Arc::new(Mutex::new(HashMap::new())),
            position_sizes: Arc::new(Mutex::new(HashMap::new())),
            pending_sell_attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Calculate variable position size based on price gap
    /// Larger gaps = larger positions (more confidence)
    /// Scales from min_size at min_gap_threshold to max_size at larger gaps
    #[allow(dead_code)]
    fn calculate_position_size(&self, price_gap: f64) -> f64 {
        // Old logic - not used anymore (replaced by target_trader)
        let min_size = 2.0;
        let max_size = 10.0;
        let min_gap_threshold = 0.05;
        
        // Scale position size based on price gap
        // Strategy: Use wider range for scaling to ensure variability
        // min_gap (0.05) → min_size ($2)
        // max_gap (0.25 = 5x min) → max_size ($10)
        // This ensures we see variation across typical gap ranges
        let min_gap = min_gap_threshold; // Minimum gap threshold (e.g., 0.05 = 5¢)
        let max_gap = min_gap_threshold * 5.0; // 5x min gap for max size (e.g., 0.25 = 25¢)
        
        let position_size = if price_gap <= min_gap {
            // At minimum gap, use minimum size
            min_size
        } else if price_gap >= max_gap {
            // At maximum gap or above, use maximum size
            max_size
        } else {
            // Linear interpolation between min and max
            let ratio = (price_gap - min_gap) / (max_gap - min_gap);
            let calculated = min_size + (max_size - min_size) * ratio;
            calculated
        };
        
        // Log the calculation for debugging
        crate::log_println!("   📊 Position Sizing: Gap=${:.4} | Range=${:.4}-${:.4} | Calculated=${:.2}", 
              price_gap, min_gap, max_gap, position_size);
        
        position_size
    }

    /// Execute buy for market-neutral strategy: Buy BOTH Up and Down tokens simultaneously
    /// Uses equal shares for both tokens (not equal dollar amounts)
    pub async fn execute_buy(&self, opportunity: &ArbitrageOpportunity) -> Result<()> {
        // Get ASK prices for both tokens (what we'll pay to buy)
        let (up_ask_result, down_ask_result) = tokio::join!(
            self.api.get_price(&opportunity.up_token_id, "SELL"),
            self.api.get_price(&opportunity.down_token_id, "SELL")
        );
        
        let up_ask = match up_ask_result {
            Ok(p) => f64::try_from(p).unwrap_or(0.0),
            Err(e) => {
                warn!("⚠️  Could not fetch Up token ASK price: {}", e);
                return Err(e);
            }
        };
        
        let down_ask = match down_ask_result {
            Ok(p) => f64::try_from(p).unwrap_or(0.0),
            Err(e) => {
                warn!("⚠️  Could not fetch Down token ASK price: {}", e);
                return Err(e);
            }
        };
        
        // Safety checks
        if up_ask <= 0.001 || down_ask <= 0.001 {
            warn!("⚠️  Invalid prices: Up=${:.4}, Down=${:.4}", up_ask, down_ask);
            return Err(anyhow::anyhow!("Invalid prices"));
        }
        
        // Check price ranges (both must be valid)
        // Old logic - using defaults since config fields removed
        let min_entry_price = 0.1;
        let max_entry_price = 0.85;
        
        if up_ask < min_entry_price || down_ask < min_entry_price {
            warn!("⚠️  Prices below minimum: Up=${:.4}, Down=${:.4}", up_ask, down_ask);
            return Err(anyhow::anyhow!("Prices below minimum"));
        }
        
        if max_entry_price > 0.0 && (up_ask > max_entry_price || down_ask > max_entry_price) {
            warn!("⚠️  Prices above maximum: Up=${:.4}, Down=${:.4}", up_ask, down_ask);
            return Err(anyhow::anyhow!("Prices above maximum"));
        }
        
        // Calculate position size: use equal shares for both tokens
        // Total investment = shares * (up_ask + down_ask)
        // So: shares = total_investment / (up_ask + down_ask)
        let total_investment = self.calculate_position_size(opportunity.price_gap);
        let mut shares_per_token = total_investment / (up_ask + down_ask);
        
        // Polymarket minimum requirements:
        // 1. Minimum 5 shares per order
        // 2. Minimum $1 USD per order (with 10% safety margin to account for price fluctuations)
        // Calculate minimum required shares for each token independently
        // Use $1.10 as minimum to account for price drops between calculation and order placement
        let min_investment_with_margin = 1.10; // $1.10 to account for ~10% price fluctuation
        let min_shares_for_up = (min_investment_with_margin / up_ask).max(5.0);
        let min_shares_for_down = (min_investment_with_margin / down_ask).max(5.0);
        
        // For market-neutral strategy, we want equal shares, so use the maximum of both
        // This ensures both tokens meet minimums even if prices fluctuate
        let min_shares_needed = min_shares_for_up.max(min_shares_for_down);
        
        // Adjust shares to meet minimum requirements
        if shares_per_token < min_shares_needed {
            shares_per_token = min_shares_needed;
            crate::log_println!("   ⚠️  Adjusted shares to meet minimum: {:.2} → {:.2} (min: {:.2})", 
                  total_investment / (up_ask + down_ask), shares_per_token, min_shares_needed);
        }
        
        let up_units = shares_per_token;
        let down_units = shares_per_token;
        
        let up_investment = up_units * up_ask;
        let down_investment = down_units * down_ask;
        
        // Final validation: ensure each order meets minimums with safety margin
        if up_units < 5.0 {
            return Err(anyhow::anyhow!("Up token shares ({:.2}) below minimum (5)", up_units));
        }
        if down_units < 5.0 {
            return Err(anyhow::anyhow!("Down token shares ({:.2}) below minimum (5)", down_units));
        }
        // Check with safety margin: each investment should be at least $1.05
        if up_investment < min_investment_with_margin {
            return Err(anyhow::anyhow!("Up token investment (${:.2}) below minimum (${:.2})", up_investment, min_investment_with_margin));
        }
        if down_investment < min_investment_with_margin {
            return Err(anyhow::anyhow!("Down token investment (${:.2}) below minimum (${:.2})", down_investment, min_investment_with_margin));
        }
        
        let adjusted_total_investment = up_investment + down_investment;
        crate::log_println!("💰 MARKET-NEUTRAL BUY: {} & {} (Equal Shares)", opportunity.up_token_name, opportunity.down_token_name);
        crate::log_println!("   Up Token: ASK=${:.4} | Shares={:.2} | Investment=${:.2}", up_ask, up_units, up_investment);
        crate::log_println!("   Down Token: ASK=${:.4} | Shares={:.2} | Investment=${:.2}", down_ask, down_units, down_investment);
        crate::log_println!("   Total Investment: ${:.2} | Price Gap: ${:.4}", adjusted_total_investment, opportunity.price_gap);
        
        if self.simulation_mode {
            crate::log_println!("   ✅ SIMULATION: Both orders executed");
        } else {
            // Place real market orders for both tokens
            let up_units_rounded = (up_units * 10000.0).round() / 10000.0;
            let down_units_rounded = (down_units * 10000.0).round() / 10000.0;
            
            let (up_result, down_result) = tokio::join!(
                self.api.place_market_order(&opportunity.up_token_id, up_units_rounded, "BUY", None),
                self.api.place_market_order(&opportunity.down_token_id, down_units_rounded, "BUY", None)
            );
            
            match up_result {
                Ok(_) => crate::log_println!("   ✅ REAL: Up token order placed"),
                Err(e) => {
                    warn!("❌ Failed to place Up token order: {}", e);
                    return Err(e);
                }
            }
            
            match down_result {
                Ok(_) => crate::log_println!("   ✅ REAL: Down token order placed"),
                Err(e) => {
                    warn!("❌ Failed to place Down token order: {}", e);
                    return Err(e);
                }
            }
        }
        
        // Create trade record for both tokens
        // Use condition_id + period_timestamp + timestamp to allow multiple trades in same period
        let unique_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let trade_key = format!("{}_{}_{}", opportunity.condition_id, opportunity.period_timestamp, unique_timestamp);
        let trade = ArbitrageTrade {
            up_token_id: opportunity.up_token_id.clone(),
            up_token_name: opportunity.up_token_name.clone(),
            up_investment_amount: up_investment,
            up_total_units: up_units,
            up_remaining_units: up_units,
            up_purchase_price: up_ask,
            up_price_history: Vec::new(),
            
            down_token_id: opportunity.down_token_id.clone(),
            down_token_name: opportunity.down_token_name.clone(),
            down_investment_amount: down_investment,
            down_total_units: down_units,
            down_remaining_units: down_units,
            down_purchase_price: down_ask,
            down_price_history: Vec::new(),
            
            condition_id: opportunity.condition_id.clone(),
            price_gap: opportunity.price_gap,
            min_profit_target: opportunity.min_profit_target,
            timestamp: std::time::Instant::now(),
            market_timestamp: opportunity.period_timestamp,
            market_duration: opportunity.market_duration,
        };
        
        // Store trade (keyed by condition_id + period + unique timestamp to allow multiple trades)
        let mut trades = self.pending_trades.lock().await;
        // Check if a trade with same key already exists - if so, append unique ID
        if trades.contains_key(&trade_key) {
            let mut counter = 1;
            let mut new_key = format!("{}_{}", trade_key, counter);
            while trades.contains_key(&new_key) {
                counter += 1;
                new_key = format!("{}_{}", trade_key, counter);
            }
            trades.insert(new_key, trade);
        } else {
            trades.insert(trade_key.clone(), trade);
        }
        
        // Track position sizes for both tokens
        let mut positions = self.position_sizes.lock().await;
        *positions.entry(opportunity.up_token_id.clone()).or_insert(0.0) += up_units;
        *positions.entry(opportunity.down_token_id.clone()).or_insert(0.0) += down_units;
        
        // Update stats (buying both tokens = 1 trade entry)
        let mut count = self.trades_executed.lock().await;
        *count += 1;
        
        crate::log_println!("   📊 Total trades this period: {}", *count);
        
        Ok(())
    }

    /// Check pending trades and sell when profit targets are hit
    /// Monitors both Up and Down tokens independently
    /// For market-neutral strategy: Focus on holding, only sell when clear profit or market closing
    pub async fn check_pending_trades(&self) -> Result<()> {
        let mut trades = self.pending_trades.lock().await;
        let mut to_remove = Vec::new();
        
        // Calculate current time to check if period is ending
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        const EARLY_EXIT_SECONDS: u64 = 30; // Exit 30 seconds before period ends (reduced from 60)
        
        // More conservative selling for market-neutral strategy
        // Higher stop-loss threshold (40% instead of 20%) - give positions time to recover
        // Old logic - using defaults since config fields removed
        let stop_loss_percent = 0.40;
        // More drops required before exit (7 instead of 3) - be more patient
        let price_drop_count = 7;
        
        for (key, trade) in trades.iter_mut() {
            // Check if period is ending soon
            let market_end_time = trade.market_timestamp + trade.market_duration;
            let time_remaining = market_end_time.saturating_sub(current_time);
            let should_exit_early = time_remaining <= EARLY_EXIT_SECONDS && time_remaining > 0;
            
            // Check Up token
            if trade.up_remaining_units > 0.001 {
                let up_price_result = self.api.get_price(&trade.up_token_id, "BUY").await;
                if let Ok(up_bid_decimal) = up_price_result {
                    let up_current_bid = f64::try_from(up_bid_decimal).unwrap_or(0.0);
                    
                    if up_current_bid > 0.001 {
                        // Track price history
                        let history_len_before = trade.up_price_history.len();
                        trade.up_price_history.push((std::time::Instant::now(), up_current_bid));
                        if trade.up_price_history.len() > 20 { // Keep more history for better analysis
                            trade.up_price_history.remove(0);
                        }
                        
                        // Calculate current P&L for this token
                        // P&L = (Current Price - Purchase Price) * Remaining Units
                        let up_profit = (up_current_bid - trade.up_purchase_price) * trade.up_remaining_units;
                        let up_profit_percent = ((up_current_bid - trade.up_purchase_price) / trade.up_purchase_price) * 100.0;
                        
                        // Log position status periodically (every 10 price updates)
                        // Only log when length transitions to a multiple of 10 (e.g., 9->10, 19->20)
                        // This prevents duplicate logging when length stays at 10, 20, etc.
                        let current_len = trade.up_price_history.len();
                        let should_log = current_len > 0 && 
                                        (current_len % 10 == 0) && 
                                        (history_len_before % 10 != 0 || history_len_before == 0);
                        if should_log {
                            crate::log_println!("📊 HOLDING: {} | Purchase: ${:.4} | Current: ${:.4} | P&L: ${:.2} ({:.1}%) | Units: {:.2} | Investment: ${:.2}", 
                                  trade.up_token_name, trade.up_purchase_price, up_current_bid, up_profit, up_profit_percent, trade.up_remaining_units, trade.up_investment_amount);
                        }
                        
                        // Only sell if:
                        // 1. Clear profit target reached (sell partial)
                        // 2. Extreme stop-loss (40%+ loss)
                        // 3. Market closing soon
                        // DO NOT sell on price direction drops for market-neutral strategy
                        
                        // Check profit target - sell partial when significant profit
                        let up_profit_target = trade.up_purchase_price + trade.min_profit_target;
                        if up_current_bid >= up_profit_target && up_profit > 0.0 {
                            // Only sell 25% at profit target (less aggressive)
                            let sell_percentage = 0.25;
                            let units_to_sell = trade.up_remaining_units * sell_percentage;
                            
                            if units_to_sell >= 0.01 {
                                crate::log_println!("✅ PROFIT TARGET: {} | Selling 25% at ${:.4} | Profit: ${:.2}", 
                                      trade.up_token_name, up_current_bid, up_profit * sell_percentage);
                                if let Ok(_) = self.execute_sell_token(&trade.up_token_id, &trade.up_token_name, units_to_sell, up_current_bid, trade.up_purchase_price).await {
                                    trade.up_remaining_units -= units_to_sell;
                                }
                            }
                        }
                        // Extreme stop-loss only (40%+ loss)
                        else if up_current_bid <= trade.up_purchase_price * (1.0 - stop_loss_percent) {
                            // Check if we've recently tried to sell this token (prevent spam retries)
                            let mut sell_attempts = self.pending_sell_attempts.lock().await;
                            let last_attempt = sell_attempts.get(&trade.up_token_id);
                            let should_retry = match last_attempt {
                                Some(last_time) => {
                                    // Only retry if last attempt was more than 30 seconds ago
                                    last_time.elapsed().as_secs() >= 30
                                }
                                None => true, // Never tried, allow attempt
                            };
                            
                            if should_retry {
                                let loss_percent = ((trade.up_purchase_price - up_current_bid) / trade.up_purchase_price) * 100.0;
                                crate::log_println!("🛑 EXTREME STOP-LOSS: {} | Purchase: ${:.4} | Current: ${:.4} | Loss: {:.1}%", 
                                      trade.up_token_name, trade.up_purchase_price, up_current_bid, loss_percent);
                                
                                // Mark that we're attempting to sell
                                sell_attempts.insert(trade.up_token_id.clone(), std::time::Instant::now());
                                drop(sell_attempts);
                                
                                match self.execute_sell_token(&trade.up_token_id, &trade.up_token_name, trade.up_remaining_units, up_current_bid, trade.up_purchase_price).await {
                                    Ok(_) => {
                                        trade.up_remaining_units = 0.0;
                                        // Clear the attempt tracking on success
                                        let mut attempts = self.pending_sell_attempts.lock().await;
                                        attempts.remove(&trade.up_token_id);
                                    }
                                    Err(e) => {
                                        // Keep the attempt time so we don't retry too soon
                                        // Will retry after 30 seconds
                                        warn!("   ⏳ Sell failed, will retry in 30s: {}", e);
                                    }
                                }
                            } else {
                                // In cooldown - don't spam logs
                                // The stop-loss condition is still true, but we're waiting before retrying
                            }
                        }
                        // Early exit only if market closing very soon
                        else if should_exit_early {
                            let profit_if_sold = (up_current_bid - trade.up_purchase_price) * trade.up_remaining_units;
                            crate::log_println!("⚠️  Market closing ({}s) - Selling all {} | P&L: ${:.2}", 
                                  time_remaining, trade.up_token_name, profit_if_sold);
                            if let Ok(_) = self.execute_sell_token(&trade.up_token_id, &trade.up_token_name, trade.up_remaining_units, up_current_bid, trade.up_purchase_price).await {
                                trade.up_remaining_units = 0.0;
                            }
                        }
                        // Otherwise: HOLD - don't sell on price drops for market-neutral strategy
                    }
                }
            }
            
            // Check Down token
            if trade.down_remaining_units > 0.001 {
                let down_price_result = self.api.get_price(&trade.down_token_id, "BUY").await;
                if let Ok(down_bid_decimal) = down_price_result {
                    let down_current_bid = f64::try_from(down_bid_decimal).unwrap_or(0.0);
                    
                    if down_current_bid > 0.001 {
                        // Track price history
                        let history_len_before = trade.down_price_history.len();
                        trade.down_price_history.push((std::time::Instant::now(), down_current_bid));
                        if trade.down_price_history.len() > 20 { // Keep more history for better analysis
                            trade.down_price_history.remove(0);
                        }
                        
                        // Calculate current P&L for this token
                        // P&L = (Current Price - Purchase Price) * Remaining Units
                        let down_profit = (down_current_bid - trade.down_purchase_price) * trade.down_remaining_units;
                        let down_profit_percent = ((down_current_bid - trade.down_purchase_price) / trade.down_purchase_price) * 100.0;
                        
                        // Log position status periodically (every 10 price updates)
                        // Only log when length transitions to a multiple of 10 (e.g., 9->10, 19->20)
                        // This prevents duplicate logging when length stays at 10, 20, etc.
                        let current_len = trade.down_price_history.len();
                        let should_log = current_len > 0 && 
                                        (current_len % 10 == 0) && 
                                        (history_len_before % 10 != 0 || history_len_before == 0);
                        if should_log {
                            crate::log_println!("📊 HOLDING: {} | Purchase: ${:.4} | Current: ${:.4} | P&L: ${:.2} ({:.1}%) | Units: {:.2} | Investment: ${:.2}", 
                                  trade.down_token_name, trade.down_purchase_price, down_current_bid, down_profit, down_profit_percent, trade.down_remaining_units, trade.down_investment_amount);
                        }
                        
                        // Only sell if:
                        // 1. Clear profit target reached (sell partial)
                        // 2. Extreme stop-loss (40%+ loss)
                        // 3. Market closing soon
                        // DO NOT sell on price direction drops for market-neutral strategy
                        
                        // Check profit target - sell partial when significant profit
                        let down_profit_target = trade.down_purchase_price + trade.min_profit_target;
                        if down_current_bid >= down_profit_target && down_profit > 0.0 {
                            // Only sell 25% at profit target (less aggressive)
                            let sell_percentage = 0.25;
                            let units_to_sell = trade.down_remaining_units * sell_percentage;
                            
                            if units_to_sell >= 0.01 {
                                crate::log_println!("✅ PROFIT TARGET: {} | Selling 25% at ${:.4} | Profit: ${:.2}", 
                                      trade.down_token_name, down_current_bid, down_profit * sell_percentage);
                                if let Ok(_) = self.execute_sell_token(&trade.down_token_id, &trade.down_token_name, units_to_sell, down_current_bid, trade.down_purchase_price).await {
                                    trade.down_remaining_units -= units_to_sell;
                                }
                            }
                        }
                        // Extreme stop-loss only (40%+ loss)
                        else if down_current_bid <= trade.down_purchase_price * (1.0 - stop_loss_percent) {
                            // Check if we've recently tried to sell this token (prevent spam retries)
                            let mut sell_attempts = self.pending_sell_attempts.lock().await;
                            let last_attempt = sell_attempts.get(&trade.down_token_id);
                            let should_retry = match last_attempt {
                                Some(last_time) => {
                                    // Only retry if last attempt was more than 30 seconds ago
                                    last_time.elapsed().as_secs() >= 30
                                }
                                None => true, // Never tried, allow attempt
                            };
                            
                            if should_retry {
                                let loss_percent = ((trade.down_purchase_price - down_current_bid) / trade.down_purchase_price) * 100.0;
                                crate::log_println!("🛑 EXTREME STOP-LOSS: {} | Purchase: ${:.4} | Current: ${:.4} | Loss: {:.1}%", 
                                      trade.down_token_name, trade.down_purchase_price, down_current_bid, loss_percent);
                                
                                // Mark that we're attempting to sell
                                sell_attempts.insert(trade.down_token_id.clone(), std::time::Instant::now());
                                drop(sell_attempts);
                                
                                match self.execute_sell_token(&trade.down_token_id, &trade.down_token_name, trade.down_remaining_units, down_current_bid, trade.down_purchase_price).await {
                                    Ok(_) => {
                                        trade.down_remaining_units = 0.0;
                                        // Clear the attempt tracking on success
                                        let mut attempts = self.pending_sell_attempts.lock().await;
                                        attempts.remove(&trade.down_token_id);
                                    }
                                    Err(e) => {
                                        // Keep the attempt time so we don't retry too soon
                                        // Will retry after 30 seconds
                                        warn!("   ⏳ Sell failed, will retry in 30s: {}", e);
                                    }
                                }
                            } else {
                                // In cooldown - don't spam logs
                                // The stop-loss condition is still true, but we're waiting before retrying
                            }
                        }
                        // Early exit only if market closing very soon
                        else if should_exit_early {
                            let profit_if_sold = (down_current_bid - trade.down_purchase_price) * trade.down_remaining_units;
                            crate::log_println!("⚠️  Market closing ({}s) - Selling all {} | P&L: ${:.2}", 
                                  time_remaining, trade.down_token_name, profit_if_sold);
                            if let Ok(_) = self.execute_sell_token(&trade.down_token_id, &trade.down_token_name, trade.down_remaining_units, down_current_bid, trade.down_purchase_price).await {
                                trade.down_remaining_units = 0.0;
                            }
                        }
                        // Otherwise: HOLD - don't sell on price drops for market-neutral strategy
                    }
                }
            }
            
            // Remove trade only if both tokens are fully sold
            if trade.up_remaining_units <= 0.001 && trade.down_remaining_units <= 0.001 {
                to_remove.push(key.clone());
            }
        }
        
        // Remove completed trades
        for key in to_remove {
            trades.remove(&key);
        }
        
        Ok(())
    }

    /// Execute sell order for a specific token
    async fn execute_sell_token(
        &self,
        token_id: &str,
        token_name: &str,
        units: f64,
        bid_price: f64,
        purchase_price: f64,
    ) -> Result<f64> {
        let revenue = units * bid_price;
        let cost = units * purchase_price;
        let profit = revenue - cost;
        
        crate::log_println!("💸 SELLING: {:.2} {} units at ${:.4} (BID)", units, token_name, bid_price);
        crate::log_println!("   Revenue: ${:.2} | Cost: ${:.2} | Profit: ${:.2}", 
              revenue, cost, profit);
        
        if self.simulation_mode {
            crate::log_println!("   ✅ SIMULATION: Sell executed");
        } else {
            let units_rounded = (units * 10000.0).round() / 10000.0;
            match self.api.place_market_order(
                token_id,
                units_rounded,
                "SELL",
                None,
            ).await {
                Ok(_) => {
                    crate::log_println!("   ✅ REAL: Sell order placed");
                }
                Err(e) => {
                    warn!("❌ Failed to place sell order: {}", e);
                    return Err(e);
                }
            }
        }
        
        // Update both cumulative and period profit
        let mut total = self.total_profit.lock().await;
        *total += profit;
        let total_profit = *total;
        drop(total);
        
        let mut period = self.period_profit.lock().await;
        *period += profit;
        let period_profit = *period;
        drop(period);
        
        crate::log_println!("   📊 Period Profit: ${:.2} | Total Profit: ${:.2}", period_profit, total_profit);
        
        Ok(profit)
    }

    /// Check and settle trades when markets close
    pub async fn check_market_closure(&self) -> Result<()> {
        let pending_trades: Vec<(String, ArbitrageTrade)> = {
            let pending = self.pending_trades.lock().await;
            pending.iter()
                .map(|(key, trade)| (key.clone(), trade.clone()))
                .collect()
        };
        
        if pending_trades.is_empty() {
            return Ok(()); // No pending trades to check
        }
        
        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        // Track if we need to log the check (only log if there are markets closing soon or closed)
        let mut has_closing_markets = false;
        let mut has_closed_markets = false;
        
        for (key, trade) in &pending_trades {
            let market_end_timestamp = trade.market_timestamp + trade.market_duration;
            if current_timestamp >= market_end_timestamp {
                has_closed_markets = true;
            } else if current_timestamp >= market_end_timestamp - 300 {
                has_closing_markets = true;
            }
        }
        
        // Only log if there are markets closing soon or already closed
        if has_closing_markets || has_closed_markets {
            crate::log_println!("🔍 Checking market closures for {} pending trade(s)...", pending_trades.len());
        }
        
        for (key, trade) in pending_trades {
            // Market closes at market_timestamp + market_duration seconds
            let market_end_timestamp = trade.market_timestamp + trade.market_duration;
            let time_since_close = current_timestamp.saturating_sub(market_end_timestamp);
            
            // Log market status
            if current_timestamp < market_end_timestamp {
                let time_until_close = market_end_timestamp - current_timestamp;
                // Log when market is closing soon (last 5 minutes) or every 2 minutes otherwise
                if time_until_close <= 300 || time_until_close % 120 == 0 {
                    let minutes = time_until_close / 60;
                    let seconds = time_until_close % 60;
                    crate::log_println!("   ⏳ Market {} still active | Closes in: {}m {}s | Up: {:.2} units | Down: {:.2} units", 
                          trade.condition_id[..8].to_string(), minutes, seconds, trade.up_remaining_units, trade.down_remaining_units);
                }
                continue; // Market hasn't closed yet
            }
            
            // Market has closed - check if it's resolved
            crate::log_println!("⏰ Market {} closed {}s ago | Checking resolution...", 
                  trade.condition_id[..8].to_string(), time_since_close);
            
            // Check if market is closed and resolved
            let market = match self.api.get_market(&trade.condition_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!("Failed to fetch market {}: {}", trade.condition_id[..8].to_string(), e);
                    continue;
                }
            };
            
            if !market.closed {
                crate::log_println!("   ⚠️  Market {} not yet closed (API says active) | Will retry...", 
                      trade.condition_id[..8].to_string());
                continue;
            }
            
            crate::log_println!("   ✅ Market {} is closed and resolved", trade.condition_id[..8].to_string());
            
            // Check which token won (Up or Down)
            let up_is_winner = market.tokens.iter().any(|t| t.token_id == trade.up_token_id && t.winner);
            let down_is_winner = market.tokens.iter().any(|t| t.token_id == trade.down_token_id && t.winner);
            
            // Handle Up token
            if trade.up_remaining_units > 0.001 {
                if up_is_winner {
                    // Redeem winning Up tokens
                    if !self.simulation_mode {
                        if let Err(e) = self.redeem_token_by_id(&trade.up_token_id, &trade.up_token_name, trade.up_remaining_units, "Up", &trade.condition_id).await {
                            warn!("Failed to redeem Up token {}: {}", trade.up_token_name, e);
                        }
                    }
                    
                    let remaining_value = trade.up_remaining_units * 1.0;
                    let remaining_cost = trade.up_purchase_price * trade.up_remaining_units;
                    let profit = remaining_value - remaining_cost;
                    
                    let mut total = self.total_profit.lock().await;
                    *total += profit;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period += profit;
                    drop(period);
                    
                    crate::log_println!("💰 Market Closed - Up Winner: {} | Remaining: {:.2} | Profit: ${:.2}", 
                          trade.up_token_name, trade.up_remaining_units, profit);
                } else {
                    // Up token lost
                    let loss = trade.up_purchase_price * trade.up_remaining_units;
                    let mut total = self.total_profit.lock().await;
                    *total -= loss;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period -= loss;
                    drop(period);
                    
                    crate::log_println!("❌ Market Closed - Up Lost: {} | Remaining: {:.2} | Loss: ${:.2}", 
                          trade.up_token_name, trade.up_remaining_units, loss);
                }
            }
            
            // Handle Down token
            if trade.down_remaining_units > 0.001 {
                if down_is_winner {
                    // Redeem winning Down tokens
                    if !self.simulation_mode {
                        if let Err(e) = self.redeem_token_by_id(&trade.down_token_id, &trade.down_token_name, trade.down_remaining_units, "Down", &trade.condition_id).await {
                            warn!("Failed to redeem Down token {}: {}", trade.down_token_name, e);
                        }
                    }
                    
                    let remaining_value = trade.down_remaining_units * 1.0;
                    let remaining_cost = trade.down_purchase_price * trade.down_remaining_units;
                    let profit = remaining_value - remaining_cost;
                    
                    let mut total = self.total_profit.lock().await;
                    *total += profit;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period += profit;
                    drop(period);
                    
                    crate::log_println!("💰 Market Closed - Down Winner: {} | Remaining: {:.2} | Profit: ${:.2}", 
                          trade.down_token_name, trade.down_remaining_units, profit);
                } else {
                    // Down token lost
                    let loss = trade.down_purchase_price * trade.down_remaining_units;
                    let mut total = self.total_profit.lock().await;
                    *total -= loss;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period -= loss;
                    drop(period);
                    
                    crate::log_println!("❌ Market Closed - Down Lost: {} | Remaining: {:.2} | Loss: ${:.2}", 
                          trade.down_token_name, trade.down_remaining_units, loss);
                }
            }
            
            // Show both period and total profit
            let total_profit = *self.total_profit.lock().await;
            let period_profit = *self.period_profit.lock().await;
            crate::log_println!("   📊 Period Profit: ${:.2} | Total Profit: ${:.2}", period_profit, total_profit);
            
            // Remove trade
            let mut pending = self.pending_trades.lock().await;
            pending.remove(&key);
            crate::log_println!("   ✅ Trade removed from pending list");
        }
        
        Ok(())
    }


    async fn redeem_token_by_id(&self, token_id: &str, token_name: &str, units: f64, outcome: &str, condition_id: &str) -> Result<()> {
        crate::log_println!("🔄 Redeeming {:.2} units of {} (outcome: {})", units, token_name, outcome);
        
        match self.api.redeem_tokens(
            condition_id,
            token_id,
            outcome,
        ).await {
            Ok(_) => {
                crate::log_println!("✅ Successfully redeemed {:.2} units", units);
                Ok(())
            }
            Err(e) => {
                warn!("❌ Failed to redeem tokens: {}", e);
                Err(e)
            }
        }
    }

    /// Reset when new period starts (only clear trades from old periods)
    pub async fn reset_period(&self, old_period: Option<u64>) {
        if let Some(old_period_timestamp) = old_period {
            // Only remove trades from the old period
            let mut trades = self.pending_trades.lock().await;
            trades.retain(|_, trade| trade.market_timestamp != old_period_timestamp);
            
            // Also clear position sizes for tokens from old period
            let mut positions = self.position_sizes.lock().await;
            // We can't easily filter by period here, so we'll just clear all
            // Position sizes will be recalculated as new trades come in
            positions.clear();
        } else {
            // If no old period specified, clear all (fallback behavior)
            let mut trades = self.pending_trades.lock().await;
            trades.clear();
            
            let mut positions = self.position_sizes.lock().await;
            positions.clear();
        }
        
        // Reset trade counter and period profit for new period
        let mut count = self.trades_executed.lock().await;
        let old_count = *count;
        *count = 0;
        
        let mut period = self.period_profit.lock().await;
        let period_profit = *period;
        *period = 0.0;
        drop(period);
        
        let total_profit = *self.total_profit.lock().await;
        crate::log_println!("🔄 Period reset: Previous period profit: ${:.2} | Total profit: ${:.2} | Trades: {}", 
              period_profit, total_profit, old_count);
    }

    /// Get statistics
    pub async fn get_stats(&self) -> (f64, u64) {
        let profit = *self.total_profit.lock().await;
        let count = *self.trades_executed.lock().await;
        (profit, count)
    }
}
