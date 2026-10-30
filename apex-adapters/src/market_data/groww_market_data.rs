use anyhow::{Context, Result};
use apex_core::{
    domain::models::*,
    ports::market_data::{AdapterHealth, MarketDataPort, TickStream},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Groww market data adapter
///
/// Provides real-time and historical market data from Groww's REST API.
/// Groww is a popular Indian brokerage for equities, mutual funds, and derivatives.
pub struct GrowwMarketDataAdapter {
    api_key: String,
    access_token: Arc<RwLock<Option<String>>>,
    client: Client,
    health: Arc<RwLock<AdapterHealth>>,
}

impl GrowwMarketDataAdapter {
    /// Create a new Groww market data adapter
    pub fn new(api_key: String, access_token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            api_key,
            access_token: Arc::new(RwLock::new(access_token)),
            client,
            health: Arc::new(RwLock::new(AdapterHealth::Healthy)),
        })
    }

    /// Set access token
    pub fn set_access_token(&self, token: String) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = Some(token);
        info!("Groww market data access token updated");
    }

    /// Clear access token.
    pub fn clear_access_token(&self) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Groww market data access token cleared");
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

    /// Map APEX symbol to Groww symbol format
    fn symbol_to_groww(&self, symbol: &Symbol) -> String {
        // Groww uses NSE/BSE symbols directly
        symbol.0.clone()
    }

    /// Map APEX timeframe to Groww interval
    fn timeframe_to_interval(&self, tf: &Timeframe) -> &'static str {
        match tf {
            Timeframe::M1 => "1m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::M30 => "30m",
            Timeframe::H1 => "1h",
            Timeframe::D1 => "1d",
            Timeframe::W1 => "1w",
            _ => "1m",
        }
    }

    /// Update health status
    fn set_health(&self, health: AdapterHealth) {
        let mut h = self.health.write().unwrap();
        *h = health;
    }
}

#[async_trait]
impl MarketDataPort for GrowwMarketDataAdapter {
    fn subscription_ready(&self) -> bool {
        self.is_authenticated()
    }
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let (tx, rx) = mpsc::channel(1000);

        let symbols_clone: Vec<Symbol> = symbols.to_vec();
        let adapter = GrowwMarketDataAdapter::new(
            self.api_key.clone(),
            self.access_token.read().unwrap().clone(),
        )?;

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
                                source: "groww".to_string(),
                                open: Some(quote.open),
                                change_pct: Some(quote.change_pct),
                            };

                            if tx.send(tick).await.is_err() {
                                warn!("Tick channel closed, stopping Groww subscription");
                                return;
                            }
                        }
                        Err(e) => {
                            error!("Failed to fetch Groww quote for {}: {}", symbol.0, e);
                        }
                    }
                }
                // Poll every 3 seconds
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });

        info!("Subscribed to {} symbols via Groww", symbols.len());
        Ok(rx)
    }

    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()> {
        debug!("Unsubscribed from {} Groww symbols", symbols.len());
        Ok(())
    }

    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote> {
        if !self.is_authenticated() {
            self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let groww_symbol = self.symbol_to_groww(symbol);
        let url = format!("https://api.groww.in/v1/stocks/quote/{}", groww_symbol);

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(&url)
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .context("Failed to fetch quote from Groww")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            self.set_health(AdapterHealth::Degraded(format!("HTTP {}", status)));
            return Err(anyhow::anyhow!(
                "Groww API error {}: {}",
                status,
                error_text
            ));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Groww response")?;

        let quote_data = data
            .get("data")
            .ok_or_else(|| anyhow::anyhow!("Quote data not found"))?;

        let last = quote_data
            .get("lastPrice")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let open = quote_data
            .get("open")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let high = quote_data
            .get("high")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let low = quote_data
            .get("low")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let volume = quote_data
            .get("volume")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let change_pct = quote_data
            .get("changePct")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        // Approximate bid/ask from last price
        let spread = last * 0.0005;
        let bid = last - spread;
        let ask = last + spread;

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
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<OHLCV>> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let groww_symbol = self.symbol_to_groww(symbol);
        let interval = self.timeframe_to_interval(&timeframe);

        let url = format!(
            "https://api.groww.in/v1/stocks/historical/{}/{}?from={}&to={}",
            groww_symbol,
            interval,
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(&url)
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .context("Failed to fetch historical data from Groww")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Groww API error: {}", response.status()));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse historical data")?;

        let candles = data
            .get("data")
            .and_then(|d| d.get("candles"))
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid historical data response"))?;

        let mut bars = Vec::new();

        for candle in candles {
            if let Some(arr) = candle.as_array() {
                if arr.len() < 6 {
                    continue;
                }

                let time_str = arr[0].as_str().unwrap_or("");
                let time = DateTime::parse_from_rfc3339(time_str)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                bars.push(OHLCV {
                    time,
                    symbol: symbol.clone(),
                    timeframe: timeframe.clone(),
                    open: arr[1].as_f64().unwrap_or(0.0),
                    high: arr[2].as_f64().unwrap_or(0.0),
                    low: arr[3].as_f64().unwrap_or(0.0),
                    close: arr[4].as_f64().unwrap_or(0.0),
                    volume: arr[5].as_u64().unwrap_or(0),
                });
            }
        }

        debug!(
            "Fetched {} historical bars for {} from Groww",
            bars.len(),
            symbol.0
        );
        Ok(bars)
    }

    fn adapter_id(&self) -> &'static str {
        "groww"
    }

    fn health(&self) -> AdapterHealth {
        self.health.read().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_mapping() {
        let adapter = GrowwMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        let symbol = Symbol("RELIANCE".to_string());
        assert_eq!(adapter.symbol_to_groww(&symbol), "RELIANCE");
    }

    #[test]
    fn test_timeframe_mapping() {
        let adapter = GrowwMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::M1), "1m");
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::M5), "5m");
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::D1), "1d");
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::W1), "1w");
    }

    #[test]
    fn test_health_default() {
        let adapter = GrowwMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
    }

    #[test]
    fn test_not_authenticated() {
        let adapter = GrowwMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        assert!(!adapter.is_authenticated());
    }

    #[test]
    fn test_set_access_token() {
        let adapter = GrowwMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        assert!(!adapter.is_authenticated());
        adapter.set_access_token("test_token".to_string());
        assert!(adapter.is_authenticated());
    }
}
