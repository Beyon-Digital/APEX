use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use apex_core::domain::models::*;
use apex_core::ports::market_data::*;

/// Coinbase Pro WebSocket base URL
const COINBASE_WS_BASE: &str = "wss://ws-feed.exchange.coinbase.com";

/// Coinbase Pro REST API base URL
const COINBASE_REST_BASE: &str = "https://api.exchange.coinbase.com";

/// Coinbase Pro market data adapter
pub struct CoinbaseAdapter {
    client: reqwest::Client,
    status: Arc<RwLock<AdapterHealth>>,
    subscribed_symbols: Arc<RwLock<Vec<Symbol>>>,
    api_key: Option<String>,
    api_secret: Option<String>,
    passphrase: Option<String>,
}

impl CoinbaseAdapter {
    /// Create a new Coinbase Pro adapter
    pub fn new() -> Self {
        Self::with_config(None, None, None)
    }

    /// Create a new Coinbase Pro adapter with API credentials
    pub fn with_config(
        api_key: Option<String>,
        api_secret: Option<String>,
        passphrase: Option<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("APEX-Terminal/0.1")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            status: Arc::new(RwLock::new(AdapterHealth::Degraded(
                "Waiting for a supported symbol".into(),
            ))),
            subscribed_symbols: Arc::new(RwLock::new(Vec::new())),
            api_key,
            api_secret,
            passphrase,
        }
    }

    /// Convert APEX symbol to Coinbase Pro format (e.g., "BTC/USDT" -> "BTC-USD")
    fn format_symbol(symbol: &Symbol) -> String {
        symbol.0.replace('/', "-").to_uppercase()
    }

    fn supports_pair(symbol: &Symbol) -> bool {
        let raw = Self::format_symbol(symbol);
        let Some((base, quote)) = raw.split_once('-') else {
            return false;
        };
        !base.is_empty()
            && base.chars().all(|c| c.is_ascii_alphanumeric())
            && ["USD", "USDT", "USDC", "BTC", "ETH", "EUR", "GBP"].contains(&quote)
    }

    /// Convert Coinbase Pro symbol to APEX format (e.g., "BTC-USD" -> "BTC/USD")
    fn parse_symbol(coinbase_symbol: &str) -> Symbol {
        Symbol(coinbase_symbol.replace('-', "/"))
    }

    /// Parse Coinbase Pro trade message into Tick
    fn parse_trade_message(msg: &CoinbaseTradeMessage) -> Result<Tick> {
        let time = DateTime::parse_from_rfc3339(&msg.time)
            .map_err(|e| anyhow!("Invalid timestamp: {}", e))?
            .with_timezone(&Utc);

        Ok(Tick {
            time,
            symbol: Self::parse_symbol(&msg.product_id),
            bid: msg.price, // Coinbase doesn't provide bid/ask in trade stream
            ask: msg.price,
            last: msg.price,
            volume: (msg.size * 1e8) as u64, // Convert to base units
            source: "coinbase".into(),
            open: None,
            change_pct: None,
        })
    }

    /// Fetch current ticker from REST API
    async fn fetch_ticker(&self, symbol: &Symbol) -> Result<Quote> {
        let coinbase_symbol = Self::format_symbol(symbol);
        let url = format!("{}/products/{}/ticker", COINBASE_REST_BASE, coinbase_symbol);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Coinbase ticker request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!("Coinbase returned status {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read Coinbase response: {}", e))?;

        let ticker: CoinbaseTicker = serde_json::from_str(&body)
            .map_err(|e| anyhow!("Failed to parse Coinbase ticker: {}", e))?;

        let time = DateTime::from_timestamp_millis(ticker.time)
            .ok_or_else(|| anyhow!("Invalid ticker timestamp"))?;

        let change_pct = if ticker.open_24h > 0.0 {
            ((ticker.price - ticker.open_24h) / ticker.open_24h) * 100.0
        } else {
            0.0
        };

        Ok(Quote {
            symbol: symbol.clone(),
            bid: ticker.bid,
            ask: ticker.ask,
            last: ticker.price,
            open: ticker.open_24h,
            high: ticker.high_24h,
            low: ticker.low_24h,
            volume: ticker.volume_24h as u64,
            change_pct,
            vwap: ticker.volume_24h, // Simplified - Coinbase doesn't provide VWAP
            updated_at: time,
        })
    }

    /// Fetch historical candles
    async fn fetch_candles(
        &self,
        symbol: &Symbol,
        granularity: u32,
        timeframe: Timeframe,
        limit: u32,
    ) -> Result<Vec<OHLCV>> {
        let coinbase_symbol = Self::format_symbol(symbol);
        let end = Utc::now().timestamp();
        let start = end - (granularity as i64 * limit as i64);

        let url = format!(
            "{}/products/{}/candles?granularity={}&start={}&end={}",
            COINBASE_REST_BASE, coinbase_symbol, granularity, start, end
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Coinbase candles request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!("Coinbase returned status {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read Coinbase candles: {}", e))?;

        let candles: Vec<Vec<f64>> = serde_json::from_str(&body)
            .map_err(|e| anyhow!("Failed to parse Coinbase candles: {}", e))?;

        let mut bars = Vec::with_capacity(candles.len());
        for candle in candles {
            if candle.len() >= 6 {
                bars.push(OHLCV {
                    time: DateTime::from_timestamp(candle[0] as i64, 0)
                        .ok_or_else(|| anyhow!("Invalid candle timestamp"))?,
                    symbol: symbol.clone(),
                    timeframe: timeframe.clone(),
                    open: candle[3],
                    high: candle[2],
                    low: candle[1],
                    close: candle[4],
                    volume: candle[5] as u64,
                });
            }
        }

        Ok(bars)
    }

    /// Convert APEX timeframe to Coinbase granularity (seconds)
    fn timeframe_to_granularity(tf: &Timeframe) -> u32 {
        match tf {
            Timeframe::S1 => 1,
            Timeframe::S5 => 5,
            Timeframe::S15 => 15,
            Timeframe::M1 => 60,
            Timeframe::M3 => 180,
            Timeframe::M5 => 300,
            Timeframe::M15 => 900,
            Timeframe::M30 => 1800,
            Timeframe::H1 => 3600,
            Timeframe::H4 => 14400,
            Timeframe::D1 => 86400,
            Timeframe::W1 => 604800,
        }
    }
}

impl Default for CoinbaseAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketDataPort for CoinbaseAdapter {
    fn supports_symbol(&self, symbol: &Symbol) -> bool {
        Self::supports_pair(symbol)
    }
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream> {
        *self.status.write().await = AdapterHealth::Degraded("Connecting".into());
        let (tx, rx) = mpsc::channel(1024);
        let status = self.status.clone();
        let symbols: Vec<Symbol> = symbols.to_vec();

        {
            let mut subs = self.subscribed_symbols.write().await;
            for s in &symbols {
                if !subs.iter().any(|existing| existing.0 == s.0) {
                    subs.push(s.clone());
                }
            }
        }

        let coinbase_symbols: Vec<String> =
            symbols.iter().map(|s| Self::format_symbol(s)).collect();

        let subscribe_msg = serde_json::json!({
            "type": "subscribe",
            "product_ids": coinbase_symbols,
            "channels": ["ticker", "matches"]
        });

        let url = COINBASE_WS_BASE;

        info!("Connecting to Coinbase WebSocket: {}", url);

        tokio::spawn(async move {
            match tokio_tungstenite::connect_async(url).await {
                Ok((ws_stream, _)) => {
                    info!("Connected to Coinbase WebSocket");

                    let mut ws_stream = ws_stream;

                    // Send subscription message
                    if ws_stream
                        .send(Message::Text(subscribe_msg.to_string()))
                        .await
                        .is_err()
                    {
                        error!("Failed to send subscription message");
                        *status.write().await =
                            AdapterHealth::Unhealthy("Failed to send subscription message".into());
                        return;
                    }

                    // Read messages
                    loop {
                        tokio::select! {
                            message = ws_stream.next() => {
                                match message {
                                    Some(Ok(Message::Text(text))) => {
                                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                                            if data.get("type").and_then(|t| t.as_str()) == Some("error") {
                                                let message = data.get("message").and_then(|m| m.as_str()).unwrap_or("Subscription rejected");
                                                error!("Coinbase subscription rejected: {}", message);
                                                *status.write().await = AdapterHealth::Unhealthy(format!("Subscription rejected: {}", message));
                                                break;
                                            }
                                            if data.get("type").and_then(|t| t.as_str()) == Some("match") {
                                                if let Ok(trade_msg) = serde_json::from_str::<CoinbaseTradeMessage>(&text) {
                                                    match Self::parse_trade_message(&trade_msg) {
                                                        Ok(tick) => {
                                                            for original in symbols.iter().filter(|s| Self::format_symbol(s) == trade_msg.product_id) {
                                                                let mut mapped = tick.clone();
                                                                mapped.symbol = original.clone();
                                                                if tx.send(mapped).await.is_err() {
                                                                    info!("Tick stream closed, stopping WebSocket");
                                                                    return;
                                                                }
                                                            }
                                                            *status.write().await = AdapterHealth::Healthy;
                                                        }
                                                        Err(e) => {
                                                            warn!("Failed to parse trade message: {}", e);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Some(Ok(Message::Ping(data))) => {
                                        if ws_stream.send(Message::Pong(data)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        info!("Coinbase WebSocket closed");
                                        *status.write().await = AdapterHealth::Degraded("Connection closed; retry on next subscription".into());
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        error!("WebSocket error: {}", e);
                                        *status.write().await = AdapterHealth::Degraded(format!("WebSocket error: {}", e));
                                        break;
                                    }
                                    None => {
                                        info!("WebSocket stream ended");
                                        *status.write().await = AdapterHealth::Degraded("Connection ended; retry on next subscription".into());
                                        break;
                                    }
                                    Some(Ok(_)) => {}
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to connect to Coinbase WebSocket: {}", e);
                    *status.write().await =
                        AdapterHealth::Unhealthy(format!("Connection failed: {}", e));
                }
            }
        });

        Ok(rx)
    }

    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()> {
        let mut subs = self.subscribed_symbols.write().await;
        subs.retain(|s| !symbols.iter().any(|unsub| unsub.0 == s.0));
        Ok(())
    }

    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote> {
        self.fetch_ticker(symbol).await
    }

    async fn get_historical_ohlcv(
        &self,
        symbol: &Symbol,
        timeframe: Timeframe,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<OHLCV>> {
        let granularity = Self::timeframe_to_granularity(&timeframe);
        let mut all_bars = Vec::new();
        let current_bars = self
            .fetch_candles(symbol, granularity, timeframe.clone(), 300)
            .await?;

        // Filter by date range
        all_bars.extend(
            current_bars
                .into_iter()
                .filter(|bar| bar.time >= from && bar.time <= to),
        );

        Ok(all_bars)
    }

    fn adapter_id(&self) -> &'static str {
        "coinbase"
    }

    fn health(&self) -> AdapterHealth {
        self.status
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(AdapterHealth::Healthy)
    }
}

// --- Coinbase API Response Types ---

#[derive(Debug, Deserialize)]
struct CoinbaseTradeMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "trade_id")]
    trade_id: u64,
    #[serde(rename = "sequence")]
    sequence: u64,
    #[serde(rename = "time")]
    time: String,
    #[serde(rename = "product_id")]
    product_id: String,
    #[serde(rename = "price", deserialize_with = "deserialize_number")]
    price: f64,
    #[serde(rename = "size", deserialize_with = "deserialize_number")]
    size: f64,
}

fn deserialize_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Numeric {
        Number(f64),
        String(String),
    }
    match Numeric::deserialize(d)? {
        Numeric::Number(n) => Ok(n),
        Numeric::String(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Deserialize)]
struct CoinbaseTicker {
    #[serde(rename = "trade_id")]
    trade_id: u64,
    price: f64,
    bid: f64,
    ask: f64,
    #[serde(rename = "volume")]
    volume: f64,
    #[serde(rename = "time")]
    time: i64,
    #[serde(rename = "low_24h")]
    low_24h: f64,
    #[serde(rename = "high_24h")]
    high_24h: f64,
    #[serde(rename = "open_24h")]
    open_24h: f64,
    #[serde(rename = "volume_24h")]
    volume_24h: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_crypto_pairs_and_parses_exchange_trade() {
        let adapter = CoinbaseAdapter::new();
        assert!(adapter.supports_symbol(&Symbol("BTC-USD".into())));
        assert!(adapter.supports_symbol(&Symbol("ETH/USDT".into())));
        assert!(!adapter.supports_symbol(&Symbol("AAPL".into())));
        assert!(!adapter.supports_symbol(&Symbol("RELIANCE.NS".into())));
        let trade: CoinbaseTradeMessage = serde_json::from_str(r#"{"type":"match","trade_id":1,"sequence":2,"time":"2024-03-09T16:00:00.000000Z","product_id":"BTC-USD","price":"64500.50","size":"0.001"}"#).unwrap();
        let tick = CoinbaseAdapter::parse_trade_message(&trade).unwrap();
        assert_eq!(tick.last, 64500.5);
        assert_eq!(tick.time.to_rfc3339(), "2024-03-09T16:00:00+00:00");
    }

    #[test]
    fn test_adapter_id() {
        let adapter = CoinbaseAdapter::new();
        assert_eq!(adapter.adapter_id(), "coinbase");
    }

    #[test]
    fn test_format_symbol() {
        assert_eq!(
            CoinbaseAdapter::format_symbol(&Symbol("BTC/USD".into())),
            "BTC-USD"
        );
        assert_eq!(
            CoinbaseAdapter::format_symbol(&Symbol("ETH/USDT".into())),
            "ETH-USDT"
        );
    }

    #[test]
    fn test_parse_symbol() {
        assert_eq!(
            CoinbaseAdapter::parse_symbol("BTC-USD"),
            Symbol("BTC/USD".into())
        );
        assert_eq!(
            CoinbaseAdapter::parse_symbol("ETH-USDT"),
            Symbol("ETH/USDT".into())
        );
    }

    #[test]
    fn test_timeframe_to_granularity() {
        assert_eq!(
            CoinbaseAdapter::timeframe_to_granularity(&Timeframe::M1),
            60
        );
        assert_eq!(
            CoinbaseAdapter::timeframe_to_granularity(&Timeframe::H1),
            3600
        );
        assert_eq!(
            CoinbaseAdapter::timeframe_to_granularity(&Timeframe::D1),
            86400
        );
    }

    #[test]
    fn test_health_default() {
        let adapter = CoinbaseAdapter::new();
        assert_eq!(
            adapter.health(),
            AdapterHealth::Degraded("Waiting for a supported symbol".into())
        );
    }
}
