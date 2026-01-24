use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trend {
    Rising,
    Falling,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Up,
    Down,
    Uncertain,
}

/// Tracks price history and detects trends
pub struct PriceMonitor {
    prices: VecDeque<(u64, f64)>, // (timestamp, price)
    max_size: usize,
}

impl PriceMonitor {
    pub fn new(max_size: usize) -> Self {
        Self {
            prices: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    pub fn add_price(&mut self, price: f64, timestamp: u64) {
        self.prices.push_back((timestamp, price));
        if self.prices.len() > self.max_size {
            self.prices.pop_front();
        }
    }

    pub fn get_current_price(&self) -> Option<f64> {
        self.prices.back().map(|(_, p)| *p)
    }

    /// Calculate moving average over last N prices
    pub fn moving_average(&self, window: usize) -> Option<f64> {
        if self.prices.len() < window {
            return None;
        }
        let recent: Vec<f64> = self.prices.iter()
            .rev()
            .take(window)
            .map(|(_, p)| *p)
            .collect();
        Some(recent.iter().sum::<f64>() / recent.len() as f64)
    }

    /// Detect trend based on price movements
    /// Returns Rising if 70%+ of recent changes are positive
    /// Returns Falling if 70%+ of recent changes are negative
    /// Returns Neutral otherwise
    pub fn detect_trend(&self, window: usize) -> Option<Trend> {
        // Need at least 2 prices to calculate changes
        if self.prices.len() < 2 {
            return None;
        }
        
        // If we have fewer prices than window, use what we have
        let actual_window = window.min(self.prices.len());

        let recent: Vec<f64> = self.prices.iter()
            .rev()
            .take(window)
            .map(|(_, p)| *p)
            .collect();

        let changes: Vec<f64> = recent.windows(2)
            .map(|w| w[1] - w[0])
            .collect();

        if changes.is_empty() {
            return None;
        }

        // Filter noise: only consider changes > threshold (default 2%)
        // This will be configurable in the future
        let noise_threshold = 0.02; // TODO: Make configurable
        let significant_changes: Vec<f64> = changes.iter()
            .filter(|&&c| c.abs() > noise_threshold)
            .copied()
            .collect();

        // Trend threshold: 70% of changes must be in same direction
        // But allow detection with fewer changes if we have limited data
        let trend_threshold = 0.7; // TODO: Make configurable
        let min_changes = if significant_changes.len() >= 3 { 3 } else { 2 };
        if significant_changes.len() >= min_changes {
            let rising = significant_changes.iter().filter(|&&c| c > 0.0).count();
            let falling = significant_changes.iter().filter(|&&c| c < 0.0).count();
            let total = significant_changes.len();

            if rising as f64 / total as f64 > trend_threshold {
                return Some(Trend::Rising);
            } else if falling as f64 / total as f64 > trend_threshold {
                return Some(Trend::Falling);
            }
        }

        // Fallback: use all changes (including noise)
        // But only if we have at least 2 changes
        if changes.len() >= 2 {
            let rising = changes.iter().filter(|&&c| c > 0.0).count();
            let falling = changes.iter().filter(|&&c| c < 0.0).count();
            let total = changes.len();

            let trend_threshold = 0.6; // Lower threshold for fallback (60% instead of 70%)
            if rising as f64 / total as f64 > trend_threshold {
                Some(Trend::Rising)
            } else if falling as f64 / total as f64 > trend_threshold {
                Some(Trend::Falling)
            } else {
                Some(Trend::Neutral)
            }
        } else {
            Some(Trend::Neutral)
        }
    }

    /// Detect turning point (reversal pattern)
    /// Pattern: up, up, down or down, down, up
    pub fn detect_turning_point(&self) -> bool {
        if self.prices.len() < 4 {
            return false;
        }

        let recent: Vec<f64> = self.prices.iter()
            .rev()
            .take(4)
            .map(|(_, p)| *p)
            .collect();

        let changes = vec![
            recent[0] - recent[1],
            recent[1] - recent[2],
            recent[2] - recent[3],
        ];

        // Pattern: positive, positive, negative (reversal from up to down)
        if changes[0] > 0.0 && changes[1] > 0.0 && changes[2] < -0.02 {
            return true;
        }

        // Pattern: negative, negative, positive (reversal from down to up)
        if changes[0] < 0.0 && changes[1] < 0.0 && changes[2] > 0.02 {
            return true;
        }

        false
    }

    pub fn len(&self) -> usize {
        self.prices.len()
    }
}

/// Detects market direction based on Up and Down token trends
pub struct DirectionDetector {
    up_monitor: PriceMonitor,
    down_monitor: PriceMonitor,
    monitoring_start: Option<u64>,
    monitoring_duration: u64, // seconds
    last_trade_time: Option<u64>,
}

#[derive(Debug)]
pub enum TradingState {
    Monitoring { start_time: u64 },
    Trading { direction: Direction, token: String },
    WaitingForConfirmation { potential_direction: Direction },
}

impl DirectionDetector {
    pub fn new(monitoring_duration: u64) -> Self {
        Self {
            up_monitor: PriceMonitor::new(20),
            down_monitor: PriceMonitor::new(20),
            monitoring_start: Some(Self::current_timestamp()),
            monitoring_duration,
            last_trade_time: None,
        }
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub fn add_price(&mut self, outcome: &str, price: f64, timestamp: u64) {
        match outcome {
            "Up" => self.up_monitor.add_price(price, timestamp),
            "Down" => self.down_monitor.add_price(price, timestamp),
            _ => {}
        }
    }

    pub fn should_enter_monitoring(&self, time_since_last_trade: u64) -> bool {
        // Long gap (30+ seconds) = monitoring period
        if time_since_last_trade > 30 {
            return true;
        }

        // Check if trends are uncertain
        let up_trend = self.up_monitor.detect_trend(5);
        let down_trend = self.down_monitor.detect_trend(5);

        if up_trend == Some(Trend::Neutral) || down_trend == Some(Trend::Neutral) {
            return true;
        }

        // Mixed signals
        if up_trend == Some(Trend::Rising) && down_trend == Some(Trend::Rising) {
            return true; // Both rising? Need to monitor
        }

        false
    }

    pub fn detect_direction(&self) -> Option<Direction> {
        // Need at least 3 prices to detect trend (reduced from 5 for faster detection)
        let up_trend = self.up_monitor.detect_trend(3);
        let down_trend = self.down_monitor.detect_trend(3);
        
        // If we don't have enough data yet, return None
        if up_trend.is_none() || down_trend.is_none() {
            return None;
        }
        
        let up_trend = up_trend.unwrap();
        let down_trend = down_trend.unwrap();

        match (up_trend, down_trend) {
            (Trend::Rising, Trend::Falling) => Some(Direction::Up),
            (Trend::Falling, Trend::Rising) => Some(Direction::Down),
            // Fallback: if one is clearly rising and other is neutral, go with rising one
            (Trend::Rising, Trend::Neutral) => Some(Direction::Up),
            (Trend::Neutral, Trend::Rising) => Some(Direction::Down),
            // Fallback: if one is clearly falling and other is neutral, go with opposite
            (Trend::Falling, Trend::Neutral) => Some(Direction::Up), // If Down falling, buy Up
            (Trend::Neutral, Trend::Falling) => Some(Direction::Down), // If Up falling, buy Down
            _ => None, // Uncertain
        }
    }

    pub fn update_state(&mut self, current_time: u64) -> TradingState {
        let time_since_last_trade = self.last_trade_time
            .map(|t| current_time.saturating_sub(t))
            .unwrap_or(u64::MAX);

        // Check if we should enter monitoring
        if self.should_enter_monitoring(time_since_last_trade) {
            if self.monitoring_start.is_none() {
                self.monitoring_start = Some(current_time);
            }
            let elapsed = current_time - self.monitoring_start.unwrap();
            
            if elapsed < self.monitoring_duration {
                return TradingState::Monitoring { start_time: self.monitoring_start.unwrap() };
            }
        }

        // Monitoring period complete, analyze and decide
        if let Some(direction) = self.detect_direction() {
            self.monitoring_start = None;
            let token = match direction {
                Direction::Up => "Up",
                Direction::Down => "Down",
                Direction::Uncertain => return TradingState::Monitoring { start_time: current_time },
            };
            TradingState::Trading {
                direction,
                token: token.to_string(),
            }
        } else {
            // Still uncertain, continue monitoring
            TradingState::Monitoring {
                start_time: self.monitoring_start.unwrap_or(current_time),
            }
        }
    }

    pub fn record_trade(&mut self, timestamp: u64) {
        self.last_trade_time = Some(timestamp);
        self.monitoring_start = None; // Reset monitoring after trade
    }

    pub fn check_turning_point(&self) -> bool {
        self.up_monitor.detect_turning_point() || self.down_monitor.detect_turning_point()
    }

    pub fn get_up_price(&self) -> Option<f64> {
        self.up_monitor.get_current_price()
    }

    pub fn get_down_price(&self) -> Option<f64> {
        self.down_monitor.get_current_price()
    }
}
