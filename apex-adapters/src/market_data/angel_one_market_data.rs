use anyhow::{Context, Result};
use apex_core::{
    domain::models::*,
    ports::market_data::{AdapterHealth, MarketDataPort, TickStream},
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::Client;
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

const BASE_URL: &str = "https://apiconnect.angelone.in";

/// Angel One SmartAPI market data adapter
///
/// Provides real-time and historical market data from Angel One SmartAPI
pub struct AngelOneMarketDataAdapter {
    api_key: String,
    jwt_token: Arc<RwLock<Option<String>>>,
    client: Client,
    health: Arc<RwLock<AdapterHealth>>,
    subscribed_symbols: Arc<RwLock<Vec<Symbol>>>,
}

/// Angel One quote request
#[derive(Debug, Serialize)]
struct AngelOneQuoteRequest {
    mode: String,
    #[serde(rename = "exchangeTokens")]
    exchange_tokens: std::collections::HashMap<String, Vec<String>>,
}

/// Angel One historical candle request
#[derive(Debug, Serialize)]
struct AngelOneHistoricalRequest {
    exchange: String,
    symboltoken: String,
    interval: String,
    fromdate: String,
    todate: String,
}

impl AngelOneMarketDataAdapter {
    /// Create a new Angel One market data adapter
    ///
    /// # Arguments
    /// * `api_key` - Angel One API key (private key)
    /// * `jwt_token` - JWT token obtained after login flow
    pub fn new(api_key: String, jwt_token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            api_key,
            jwt_token: Arc::new(RwLock::new(jwt_token)),
            client,
            health: Arc::new(RwLock::new(AdapterHealth::Healthy)),
            subscribed_symbols: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Set JWT token (after login flow)
    pub fn set_jwt_token(&self, token: String) {
        let mut jwt_token = self.jwt_token.write().unwrap();
        *jwt_token = Some(token);
        info!("Angel One market data JWT token updated");
    }

    /// Clear JWT token.
    pub fn clear_jwt_token(&self) {
        let mut jwt_token = self.jwt_token.write().unwrap();
        *jwt_token = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Angel One market data JWT token cleared");
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.jwt_token.read().unwrap().is_some()
    }

    /// Get authorization headers for Angel One SmartAPI
    fn get_auth_headers(&self) -> Result<Vec<(&'static str, String)>> {
        let token = self.jwt_token.read().unwrap();
        match token.as_ref() {
            Some(t) => Ok(vec![
                ("Authorization", format!("Bearer {}", t)),
                ("X-PrivateKey", self.api_key.clone()),
                ("X-ClientLocalIP", "127.0.0.1".to_string()),
                ("X-ClientPublicIP", "127.0.0.1".to_string()),
                ("X-MACAddress", "00:00:00:00:00:00".to_string()),
                ("X-UserType", "USER".to_string()),
                ("Content-Type", "application/json".to_string()),
            ]),
            None => Err(anyhow::anyhow!("No JWT token available")),
        }
    }

    /// Map APEX symbol to Angel One symbol token
    /// Simplified mapping - in production, use instrument master
    fn symbol_to_token(&self, symbol: &Symbol) -> String {
        symbol.0.clone()
    }

    /// Map APEX timeframe to Angel One interval string
    fn timeframe_to_interval(&self, tf: &Timeframe) -> &'static str {
        match tf {
            Timeframe::M1 => "ONE_MINUTE",
            Timeframe::M5 => "FIVE_MINUTE",
            Timeframe::M15 => "FIFTEEN_MINUTE",
            Timeframe::M30 => "THIRTY_MINUTE",
            Timeframe::H1 => "ONE_HOUR",
            Timeframe::D1 => "ONE_DAY",
            _ => "ONE_MINUTE", // Default fallback
        }
    }

    /// Update health status
    fn set_health(&self, health: AdapterHealth) {
        let mut h = self.health.write().unwrap();
        *h = health;
    }
}

#[async_trait]
impl MarketDataPort for AngelOneMarketDataAdapter {
    fn subscription_ready(&self) -> bool {
        self.is_authenticated()
    }
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        // Track subscribed symbols
        {
            let mut subscribed = self.subscribed_symbols.write().unwrap();
            for symbol in symbols {
                if !subscribed.contains(symbol) {
                    subscribed.push(symbol.clone());
                }
            }
        }

        let (tx, rx) = mpsc::channel(1000);

        let symbols_clone: Vec<Symbol> = symbols.to_vec();
        let adapter = AngelOneMarketDataAdapter::new(
            self.api_key.clone(),
            self.jwt_token.read().unwrap().clone(),
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
                                source: "angel_one".to_string(),
                                open: Some(quote.open),
                                change_pct: Some(quote.change_pct),
                            };

                            if tx.send(tick).await.is_err() {
                                warn!("Tick channel closed, stopping subscription");
                                return;
                            }
                        }
                        Err(e) => {
                            error!("Failed to fetch quote for {}: {}", symbol.0, e);
                        }
                    }
                }

                // Poll every 3 seconds
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });

        info!("Subscribed to {} symbols via Angel One", symbols.len());
        Ok(rx)
    }

    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()> {
        let mut subscribed = self.subscribed_symbols.write().unwrap();
        subscribed.retain(|s| !symbols.contains(s));
        debug!("Unsubscribed from {} symbols", symbols.len());
        Ok(())
    }

    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote> {
        if !self.is_authenticated() {
            self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let symboltoken = self.symbol_to_token(symbol);

        let mut exchange_tokens = std::collections::HashMap::new();
        exchange_tokens.insert("NSE".to_string(), vec![symboltoken]);

        let quote_request = AngelOneQuoteRequest {
            mode: "FULL".to_string(),
            exchange_tokens: exchange_tokens,
        };

        let headers = self.get_auth_headers()?;

        let mut request = self.client.post(format!(
            "{}/rest/secure/angelbroking/market/v1/quote",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .json(&quote_request)
            .send()
            .await
            .context("Failed to fetch quote from Angel One")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            self.set_health(AdapterHealth::Degraded(format!("HTTP {}", status)));
            return Err(anyhow::anyhow!(
                "Angel One API error {}: {}",
                status,
                error_text
            ));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Angel One response")?;

        let fetched = data
            .get("data")
            .and_then(|d| d.get("fetched"))
            .and_then(|f| f.as_array())
            .and_then(|arr| arr.first())
            .ok_or_else(|| anyhow::anyhow!("Quote data not found in response"))?;

        let ltp = fetched.get("ltp").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let open = fetched.get("open").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let high = fetched.get("high").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let low = fetched.get("low").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let close = fetched.get("close").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let volume = fetched.get("volume").and_then(|v| v.as_u64()).unwrap_or(0);

        // Approximate bid/ask from last traded price
        let spread = ltp * 0.0005;
        let bid = ltp - spread;
        let ask = ltp + spread;

        let change_pct = if close > 0.0 {
            ((ltp - close) / close) * 100.0
        } else {
            0.0
        };

        self.set_health(AdapterHealth::Healthy);

        Ok(Quote {
            symbol: symbol.clone(),
            bid,
            ask,
            last: ltp,
            open,
            high,
            low,
            volume,
            change_pct,
            vwap: ltp, // VWAP not provided by Angel One quote API
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
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let symboltoken = self.symbol_to_token(symbol);
        let interval = self.timeframe_to_interval(&timeframe);

        let hist_request = AngelOneHistoricalRequest {
            exchange: "NSE".to_string(),
            symboltoken,
            interval: interval.to_string(),
            fromdate: from.format("%Y-%m-%d %H:%M").to_string(),
            todate: to.format("%Y-%m-%d %H:%M").to_string(),
        };

        let headers = self.get_auth_headers()?;

        let mut request = self.client.post(format!(
            "{}/rest/secure/angelbroking/historical/v1/getCandleData",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .json(&hist_request)
            .send()
            .await
            .context("Failed to fetch historical data from Angel One")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Angel One API error: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse historical data")?;

        let candles = data
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid historical data response"))?;

        let mut bars = Vec::new();

        for candle in candles {
            let candle_arr = candle.as_array();
            let candle_arr = match candle_arr {
                Some(arr) if arr.len() >= 6 => arr,
                _ => continue,
            };

            // Candle format: [timestamp_str, open, high, low, close, volume]
            let time_str = candle_arr[0].as_str().unwrap_or("");
            let time = NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc())
                .or_else(|| {
                    DateTime::parse_from_rfc3339(time_str)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                })
                .unwrap_or_else(Utc::now);

            let bar = OHLCV {
                time,
                symbol: symbol.clone(),
                timeframe: timeframe.clone(),
                open: candle_arr[1].as_f64().unwrap_or(0.0),
                high: candle_arr[2].as_f64().unwrap_or(0.0),
                low: candle_arr[3].as_f64().unwrap_or(0.0),
                close: candle_arr[4].as_f64().unwrap_or(0.0),
                volume: candle_arr[5].as_u64().unwrap_or(0),
            };

            bars.push(bar);
        }

        debug!(
            "Fetched {} historical bars for {} from Angel One",
            bars.len(),
            symbol.0
        );
        Ok(bars)
    }

    fn adapter_id(&self) -> &'static str {
        "angel_one"
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
        let adapter = AngelOneMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::M1), "ONE_MINUTE");
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::M5), "FIVE_MINUTE");
        assert_eq!(
            adapter.timeframe_to_interval(&Timeframe::M15),
            "FIFTEEN_MINUTE"
        );
        assert_eq!(
            adapter.timeframe_to_interval(&Timeframe::M30),
            "THIRTY_MINUTE"
        );
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::H1), "ONE_HOUR");
        assert_eq!(adapter.timeframe_to_interval(&Timeframe::D1), "ONE_DAY");
    }

    #[test]
    fn test_symbol_mapping() {
        let adapter = AngelOneMarketDataAdapter::new("test_key".to_string(), None).unwrap();
        let symbol = Symbol("RELIANCE".to_string());
        let token = adapter.symbol_to_token(&symbol);
        assert_eq!(token, "RELIANCE");
    }
}
