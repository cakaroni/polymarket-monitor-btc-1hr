mod api;
mod config;
mod models;
mod monitor;
mod research;

use anyhow::{Context, Result};
use clap::Parser;
use config::{Args, Config};
use log::warn;
use std::sync::Arc;
use std::io::{self, Write};
use std::fs::{File, OpenOptions};
use std::sync::{Mutex, OnceLock};
use chrono::{TimeZone, Utc, Datelike, Timelike};

use api::PolymarketApi;
use monitor::MarketMonitor;

/// A writer that writes to both stderr (terminal) and a file
/// Wrapped in Arc<Mutex<>> for thread-safe access
struct DualWriter {
    stderr: io::Stderr,
    file: Mutex<File>,
}

impl Write for DualWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Write to stderr (terminal) - stderr is already thread-safe
        let _ = self.stderr.write_all(buf);
        let _ = self.stderr.flush();
        
        // Write to file (protected by Mutex for thread safety)
        let mut file = self.file.lock().unwrap();
        file.write_all(buf)?;
        file.flush()?;
        
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stderr.flush()?;
        let mut file = self.file.lock().unwrap();
        file.flush()?;
        Ok(())
    }
}

// Make DualWriter Send + Sync for use with env_logger
unsafe impl Send for DualWriter {}
unsafe impl Sync for DualWriter {}

/// Global file writer for eprintln! messages to be saved to history.toml
static HISTORY_FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// Initialize the global history file writer
fn init_history_file(file: File) {
    HISTORY_FILE.set(Mutex::new(file)).expect("History file already initialized");
}

/// Write a message to both stderr and history.toml (without timestamp/level prefix)
pub fn log_to_history(message: &str) {
    // Write to stderr
    eprint!("{}", message);
    let _ = io::stderr().flush();
    
    // Write to history file
    if let Some(file_mutex) = HISTORY_FILE.get() {
        if let Ok(mut file) = file_mutex.lock() {
            let _ = write!(file, "{}", message);
            let _ = file.flush();
        }
    }
}

/// Macro to log to both stderr and history.toml (like eprintln! but also saves to file)
#[macro_export]
macro_rules! log_println {
    ($($arg:tt)*) => {
        {
            let message = format!($($arg)*);
            $crate::log_to_history(&format!("{}\n", message));
        }
    };
}

#[tokio::main]
async fn main() -> Result<()> {
    // Open log file in append mode
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("history.toml")
        .context("Failed to open history.toml for logging")?;
    
    // Initialize global history file for eprintln! messages
    init_history_file(log_file.try_clone().context("Failed to clone history file")?);
    
    // Create dual writer
    let dual_writer = DualWriter {
        stderr: io::stderr(),
        file: Mutex::new(log_file),
    };
    
    // Initialize logger with dual writer
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(Box::new(dual_writer)))
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;

    // Handle research mode
    if args.research {
        eprintln!("🔬 Research Mode: Fetching trade history");
        
        // Get target address from CLI args or config
        let target_address = args.target_address
            .or_else(|| config.research.as_ref().and_then(|r| r.target_address.clone()))
            .ok_or_else(|| anyhow::anyhow!(
                "Target address is required. Set it via:\n  - --target-address <ADDRESS> (CLI)\n  - research.target_address in config.json"
            ))?;
        
        // Get condition ID from CLI args or config
        let condition_id = args.condition_id
            .or_else(|| config.research.as_ref().and_then(|r| r.condition_id.clone()))
            .ok_or_else(|| anyhow::anyhow!(
                "Condition ID is required. Set it via:\n  - --condition-id <ID> (CLI)\n  - research.condition_id in config.json"
            ))?;
        
        // Get output file from CLI args or config
        let output_file = if args.research_output != "research.toml" {
            // CLI arg was explicitly set
            args.research_output
        } else {
            // Use config default or fallback to "research.toml"
            config.research.as_ref()
                .map(|r| r.output_file.clone())
                .unwrap_or_else(|| "research.toml".to_string())
        };
        
        eprintln!("   Target Address: {}", target_address);
        eprintln!("   Condition ID: {}", condition_id);
        eprintln!("   Output File: {}", output_file);
        
        // Initialize API client (no auth needed for public data)
        let api = PolymarketApi::new(
            config.polymarket.gamma_api_url.clone(),
            config.polymarket.clob_api_url.clone(),
            config.polymarket.api_key.clone(),
            config.polymarket.api_secret.clone(),
            config.polymarket.api_passphrase.clone(),
            config.polymarket.private_key.clone(),
            config.polymarket.proxy_wallet_address.clone(),
            config.polymarket.signature_type,
        );
        
        // Fetch and save trade history
        research::fetch_and_save_trade_history(
            &api,
            &target_address,
            &condition_id,
            &output_file,
        ).await?;
        
        eprintln!("✅ Research complete! Data saved to: {}", output_file);
        return Ok(());
    }

    eprintln!("🚀 Starting Polymarket BTC 1hr Monitor");
    eprintln!("📝 Logs are being saved to: history.toml");

    // Initialize API client (no auth needed for monitoring)
    let api = Arc::new(PolymarketApi::new(
        config.polymarket.gamma_api_url.clone(),
        config.polymarket.clob_api_url.clone(),
        None, // No API credentials needed for monitoring
        None,
        None,
        None, // No private key needed
        None,
        None,
    ));

    // Discover BTC 1h market
    let btc_market_1h = discover_btc_1h_market(&api).await?;
    eprintln!("✅ Found BTC 1h market: {} ({})", btc_market_1h.slug, btc_market_1h.condition_id);

    // Initialize monitor (only BTC 1h)
    let monitor = MarketMonitor::new(
        api.clone(),
        btc_market_1h,
        config.monitoring.check_interval_ms,
    );
    let monitor_arc = Arc::new(monitor);

    // Start period reset task (for market discovery when new hour starts)
    let monitor_for_period_check = monitor_arc.clone();
    let api_for_period_check = api.clone();
    tokio::spawn(async move {
        let mut last_processed_hour: Option<u64> = None;
        loop {
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            
            // Check every hour (3600 seconds)
            let current_hour = (current_time / 3600) * 3600;
            
            if let Some(last_hour) = last_processed_hour {
                if current_hour == last_hour {
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    continue;
                }
            }
            
            eprintln!("🔄 New hour detected! Discovering new BTC 1h market...");
            last_processed_hour = Some(current_hour);
            
            match discover_btc_1h_market(&api_for_period_check).await {
                Ok(new_market) => {
                    if let Err(e) = monitor_for_period_check.update_market(new_market).await {
                        warn!("Failed to update market: {}", e);
                    }
                }
                Err(e) => {
                    warn!("Failed to discover new BTC 1h market: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                }
            }
        }
    });
    
    // Start monitoring (no trading callback)
    monitor_arc.start_monitoring().await;

    Ok(())
}

async fn discover_btc_1h_market(api: &PolymarketApi) -> Result<crate::models::Market> {
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    
    discover_market(api, "BTC", "bitcoin", 60, current_time).await
        .context("Failed to discover BTC 1h market")
}

async fn discover_market(
    api: &PolymarketApi,
    market_name: &str,
    full_asset_name: &str,
    _market_duration_minutes: u64,
    _current_time: u64,
) -> Result<crate::models::Market> {
    // 1-hour markets use format: bitcoin-up-or-down-{date-time}
    // Use slug-based approach with ET timezone
    // Polymarket uses ET (Eastern Time), not UTC
    use chrono_tz::America::New_York;
    
    // Helper function to convert UTC timestamp to ET date/time format: january-19-9am-et
    let timestamp_to_slug_date_et = |utc_ts: u64| -> String {
        // Convert UTC timestamp to ET (Eastern Time)
        let dt_utc = Utc.timestamp_opt(utc_ts as i64, 0).unwrap();
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
    
    // Get current ET time for slug construction
    let current_time_utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    
    // Try current hour in ET, and a few hours around it
    // Since future markets are also "active", try current hour and next few hours
    for hour_offset in 0..=5 {
        // hour_offset 0: current hour in ET
        // hour_offset 1-2: previous hours (in case we're at the start of an hour)
        // hour_offset 3-5: future hours (future markets are also "active")
        let try_time_utc = if hour_offset <= 2 {
            current_time_utc - (hour_offset * 3600) // Go back
        } else {
            current_time_utc + ((hour_offset - 2) * 3600) // Go forward
        };
        let date_time_str = timestamp_to_slug_date_et(try_time_utc);
        let slug = format!("{}-up-or-down-{}", full_asset_name, date_time_str);
        
        eprintln!("Trying {} 1h market by slug: {}", market_name, slug);
        match api.get_market_by_slug(&slug).await {
            Ok(market) => {
                eprintln!("Found market {}: active={}, closed={}, condition_id={}", market.slug, market.active, market.closed, market.condition_id);
                
                // For 1-hour markets, if active=true and closed=false, accept it
                if market.active && !market.closed {
                    eprintln!("Found {} 1h market by slug: {} | Condition ID: {}", market_name, market.slug, market.condition_id);
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
    
    anyhow::bail!("Could not find active {} 1h up/down market", market_name)
}

