use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "config.json")]
    pub config: PathBuf,

    /// Research mode: Fetch trade history for a target wallet
    #[arg(long)]
    pub research: bool,

    /// Target wallet address for research (required if --research is used)
    #[arg(long)]
    pub target_address: Option<String>,

    /// Condition ID for research (required if --research is used)
    #[arg(long)]
    pub condition_id: Option<String>,

    /// Output file for research data (default: research.toml)
    #[arg(long, default_value = "research.toml")]
    pub research_output: String,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub polymarket: PolymarketConfig,
    pub monitoring: MonitoringConfig,
    /// Research configuration for analyzing target wallet trades
    #[serde(default)]
    pub research: Option<ResearchConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketConfig {
    pub gamma_api_url: String,
    pub clob_api_url: String,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    pub api_passphrase: Option<String>,
    /// Private key for signing orders (optional, but may be required for order placement)
    /// Format: hex string (with or without 0x prefix) or raw private key
    pub private_key: Option<String>,
    /// Proxy wallet address (Polymarket proxy wallet address where your balance is)
    /// If set, the bot will trade using this proxy wallet instead of the EOA (private key account)
    /// Format: Ethereum address (with or without 0x prefix)
    pub proxy_wallet_address: Option<String>,
    /// Signature type for authentication (optional, defaults to EOA if not set)
    /// 0 = EOA (Externally Owned Account - private key account)
    /// 1 = Proxy (Polymarket proxy wallet)
    /// 2 = GnosisSafe (Gnosis Safe wallet)
    /// If proxy_wallet_address is set, this should be 1 (Proxy)
    pub signature_type: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    /// Interval between market data fetches (in milliseconds)
    /// Default: 2000 (2 seconds)
    pub check_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchConfig {
    /// Target wallet address to analyze (Ethereum address)
    pub target_address: Option<String>,
    /// Condition ID for the market to analyze (can be set per research run)
    pub condition_id: Option<String>,
    /// Default output file for research data
    #[serde(default = "default_research_output")]
    pub output_file: String,
}

fn default_research_output() -> String {
    "research.toml".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            polymarket: PolymarketConfig {
                gamma_api_url: "https://gamma-api.polymarket.com".to_string(),
                clob_api_url: "https://clob.polymarket.com".to_string(),
                api_key: None,
                api_secret: None,
                api_passphrase: None,
                private_key: None,
                proxy_wallet_address: None,
                signature_type: None,
            },
            monitoring: MonitoringConfig {
                check_interval_ms: 2000,
            },
            research: None, // No default research config
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            let config = Config::default();
            let content = serde_json::to_string_pretty(&config)?;
            std::fs::write(path, content)?;
            Ok(config)
        }
    }
}

