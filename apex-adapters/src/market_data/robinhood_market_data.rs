use anyhow::{Context, Result};
use apex_core::{
    domain::models::*,
    ports::market_data::{AdapterHealth, MarketDataPort, TickStream},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

const ROBINHOOD_API_BASE: &str = "https://api.robinhood.com";

/// Robinhood market data adapter
///
/// Provides real-time and historical market data from Robinhood API.
/// Uses polling-based subscription (Robinhood does not provide public WebSocket feeds).
pub struct RobinhoodMarketDataAdapter {
    access_token: Arc<RwLock<Option<String>>>,
    client: Client,
    health: Arc<RwLock<AdapterHealth>>,
}

/// Robinhood quote response
#[derive(Debug, Deserialize)]
struct RobinhoodQuote {
    last_trade_price: String,
    #[serde(default)]
    bid_price: String,
    #[serde(default)]
    ask_price: String,
    #[serde(default)]
    previous_close: String,
}

/// Robinhood historical response
#[derive(Debug, Deserialize)]
struct RobinhoodHistoricals {
    #[serde(default)]
    historicals: Vec<RobinhoodHistorical>,
}

#[derive(Debug, Deserialize)]
struct RobinhoodHistorical {
    begins_at: String,
    open_price: String,
    high_price: String,
    low_price: String,
    close_price: String,
    volume: u64,
}

/// Robinhood fundamentals response
#[derive(Debug, Deserialize)]
struct RobinhoodFundamentals {
    #[serde(default)]
    open: Option<String>,
    #[serde(default)]
    high: Option<String>,
    #[serde(default)]
    low: Option<String>,
    #[serde(default)]
    volume: Option<String>,
}

impl RobinhoodMarketDataAdapter {
    /// Create a new Robinhood market data adapter
    pub fn new(access_token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            access_token: Arc::new(RwLock::new(access_token)),
            client,
            health: Arc::new(RwLock::new(AdapterHealth::Healthy)),
        })
    }

    /// Set access token
    pub fn set_access_token(&self, token: String) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = Some(token);
        info!("Robinhood market data access token updated");
    }

    /// Clear access token.
    pub fn clear_access_token(&self) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Robinhood market data access token cleared");
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.access_token.read().unwrap().is_some()
    }

    /// Get authorization header
    fn get_auth_header(&self) -> Result<String> {
        let token = self.access_token.read().unwrap();
        match token.as_ref() {
            Some(t) => Ok(format!("Bearer {}", t)),
            None => Err(anyhow::anyhow!("No access token available")),
        }
    }

    /// Map APEX timeframe to Robinhood interval and span
    fn timeframe_to_robinhood(&self, tf: &Timeframe) -> (&'static str, &'static str) {
        match tf {
            Timeframe::M5 => ("5minute", "day"),
            Timeframe::M15 => ("10minute", "week"), // Robinhood uses 10min, closest to 15
            Timeframe::H1 => ("hour", "month"),
            Timeframe::D1 => ("day", "year"),
            Timeframe::W1 => ("week", "5year"),
            _ => ("5minute", "day"), // default
        }
    }

    /// Update health status
    fn set_health(&self, health: AdapterHealth) {
        let mut h = self.health.write().unwrap();
        *h = health;
    }
}

#[async_trait]
impl MarketDataPort for RobinhoodMarketDataAdapter {
    fn subscription_ready(&self) -> bool {
        self.is_authenticated()
    }
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let (tx, rx) = mpsc::channel(1000);

        let symbols_clone: Vec<Symbol> = symbols.to_vec();
        let adapter = RobinhoodMarketDataAdapter::new(self.access_token.read().unwrap().clone())?;

        tokio::spawn(async move {
            loop {
                for symbol in &symbols_clone {
                    match adapter.get_snapshot(symbol).await {
                        Ok(quote) => {
                            let tick = Tick {
                                time: Utc::now(),
                                symbol: symbol.clone(),
                                bid: quote.bid,
                                ask: quote.ask,
                                last: quote.last,
                                volume: quote.volume,
                                source: "robinhood".to_string(),
                                open: Some(quote.open),
                                change_pct: Some(quote.change_pct),
                            };

                            if tx.send(tick).await.is_err() {
                                warn!("Tick channel closed, stopping Robinhood subscription");
                                return;
                            }
                        }
                        Err(e) => {
                            error!("Failed to fetch Robinhood quote for {}: {}", symbol.0, e);
                        }
                    }
                }
                // Poll every 3 seconds
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });

        info!("Subscribed to {} symbols via Robinhood", symbols.len());
        Ok(rx)
    }

    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()> {
        debug!("Unsubscribed from {} Robinhood symbols", symbols.len());
        Ok(())
    }

    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote> {
        if !self.is_authenticated() {
            self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let url = format!("{}/quotes/{}/", ROBINHOOD_API_BASE, symbol.0);
        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(&url)
            .header("Authorization", &auth_header)
            .send()
            .await
            .context("Failed to fetch quote from Robinhood")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            self.set_health(AdapterHealth::Degraded(format!("HTTP {}", status)));
            return Err(anyhow::anyhow!(
                "Robinhood API error {}: {}",
                status,
                error_text
            ));
        }

        let rh_quote: RobinhoodQuote = response
            .json()
            .await
            .context("Failed to parse Robinhood quote")?;

        let last: f64 = rh_quote.last_trade_price.parse().unwrap_or(0.0);
        let bid: f64 = rh_quote.bid_price.parse().unwrap_or(last);
        let ask: f64 = rh_quote.ask_price.parse().unwrap_or(last);
        let prev_close: f64 = rh_quote.previous_close.parse().unwrap_or(0.0);

        let change_pct = if prev_close > 0.0 {
            ((last - prev_close) / prev_close) * 100.0
        } else {
            0.0
        };

        // Fetch fundamentals for open/high/low/volume
        let fund_url = format!("{}/fundamentals/{}/", ROBINHOOD_API_BASE, symbol.0);
        let fund_response = self
            .client
            .get(&fund_url)
            .header("Authorization", self.get_auth_header()?)
            .send()
            .await;

        let (open, high, low, volume) = if let Ok(resp) = fund_response {
            if resp.status().is_success() {
                if let Ok(fund) = resp.json::<RobinhoodFundamentals>().await {
                    (
                        fund.open
                            .as_deref()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0.0),
                        fund.high
                            .as_deref()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0.0),
                        fund.low
                            .as_deref()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0.0),
                        fund.volume
                            .as_deref()
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0) as u64,
                    )
                } else {
                    (0.0, 0.0, 0.0, 0)
                }
            } else {
                (0.0, 0.0, 0.0, 0)
            }
        } else {
            (0.0, 0.0, 0.0, 0)
        };

        self.set_health(AdapterHealth::Healthy);

        Ok(Quote {
            symbol: symbol.clone(),
            bid,
            ask,
            last,
            open,
            high,
            low,
            volume,
            change_pct,
            vwap: last,
            updated_at: Utc::now(),
        })
    }

    async fn get_historical_ohlcv(
        &self,
        symbol: &Symbol,
        timeframe: Timeframe,
        _from: DateTime<Utc>,
        _to: DateTime<Utc>,
    ) -> Result<Vec<OHLCV>> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let (interval, span) = self.timeframe_to_robinhood(&timeframe);
        let url = format!(
            "{}/quotes/historicals/{}/?interval={}&span={}",
            ROBINHOOD_API_BASE, symbol.0, interval, span
        );

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(&url)
            .header("Authorization", &auth_header)
            .send()
            .await
            .context("Failed to fetch historical data from Robinhood")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Robinhood API error: {}",
                response.status()
            ));
        }

        let hist: RobinhoodHistoricals = response
            .json()
            .await
            .context("Failed to parse historical data")?;

        let mut bars = Vec::new();
        for candle in hist.historicals {
            let time = DateTime::parse_from_rfc3339(&candle.begins_at)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);

            bars.push(OHLCV {
                time,
                symbol: symbol.clone(),
                timeframe: timeframe.clone(),
                open: candle.open_price.parse().unwrap_or(0.0),
                high: candle.high_price.parse().unwrap_or(0.0),
                low: candle.low_price.parse().unwrap_or(0.0),
                close: candle.close_price.parse().unwrap_or(0.0),
                volume: candle.volume,
            });
        }

        debug!(
            "Fetched {} historical bars for {} from Robinhood",
            bars.len(),
            symbol.0
        );
        Ok(bars)
    }

    fn adapter_id(&self) -> &'static str {
        "robinhood"
    }

    fn health(&self) -> AdapterHealth {
        self.health.read().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeframe_mapping() {
        let adapter = RobinhoodMarketDataAdapter::new(None).unwrap();
        assert_eq!(
            adapter.timeframe_to_robinhood(&Timeframe::M5),
            ("5minute", "day")
        );
        assert_eq!(
            adapter.timeframe_to_robinhood(&Timeframe::H1),
            ("hour", "month")
        );
        assert_eq!(
            adapter.timeframe_to_robinhood(&Timeframe::D1),
            ("day", "year")
        );
        assert_eq!(
            adapter.timeframe_to_robinhood(&Timeframe::W1),
            ("week", "5year")
        );
    }

    #[test]
    fn test_health_default() {
        let adapter = RobinhoodMarketDataAdapter::new(None).unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
    }

    #[test]
    fn test_not_authenticated() {
        let adapter = RobinhoodMarketDataAdapter::new(None).unwrap();
        assert!(!adapter.is_authenticated());
    }

    #[test]
    fn test_set_access_token() {
        let adapter = RobinhoodMarketDataAdapter::new(None).unwrap();
        assert!(!adapter.is_authenticated());
        adapter.set_access_token("test_token".to_string());
        assert!(adapter.is_authenticated());
    }
}
