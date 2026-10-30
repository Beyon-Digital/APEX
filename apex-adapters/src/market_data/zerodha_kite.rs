use anyhow::{anyhow, Result};
use apex_core::{
    domain::models::*,
    ports::market_data::{AdapterHealth, MarketDataPort, TickStream},
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::zerodha_ticker::{quote_from_rest, TickerHub};
use crate::kite::instruments::{Instrument, InstrumentMaster};
use crate::kite::{is_explicit_indian_symbol, parse_kite_time, parse_symbol, Bucket, KiteSession};

/// Zerodha Kite market data adapter
///
/// Real-time ticks come from the KiteTicker WebSocket (falling back to batched
/// REST polling when the socket is unavailable). Historical candles use the
/// instrument token from the instruments master.
/// Documentation: https://kite.trade/docs/connect/v3/
pub struct ZerodhaKiteAdapter {
    session: Arc<KiteSession>,
    instruments: Arc<InstrumentMaster>,
    hub: Arc<TickerHub>,
}

impl ZerodhaKiteAdapter {
    /// Create a new Zerodha Kite adapter with its own session.
    ///
    /// # Arguments
    /// * `api_key` - Zerodha API key from the Kite Connect console
    /// * `access_token` - Access token obtained after login flow
    pub fn new(api_key: String, access_token: Option<String>) -> Result<Self> {
        Ok(Self::from_shared(
            KiteSession::new(api_key, access_token)?,
            Arc::new(InstrumentMaster::new()),
        ))
    }

    /// Create an adapter on a session shared with the execution adapter.
    pub fn from_shared(session: Arc<KiteSession>, instruments: Arc<InstrumentMaster>) -> Self {
        let hub = TickerHub::new(session.clone(), instruments.clone());
        Self { session, instruments, hub }
    }

    pub fn session(&self) -> &Arc<KiteSession> {
        &self.session
    }

    pub fn instruments(&self) -> &Arc<InstrumentMaster> {
        &self.instruments
    }

    /// Start the WebSocket connection without waiting for a subscription, so
    /// order updates flow as soon as the user logs in. Needs a Tokio runtime.
    pub fn start_background(&self) {
        if self.session.is_authenticated() {
            self.hub.start();
        }
    }

    pub fn set_access_token(&self, token: String) {
        self.session.set_token(token);
        self.start_background_if_runtime();
    }

    pub fn clear_access_token(&self) {
        self.session.clear_token();
    }

    pub fn is_authenticated(&self) -> bool {
        self.session.is_authenticated()
    }

    fn start_background_if_runtime(&self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            self.start_background();
        }
    }

    /// Map APEX timeframe to Zerodha interval string and the longest window
    /// Kite serves per request.
    fn timeframe_to_interval(tf: &Timeframe) -> Option<(&'static str, i64)> {
        match tf {
            Timeframe::M1 => Some(("minute", 60)),
            Timeframe::M3 => Some(("3minute", 100)),
            Timeframe::M5 => Some(("5minute", 100)),
            Timeframe::M15 => Some(("15minute", 200)),
            Timeframe::M30 => Some(("30minute", 200)),
            Timeframe::H1 => Some(("60minute", 400)),
            Timeframe::D1 => Some(("day", 2000)),
            _ => None,
        }
    }

    async fn resolve(&self, symbol: &Symbol) -> Result<Instrument> {
        self.instruments.ensure_loaded(&self.session).await?;
        self.instruments
            .resolve(symbol)
            .ok_or_else(|| anyhow!("{} is not a known Zerodha instrument", symbol.0))
    }
}

#[async_trait]
impl MarketDataPort for ZerodhaKiteAdapter {
    fn subscription_ready(&self) -> bool {
        self.is_authenticated()
    }

    fn supports_symbol(&self, symbol: &Symbol) -> bool {
        if is_explicit_indian_symbol(&symbol.0) {
            return true;
        }
        // Bare symbols are only claimed once the master confirms them, so US
        // tickers like AAPL never get routed here.
        self.instruments.is_loaded() && self.instruments.resolve(symbol).is_some()
    }

    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream> {
        if !self.is_authenticated() {
            return Err(anyhow!("Not authenticated with Zerodha"));
        }
        self.instruments.ensure_loaded(&self.session).await?;

        let mut tokens: HashMap<u32, Symbol> = HashMap::new();
        for symbol in symbols {
            match self.instruments.resolve(symbol) {
                Some(instrument) => {
                    tokens.insert(instrument.token, symbol.clone());
                }
                None => warn!(symbol = %symbol.0, "Symbol not found in Zerodha instruments"),
            }
        }
        if tokens.is_empty() {
            return Err(anyhow!("None of the requested symbols exist on Zerodha"));
        }

        let (tx, rx) = mpsc::channel(2048);
        let count = tokens.len();
        self.hub.subscribe(tokens, tx);
        info!("Subscribed to {count} symbols via Zerodha KiteTicker");
        Ok(rx)
    }

    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()> {
        // Subscriber channels close when the aggregator drops its receiver; the
        // hub prunes them on the next tick.
        debug!("Unsubscribed from {} symbols", symbols.len());
        Ok(())
    }

    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote> {
        if !self.is_authenticated() {
            return Err(anyhow!("Not authenticated with Zerodha"));
        }
        let key = parse_symbol(&symbol.0).key();
        let data = self
            .session
            .get("/quote", &[("i", key.clone())], Bucket::Quote)
            .await?;
        let entry = data
            .get(&key)
            .ok_or_else(|| anyhow!("Quote for {key} not found in response"))?;
        let q = quote_from_rest(entry).ok_or_else(|| anyhow!("Malformed quote for {key}"))?;
        let ohlc = entry.get("ohlc");
        let field = |name: &str| ohlc.and_then(|o| o.get(name)).and_then(|v| v.as_f64());
        let vwap = entry
            .get("average_price")
            .and_then(|v| v.as_f64())
            .filter(|v| *v > 0.0)
            .unwrap_or(q.last);
        Ok(Quote {
            symbol: symbol.clone(),
            bid: q.bid.unwrap_or(q.last),
            ask: q.ask.unwrap_or(q.last),
            last: q.last,
            open: field("open").unwrap_or(q.last),
            high: field("high").unwrap_or(q.last),
            low: field("low").unwrap_or(q.last),
            volume: q.volume.unwrap_or(0),
            change_pct: q.change_pct().unwrap_or(0.0),
            vwap,
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
            return Err(anyhow!("Not authenticated with Zerodha"));
        }
        let (interval, max_days) = Self::timeframe_to_interval(&timeframe)
            .ok_or_else(|| anyhow!("Timeframe {timeframe:?} is not supported by Zerodha"))?;
        let instrument = self.resolve(symbol).await?;
        let ist = FixedOffset::east_opt(5 * 3600 + 1800).expect("valid offset");
        let format_ist = |t: DateTime<Utc>| t.with_timezone(&ist).format("%Y-%m-%d %H:%M:%S").to_string();

        // Kite caps the window per request, so walk the range in chunks.
        let mut bars: Vec<OHLCV> = Vec::new();
        let mut cursor = from;
        while cursor < to {
            let end = (cursor + Duration::days(max_days)).min(to);
            let data = self
                .session
                .get(
                    &format!("/instruments/historical/{}/{interval}", instrument.token),
                    &[("from", format_ist(cursor)), ("to", format_ist(end))],
                    Bucket::Historical,
                )
                .await?;
            for candle in data.get("candles").and_then(|c| c.as_array()).into_iter().flatten() {
                // [timestamp, open, high, low, close, volume]
                let Some(fields) = candle.as_array().filter(|f| f.len() >= 6) else { continue };
                let Some(time) = fields[0].as_str().and_then(parse_kite_time) else { continue };
                bars.push(OHLCV {
                    time,
                    symbol: symbol.clone(),
                    timeframe: timeframe.clone(),
                    open: fields[1].as_f64().unwrap_or(0.0),
                    high: fields[2].as_f64().unwrap_or(0.0),
                    low: fields[3].as_f64().unwrap_or(0.0),
                    close: fields[4].as_f64().unwrap_or(0.0),
                    volume: fields[5].as_u64().unwrap_or(0),
                });
            }
            cursor = end;
        }
        bars.sort_by_key(|b| b.time);
        bars.dedup_by_key(|b| b.time);
        debug!("Fetched {} historical bars for {} from Zerodha", bars.len(), symbol.0);
        Ok(bars)
    }

    fn adapter_id(&self) -> &'static str {
        "zerodha_kite"
    }

    fn health(&self) -> AdapterHealth {
        match self.session.health() {
            AdapterHealth::Healthy if self.hub.is_started() && self.hub.is_polling() => {
                AdapterHealth::Degraded("KiteTicker unavailable; polling REST quotes".into())
            }
            AdapterHealth::Healthy if self.hub.is_started() && !self.hub.is_connected() => {
                AdapterHealth::Degraded("KiteTicker connecting".into())
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CSV: &str = "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n\
738561,2885,RELIANCE,RELIANCE INDUSTRIES,0,,0,0.05,1,EQ,NSE,NSE\n";

    fn adapter(server: &MockServer) -> ZerodhaKiteAdapter {
        let session =
            KiteSession::with_base_url("key".into(), Some("tok".into()), server.uri()).unwrap();
        let instruments = Arc::new(InstrumentMaster::new());
        instruments.load_csv(CSV);
        ZerodhaKiteAdapter::from_shared(session, instruments)
    }

    #[test]
    fn test_timeframe_mapping() {
        assert_eq!(ZerodhaKiteAdapter::timeframe_to_interval(&Timeframe::M1), Some(("minute", 60)));
        assert_eq!(ZerodhaKiteAdapter::timeframe_to_interval(&Timeframe::M5).unwrap().0, "5minute");
        assert_eq!(ZerodhaKiteAdapter::timeframe_to_interval(&Timeframe::D1).unwrap().0, "day");
        assert!(ZerodhaKiteAdapter::timeframe_to_interval(&Timeframe::S1).is_none());
    }

    #[tokio::test]
    async fn supports_only_known_indian_symbols() {
        let server = MockServer::start().await;
        let adapter = adapter(&server);
        assert!(adapter.supports_symbol(&Symbol("RELIANCE.NS".into())));
        assert!(adapter.supports_symbol(&Symbol("RELIANCE".into())));
        assert!(!adapter.supports_symbol(&Symbol("AAPL".into())));
        assert!(!adapter.supports_symbol(&Symbol("BTCUSDT".into())));
    }

    #[tokio::test]
    async fn snapshot_reads_depth_and_change() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("i", "NSE:RELIANCE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"NSE:RELIANCE": {
                    "instrument_token": 738561, "last_price": 1250.0, "volume": 5000,
                    "average_price": 1245.0,
                    "ohlc": {"open": 1240.0, "high": 1255.0, "low": 1238.0, "close": 1230.0},
                    "depth": {"buy": [{"price": 1249.9}], "sell": [{"price": 1250.1}]}
                }}
            })))
            .mount(&server)
            .await;
        let quote = adapter(&server).get_snapshot(&Symbol("RELIANCE.NS".into())).await.unwrap();
        assert_eq!(quote.bid, 1249.9);
        assert_eq!(quote.vwap, 1245.0);
        assert!((quote.change_pct - 1.626).abs() < 0.01);
    }

    #[tokio::test]
    async fn historical_uses_instrument_token_and_parses_kite_timestamps() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/instruments/historical/738561/day"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"candles": [
                    ["2024-01-15T00:00:00+0530", 100.0, 110.0, 95.0, 105.0, 1000],
                    ["2024-01-16T00:00:00+0530", 105.0, 112.0, 101.0, 111.0, 2000]
                ]}
            })))
            .mount(&server)
            .await;
        let from = Utc::now() - Duration::days(5);
        let bars = adapter(&server)
            .get_historical_ohlcv(&Symbol("RELIANCE.NS".into()), Timeframe::D1, from, Utc::now())
            .await
            .unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].close, 105.0);
        assert_eq!(bars[0].time.to_rfc3339(), "2024-01-14T18:30:00+00:00");
    }

    #[tokio::test]
    async fn subscribe_rejects_unknown_symbols() {
        let server = MockServer::start().await;
        let adapter = adapter(&server);
        assert!(adapter.subscribe(&[Symbol("NOPE.NS".into())]).await.is_err());
    }
}
