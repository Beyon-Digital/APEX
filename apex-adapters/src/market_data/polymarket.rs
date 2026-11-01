use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use apex_core::domain::models::*;
use apex_core::ports::market_data::*;

/// Polymarket GraphQL API endpoint
const POLYMARKET_GRAPHQL: &str = "https://gamma-api.polymarket.com/query";

/// Polymarket WebSocket endpoint
const POLYMARKET_WS: &str = "wss://gamma-api.polymarket.com/ws";

/// Polymarket market data adapter
pub struct PolymarketAdapter {
    client: reqwest::Client,
    status: Arc<RwLock<AdapterHealth>>,
    subscribed_symbols: Arc<RwLock<Vec<Symbol>>>,
}

impl PolymarketAdapter {
    /// Create a new Polymarket adapter
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("APEX-Terminal/0.1")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            status: Arc::new(RwLock::new(AdapterHealth::Degraded(
                "Waiting for a supported symbol".into(),
            ))),
            subscribed_symbols: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Convert APEX symbol to Polymarket token ID
    /// Polymarket uses token IDs instead of traditional symbols
    fn symbol_to_token_id(symbol: &Symbol) -> String {
        // For now, use the symbol directly as token ID
        // In production, need a mapping system
        symbol.0.clone()
    }

    /// Convert Polymarket token ID to APEX symbol
    fn token_id_to_symbol(token_id: &str) -> Symbol {
        Symbol(token_id.to_string())
    }

    /// Fetch market data from Polymarket GraphQL API
    pub async fn fetch_markets(&self) -> Result<Vec<PolymarketMarket>> {
        let query = r#"
            query {
                markets(orderBy: volumeDesc) {
                    id
                    question
                    outcomeAssetCount
                    marketType
                    outcomes {
                        id
                        name
                        price
                        orderBook {
                            bestAsk {
                                price
                                size
                            }
                            bestBid {
                                price
                                size
                            }
                        }
                    }
                    volume
                    liquidity
                    endDateTime
                }
            }
        "#;

        let response = self
            .client
            .post(POLYMARKET_GRAPHQL)
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await
            .map_err(|e| anyhow!("Polymarket GraphQL request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!("Polymarket returned status {}", response.status()));
        }

        let body: GraphQLResponse<MarketsResponse> = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Polymarket response: {}", e))?;

        Ok(body
            .data
            .ok_or_else(|| anyhow!("No data in response"))?
            .markets)
    }

    /// Convert Polymarket market to APEX Quote
    fn market_to_quote(market: &PolymarketMarket) -> Vec<Quote> {
        let mut quotes = Vec::new();

        for outcome in &market.outcomes {
            let bid = outcome
                .order_book
                .as_ref()
                .and_then(|ob| ob.best_bid.as_ref())
                .map(|b| b.price)
                .unwrap_or(0.0);

            let ask = outcome
                .order_book
                .as_ref()
                .and_then(|ob| ob.best_ask.as_ref())
                .map(|a| a.price)
                .unwrap_or(0.0);

            let last = outcome.price;

            quotes.push(Quote {
                symbol: Symbol(outcome.id.clone()),
                bid,
                ask,
                last,
                open: last, // Polymarket doesn't provide open
                high: last, // Polymarket doesn't provide high
                low: last,  // Polymarket doesn't provide low
                volume: market.volume as u64,
                change_pct: 0.0, // Polymarket doesn't provide change
                vwap: 0.0,       // Polymarket doesn't provide VWAP
                updated_at: Utc::now(),
            });
        }

        quotes
    }

    /// Parse Polymarket WebSocket message
    fn parse_ws_message(msg: &str) -> Result<Option<Tick>> {
        // Polymarket WebSocket sends updates in various formats
        // This is a simplified parser - production needs full implementation
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(msg) {
            if let Some(price) = data.get("price").and_then(|p| p.as_f64()) {
                if let Some(token_id) = data.get("id").and_then(|i| i.as_str()) {
                    return Ok(Some(Tick {
                        time: Utc::now(),
                        symbol: Symbol(token_id.to_string()),
                        bid: price,
                        ask: price,
                        last: price,
                        volume: 0,
                        source: "polymarket".into(),
                        open: None,
                        change_pct: None,
                    }));
                }
            }
        }
        Ok(None)
    }
}

impl Default for PolymarketAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketDataPort for PolymarketAdapter {
    fn supports_symbol(&self, symbol: &Symbol) -> bool {
        !symbol.0.is_empty() && symbol.0.chars().all(|c| c.is_ascii_digit())
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

        // Polymarket uses WebSocket for real-time updates
        let url = POLYMARKET_WS;

        info!("Connecting to Polymarket WebSocket: {}", url);

        tokio::spawn(async move {
            match tokio_tungstenite::connect_async(url).await {
                Ok((ws_stream, _)) => {
                    info!("Connected to Polymarket WebSocket");

                    let mut ws_stream = ws_stream;

                    // Subscribe to markets
                    for symbol in &symbols {
                        let subscribe_msg = serde_json::json!({
                            "type": "subscribe",
                            "id": symbol.0
                        });
                        if ws_stream
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                subscribe_msg.to_string(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }

                    // Read messages
                    loop {
                        tokio::select! {
                            message = ws_stream.next() => {
                                match message {
                                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                        match Self::parse_ws_message(&text) {
                                            Ok(Some(tick)) => {
                                                if tx.send(tick).await.is_err() {
                                                    info!("Tick stream closed, stopping WebSocket");
                                                    break;
                                                }
                                                *status.write().await = AdapterHealth::Healthy;
                                            }
                                            Ok(None) => {}
                                            Err(e) => {
                                                warn!("Failed to parse Polymarket message: {}", e);
                                            }
                                        }
                                    }
                                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                                        if ws_stream.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                                        info!("Polymarket WebSocket closed");
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
                    error!("Failed to connect to Polymarket WebSocket: {}", e);
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
        let markets = self.fetch_markets().await?;

        for market in &markets {
            for outcome in &market.outcomes {
                if outcome.id == symbol.0 {
                    let quotes = Self::market_to_quote(market);
                    for quote in quotes {
                        if quote.symbol.0 == symbol.0 {
                            return Ok(quote);
                        }
                    }
                }
            }
        }

        Err(anyhow!("Symbol not found: {}", symbol.0))
    }

    async fn get_historical_ohlcv(
        &self,
        _symbol: &Symbol,
        _timeframe: Timeframe,
        _from: DateTime<Utc>,
        _to: DateTime<Utc>,
    ) -> Result<Vec<OHLCV>> {
        // Polymarket doesn't provide historical OHLCV data via API
        // For production, need to implement custom data collection
        warn!("Historical OHLCV not available for Polymarket markets");
        Ok(vec![])
    }

    fn adapter_id(&self) -> &'static str {
        "polymarket"
    }

    fn health(&self) -> AdapterHealth {
        self.status
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(AdapterHealth::Healthy)
    }
}

// --- Polymarket API Types ---

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct MarketsResponse {
    markets: Vec<PolymarketMarket>,
}

#[derive(Debug, Deserialize)]
pub struct PolymarketMarket {
    id: String,
    question: String,
    #[serde(rename = "outcomeAssetCount")]
    outcome_asset_count: i32,
    #[serde(rename = "marketType")]
    market_type: String,
    outcomes: Vec<PolymarketOutcome>,
    volume: f64,
    liquidity: f64,
    #[serde(rename = "endDateTime")]
    end_date_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PolymarketOutcome {
    id: String,
    name: String,
    price: f64,
    #[serde(rename = "orderBook")]
    order_book: Option<OrderBook>,
}

#[derive(Debug, Deserialize)]
struct OrderBook {
    #[serde(rename = "bestAsk")]
    best_ask: Option<OrderBookEntry>,
    #[serde(rename = "bestBid")]
    best_bid: Option<OrderBookEntry>,
}

#[derive(Debug, Deserialize)]
struct OrderBookEntry {
    price: f64,
    size: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_id() {
        let adapter = PolymarketAdapter::new();
        assert_eq!(adapter.adapter_id(), "polymarket");
    }

    #[test]
    fn test_symbol_conversion() {
        let symbol = Symbol("0x1234".into());
        let token_id = PolymarketAdapter::symbol_to_token_id(&symbol);
        assert_eq!(token_id, "0x1234");

        let converted = PolymarketAdapter::token_id_to_symbol(&token_id);
        assert_eq!(converted, symbol);
    }

    #[test]
    fn test_health_default() {
        let adapter = PolymarketAdapter::new();
        assert_eq!(
            adapter.health(),
            AdapterHealth::Degraded("Waiting for a supported symbol".into())
        );
    }
}
