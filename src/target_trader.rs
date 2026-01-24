use crate::api::PolymarketApi;
use crate::models::*;
use crate::config::TradingConfig;
use crate::direction_detector::{DirectionDetector, TradingState, Direction};
use crate::position_tracker::PositionTracker;
use crate::monitor::MarketSnapshot;
use anyhow::Result;
use log::warn;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;

/// Trader that clones target bot's strategy:
/// - Price monitoring and direction detection
/// - Real-time P&L calculation
/// - Dynamic position sizing based on P&L
/// - Time-based risk management
/// - Selective selling in late period
pub struct TargetTrader {
    api: Arc<PolymarketApi>,
    config: TradingConfig,
    simulation_mode: bool,
    
    // Per-market state
    markets: Arc<Mutex<HashMap<String, MarketState>>>,
    
    // Track trades for market closure checking
    market_trades: Arc<Mutex<HashMap<String, MarketTrade>>>,
    
    // Profit tracking
    total_profit: Arc<Mutex<f64>>,
    period_profit: Arc<Mutex<f64>>,
}

#[derive(Clone)]
struct MarketTrade {
    condition_id: String,
    period_timestamp: u64,
    market_duration: u64,
    up_token_id: String,
    down_token_id: String,
    up_shares: f64,
    down_shares: f64,
    up_avg_price: f64,
    down_avg_price: f64,
}

struct MarketState {
    condition_id: String,
    period_timestamp: u64,
    market_duration: u64, // 900 for 15m, 3600 for 1h
    
    // Direction detection
    direction_detector: DirectionDetector,
    
    // Position tracking
    position_tracker: PositionTracker,
    
    // Token IDs
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    
    // Last prices
    last_up_price: Option<f64>,
    last_down_price: Option<f64>,
    
    // Trading state
    trading_state: TradingState,
    
    // Last trade time
    last_trade_time: Option<u64>,
    
    // Track if we've checked closure for this market
    closure_checked: bool,
}

impl TargetTrader {
    pub fn new(api: Arc<PolymarketApi>, config: TradingConfig, simulation_mode: bool) -> Self {
        Self {
            api,
            config,
            simulation_mode,
            markets: Arc::new(Mutex::new(HashMap::new())),
            market_trades: Arc::new(Mutex::new(HashMap::new())),
            total_profit: Arc::new(Mutex::new(0.0)),
            period_profit: Arc::new(Mutex::new(0.0)),
        }
    }

    /// Process market snapshot and make trading decisions
    pub async fn process_snapshot(&self, snapshot: &MarketSnapshot) -> Result<()> {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Process each market
        self.process_market(
            "ETH_15M",
            &snapshot.eth_market_15m,
            snapshot.eth_15m_period_timestamp,
            snapshot.eth_15m_time_remaining,
            current_time,
        ).await?;

        self.process_market(
            "ETH_1H",
            &snapshot.eth_market_1h,
            snapshot.eth_1h_period_timestamp,
            snapshot.eth_1h_time_remaining,
            current_time,
        ).await?;

        self.process_market(
            "BTC_15M",
            &snapshot.btc_market_15m,
            snapshot.btc_15m_period_timestamp,
            snapshot.btc_15m_time_remaining,
            current_time,
        ).await?;

        self.process_market(
            "BTC_1H",
            &snapshot.btc_market_1h,
            snapshot.btc_1h_period_timestamp,
            snapshot.btc_1h_time_remaining,
            current_time,
        ).await?;

        Ok(())
    }

    async fn process_market(
        &self,
        market_name: &str,
        market_data: &MarketData,
        period_timestamp: u64,
        time_remaining: u64,
        current_time: u64,
    ) -> Result<()> {
        let mut markets = self.markets.lock().await;
        let market_key = format!("{}_{}", market_name, period_timestamp);
        
        // Get or create market state
        let market_state = markets.entry(market_key.clone())
            .or_insert_with(|| {
                MarketState {
                    condition_id: market_data.condition_id.clone(),
                    period_timestamp,
                    market_duration: if market_name.contains("15M") { 900 } else { 3600 },
                    direction_detector: DirectionDetector::new(
                        self.config.direction_monitoring_duration_seconds.unwrap_or(30)
                    ),
                    position_tracker: PositionTracker::new(),
                    up_token_id: market_data.up_token.as_ref().map(|t| t.token_id.clone()),
                    down_token_id: market_data.down_token.as_ref().map(|t| t.token_id.clone()),
                    last_up_price: market_data.up_token.as_ref().and_then(|t| t.ask_price().to_string().parse::<f64>().ok()),
                    last_down_price: market_data.down_token.as_ref().and_then(|t| t.ask_price().to_string().parse::<f64>().ok()),
                    trading_state: TradingState::Monitoring { start_time: current_time },
                    last_trade_time: None,
                    closure_checked: false,
                }
            });
        
        // Update token IDs and prices if available
        if let Some(up_token) = &market_data.up_token {
            market_state.up_token_id = Some(up_token.token_id.clone());
            market_state.last_up_price = up_token.ask_price().to_string().parse::<f64>().ok();
        }
        if let Some(down_token) = &market_data.down_token {
            market_state.down_token_id = Some(down_token.token_id.clone());
            market_state.last_down_price = down_token.ask_price().to_string().parse::<f64>().ok();
        }

        // Update prices in direction detector
        if let Some(up_price) = market_state.last_up_price {
            market_state.direction_detector.add_price("Up", up_price, current_time);
        }
        if let Some(down_price) = market_state.last_down_price {
            market_state.direction_detector.add_price("Down", down_price, current_time);
        }

        // Update trading state
        let old_state = format!("{:?}", market_state.trading_state);
        market_state.trading_state = market_state.direction_detector.update_state(current_time);
        let new_state = format!("{:?}", market_state.trading_state);
        if old_state != new_state {
            crate::log_println!("🔄 {} State change: {} → {}", market_name, old_state, new_state);
        }

        // Make trading decision
        self.make_trading_decision(market_name, market_state, time_remaining, current_time).await?;

        Ok(())
    }

    async fn make_trading_decision(
        &self,
        market_name: &str,
        market_state: &mut MarketState,
        time_remaining: u64,
        current_time: u64,
    ) -> Result<()> {
        let up_price = market_state.last_up_price.unwrap_or(0.0);
        let down_price = market_state.last_down_price.unwrap_or(0.0);

        if up_price <= 0.0 || down_price <= 0.0 {
            return Ok(()); // No valid prices
        }

        // Calculate current P&L
        let pnl_if_up = market_state.position_tracker.calculate_pnl_if_up();
        let pnl_if_down = market_state.position_tracker.calculate_pnl_if_down();

        // Check for turning point
        if market_state.direction_detector.check_turning_point() {
            // Enter monitoring mode
            market_state.trading_state = TradingState::Monitoring { start_time: current_time };
            return Ok(());
        }

        match &market_state.trading_state {
            TradingState::Monitoring { start_time } => {
                // Still monitoring, but allow initial trade if we have enough data
                let elapsed = current_time - start_time;
                let total_investment = market_state.position_tracker.get_total_investment();
                
                // If we have no positions yet and monitoring for a while, make an initial trade
                if total_investment < 0.01 && elapsed >= 10 {
                    // Make initial trade based on simple price comparison
                    if up_price < down_price {
                        // Up is cheaper, buy Up
                        crate::log_println!("📊 {} Initial trade: Up price (${:.4}) < Down price (${:.4}), buying Up", 
                            market_name, up_price, down_price);
                        let initial_size = 20.0; // Start with 20 shares
                        self.execute_buy(market_name, market_state, "Up", initial_size, up_price, current_time).await?;
                        return Ok(());
                    } else {
                        // Down is cheaper, buy Down
                        crate::log_println!("📊 {} Initial trade: Down price (${:.4}) < Up price (${:.4}), buying Down", 
                            market_name, down_price, up_price);
                        let initial_size = 20.0;
                        self.execute_buy(market_name, market_state, "Down", initial_size, down_price, current_time).await?;
                        return Ok(());
                    }
                }
                // Still monitoring, don't trade yet
                return Ok(());
            }

            TradingState::Trading { direction, token: _ } => {
                match direction {
                    Direction::Up => {
                        // Up momentum detected
                        // Allow trading even if P&L is 0 (no positions yet)
                        if pnl_if_up <= 0.0 {
                            // Need to flip P&L positive
                            let shares_needed = (-pnl_if_up) / (1.0 - up_price).max(0.01);
                            let multiplier = self.config.pnl_flip_multiplier.unwrap_or(1.2);
                            let max_shares = self.config.position_max_shares.unwrap_or(200.0);
                            let buy_size = (shares_needed * multiplier).min(max_shares);
                            
                            if buy_size >= 5.0 { // Minimum order size
                                self.execute_buy(
                                    market_name,
                                    market_state,
                                    "Up",
                                    buy_size,
                                    up_price,
                                    current_time,
                                ).await?;
                            }
                        } else if time_remaining < self.config.late_period_threshold_seconds.unwrap_or(300) {
                            // Late period - add more based on confidence
                            let confidence = self.calculate_confidence(time_remaining, market_state.market_duration);
                            let add_size = market_state.position_tracker.get_up_position().total_shares * confidence * 0.1;
                            
                            if add_size >= 5.0 {
                                self.execute_buy(
                                    market_name,
                                    market_state,
                                    "Up",
                                    add_size,
                                    up_price,
                                    current_time,
                                ).await?;
                            }
                        }

                        // Cut losses on Down if time running out
                        let cut_losses_threshold = self.config.cut_losses_threshold_seconds.unwrap_or(180);
                        let risk_threshold = self.config.cut_losses_risk_threshold.unwrap_or(50.0);
                        let cut_percentage = self.config.cut_losses_percentage.unwrap_or(0.8);
                        if time_remaining < cut_losses_threshold && pnl_if_down < -risk_threshold {
                            let down_shares = market_state.position_tracker.get_down_position().total_shares;
                            if down_shares > 0.0 {
                                self.execute_sell(
                                    market_name,
                                    market_state,
                                    "Down",
                                    down_shares * cut_percentage,
                                    down_price,
                                    current_time,
                                ).await?;
                            }
                        }
                    }

                    Direction::Down => {
                        // Down momentum detected
                        // Allow trading even if P&L is 0 (no positions yet)
                        if pnl_if_down <= 0.0 {
                            // Need to flip P&L positive
                            let shares_needed = (-pnl_if_down) / (1.0 - down_price).max(0.01);
                            let multiplier = self.config.pnl_flip_multiplier.unwrap_or(1.2);
                            let max_shares = self.config.position_max_shares.unwrap_or(200.0);
                            let buy_size = (shares_needed * multiplier).min(max_shares);
                            
                            if buy_size >= 5.0 {
                                self.execute_buy(
                                    market_name,
                                    market_state,
                                    "Down",
                                    buy_size,
                                    down_price,
                                    current_time,
                                ).await?;
                            }
                        } else if time_remaining < self.config.late_period_threshold_seconds.unwrap_or(300) {
                            // Late period - add more
                            let confidence = self.calculate_confidence(time_remaining, market_state.market_duration);
                            let add_size = market_state.position_tracker.get_down_position().total_shares * confidence * 0.1;
                            
                            if add_size >= 5.0 {
                                self.execute_buy(
                                    market_name,
                                    market_state,
                                    "Down",
                                    add_size,
                                    down_price,
                                    current_time,
                                ).await?;
                            }
                        }

                        // Cut losses on Up if time running out
                        let cut_losses_threshold = self.config.cut_losses_threshold_seconds.unwrap_or(180);
                        let risk_threshold = self.config.cut_losses_risk_threshold.unwrap_or(50.0);
                        let cut_percentage = self.config.cut_losses_percentage.unwrap_or(0.8);
                        if time_remaining < cut_losses_threshold && pnl_if_up < -risk_threshold {
                            let up_shares = market_state.position_tracker.get_up_position().total_shares;
                            if up_shares > 0.0 {
                                self.execute_sell(
                                    market_name,
                                    market_state,
                                    "Up",
                                    up_shares * cut_percentage,
                                    up_price,
                                    current_time,
                                ).await?;
                            }
                        }
                    }

                    Direction::Uncertain => {
                        // Uncertain, don't trade
                    }
                }
            }

            TradingState::WaitingForConfirmation { .. } => {
                // Waiting for confirmation, don't trade
            }
        }

        Ok(())
    }

    async fn execute_buy(
        &self,
        market_name: &str,
        market_state: &mut MarketState,
        outcome: &str,
        shares: f64,
        price: f64,
        current_time: u64,
    ) -> Result<()> {
        let token_id = match outcome {
            "Up" => market_state.up_token_id.as_ref(),
            "Down" => market_state.down_token_id.as_ref(),
            _ => return Ok(()),
        };

        let token_id = match token_id {
            Some(id) => id,
            None => {
                warn!("No token ID for {} {}", market_name, outcome);
                return Ok(());
            }
        };

        // Ensure minimum order size
        let min_shares = self.config.position_min_shares.unwrap_or(5.0);
        let min_usd = self.config.position_min_usd.unwrap_or(1.10);
        let max_shares = self.config.position_max_shares.unwrap_or(200.0);
        let shares = shares.max(min_shares).max(min_usd / price).min(max_shares);

        crate::log_println!(
            "🔼 {} BUY {} {} shares @ ${:.4} | P&L if Up: ${:.2} | P&L if Down: ${:.2}",
            market_name,
            outcome,
            shares,
            price,
            market_state.position_tracker.calculate_pnl_if_up(),
            market_state.position_tracker.calculate_pnl_if_down(),
        );

        if self.simulation_mode {
            crate::log_println!("   ✅ SIMULATION: Order executed");
        } else {
            let shares_rounded = (shares * 10000.0).round() / 10000.0;
            match self.api.place_market_order(token_id, shares_rounded, "BUY", None).await {
                Ok(_) => crate::log_println!("   ✅ REAL: Order placed"),
                Err(e) => {
                    warn!("❌ Failed to place order: {}", e);
                    return Err(e);
                }
            }
        }

        // Update position tracker
        market_state.position_tracker.add_position(outcome, shares, price);
        market_state.direction_detector.record_trade(current_time);
        market_state.last_trade_time = Some(current_time);
        
        // Update market trade tracking
        let market_key = format!("{}_{}", market_name, market_state.period_timestamp);
        let mut trades = self.market_trades.lock().await;
        let trade = trades.entry(market_key.clone())
            .or_insert_with(|| MarketTrade {
                condition_id: market_state.condition_id.clone(),
                period_timestamp: market_state.period_timestamp,
                market_duration: market_state.market_duration,
                up_token_id: market_state.up_token_id.clone().unwrap_or_default(),
                down_token_id: market_state.down_token_id.clone().unwrap_or_default(),
                up_shares: 0.0,
                down_shares: 0.0,
                up_avg_price: 0.0,
                down_avg_price: 0.0,
            });
        
        match outcome {
            "Up" => {
                let old_total = trade.up_shares * trade.up_avg_price;
                trade.up_shares += shares;
                trade.up_avg_price = (old_total + shares * price) / trade.up_shares;
            }
            "Down" => {
                let old_total = trade.down_shares * trade.down_avg_price;
                trade.down_shares += shares;
                trade.down_avg_price = (old_total + shares * price) / trade.down_shares;
            }
            _ => {}
        }

        Ok(())
    }

    async fn execute_sell(
        &self,
        market_name: &str,
        market_state: &mut MarketState,
        outcome: &str,
        shares: f64,
        price: f64,
        current_time: u64,
    ) -> Result<()> {
        let token_id = match outcome {
            "Up" => market_state.up_token_id.as_ref(),
            "Down" => market_state.down_token_id.as_ref(),
            _ => return Ok(()),
        };

        let token_id = match token_id {
            Some(id) => id,
            None => {
                warn!("No token ID for {} {}", market_name, outcome);
                return Ok(());
            }
        };

        if shares < 0.1 {
            return Ok(());
        }

        crate::log_println!(
            "🔽 {} SELL {} {} shares @ ${:.4} | Cutting losses | P&L if Up: ${:.2} | P&L if Down: ${:.2}",
            market_name,
            outcome,
            shares,
            price,
            market_state.position_tracker.calculate_pnl_if_up(),
            market_state.position_tracker.calculate_pnl_if_down(),
        );

        if self.simulation_mode {
            crate::log_println!("   ✅ SIMULATION: Sell order executed");
        } else {
            let shares_rounded = (shares * 10000.0).round() / 10000.0;
            match self.api.place_market_order(token_id, shares_rounded, "SELL", None).await {
                Ok(_) => crate::log_println!("   ✅ REAL: Sell order placed"),
                Err(e) => {
                    warn!("❌ Failed to place sell order: {}", e);
                    return Err(e);
                }
            }
        }

        // Update position tracker
        market_state.position_tracker.remove_position(outcome, shares, price);
        market_state.direction_detector.record_trade(current_time);
        market_state.last_trade_time = Some(current_time);

        Ok(())
    }

    fn calculate_confidence(&self, time_remaining: u64, _market_duration: u64) -> f64 {
        // Confidence increases as time runs out (trend becomes clearer)
        let late_threshold = self.config.late_period_threshold_seconds.unwrap_or(300);
        let mid_threshold = 600;
        
        if time_remaining < late_threshold {
            self.config.late_period_confidence_late.unwrap_or(0.8)
        } else if time_remaining < mid_threshold {
            self.config.late_period_confidence_mid.unwrap_or(0.5)
        } else {
            self.config.late_period_confidence_early.unwrap_or(0.2)
        }
    }
    
    /// Check and settle trades when markets close
    pub async fn check_market_closure(&self) -> Result<()> {
        let trades: Vec<(String, MarketTrade)> = {
            let market_trades = self.market_trades.lock().await;
            market_trades.iter()
                .map(|(key, trade)| (key.clone(), (*trade).clone()))
                .collect()
        };
        
        if trades.is_empty() {
            return Ok(()); // No trades to check
        }
        
        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        for (market_key, trade) in trades {
            let market_end_timestamp = trade.period_timestamp + trade.market_duration;
            
            // Check if market has closed
            if current_timestamp < market_end_timestamp {
                continue; // Market hasn't closed yet
            }
            
            // Check if we've already successfully processed this market
            let mut markets = self.markets.lock().await;
            if let Some(market_state) = markets.get_mut(&market_key) {
                if market_state.closure_checked {
                    continue; // Already successfully processed
                }
            }
            drop(markets);
            
            let time_since_close = current_timestamp - market_end_timestamp;
            let minutes_since_close = time_since_close / 60;
            let seconds_since_close = time_since_close % 60;
            
            crate::log_println!("⏰ Market {} closed {}m {}s ago | Checking resolution...", 
                  trade.condition_id[..8].to_string(), minutes_since_close, seconds_since_close);
            
            // Check if market is closed and resolved
            let market = match self.api.get_market(&trade.condition_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!("Failed to fetch market {}: {}", trade.condition_id[..8].to_string(), e);
                    continue;
                }
            };
            
            if !market.closed {
                // Market hasn't closed yet according to API - keep checking
                // This is normal for 15m markets (4-5 min delay) and 1h markets (30-40 min delay)
                if time_since_close % 60 == 0 || time_since_close < 60 {
                    // Log every minute or in first minute
                    crate::log_println!("   ⏳ Market {} not yet closed (API says active) | Will retry in next check...", 
                          trade.condition_id[..8].to_string());
                }
                continue;
            }
            
            crate::log_println!("   ✅ Market {} is closed and resolved", trade.condition_id[..8].to_string());
            
            // Check which token won (Up or Down)
            let up_is_winner = market.tokens.iter().any(|t| t.token_id == trade.up_token_id && t.winner);
            let down_is_winner = market.tokens.iter().any(|t| t.token_id == trade.down_token_id && t.winner);
            
            // Handle Up token
            if trade.up_shares > 0.001 {
                if up_is_winner {
                    // Redeem winning Up tokens
                    if !self.simulation_mode {
                        if let Err(e) = self.redeem_token_by_id(&trade.up_token_id, "Up", trade.up_shares, "Up", &trade.condition_id).await {
                            warn!("Failed to redeem Up token: {}", e);
                        }
                    }
                    
                    let remaining_value = trade.up_shares * 1.0;
                    let remaining_cost = trade.up_avg_price * trade.up_shares;
                    let profit = remaining_value - remaining_cost;
                    
                    let mut total = self.total_profit.lock().await;
                    *total += profit;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period += profit;
                    drop(period);
                    
                    crate::log_println!("💰 Market Closed - Up Winner: {:.2} shares @ ${:.4} avg | Profit: ${:.2}", 
                          trade.up_shares, trade.up_avg_price, profit);
                } else {
                    // Up token lost
                    let loss = trade.up_avg_price * trade.up_shares;
                    let mut total = self.total_profit.lock().await;
                    *total -= loss;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period -= loss;
                    drop(period);
                    
                    crate::log_println!("❌ Market Closed - Up Lost: {:.2} shares @ ${:.4} avg | Loss: ${:.2}", 
                          trade.up_shares, trade.up_avg_price, loss);
                }
            }
            
            // Handle Down token
            if trade.down_shares > 0.001 {
                if down_is_winner {
                    // Redeem winning Down tokens
                    if !self.simulation_mode {
                        if let Err(e) = self.redeem_token_by_id(&trade.down_token_id, "Down", trade.down_shares, "Down", &trade.condition_id).await {
                            warn!("Failed to redeem Down token: {}", e);
                        }
                    }
                    
                    let remaining_value = trade.down_shares * 1.0;
                    let remaining_cost = trade.down_avg_price * trade.down_shares;
                    let profit = remaining_value - remaining_cost;
                    
                    let mut total = self.total_profit.lock().await;
                    *total += profit;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period += profit;
                    drop(period);
                    
                    crate::log_println!("💰 Market Closed - Down Winner: {:.2} shares @ ${:.4} avg | Profit: ${:.2}", 
                          trade.down_shares, trade.down_avg_price, profit);
                } else {
                    // Down token lost
                    let loss = trade.down_avg_price * trade.down_shares;
                    let mut total = self.total_profit.lock().await;
                    *total -= loss;
                    drop(total);
                    
                    let mut period = self.period_profit.lock().await;
                    *period -= loss;
                    drop(period);
                    
                    crate::log_println!("❌ Market Closed - Down Lost: {:.2} shares @ ${:.4} avg | Loss: ${:.2}", 
                          trade.down_shares, trade.down_avg_price, loss);
                }
            }
            
            // Show both period and total profit
            let total_profit = *self.total_profit.lock().await;
            let period_profit = *self.period_profit.lock().await;
            crate::log_println!("   📊 Period Profit: ${:.2} | Total Profit: ${:.2}", period_profit, total_profit);
            
            // Mark as successfully processed
            let mut markets = self.markets.lock().await;
            if let Some(market_state) = markets.get_mut(&market_key) {
                market_state.closure_checked = true;
            }
            drop(markets);
            
            // Remove trade
            let mut market_trades = self.market_trades.lock().await;
            market_trades.remove(&market_key);
            crate::log_println!("   ✅ Trade removed from tracking");
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
    
    /// Reset when new period starts
    pub async fn reset_period(&self, old_period: Option<u64>) {
        if let Some(old_period_timestamp) = old_period {
            // Remove trades from old period
            let mut trades = self.market_trades.lock().await;
            trades.retain(|_, trade| trade.period_timestamp != old_period_timestamp);
            
            // Remove market states from old period
            let mut markets = self.markets.lock().await;
            markets.retain(|key, _| !key.ends_with(&format!("_{}", old_period_timestamp)));
        } else {
            // Clear all if no old period specified
            let mut trades = self.market_trades.lock().await;
            trades.clear();
            
            let mut markets = self.markets.lock().await;
            markets.clear();
        }
        
        // Reset period profit for new period
        let mut period = self.period_profit.lock().await;
        let period_profit = *period;
        *period = 0.0;
        drop(period);
        
        let total_profit = *self.total_profit.lock().await;
        crate::log_println!("🔄 Period reset: Previous period profit: ${:.2} | Total profit: ${:.2}", 
              period_profit, total_profit);
    }
}
