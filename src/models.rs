use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    #[serde(rename = "id")]
    pub market_id: Option<String>, // Market ID (numeric string)
    pub question: String,
    pub slug: String,
    #[serde(rename = "resolutionSource")]
    pub resolution_source: Option<String>,
    #[serde(rename = "endDateISO")]
    pub end_date_iso: Option<String>,
    #[serde(rename = "endDateIso")]
    pub end_date_iso_alt: Option<String>,
    pub active: bool,
    pub closed: bool,
    pub tokens: Option<Vec<Token>>,
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>, // JSON string array
    pub outcomes: Option<String>, // JSON string array like "[\"Up\", \"Down\"]"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(rename = "tokenId")]
    pub token_id: String,
    pub outcome: String,
    pub price: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub bids: Vec<OrderBookEntry>,
    pub asks: Vec<OrderBookEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookEntry {
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone)]
pub struct TokenPrice {
    pub token_id: String,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
}

impl TokenPrice {
    pub fn mid_price(&self) -> Option<Decimal> {
        match (self.bid, self.ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::from(2)),
            (Some(bid), None) => Some(bid),
            (None, Some(ask)) => Some(ask),
            (None, None) => None,
        }
    }

    pub fn ask_price(&self) -> Decimal {
        self.ask.unwrap_or(Decimal::ZERO)
    }
}

/// Order structure for creating orders (before signing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub token_id: String,
    pub side: String, // "BUY" or "SELL"
    pub size: String,
    pub price: String,
    #[serde(rename = "type")]
    pub order_type: String, // "LIMIT" or "MARKET"
}

/// Signed order structure for posting to Polymarket
/// According to Polymarket docs, orders must be signed with private key before posting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedOrder {
    // Order fields
    #[serde(rename = "tokenID")]
    pub token_id: String,
    pub side: String, // "BUY" or "SELL"
    pub size: String,
    pub price: String,
    #[serde(rename = "type")]
    pub order_type: String, // "LIMIT" or "MARKET"
    
    // Signature fields (will be populated when signing)
    pub signature: Option<String>,
    pub signer: Option<String>, // Address derived from private key
    pub nonce: Option<u64>,
    pub expiration: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    pub order_id: Option<String>,
    pub status: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub balance: String,
    pub allowance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedeemResponse {
    pub success: bool,
    pub message: Option<String>,
    pub transaction_hash: Option<String>,
    pub amount_redeemed: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MarketData {
    pub condition_id: String,
    pub market_name: String,
    pub up_token: Option<TokenPrice>,
    pub down_token: Option<TokenPrice>,
}

/// Trade for D-based strategy (buy ETH Down when ETH higher token hits $0.98)
#[derive(Debug, Clone)]
pub struct PendingTrade {
    pub token_id: String,              // ETH Down token ID
    pub condition_id: String,          // ETH condition ID
    pub investment_amount: f64,        // Fixed trade amount
    pub total_units: f64,              // Total shares purchased
    pub remaining_units: f64,          // Remaining shares to sell
    pub purchase_price: f64,           // Price at which token was purchased (~$0.01)
    pub difference_d: f64,             // D value when trade was made
    pub timestamp: std::time::Instant, // When the trade was executed
    pub market_timestamp: u64,         // The 15-minute period timestamp
    pub sell_points: Vec<f64>,         // Price points to sell at (e.g., [0.02, 0.04, 0.08])
    pub sell_percentages: Vec<f64>,    // Percentage to sell at each point (e.g., [0.5, 0.5, 1.0])
    pub next_sell_index: usize,        // Index of next sell point
}

/// Trade for arbitrage strategy (buy cheaper token, sell progressively)
#[derive(Debug, Clone)]
/// Market-neutral trade: buy both Up and Down tokens simultaneously
pub struct ArbitrageTrade {
    // Up token position
    pub up_token_id: String,
    pub up_token_name: String,
    pub up_investment_amount: f64,      // Amount invested in Up token
    pub up_total_units: f64,            // Total Up shares purchased
    pub up_remaining_units: f64,        // Remaining Up shares to sell
    pub up_purchase_price: f64,         // Price at which Up token was purchased (ASK)
    pub up_price_history: Vec<(std::time::Instant, f64)>, // Track recent BID prices for Up token
    
    // Down token position
    pub down_token_id: String,
    pub down_token_name: String,
    pub down_investment_amount: f64,    // Amount invested in Down token
    pub down_total_units: f64,          // Total Down shares purchased
    pub down_remaining_units: f64,      // Remaining Down shares to sell
    pub down_purchase_price: f64,       // Price at which Down token was purchased (ASK)
    pub down_price_history: Vec<(std::time::Instant, f64)>, // Track recent BID prices for Down token
    
    // Market info
    pub condition_id: String,           // Market condition ID
    pub price_gap: f64,                 // Price gap when purchased
    pub min_profit_target: f64,         // Minimum profit target (absolute price increase)
    pub timestamp: std::time::Instant,  // When the trade was executed
    pub market_timestamp: u64,          // The period timestamp (15m or 1h)
    pub market_duration: u64,           // Market duration in seconds (900 for 15m, 3600 for 1h)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketToken {
    pub outcome: String,
    pub price: rust_decimal::Decimal,
    #[serde(rename = "token_id")]
    pub token_id: String,
    pub winner: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDetails {
    #[serde(rename = "accepting_order_timestamp")]
    pub accepting_order_timestamp: Option<String>,
    #[serde(rename = "accepting_orders")]
    pub accepting_orders: bool,
    pub active: bool,
    pub archived: bool,
    pub closed: bool,
    #[serde(rename = "condition_id")]
    pub condition_id: String,
    pub description: String,
    #[serde(rename = "enable_order_book")]
    pub enable_order_book: bool,
    #[serde(rename = "end_date_iso")]
    pub end_date_iso: String,
    pub fpmm: String,
    #[serde(rename = "game_start_time")]
    pub game_start_time: Option<String>,
    pub icon: String,
    pub image: String,
    #[serde(rename = "is_50_50_outcome")]
    pub is_50_50_outcome: bool,
    #[serde(rename = "maker_base_fee")]
    pub maker_base_fee: rust_decimal::Decimal,
    #[serde(rename = "market_slug")]
    pub market_slug: String,
    #[serde(rename = "minimum_order_size")]
    pub minimum_order_size: rust_decimal::Decimal,
    #[serde(rename = "minimum_tick_size")]
    pub minimum_tick_size: rust_decimal::Decimal,
    #[serde(rename = "neg_risk")]
    pub neg_risk: bool,
    #[serde(rename = "neg_risk_market_id")]
    pub neg_risk_market_id: String,
    #[serde(rename = "neg_risk_request_id")]
    pub neg_risk_request_id: String,
    #[serde(rename = "notifications_enabled")]
    pub notifications_enabled: bool,
    pub question: String,
    #[serde(rename = "question_id")]
    pub question_id: String,
    pub rewards: Rewards,
    #[serde(rename = "seconds_delay")]
    pub seconds_delay: u32,
    pub tags: Vec<String>,
    #[serde(rename = "taker_base_fee")]
    pub taker_base_fee: rust_decimal::Decimal,
    pub tokens: Vec<MarketToken>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rewards {
    #[serde(rename = "max_spread")]
    pub max_spread: rust_decimal::Decimal,
    #[serde(rename = "min_size")]
    pub min_size: rust_decimal::Decimal,
    pub rates: Option<serde_json::Value>,
}

/// Fill/Trade structure from Polymarket Data API /activity endpoint
/// Documentation: https://docs.polymarket.com/developers/misc-endpoints/data-api-activity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    #[serde(rename = "id")]
    pub id: Option<String>,
    #[serde(rename = "tokenID")]
    pub token_id: Option<String>,
    #[serde(rename = "asset")]
    pub asset: Option<String>, // Asset ID (token ID) from Data API
    #[serde(rename = "tokenName")]
    pub token_name: Option<String>,
    pub side: String, // "BUY" or "SELL"
    #[serde(rename = "size")]
    pub size: f64, // Size as number (not string)
    #[serde(rename = "usdcSize")]
    pub usdc_size: Option<f64>, // USDC size from activity API
    #[serde(rename = "price")]
    pub price: f64, // Price as number (not string)
    #[serde(rename = "timestamp")]
    pub timestamp: u64, // Timestamp as number (not string)
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    #[serde(rename = "user")]
    pub user: Option<String>, // User address
    #[serde(rename = "proxyWallet")]
    pub proxy_wallet: Option<String>, // Proxy wallet from Data API
    #[serde(rename = "maker")]
    pub maker: Option<String>,
    #[serde(rename = "taker")]
    pub taker: Option<String>,
    #[serde(rename = "fee")]
    pub fee: Option<String>,
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>, // Condition ID from Data API
    #[serde(rename = "outcomeIndex")]
    pub outcome_index: Option<u32>, // Outcome index (0=Up, 1=Down)
    #[serde(rename = "outcome")]
    pub outcome: Option<String>, // "Up" or "Down"
    #[serde(rename = "type")]
    pub activity_type: Option<String>, // "TRADE", "REDEMPTION", etc.
    #[serde(rename = "transactionHash")]
    pub transaction_hash: Option<String>,
    #[serde(rename = "title")]
    pub title: Option<String>, // Market title
    #[serde(rename = "slug")]
    pub slug: Option<String>, // Market slug
}

impl Fill {
    /// Get the token ID, trying multiple fields
    pub fn get_token_id(&self) -> Option<&String> {
        self.token_id.as_ref()
            .or_else(|| self.asset.as_ref())
    }
    
    /// Get the user address, trying multiple fields
    pub fn get_user_address(&self) -> Option<&String> {
        self.user.as_ref()
            .or_else(|| self.proxy_wallet.as_ref())
            .or_else(|| self.maker.as_ref())
            .or_else(|| self.taker.as_ref())
    }
}

/// Response from fills endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillsResponse {
    pub fills: Option<Vec<Fill>>,
    #[serde(flatten)]
    pub other: serde_json::Value,
}
