use crate::api::PolymarketApi;
use crate::models::Fill;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Research data structure for saving to research.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchData {
    pub target_address: String,
    pub market_condition_id: String,
    pub market_slug: Option<String>,
    pub market_question: Option<String>,
    pub period_timestamp: Option<u64>,
    pub fills: Vec<FillEntry>,
    pub analysis: Option<AnalysisNotes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detailed_analysis: Option<DetailedAnalysis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillEntry {
    pub id: String,
    pub token_id: String,
    pub token_name: Option<String>,
    pub side: String, // "BUY" or "SELL"
    pub size: String,
    pub price: String,
    pub timestamp: String,
    pub order_id: Option<String>,
    pub fee: Option<String>,
    pub outcome: Option<String>, // "Up" or "Down" from activity API
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisNotes {
    pub total_buys: usize,
    pub total_sells: usize,
    pub buy_up_count: usize,
    pub buy_down_count: usize,
    pub sell_up_count: usize,
    pub sell_down_count: usize,
    pub price_range_up: Option<(String, String)>, // (min, max)
    pub price_range_down: Option<(String, String)>, // (min, max)
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailedAnalysis {
    /// Trading timeline analysis
    pub timeline: TimelineAnalysis,
    /// Price momentum analysis
    pub momentum: MomentumAnalysis,
    /// Position sizing patterns
    pub sizing: SizingAnalysis,
    /// Up vs Down trading correlation
    pub correlation: CorrelationAnalysis,
    /// Key insights and patterns
    pub insights: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineAnalysis {
    pub total_trades: usize,
    pub period_start_timestamp: Option<u64>,
    pub period_end_timestamp: Option<u64>,
    pub first_trade_timestamp: Option<u64>,
    pub last_trade_timestamp: Option<u64>,
    pub trades_per_minute: Option<f64>,
    pub active_trading_periods: Vec<String>, // e.g., "0-5min", "5-10min", etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumAnalysis {
    pub up_token_price_trend: String, // "RISING", "FALLING", "MIXED"
    pub down_token_price_trend: String,
    pub up_buy_when_rising: usize, // Count of Up buys when price was rising
    pub down_buy_when_rising: usize, // Count of Down buys when price was rising
    pub average_price_change_before_buy: Option<f64>, // Average price change in last N trades before buy
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizingAnalysis {
    pub common_sizes: Vec<(String, usize)>, // (size, count)
    pub average_size_up: Option<f64>,
    pub average_size_down: Option<f64>,
    pub max_size: Option<f64>,
    pub min_size: Option<f64>,
    pub size_patterns: Vec<String>, // e.g., "Often uses 201 shares"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationAnalysis {
    pub simultaneous_up_down_buys: usize, // Buys both tokens within same timestamp
    pub up_down_ratio: f64, // Up buys / Down buys
    pub price_gap_when_buying_both: Vec<f64>, // Price gap (Up + Down) when buying both
    pub correlation_notes: Vec<String>,
}

impl ResearchData {
    /// Create research data from fills
    pub fn from_fills(
        target_address: String,
        market_condition_id: String,
        fills: Vec<Fill>,
        market_slug: Option<String>,
        market_question: Option<String>,
        period_timestamp: Option<u64>,
    ) -> Self {
        let fill_entries: Vec<FillEntry> = fills
            .iter()
            .map(|fill| FillEntry {
                id: fill.id.clone().unwrap_or_else(|| "unknown".to_string()),
                token_id: fill.get_token_id()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
                token_name: fill.token_name.clone()
                    .or_else(|| fill.outcome.clone()), // Use outcome as token name if available
                side: fill.side.clone(),
                size: fill.size.to_string(), // Convert f64 to string
                price: fill.price.to_string(), // Convert f64 to string
                timestamp: fill.timestamp.to_string(), // Convert u64 to string
                order_id: fill.order_id.clone(),
                fee: fill.fee.clone(),
                outcome: fill.outcome.clone(), // Include outcome
            })
            .collect();
        
        // Generate basic analysis
        let analysis = Self::analyze_fills(&fill_entries);
        
        // Generate detailed analysis
        let detailed_analysis = Self::detailed_analyze_fills(&fill_entries, period_timestamp);
        
        Self {
            target_address,
            market_condition_id,
            market_slug,
            market_question,
            period_timestamp,
            fills: fill_entries,
            analysis: Some(analysis),
            detailed_analysis: Some(detailed_analysis),
        }
    }
    
    /// Analyze fills and generate insights
    fn analyze_fills(fills: &[FillEntry]) -> AnalysisNotes {
        let mut total_buys = 0;
        let mut total_sells = 0;
        let mut buy_up_count = 0;
        let mut buy_down_count = 0;
        let mut sell_up_count = 0;
        let mut sell_down_count = 0;
        
        let mut up_prices: Vec<f64> = Vec::new();
        let mut down_prices: Vec<f64> = Vec::new();
        let mut notes = Vec::new();
        
        for fill in fills {
            if fill.side == "BUY" {
                total_buys += 1;
                // Check outcome or token_name to determine Up/Down
                let is_up = fill.outcome.as_ref()
                    .map(|o| o.to_uppercase().contains("UP"))
                    .unwrap_or_else(|| {
                        fill.token_name.as_ref()
                            .map(|n| n.to_uppercase().contains("UP"))
                            .unwrap_or(false)
                    });
                let is_down = fill.outcome.as_ref()
                    .map(|o| o.to_uppercase().contains("DOWN"))
                    .unwrap_or_else(|| {
                        fill.token_name.as_ref()
                            .map(|n| n.to_uppercase().contains("DOWN"))
                            .unwrap_or(false)
                    });
                
                if is_up {
                    buy_up_count += 1;
                    if let Ok(price) = fill.price.parse::<f64>() {
                        up_prices.push(price);
                    }
                } else if is_down {
                    buy_down_count += 1;
                    if let Ok(price) = fill.price.parse::<f64>() {
                        down_prices.push(price);
                    }
                }
            } else if fill.side == "SELL" {
                total_sells += 1;
                // Check outcome or token_name to determine Up/Down
                let is_up = fill.outcome.as_ref()
                    .map(|o| o.to_uppercase().contains("UP"))
                    .unwrap_or_else(|| {
                        fill.token_name.as_ref()
                            .map(|n| n.to_uppercase().contains("UP"))
                            .unwrap_or(false)
                    });
                let is_down = fill.outcome.as_ref()
                    .map(|o| o.to_uppercase().contains("DOWN"))
                    .unwrap_or_else(|| {
                        fill.token_name.as_ref()
                            .map(|n| n.to_uppercase().contains("DOWN"))
                            .unwrap_or(false)
                    });
                
                if is_up {
                    sell_up_count += 1;
                } else if is_down {
                    sell_down_count += 1;
                }
            }
        }
        
        // Calculate price ranges
        let price_range_up = if !up_prices.is_empty() {
            let min = up_prices.iter().fold(f64::INFINITY, |a, &b| a.min(b));
            let max = up_prices.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
            Some((format!("{:.4}", min), format!("{:.4}", max)))
        } else {
            None
        };
        
        let price_range_down = if !down_prices.is_empty() {
            let min = down_prices.iter().fold(f64::INFINITY, |a, &b| a.min(b));
            let max = down_prices.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
            Some((format!("{:.4}", min), format!("{:.4}", max)))
        } else {
            None
        };
        
        // Generate notes
        if total_buys > total_sells {
            notes.push(format!("More buys ({}) than sells ({}) - net accumulation", total_buys, total_sells));
        }
        
        if buy_up_count > 0 && buy_down_count > 0 {
            notes.push("Traded both Up and Down tokens".to_string());
        } else if buy_up_count > 0 {
            notes.push("Only traded Up tokens".to_string());
        } else if buy_down_count > 0 {
            notes.push("Only traded Down tokens".to_string());
        }
        
        if sell_up_count > 0 && sell_down_count == 0 {
            notes.push("Only sold Up tokens (not Down) - possible outcome prediction".to_string());
        } else if sell_down_count > 0 && sell_up_count == 0 {
            notes.push("Only sold Down tokens (not Up) - possible outcome prediction".to_string());
        }
        
        AnalysisNotes {
            total_buys,
            total_sells,
            buy_up_count,
            buy_down_count,
            sell_up_count,
            sell_down_count,
            price_range_up,
            price_range_down,
            notes,
        }
    }
    
    /// Detailed analysis of fills for deeper insights
    fn detailed_analyze_fills(fills: &[FillEntry], period_timestamp: Option<u64>) -> DetailedAnalysis {
        let mut insights = Vec::new();
        
        // Parse all fills with timestamps and prices
        let mut parsed_fills: Vec<(u64, String, f64, f64)> = Vec::new(); // (timestamp, outcome, price, size)
        for fill in fills {
            if let (Ok(ts), Ok(price), Ok(size)) = (
                fill.timestamp.parse::<u64>(),
                fill.price.parse::<f64>(),
                fill.size.parse::<f64>(),
            ) {
                let outcome = fill.outcome.as_ref()
                    .or_else(|| fill.token_name.as_ref())
                    .map(|s| s.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                parsed_fills.push((ts, outcome, price, size));
            }
        }
        
        // Sort by timestamp
        parsed_fills.sort_by_key(|(ts, _, _, _)| *ts);
        
        // Timeline Analysis
        let timeline = Self::analyze_timeline(&parsed_fills, period_timestamp);
        
        // Momentum Analysis
        let momentum = Self::analyze_momentum(&parsed_fills);
        
        // Sizing Analysis
        let sizing = Self::analyze_sizing(&parsed_fills);
        
        // Correlation Analysis
        let correlation = Self::analyze_correlation(&parsed_fills);
        
        // Generate insights
        if parsed_fills.is_empty() {
            insights.push("No trades found in this market".to_string());
        } else {
            // Insight: Trading pattern
            if correlation.simultaneous_up_down_buys > 0 {
                insights.push(format!(
                    "Buys both Up and Down tokens simultaneously {} times - market-neutral strategy",
                    correlation.simultaneous_up_down_buys
                ));
            } else {
                insights.push("Buys Up and Down tokens separately - directional/momentum strategy".to_string());
            }
            
            // Insight: Price range
            let up_prices: Vec<f64> = parsed_fills.iter()
                .filter(|(_, outcome, _, _)| outcome.to_uppercase().contains("UP"))
                .map(|(_, _, price, _)| *price)
                .collect();
            let down_prices: Vec<f64> = parsed_fills.iter()
                .filter(|(_, outcome, _, _)| outcome.to_uppercase().contains("DOWN"))
                .map(|(_, _, price, _)| *price)
                .collect();
            
            if !up_prices.is_empty() && !down_prices.is_empty() {
                let up_min = up_prices.iter().fold(f64::INFINITY, |a, &b| a.min(b));
                let up_max = up_prices.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
                let down_min = down_prices.iter().fold(f64::INFINITY, |a, &b| a.min(b));
                let down_max = down_prices.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
                
                insights.push(format!(
                    "Up token price range: ${:.2} - ${:.2}, Down token price range: ${:.2} - ${:.2}",
                    up_min, up_max, down_min, down_max
                ));
            }
            
            // Insight: Trading frequency
            if let Some(trades_per_min) = timeline.trades_per_minute {
                if trades_per_min > 5.0 {
                    insights.push(format!("Very active trading: {:.1} trades per minute", trades_per_min));
                } else if trades_per_min > 1.0 {
                    insights.push(format!("Active trading: {:.1} trades per minute", trades_per_min));
                } else {
                    insights.push(format!("Moderate trading: {:.1} trades per minute", trades_per_min));
                }
            }
            
            // Insight: Position sizing
            if let Some(common_size) = sizing.common_sizes.first() {
                insights.push(format!(
                    "Most common position size: {} shares (used {} times)",
                    common_size.0, common_size.1
                ));
            }
            
            // Insight: Momentum
            if momentum.up_buy_when_rising > momentum.up_buy_when_rising / 2 {
                insights.push("Tends to buy Up tokens when price is rising (momentum following)".to_string());
            }
            if momentum.down_buy_when_rising > momentum.down_buy_when_rising / 2 {
                insights.push("Tends to buy Down tokens when price is rising (momentum following)".to_string());
            }
        }
        
        DetailedAnalysis {
            timeline,
            momentum,
            sizing,
            correlation,
            insights,
        }
    }
    
    fn analyze_timeline(fills: &[(u64, String, f64, f64)], period_timestamp: Option<u64>) -> TimelineAnalysis {
        if fills.is_empty() {
            return TimelineAnalysis {
                total_trades: 0,
                period_start_timestamp: period_timestamp,
                period_end_timestamp: None,
                first_trade_timestamp: None,
                last_trade_timestamp: None,
                trades_per_minute: None,
                active_trading_periods: Vec::new(),
            };
        }
        
        let first_ts = fills.first().map(|(ts, _, _, _)| *ts);
        let last_ts = fills.last().map(|(ts, _, _, _)| *ts);
        
        // Calculate period end (assuming 1-hour market = 3600 seconds)
        let period_end = period_timestamp.map(|start| start + 3600);
        
        // Calculate trades per minute
        let trades_per_minute = if let (Some(first), Some(last)) = (first_ts, last_ts) {
            let duration_minutes = if last > first {
                ((last - first) as f64) / 60.0
            } else {
                1.0
            };
            Some(fills.len() as f64 / duration_minutes.max(1.0))
        } else {
            None
        };
        
        // Analyze active trading periods (divide into 5-minute intervals)
        let mut period_counts: HashMap<String, usize> = HashMap::new();
        if let Some(period_start) = period_timestamp {
            for (ts, _, _, _) in fills {
                let elapsed = *ts - period_start;
                let minute = (elapsed / 60) as usize;
                let period = format!("{}-{}min", (minute / 5) * 5, ((minute / 5) + 1) * 5);
                *period_counts.entry(period).or_insert(0) += 1;
            }
        }
        
        let mut active_periods: Vec<(String, usize)> = period_counts.into_iter().collect();
        active_periods.sort_by_key(|(_, count)| *count);
        active_periods.reverse();
        let active_trading_periods: Vec<String> = active_periods.iter()
            .take(5)
            .map(|(period, count)| format!("{} ({} trades)", period, count))
            .collect();
        
        TimelineAnalysis {
            total_trades: fills.len(),
            period_start_timestamp: period_timestamp,
            period_end_timestamp: period_end,
            first_trade_timestamp: first_ts,
            last_trade_timestamp: last_ts,
            trades_per_minute,
            active_trading_periods,
        }
    }
    
    fn analyze_momentum(fills: &[(u64, String, f64, f64)]) -> MomentumAnalysis {
        let mut up_prices: Vec<(u64, f64)> = Vec::new();
        let mut down_prices: Vec<(u64, f64)> = Vec::new();
        
        for (ts, outcome, price, _) in fills {
            if outcome.to_uppercase().contains("UP") {
                up_prices.push((*ts, *price));
            } else if outcome.to_uppercase().contains("DOWN") {
                down_prices.push((*ts, *price));
            }
        }
        
        // Analyze price trends
        let up_trend = Self::calculate_trend(&up_prices);
        let down_trend = Self::calculate_trend(&down_prices);
        
        // Count buys when price was rising (simplified: compare with previous price)
        let mut up_buy_when_rising = 0;
        let mut down_buy_when_rising = 0;
        
        for i in 1..up_prices.len() {
            if up_prices[i].1 > up_prices[i-1].1 {
                up_buy_when_rising += 1;
            }
        }
        
        for i in 1..down_prices.len() {
            if down_prices[i].1 > down_prices[i-1].1 {
                down_buy_when_rising += 1;
            }
        }
        
        MomentumAnalysis {
            up_token_price_trend: up_trend,
            down_token_price_trend: down_trend,
            up_buy_when_rising,
            down_buy_when_rising,
            average_price_change_before_buy: None, // Could be enhanced
        }
    }
    
    fn calculate_trend(prices: &[(u64, f64)]) -> String {
        if prices.len() < 2 {
            return "INSUFFICIENT_DATA".to_string();
        }
        
        let first_half_avg = prices.iter().take(prices.len() / 2).map(|(_, p)| p).sum::<f64>() / (prices.len() / 2) as f64;
        let second_half_avg = prices.iter().skip(prices.len() / 2).map(|(_, p)| p).sum::<f64>() / (prices.len() - prices.len() / 2) as f64;
        
        if second_half_avg > first_half_avg * 1.05 {
            "RISING".to_string()
        } else if second_half_avg < first_half_avg * 0.95 {
            "FALLING".to_string()
        } else {
            "MIXED".to_string()
        }
    }
    
    fn analyze_sizing(fills: &[(u64, String, f64, f64)]) -> SizingAnalysis {
        let mut size_counts: HashMap<String, usize> = HashMap::new();
        let mut up_sizes: Vec<f64> = Vec::new();
        let mut down_sizes: Vec<f64> = Vec::new();
        let mut all_sizes: Vec<f64> = Vec::new();
        
        for (_, outcome, _, size) in fills {
            all_sizes.push(*size);
            
            // Round size to nearest integer for common size detection
            let rounded_size = size.round() as i64;
            let size_key = rounded_size.to_string();
            *size_counts.entry(size_key).or_insert(0) += 1;
            
            if outcome.to_uppercase().contains("UP") {
                up_sizes.push(*size);
            } else if outcome.to_uppercase().contains("DOWN") {
                down_sizes.push(*size);
            }
        }
        
        // Get most common sizes
        let mut common_sizes: Vec<(String, usize)> = size_counts.into_iter().collect();
        common_sizes.sort_by_key(|(_, count)| *count);
        common_sizes.reverse();
        let common_sizes: Vec<(String, usize)> = common_sizes.into_iter().take(5).collect();
        
        // Calculate averages
        let average_size_up = if !up_sizes.is_empty() {
            Some(up_sizes.iter().sum::<f64>() / up_sizes.len() as f64)
        } else {
            None
        };
        
        let average_size_down = if !down_sizes.is_empty() {
            Some(down_sizes.iter().sum::<f64>() / down_sizes.len() as f64)
        } else {
            None
        };
        
        let max_size = all_sizes.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        let min_size = all_sizes.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        
        let mut size_patterns = Vec::new();
        if let Some(common) = common_sizes.first() {
            if common.1 > fills.len() / 10 {
                size_patterns.push(format!("Frequently uses {} shares", common.0));
            }
        }
        
        SizingAnalysis {
            common_sizes,
            average_size_up,
            average_size_down,
            max_size: if all_sizes.is_empty() { None } else { Some(max_size) },
            min_size: if all_sizes.is_empty() { None } else { Some(min_size) },
            size_patterns,
        }
    }
    
    fn analyze_correlation(fills: &[(u64, String, f64, f64)]) -> CorrelationAnalysis {
        // Group fills by timestamp (within 2 seconds = same trade)
        let mut timestamp_groups: HashMap<u64, Vec<&(u64, String, f64, f64)>> = HashMap::new();
        for fill in fills {
            let rounded_ts = (fill.0 / 2) * 2; // Round to nearest 2 seconds
            timestamp_groups.entry(rounded_ts).or_insert_with(Vec::new).push(fill);
        }
        
        let mut simultaneous_buys = 0;
        let mut up_count = 0;
        let mut down_count = 0;
        let mut price_gaps: Vec<f64> = Vec::new();
        
        for group in timestamp_groups.values() {
            let has_up = group.iter().any(|(_, outcome, _, _)| outcome.to_uppercase().contains("UP"));
            let has_down = group.iter().any(|(_, outcome, _, _)| outcome.to_uppercase().contains("DOWN"));
            
            if has_up && has_down {
                simultaneous_buys += 1;
                
                // Calculate price gap
                let up_price = group.iter()
                    .find(|(_, outcome, _, _)| outcome.to_uppercase().contains("UP"))
                    .map(|(_, _, price, _)| *price);
                let down_price = group.iter()
                    .find(|(_, outcome, _, _)| outcome.to_uppercase().contains("DOWN"))
                    .map(|(_, _, price, _)| *price);
                
                if let (Some(up), Some(down)) = (up_price, down_price) {
                    price_gaps.push(up + down);
                }
            }
            
            if has_up { up_count += 1; }
            if has_down { down_count += 1; }
        }
        
        let up_down_ratio = if down_count > 0 {
            up_count as f64 / down_count as f64
        } else {
            0.0
        };
        
        let mut correlation_notes = Vec::new();
        if simultaneous_buys > 0 {
            correlation_notes.push(format!(
                "Buys both tokens simultaneously {} times (within 2 seconds)",
                simultaneous_buys
            ));
        } else {
            correlation_notes.push("Buys Up and Down tokens separately (not simultaneously)".to_string());
        }
        
        if up_down_ratio > 1.2 {
            correlation_notes.push(format!("Prefers Up tokens ({}:1 ratio)", up_down_ratio));
        } else if up_down_ratio < 0.8 {
            correlation_notes.push(format!("Prefers Down tokens (1:{} ratio)", 1.0 / up_down_ratio));
        } else {
            correlation_notes.push("Balanced Up/Down token trading".to_string());
        }
        
        CorrelationAnalysis {
            simultaneous_up_down_buys: simultaneous_buys,
            up_down_ratio,
            price_gap_when_buying_both: price_gaps,
            correlation_notes,
        }
    }
    
    /// Save research data to research.toml
    pub fn save_to_file(&self, file_path: &str) -> Result<()> {
        let toml_string = toml::to_string_pretty(self)
            .context("Failed to serialize research data to TOML")?;
        
        fs::write(file_path, toml_string)
            .context(format!("Failed to write research data to {}", file_path))?;
        
        eprintln!("✅ Saved research data to {}", file_path);
        Ok(())
    }
    
    /// Load research data from research.toml
    pub fn load_from_file(file_path: &str) -> Result<Self> {
        let content = fs::read_to_string(file_path)
            .context(format!("Failed to read research file: {}", file_path))?;
        
        let data: ResearchData = toml::from_str(&content)
            .context("Failed to parse research data from TOML")?;
        
        Ok(data)
    }
}

/// Fetch and save target wallet's trade history for a specific market
pub async fn fetch_and_save_trade_history(
    api: &PolymarketApi,
    target_address: &str,
    condition_id: &str,
    output_file: &str,
) -> Result<()> {
    eprintln!("🔍 Fetching trade history for target wallet...");
    eprintln!("   Address: {}", target_address);
    eprintln!("   Condition ID: {}", condition_id);
    
    // Fetch market details to get slug and question
    let market = api.get_market(condition_id).await
        .context(format!("Failed to fetch market for condition_id: {}", condition_id))?;
    
    eprintln!("   Market: {}", market.question);
    eprintln!("   Slug: {}", market.market_slug);
    
    // Extract period timestamp from slug if it's a 15m or 1h market
    let period_timestamp = extract_period_timestamp(&market.market_slug);
    
    // Fetch fills for this market
    let fills = api.get_user_fills_for_market(target_address, condition_id, Some(1000)).await
        .context("Failed to fetch user fills for market")?;
    
    eprintln!("   Found {} fills", fills.len());
    
    // Create research data
    let research_data = ResearchData::from_fills(
        target_address.to_string(),
        condition_id.to_string(),
        fills,
        Some(market.market_slug),
        Some(market.question),
        period_timestamp,
    );
    
    // Print analysis summary
    if let Some(analysis) = &research_data.analysis {
        eprintln!("\n📊 Analysis Summary:");
        eprintln!("   Total Buys: {}", analysis.total_buys);
        eprintln!("   Total Sells: {}", analysis.total_sells);
        eprintln!("   Buy Up: {}, Buy Down: {}", analysis.buy_up_count, analysis.buy_down_count);
        eprintln!("   Sell Up: {}, Sell Down: {}", analysis.sell_up_count, analysis.sell_down_count);
        if let Some((min, max)) = &analysis.price_range_up {
            eprintln!("   Up Price Range: ${} - ${}", min, max);
        }
        if let Some((min, max)) = &analysis.price_range_down {
            eprintln!("   Down Price Range: ${} - ${}", min, max);
        }
        if !analysis.notes.is_empty() {
            eprintln!("   Notes:");
            for note in &analysis.notes {
                eprintln!("     - {}", note);
            }
        }
    }
    
    // Print detailed analysis
    if let Some(detailed) = &research_data.detailed_analysis {
        eprintln!("\n🔬 Detailed Analysis:");
        eprintln!("   Timeline:");
        eprintln!("     - Total Trades: {}", detailed.timeline.total_trades);
        if let Some(tpm) = detailed.timeline.trades_per_minute {
            eprintln!("     - Trades per Minute: {:.2}", tpm);
        }
        if !detailed.timeline.active_trading_periods.is_empty() {
            eprintln!("     - Most Active Periods:");
            for period in &detailed.timeline.active_trading_periods {
                eprintln!("       * {}", period);
            }
        }
        
        eprintln!("   Momentum:");
        eprintln!("     - Up Token Trend: {}", detailed.momentum.up_token_price_trend);
        eprintln!("     - Down Token Trend: {}", detailed.momentum.down_token_price_trend);
        eprintln!("     - Up Buys When Rising: {}", detailed.momentum.up_buy_when_rising);
        eprintln!("     - Down Buys When Rising: {}", detailed.momentum.down_buy_when_rising);
        
        eprintln!("   Sizing:");
        if let Some(avg_up) = detailed.sizing.average_size_up {
            eprintln!("     - Average Up Size: {:.2} shares", avg_up);
        }
        if let Some(avg_down) = detailed.sizing.average_size_down {
            eprintln!("     - Average Down Size: {:.2} shares", avg_down);
        }
        if !detailed.sizing.common_sizes.is_empty() {
            eprintln!("     - Most Common Sizes:");
            for (size, count) in &detailed.sizing.common_sizes {
                eprintln!("       * {} shares: {} times", size, count);
            }
        }
        
        eprintln!("   Correlation:");
        eprintln!("     - Simultaneous Up/Down Buys: {}", detailed.correlation.simultaneous_up_down_buys);
        eprintln!("     - Up/Down Ratio: {:.2}", detailed.correlation.up_down_ratio);
        
        eprintln!("   Key Insights:");
        for insight in &detailed.insights {
            eprintln!("     - {}", insight);
        }
    }
    
    // Save to file
    research_data.save_to_file(output_file)?;
    
    Ok(())
}

/// Extract period timestamp from market slug
/// For 15m markets: eth-updown-15m-{timestamp} or btc-updown-15m-{timestamp}
/// For 1h markets: ethereum-up-or-down-{date-time} or bitcoin-up-or-down-{date-time}
fn extract_period_timestamp(slug: &str) -> Option<u64> {
    // Try to extract from 15m format: {asset}-updown-15m-{timestamp}
    if let Some(timestamp_str) = slug.split("-updown-15m-").nth(1) {
        if let Ok(timestamp) = timestamp_str.parse::<u64>() {
            return Some(timestamp);
        }
    }
    
    // Try to extract from 1h format: {asset}-up-or-down-{date-time}
    // This is more complex, would need date parsing
    // For now, return None for 1h markets
    None
}
