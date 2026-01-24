use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Position {
    pub outcome: String, // "Up" or "Down"
    pub total_shares: f64,
    pub total_cost: f64,
    pub avg_price: f64,
    pub entry_prices: Vec<(f64, f64)>, // (shares, price) for each entry
}

impl Position {
    pub fn new(outcome: String) -> Self {
        Self {
            outcome,
            total_shares: 0.0,
            total_cost: 0.0,
            avg_price: 0.0,
            entry_prices: Vec::new(),
        }
    }

    pub fn add(&mut self, shares: f64, price: f64) {
        self.total_shares += shares;
        self.total_cost += shares * price;
        self.avg_price = if self.total_shares > 0.0 {
            self.total_cost / self.total_shares
        } else {
            0.0
        };
        self.entry_prices.push((shares, price));
    }

    pub fn remove(&mut self, shares: f64, price: f64) {
        if self.total_shares > 0.0 {
            // Assume selling at average cost basis (FIFO would be more accurate but this is simpler)
            let sold_cost = shares * self.avg_price;
            self.total_shares -= shares;
            self.total_cost -= sold_cost;
            if self.total_shares > 0.0 {
                self.avg_price = self.total_cost / self.total_shares;
            } else {
                self.avg_price = 0.0;
                self.total_cost = 0.0;
            }
        }
    }
}

/// Tracks positions and calculates P&L for both outcomes
pub struct PositionTracker {
    up_position: Position,
    down_position: Position,
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            up_position: Position::new("Up".to_string()),
            down_position: Position::new("Down".to_string()),
        }
    }

    pub fn add_position(&mut self, outcome: &str, shares: f64, price: f64) {
        match outcome {
            "Up" => self.up_position.add(shares, price),
            "Down" => self.down_position.add(shares, price),
            _ => {}
        }
    }

    pub fn remove_position(&mut self, outcome: &str, shares: f64, price: f64) {
        match outcome {
            "Up" => self.up_position.remove(shares, price),
            "Down" => self.down_position.remove(shares, price),
            _ => {}
        }
    }

    /// Calculate P&L if market resolves as "Up"
    /// Up tokens worth $1.00, Down tokens worth $0.00
    pub fn calculate_pnl_if_up(&self) -> f64 {
        let up_value = 1.0 * self.up_position.total_shares;
        let up_cost = self.up_position.total_cost;
        let down_cost = self.down_position.total_cost; // Down tokens worth $0
        up_value - up_cost - down_cost
    }

    /// Calculate P&L if market resolves as "Down"
    /// Down tokens worth $1.00, Up tokens worth $0.00
    pub fn calculate_pnl_if_down(&self) -> f64 {
        let down_value = 1.0 * self.down_position.total_shares;
        let down_cost = self.down_position.total_cost;
        let up_cost = self.up_position.total_cost; // Up tokens worth $0
        down_value - down_cost - up_cost
    }

    pub fn get_up_position(&self) -> &Position {
        &self.up_position
    }

    pub fn get_down_position(&self) -> &Position {
        &self.down_position
    }

    pub fn get_net_exposure(&self) -> f64 {
        self.up_position.total_shares - self.down_position.total_shares
    }

    pub fn get_total_investment(&self) -> f64 {
        self.up_position.total_cost + self.down_position.total_cost
    }
}

impl Default for PositionTracker {
    fn default() -> Self {
        Self::new()
    }
}
