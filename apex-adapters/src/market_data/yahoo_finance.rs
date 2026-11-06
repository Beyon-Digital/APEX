use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::{mpsc, RwLock};

use apex_core::domain::models::*;
use apex_core::ports::market_data::*;

/// Yahoo Finance polling interval
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Yahoo Finance API base URL
const YAHOO_API_BASE: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Default half-spread applied when bid/ask are unavailable from Yahoo
const DEFAULT_HALF_SPREAD: f64 = 0.01;

/// Yahoo Finance market data adapter (polling-based)
pub struct YahooFinanceAdapter {
    client: reqwest::Client,
    status: Arc<RwLock<AdapterHealth>>,
    subscribed_symbols: Arc<RwLock<Vec<Symbol>>>,
    poll_started: Arc<std::sync::atomic::AtomicBool>,
}

impl YahooFinanceAdapter {
    /// Create a new Yahoo Finance adapter
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (compatible; APEX Terminal/0.1)")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            status: Arc::new(RwLock::new(AdapterHealth::Healthy)),
            subscribed_symbols: Arc::new(RwLock::new(Vec::new())),
            poll_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Format a symbol for Yahoo Finance API.
    /// Symbols with a suffix (`.NS`, `.BO`, `/`) are passed through as-is.
    /// Plain symbols (e.g. "AAPL") are assumed to be US stocks and need no suffix.
    fn format_symbol(symbol: &Symbol) -> String {
        let s = symbol.0.to_uppercase();
        // Normalise exchange-style crypto pairs (BTCUSDT, ETHUSDC) to Yahoo's
        // dash convention (BTC-USD, ETH-USD).
        for quote_ccy in ["USDT", "USDC"] {
            if let Some(base) = s.strip_suffix(quote_ccy) {
                if base.len() >= 2 && base.chars().all(|c| c.is_ascii_alphabetic()) {
                    return format!("{}-USD", base);
                }
            }
        }
        symbol.0.clone()
    }

    /// Convert timeframe to Yahoo Finance interval string
    fn timeframe_to_interval(tf: &Timeframe) -> &'static str {
        match tf {
            Timeframe::S1 => "1m",
            Timeframe::S5 => "1m",
            Timeframe::S15 => "1m",
            Timeframe::M1 => "1m",
            Timeframe::M3 => "5m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::M30 => "30m",
            Timeframe::H1 => "1h",
            Timeframe::H4 => "1d",
            Timeframe::D1 => "1d",
            Timeframe::W1 => "1wk",
        }
    }

    /// Parse a Yahoo Finance chart API response into a Quote
    fn parse_quote_response(symbol: &Symbol, body: &str) -> Result<Quote> {
        let response: YahooChartResponse = serde_json::from_str(body)
            .map_err(|e| anyhow!("Failed to parse Yahoo response: {}", e))?;

        let result = response
            .chart
            .result
            .first()
            .ok_or_else(|| anyhow!("No results in Yahoo response"))?;

        let meta = &result.meta;
        let last = meta.regular_market_price;
        let prev_close = meta.chart_previous_close.unwrap_or(last);
        let change_pct = if prev_close > 0.0 {
            ((last - prev_close) / prev_close) * 100.0
        } else {
            0.0
        };

        let (open, high, low, volume) = if let Some(indicators) = &result.indicators {
            if let Some(quotes) = &indicators.quote {
                if let Some(q) = quotes.first() {
                    let o = q
                        .open
                        .as_ref()
                        .and_then(|v| v.last().copied().flatten())
                        .unwrap_or(last);
                    let h = q
                        .high
                        .as_ref()
                        .and_then(|v| v.last().copied().flatten())
                        .unwrap_or(last);
                    let l = q
                        .low
                        .as_ref()
                        .and_then(|v| v.last().copied().flatten())
                        .unwrap_or(last);
                    let vol = q
                        .volume
                        .as_ref()
                        .and_then(|v| v.last().copied().flatten())
                        .unwrap_or(0);
                    (o, h, l, vol)
                } else {
                    (last, last, last, 0)
                }
            } else {
                (last, last, last, 0)
            }
        } else {
            (last, last, last, 0)
        };

        Ok(Quote {
            symbol: symbol.clone(),
            // Yahoo doesn't provide bid/ask; approximate from last price
            bid: last - DEFAULT_HALF_SPREAD,
            ask: last + DEFAULT_HALF_SPREAD,
            last,
            open,
            high,
            low,
            volume,
            change_pct,
            // True VWAP requires intraday volume-weighted averaging which Yahoo
            // does not expose; use last price as a rough approximation.
            vwap: last,
            updated_at: meta
                .regular_market_time
                .or_else(|| {
                    result
                        .timestamp
                        .as_ref()
                        .and_then(|times| times.last().copied())
                })
                .and_then(|stamp| DateTime::from_timestamp(stamp, 0))
                .ok_or_else(|| anyhow!("Yahoo quote has no valid market timestamp"))?,
        })
    }

    /// Parse historical OHLCV data from Yahoo response
    fn parse_ohlcv_response(
        symbol: &Symbol,
        timeframe: &Timeframe,
        body: &str,
    ) -> Result<Vec<OHLCV>> {
        let response: YahooChartResponse = serde_json::from_str(body)
            .map_err(|e| anyhow!("Failed to parse Yahoo response: {}", e))?;

        let result = response
            .chart
            .result
            .first()
            .ok_or_else(|| anyhow!("No results in Yahoo response"))?;

        let timestamps = result
            .timestamp
            .as_ref()
            .ok_or_else(|| anyhow!("No timestamps in response"))?;

        let indicators = result
            .indicators
            .as_ref()
            .ok_or_else(|| anyhow!("No indicators in response"))?;

        let quotes = indicators
            .quote
            .as_ref()
            .and_then(|q| q.first())
            .ok_or_else(|| anyhow!("No quote data in response"))?;

        let opens = quotes
            .open
            .as_ref()
            .ok_or_else(|| anyhow!("No open data"))?;
        let highs = quotes
            .high
            .as_ref()
            .ok_or_else(|| anyhow!("No high data"))?;
        let lows = quotes.low.as_ref().ok_or_else(|| anyhow!("No low data"))?;
        let closes = quotes
            .close
            .as_ref()
            .ok_or_else(|| anyhow!("No close data"))?;
        let volumes = quotes
            .volume
            .as_ref()
            .ok_or_else(|| anyhow!("No volume data"))?;

        let mut bars = Vec::with_capacity(timestamps.len());

        for i in 0..timestamps.len() {
            let time = DateTime::from_timestamp(timestamps[i], 0)
                .ok_or_else(|| anyhow!("Invalid timestamp"))?;

            let open = opens.get(i).copied().flatten();
            let high = highs.get(i).copied().flatten();
            let low = lows.get(i).copied().flatten();
            let close = closes.get(i).copied().flatten();
            let volume = volumes.get(i).copied().flatten();

            // Skip bars with missing data
            if let (Some(o), Some(h), Some(l), Some(c), Some(v)) = (open, high, low, close, volume)
            {
                bars.push(OHLCV {
                    time,
                    symbol: symbol.clone(),
                    timeframe: timeframe.clone(),
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume: v,
                });
            }
        }

        Ok(bars)
    }

    /// Fetch a quote from Yahoo Finance
    async fn fetch_quote(&self, symbol: &Symbol) -> Result<Quote> {
        let yahoo_symbol = Self::format_symbol(symbol);
        let url = format!("{}/{}?interval=1d&range=1d", YAHOO_API_BASE, yahoo_symbol);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Yahoo Finance request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Yahoo Finance returned status {}",
                response.status()
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read Yahoo response body: {}", e))?;

        Self::parse_quote_response(symbol, &body)
    }
}

impl Default for YahooFinanceAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketDataPort for YahooFinanceAdapter {
    /// Delayed public data: any broker feed for the same symbol takes precedence.
    fn priority(&self) -> u8 {
        0
    }
    fn releases_on_unsubscribe(&self) -> bool {
        true
    }
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream> {
        let (tx, rx) = mpsc::channel(1024);
        let client = self.client.clone();
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

        // Polling loop: fetch quotes and emit ticks every POLL_INTERVAL.
        // Spawned once — subsequent subscribe() calls only extend the shared
        // symbol set, which the loop re-reads each pass. The extra receiver
        // handed back to the caller simply stays idle.
        if self
            .poll_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(rx);
        }
        let subscribed = self.subscribed_symbols.clone();
        tokio::spawn(async move {
            loop {
                let symbols: Vec<Symbol> = subscribed.read().await.clone();
                for symbol in &symbols {
                    let yahoo_symbol = YahooFinanceAdapter::format_symbol(symbol);
                    let url = format!("{}/{}?interval=1d&range=1d", YAHOO_API_BASE, yahoo_symbol);

                    match client.get(&url).send().await {
                        Ok(response) => {
                            if let Ok(body) = response.text().await {
                                if let Ok(quote) =
                                    YahooFinanceAdapter::parse_quote_response(symbol, &body)
                                {
                                    let tick = Tick {
                                        time: quote.updated_at,
                                        symbol: symbol.clone(),
                                        bid: quote.bid,
                                        ask: quote.ask,
                                        last: quote.last,
                                        volume: quote.volume,
                                        source: "yahoo".into(),
                                        open: Some(quote.open),
                                        change_pct: Some(quote.change_pct),
                                    };

                                    if tx.send(tick).await.is_err() {
                                        return;
                                    }

                                    *status.write().await = AdapterHealth::Healthy;
                                }
                            }
                        }
                        Err(e) => {
                            *status.write().await =
                                AdapterHealth::Degraded(format!("Yahoo fetch error: {}", e));
                        }
                    }
                }

                tokio::time::sleep(POLL_INTERVAL).await;
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
        let yahoo_symbol = Self::format_symbol(symbol);
        let interval = Self::timeframe_to_interval(&timeframe);
        let url = format!(
            "{}/{}?interval={}&period1={}&period2={}",
            YAHOO_API_BASE,
            yahoo_symbol,
            interval,
            from.timestamp(),
            to.timestamp(),
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Yahoo Finance request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Yahoo Finance returned status {}",
                response.status()
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read Yahoo response body: {}", e))?;

        Self::parse_ohlcv_response(symbol, &timeframe, &body)
    }

    fn adapter_id(&self) -> &'static str {
        "yahoo"
    }

    fn health(&self) -> AdapterHealth {
        self.status
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(AdapterHealth::Healthy)
    }
}

// --- Yahoo Finance API response types ---

#[derive(Debug, Deserialize)]
struct YahooChartResponse {
    chart: YahooChart,
}

#[derive(Debug, Deserialize)]
struct YahooChart {
    result: Vec<YahooChartResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YahooChartResult {
    meta: YahooMeta,
    timestamp: Option<Vec<i64>>,
    indicators: Option<YahooIndicators>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YahooMeta {
    regular_market_price: f64,
    regular_market_time: Option<i64>,
    chart_previous_close: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct YahooIndicators {
    quote: Option<Vec<YahooQuoteData>>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteData {
    open: Option<Vec<Option<f64>>>,
    high: Option<Vec<Option<f64>>>,
    low: Option<Vec<Option<f64>>>,
    close: Option<Vec<Option<f64>>>,
    volume: Option<Vec<Option<u64>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_QUOTE_RESPONSE: &str = r#"{
        "chart": {
            "result": [{
                "meta": {
                    "currency": "USD",
                    "symbol": "AAPL",
                    "regularMarketPrice": 175.50,
                    "chartPreviousClose": 174.00
                },
                "timestamp": [1700000000],
                "indicators": {
                    "quote": [{
                        "open": [174.50],
                        "high": [176.00],
                        "low": [174.00],
                        "close": [175.50],
                        "volume": [50000000]
                    }]
                }
            }]
        }
    }"#;

    #[test]
    fn quote_preserves_market_time_instead_of_refresh_time() {
        let quote = YahooFinanceAdapter::parse_quote_response(
            &Symbol("AAPL".into()),
            SAMPLE_QUOTE_RESPONSE,
        )
        .unwrap();
        assert_eq!(quote.updated_at.timestamp(), 1700000000);
        let with_meta = SAMPLE_QUOTE_RESPONSE.replace(
            "\"regularMarketPrice\": 175.50",
            "\"regularMarketTime\": 1700000005, \"regularMarketPrice\": 175.50",
        );
        let quote =
            YahooFinanceAdapter::parse_quote_response(&Symbol("AAPL".into()), &with_meta).unwrap();
        assert_eq!(quote.updated_at.timestamp(), 1700000005);
    }

    const SAMPLE_HISTORICAL_RESPONSE: &str = r#"{
        "chart": {
            "result": [{
                "meta": {
                    "currency": "USD",
                    "symbol": "AAPL",
                    "regularMarketPrice": 175.50,
                    "chartPreviousClose": 170.00
                },
                "timestamp": [1699900000, 1699986400, 1700072800],
                "indicators": {
                    "quote": [{
                        "open": [170.00, 172.00, 174.00],
                        "high": [172.50, 174.50, 176.00],
                        "low": [169.50, 171.50, 173.50],
                        "close": [172.00, 174.00, 175.50],
                        "volume": [45000000, 48000000, 50000000]
                    }]
                }
            }]
        }
    }"#;

    #[test]
    fn test_adapter_id() {
        let adapter = YahooFinanceAdapter::new();
        assert_eq!(adapter.adapter_id(), "yahoo");
    }

    #[test]
    fn test_health_default() {
        let adapter = YahooFinanceAdapter::new();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
    }

    #[test]
    fn test_format_symbol_plain() {
        let symbol = Symbol("AAPL".into());
        assert_eq!(YahooFinanceAdapter::format_symbol(&symbol), "AAPL");
    }

    #[test]
    fn test_format_symbol_nse() {
        let symbol = Symbol("RELIANCE.NS".into());
        assert_eq!(YahooFinanceAdapter::format_symbol(&symbol), "RELIANCE.NS");
    }

    #[test]
    fn test_parse_quote_response() {
        let symbol = Symbol("AAPL".into());
        let quote =
            YahooFinanceAdapter::parse_quote_response(&symbol, SAMPLE_QUOTE_RESPONSE).unwrap();
        assert_eq!(quote.symbol, Symbol("AAPL".into()));
        assert!((quote.last - 175.50).abs() < 0.01);
        assert!((quote.open - 174.50).abs() < 0.01);
        assert!((quote.high - 176.00).abs() < 0.01);
        assert!((quote.low - 174.00).abs() < 0.01);
        assert_eq!(quote.volume, 50000000);
        // change_pct = (175.50 - 174.00) / 174.00 * 100 ≈ 0.862
        assert!((quote.change_pct - 0.862).abs() < 0.01);
    }

    #[test]
    fn test_parse_historical_response() {
        let symbol = Symbol("AAPL".into());
        let bars = YahooFinanceAdapter::parse_ohlcv_response(
            &symbol,
            &Timeframe::D1,
            SAMPLE_HISTORICAL_RESPONSE,
        )
        .unwrap();
        assert_eq!(bars.len(), 3);
        assert!((bars[0].open - 170.00).abs() < 0.01);
        assert!((bars[0].close - 172.00).abs() < 0.01);
        assert!((bars[2].close - 175.50).abs() < 0.01);
        assert_eq!(bars[1].volume, 48000000);
    }

    #[test]
    fn test_timeframe_to_interval() {
        assert_eq!(
            YahooFinanceAdapter::timeframe_to_interval(&Timeframe::M1),
            "1m"
        );
        assert_eq!(
            YahooFinanceAdapter::timeframe_to_interval(&Timeframe::D1),
            "1d"
        );
        assert_eq!(
            YahooFinanceAdapter::timeframe_to_interval(&Timeframe::W1),
            "1wk"
        );
        assert_eq!(
            YahooFinanceAdapter::timeframe_to_interval(&Timeframe::H1),
            "1h"
        );
    }

    #[test]
    fn test_parse_invalid_response() {
        let symbol = Symbol("AAPL".into());
        let result = YahooFinanceAdapter::parse_quote_response(&symbol, "invalid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_results() {
        let symbol = Symbol("AAPL".into());
        let result =
            YahooFinanceAdapter::parse_quote_response(&symbol, r#"{"chart": {"result": []}}"#);
        assert!(result.is_err());
    }
}
