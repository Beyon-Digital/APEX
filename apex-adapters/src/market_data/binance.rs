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

/// Binance WebSocket base URL
const BINANCE_WS_BASE: &str = "wss://stream.binance.com:9443/ws";

/// Binance REST API base URL
const BINANCE_REST_BASE: &str = "https://api.binance.com";

/// Binance market data adapter (WebSocket-based)
pub struct BinanceAdapter {
    client: reqwest::Client,
    status: Arc<RwLock<AdapterHealth>>,
    subscribed_symbols: Arc<RwLock<Vec<Symbol>>>,
    api_key: Option<String>,
    testnet: bool,
}

impl BinanceAdapter {
    /// Create a new Binance adapter
    pub fn new() -> Self {
        Self::with_config(None, false)
    }

    /// Create a new Binance adapter with API credentials
    pub fn with_config(api_key: Option<String>, testnet: bool) -> Self {
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
            testnet,
        }
    }

    /// Get the WebSocket base URL based on testnet setting
    fn ws_base_url(&self) -> &str {
        if self.testnet {
            "wss://testnet.binance.vision/ws"
        } else {
            BINANCE_WS_BASE
        }
    }

    /// Get the REST base URL based on testnet setting
    fn rest_base_url(&self) -> &str {
        if self.testnet {
            "https://testnet.binance.vision"
        } else {
            BINANCE_REST_BASE
        }
    }

    /// Convert APEX symbol to Binance format (e.g., "BTC/USDT" -> "btcusdt")
    fn format_symbol(symbol: &Symbol) -> String {
        symbol.0.replace(['/', '-'], "").to_lowercase()
    }

    fn supports_pair(symbol: &Symbol) -> bool {
        let raw = symbol.0.to_uppercase();
        let pair = raw.replace(['/', '-'], "");
        pair.chars().all(|c| c.is_ascii_alphanumeric())
            && ["USDT", "USDC", "BUSD", "BTC", "ETH", "BNB"]
                .iter()
                .any(|quote| pair.ends_with(quote) && pair.len() > quote.len())
    }

    /// Convert Binance symbol to APEX format (e.g., "btcusdt" -> "BTC/USDT")
    fn parse_symbol(binance_symbol: &str) -> Symbol {
        // Simple conversion - for production, need proper pair detection
        let upper = binance_symbol.to_uppercase();
        // Try to insert / for common pairs
        if upper.ends_with("USDT") {
            let base = &upper[..upper.len() - 4];
            Symbol(format!("{}/USDT", base))
        } else if upper.ends_with("BUSD") {
            let base = &upper[..upper.len() - 4];
            Symbol(format!("{}/BUSD", base))
        } else if upper.ends_with("BTC") {
            let base = &upper[..upper.len() - 3];
            Symbol(format!("{}/BTC", base))
        } else if upper.ends_with("ETH") {
            let base = &upper[..upper.len() - 3];
            Symbol(format!("{}/ETH", base))
        } else {
            Symbol(upper)
        }
    }

    /// Parse Binance trade message into Tick
    fn parse_trade_message(msg: &BinanceTradeMessage) -> Result<Tick> {
        let time = DateTime::from_timestamp_millis(msg.trade_time)
            .ok_or_else(|| anyhow!("Invalid timestamp"))?;

        Ok(Tick {
            time,
            symbol: Self::parse_symbol(&msg.symbol),
            bid: msg.price, // Binance doesn't provide bid/ask in trade stream
            ask: msg.price,
            last: msg.price,
            volume: msg.quantity as u64,
            source: "binance".into(),
            open: None,
            change_pct: None,
        })
    }

    /// Parse Binance ticker message into Quote
    fn parse_ticker_message(msg: &BinanceTickerMessage) -> Result<Quote> {
        let time = DateTime::from_timestamp_millis(msg.event_time)
            .ok_or_else(|| anyhow!("Invalid timestamp"))?;

        let change_pct = if msg.prev_close_price > 0.0 {
            ((msg.last_price - msg.prev_close_price) / msg.prev_close_price) * 100.0
        } else {
            0.0
        };

        Ok(Quote {
            symbol: Self::parse_symbol(&msg.symbol),
            bid: msg.best_bid_price,
            ask: msg.best_ask_price,
            last: msg.last_price,
            open: msg.open_price,
            high: msg.high_price,
            low: msg.low_price,
            volume: msg.volume as u64,
            change_pct,
            vwap: msg.weighted_avg_price,
            updated_at: time,
        })
    }

    /// Fetch current quote from REST API
    async fn fetch_quote(&self, symbol: &Symbol) -> Result<Quote> {
        let binance_symbol = Self::format_symbol(symbol);
        let url = format!(
            "{}/api/v3/ticker/24hr?symbol={}",
            self.rest_base_url(),
            binance_symbol
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Binance REST request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!("Binance returned status {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read Binance response: {}", e))?;

        let ticker: Binance24hTicker = serde_json::from_str(&body)
            .map_err(|e| anyhow!("Failed to parse Binance ticker: {}", e))?;

        let time = Utc::now();
        let change_pct = if ticker.prev_close_price > 0.0 {
            ((ticker.last_price - ticker.prev_close_price) / ticker.prev_close_price) * 100.0
        } else {
            0.0
        };

        Ok(Quote {
            symbol: symbol.clone(),
            bid: ticker.best_bid_price,
            ask: ticker.best_ask_price,
            last: ticker.last_price,
            open: ticker.open_price,
            high: ticker.high_price,
            low: ticker.low_price,
            volume: ticker.volume as u64,
            change_pct,
            vwap: ticker.weighted_avg_price,
            updated_at: time,
        })
    }

    /// Fetch historical klines (candlestick data)
    async fn fetch_klines(
        &self,
        symbol: &Symbol,
        interval: &str,
        timeframe: Timeframe,
        limit: u32,
    ) -> Result<Vec<OHLCV>> {
        let binance_symbol = Self::format_symbol(symbol);
        let url = format!(
            "{}/api/v3/klines?symbol={}&interval={}&limit={}",
            self.rest_base_url(),
            binance_symbol,
            interval,
            limit
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Binance klines request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!("Binance returned status {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read Binance klines: {}", e))?;

        let klines: Vec<Vec<serde_json::Value>> = serde_json::from_str(&body)
            .map_err(|e| anyhow!("Failed to parse Binance klines: {}", e))?;

        let mut bars = Vec::with_capacity(klines.len());
        for kline in klines {
            if kline.len() >= 6 {
                let time = DateTime::from_timestamp_millis(kline[0].as_i64().unwrap())
                    .ok_or_else(|| anyhow!("Invalid kline timestamp"))?;

                bars.push(OHLCV {
                    time,
                    symbol: symbol.clone(),
                    timeframe: timeframe.clone(),
                    open: kline[1].as_str().unwrap().parse().unwrap_or(0.0),
                    high: kline[2].as_str().unwrap().parse().unwrap_or(0.0),
                    low: kline[3].as_str().unwrap().parse().unwrap_or(0.0),
                    close: kline[4].as_str().unwrap().parse().unwrap_or(0.0),
                    volume: kline[5].as_str().unwrap().parse().unwrap_or(0.0) as u64,
                });
            }
        }

        Ok(bars)
    }

    /// Convert APEX timeframe to Binance interval
    fn timeframe_to_interval(tf: &Timeframe) -> &'static str {
        match tf {
            Timeframe::S1 | Timeframe::S5 | Timeframe::S15 => "1s",
            Timeframe::M1 => "1m",
            Timeframe::M3 => "3m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::M30 => "30m",
            Timeframe::H1 => "1h",
            Timeframe::H4 => "4h",
            Timeframe::D1 => "1d",
            Timeframe::W1 => "1w",
        }
    }
}

impl Default for BinanceAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketDataPort for BinanceAdapter {
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

        // Build streams for all symbols
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@trade", Self::format_symbol(s)))
            .collect();

        let url = if streams.len() == 1 {
            format!("{}/{}", self.ws_base_url(), streams[0])
        } else {
            format!(
                "{}?streams={}",
                self.ws_base_url().trim_end_matches("/ws").to_owned() + "/stream",
                streams.join("/")
            )
        };

        info!("Connecting to Binance WebSocket: {}", url);

        tokio::spawn(async move {
            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws_stream, _)) => {
                    info!("Connected to Binance WebSocket");

                    let mut ws_stream = ws_stream;

                    // Read messages
                    loop {
                        tokio::select! {
                            // Handle incoming messages
                            message = ws_stream.next() => {
                                match message {
                                    Some(Ok(Message::Text(text))) => {
                                        let payload = serde_json::from_str::<serde_json::Value>(&text).ok();
                                        let data = payload.as_ref().and_then(|v| v.get("data")).or(payload.as_ref());
                                        if let Some(Ok(trade_msg)) = data.map(|v| serde_json::from_value::<BinanceTradeMessage>(v.clone())) {
                                            match BinanceAdapter::parse_trade_message(&trade_msg) {
                                                Ok(tick) => {
                                                    for original in symbols.iter().filter(|s| Self::format_symbol(s).eq_ignore_ascii_case(&trade_msg.symbol)) {
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
                                    Some(Ok(Message::Ping(data))) => {
                                        if ws_stream.send(Message::Pong(data)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        info!("Binance WebSocket closed");
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
                    error!("Failed to connect to Binance WebSocket: {}", e);
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
        self.fetch_quote(symbol).await
    }

    async fn get_historical_ohlcv(
        &self,
        symbol: &Symbol,
        timeframe: Timeframe,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<OHLCV>> {
        // Binance API doesn't support date range directly, fetch by limit
        // For production, need to implement proper date range fetching
        let interval = Self::timeframe_to_interval(&timeframe);
        let mut all_bars = Vec::new();
        let current_bars = self
            .fetch_klines(symbol, interval, timeframe.clone(), 1000)
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
        "binance"
    }

    fn health(&self) -> AdapterHealth {
        self.status
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(AdapterHealth::Healthy)
    }
}

// --- Binance API Response Types ---

#[derive(Debug, Deserialize)]
struct BinanceTradeMessage {
    #[serde(rename = "e")]
    event_type: String,
    #[serde(rename = "E")]
    event_time: i64,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "t")]
    trade_id: u64,
    #[serde(rename = "p", deserialize_with = "deserialize_number")]
    price: f64,
    #[serde(rename = "q", deserialize_with = "deserialize_number")]
    quantity: f64,
    #[serde(rename = "T")]
    trade_time: i64,
    #[serde(rename = "m")]
    is_buyer_maker: bool,
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
struct BinanceTickerMessage {
    #[serde(rename = "e")]
    event_type: String,
    #[serde(rename = "E")]
    event_time: i64,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "b")]
    best_bid_price: f64,
    #[serde(rename = "a")]
    best_ask_price: f64,
    #[serde(rename = "c")]
    last_price: f64,
    #[serde(rename = "o")]
    open_price: f64,
    #[serde(rename = "h")]
    high_price: f64,
    #[serde(rename = "l")]
    low_price: f64,
    #[serde(rename = "v")]
    volume: f64,
    #[serde(rename = "w")]
    weighted_avg_price: f64,
    #[serde(rename = "x")]
    prev_close_price: f64,
}

#[derive(Debug, Deserialize)]
struct Binance24hTicker {
    #[serde(rename = "symbol")]
    symbol: String,
    #[serde(rename = "bidPrice")]
    best_bid_price: f64,
    #[serde(rename = "askPrice")]
    best_ask_price: f64,
    #[serde(rename = "lastPrice")]
    last_price: f64,
    #[serde(rename = "openPrice")]
    open_price: f64,
    #[serde(rename = "highPrice")]
    high_price: f64,
    #[serde(rename = "lowPrice")]
    low_price: f64,
    #[serde(rename = "volume")]
    volume: f64,
    #[serde(rename = "weightedAvgPrice")]
    weighted_avg_price: f64,
    #[serde(rename = "prevClosePrice")]
    prev_close_price: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_binance_pairs_and_parses_real_trade_numbers() {
        let adapter = BinanceAdapter::new();
        assert!(adapter.supports_symbol(&Symbol("BTC/USDT".into())));
        assert!(adapter.supports_symbol(&Symbol("ETHUSDT".into())));
        assert!(!adapter.supports_symbol(&Symbol("BTC-USD".into())));
        assert!(!adapter.supports_symbol(&Symbol("AAPL".into())));
        let trade: BinanceTradeMessage = serde_json::from_str(r#"{"e":"trade","E":1710000000000,"s":"BTCUSDT","t":1,"p":"64500.50","q":"0.001","T":1710000000000,"m":false}"#).unwrap();
        assert_eq!(
            BinanceAdapter::parse_trade_message(&trade).unwrap().last,
            64500.5
        );
    }

    #[test]
    fn test_adapter_id() {
        let adapter = BinanceAdapter::new();
        assert_eq!(adapter.adapter_id(), "binance");
    }

    #[test]
    fn test_format_symbol() {
        assert_eq!(
            BinanceAdapter::format_symbol(&Symbol("BTC/USDT".into())),
            "btcusdt"
        );
        assert_eq!(
            BinanceAdapter::format_symbol(&Symbol("ETH/BTC".into())),
            "ethbtc"
        );
    }

    #[test]
    fn test_parse_symbol() {
        assert_eq!(
            BinanceAdapter::parse_symbol("btcusdt"),
            Symbol("BTC/USDT".into())
        );
        assert_eq!(
            BinanceAdapter::parse_symbol("ethbtc"),
            Symbol("ETH/BTC".into())
        );
    }

    #[test]
    fn test_timeframe_to_interval() {
        assert_eq!(BinanceAdapter::timeframe_to_interval(&Timeframe::M1), "1m");
        assert_eq!(BinanceAdapter::timeframe_to_interval(&Timeframe::H1), "1h");
        assert_eq!(BinanceAdapter::timeframe_to_interval(&Timeframe::D1), "1d");
    }

    #[test]
    fn test_health_default() {
        let adapter = BinanceAdapter::new();
        assert_eq!(
            adapter.health(),
            AdapterHealth::Degraded("Waiting for a supported symbol".into())
        );
    }
}
