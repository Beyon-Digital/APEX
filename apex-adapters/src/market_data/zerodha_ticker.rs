//! KiteTicker WebSocket client with a REST-polling fallback.
//!
//! One connection per API key serves every subscriber. Subscribers register a
//! token->symbol map and receive [`Tick`]s through their own channel. Order
//! postbacks arriving on the same socket are republished on the session.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apex_core::domain::models::{Symbol, Tick};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::kite::instruments::InstrumentMaster;
use crate::kite::orders::order_from_json;
use crate::kite::packets::{decode_frame, TickerQuote};
use crate::kite::{Bucket, KiteSession};

/// Kite allows at most 3000 instruments per WebSocket connection.
const MAX_INSTRUMENTS: usize = 3000;
/// After this many consecutive WebSocket failures, poll `/quote` instead.
const POLL_FALLBACK_AFTER: u32 = 3;
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// `/quote` accepts up to 500 instruments per request.
const QUOTE_BATCH: usize = 200;

struct Subscriber {
    symbols: HashMap<u32, Symbol>,
    tx: mpsc::Sender<Tick>,
}

enum Command {
    Subscribe(Vec<u32>),
}

pub struct TickerHub {
    session: Arc<KiteSession>,
    instruments: Arc<InstrumentMaster>,
    subscribers: Mutex<Vec<Subscriber>>,
    commands: Mutex<Option<mpsc::UnboundedSender<Command>>>,
    started: AtomicBool,
    connected: AtomicBool,
    failures: AtomicU32,
}

impl TickerHub {
    pub fn new(session: Arc<KiteSession>, instruments: Arc<InstrumentMaster>) -> Arc<Self> {
        Arc::new(Self {
            session,
            instruments,
            subscribers: Mutex::new(Vec::new()),
            commands: Mutex::new(None),
            started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            failures: AtomicU32::new(0),
        })
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Relaxed)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// True while the hub has given up on the socket and is polling REST.
    pub fn is_polling(&self) -> bool {
        self.failures.load(Ordering::Relaxed) >= POLL_FALLBACK_AFTER
    }

    /// Register a subscriber for `symbols` (token -> APEX symbol) and make sure
    /// the background connection is running.
    pub fn subscribe(self: &Arc<Self>, symbols: HashMap<u32, Symbol>, tx: mpsc::Sender<Tick>) {
        let tokens: Vec<u32> = symbols.keys().copied().collect();
        self.subscribers.lock().unwrap().push(Subscriber { symbols, tx });
        self.start();
        if let Some(commands) = self.commands.lock().unwrap().as_ref() {
            let _ = commands.send(Command::Subscribe(tokens));
        }
    }

    /// Start the background connection loop (idempotent). Needs a Tokio runtime.
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        *self.commands.lock().unwrap() = Some(tx);
        let hub = self.clone();
        tokio::spawn(async move { hub.run(rx).await });
    }

    fn all_tokens(&self) -> Vec<u32> {
        let mut set = HashSet::new();
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|s| !s.tx.is_closed());
        for s in subscribers.iter() {
            set.extend(s.symbols.keys().copied());
        }
        let mut tokens: Vec<u32> = set.into_iter().collect();
        tokens.truncate(MAX_INSTRUMENTS);
        tokens
    }

    async fn run(self: Arc<Self>, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            if !self.session.is_authenticated() {
                self.connected.store(false, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            if self.is_polling() {
                self.poll_quotes().await;
                // Retry the socket now and then in case the cause was transient.
                if self.failures.load(Ordering::Relaxed) % 30 == 0 {
                    self.failures.store(POLL_FALLBACK_AFTER - 1, Ordering::Relaxed);
                } else {
                    self.failures.fetch_add(1, Ordering::Relaxed);
                }
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            match self.session_once(&mut commands).await {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(error) => {
                    let n = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
                    warn!(%error, attempt = n, "KiteTicker connection failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
            self.connected.store(false, Ordering::Relaxed);
        }
    }

    /// One WebSocket session. Returns `Ok` on a clean close, `Err` on failure.
    async fn session_once(
        &self,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> anyhow::Result<()> {
        let url = self.session.ws_url()?;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await?;
        info!("KiteTicker connected");
        self.connected.store(true, Ordering::Relaxed);
        self.failures.store(0, Ordering::Relaxed);

        let tokens = self.all_tokens();
        if !tokens.is_empty() {
            send_subscription(&mut ws, &tokens).await?;
        }

        let mut idle_check = tokio::time::interval(Duration::from_secs(20));
        loop {
            tokio::select! {
                message = ws.next() => match message {
                    Some(Ok(Message::Binary(frame))) => self.dispatch_frame(&frame),
                    Some(Ok(Message::Text(text))) => self.handle_text(&text),
                    Some(Ok(Message::Ping(data))) => ws.send(Message::Pong(data)).await?,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.into()),
                },
                command = commands.recv() => match command {
                    Some(Command::Subscribe(tokens)) if !tokens.is_empty() => {
                        send_subscription(&mut ws, &tokens).await?;
                    }
                    Some(_) => {}
                    None => return Ok(()),
                },
                _ = idle_check.tick() => {
                    // A token cleared by a TokenException elsewhere ends the session.
                    if !self.session.is_authenticated() {
                        let _ = ws.close(None).await;
                        return Ok(());
                    }
                    if self.all_tokens().is_empty() && self.subscribers.lock().unwrap().is_empty() {
                        debug!("KiteTicker has no subscribers; staying connected for order updates");
                    }
                }
            }
        }
    }

    fn handle_text(&self, text: &str) {
        let Ok(value) = serde_json::from_str::<Value>(text) else { return };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("order") => {
                if let Some(order) = value.get("data").and_then(order_from_json) {
                    self.session.publish_order(order);
                }
            }
            Some("error") => {
                let message = value.get("data").map(|d| d.to_string()).unwrap_or_default();
                warn!(%message, "KiteTicker error");
            }
            _ => {}
        }
    }

    fn dispatch_frame(&self, frame: &[u8]) {
        for quote in decode_frame(frame, &self.instruments) {
            self.dispatch(&quote);
        }
    }

    fn dispatch(&self, q: &TickerQuote) {
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|s| !s.tx.is_closed());
        for subscriber in subscribers.iter() {
            let Some(symbol) = subscriber.symbols.get(&q.token) else { continue };
            let tick = Tick {
                time: Utc::now(),
                symbol: symbol.clone(),
                bid: q.bid.unwrap_or(q.last),
                ask: q.ask.unwrap_or(q.last),
                last: q.last,
                volume: q.volume.unwrap_or(0),
                source: "zerodha_kite".to_string(),
                open: q.open,
                change_pct: q.change_pct(),
            };
            // A slow consumer drops ticks instead of stalling the socket.
            let _ = subscriber.tx.try_send(tick);
        }
    }

    /// REST fallback for accounts without WebSocket access: batch `/quote`.
    async fn poll_quotes(&self) {
        let tokens = self.all_tokens();
        let keys: Vec<String> = tokens
            .iter()
            .filter_map(|t| self.instruments.by_token(*t).map(|i| i.key()))
            .collect();
        for batch in keys.chunks(QUOTE_BATCH) {
            let query: Vec<(&str, String)> = batch.iter().map(|k| ("i", k.clone())).collect();
            match self.session.get("/quote", &query, Bucket::Quote).await {
                Ok(data) => {
                    if let Some(map) = data.as_object() {
                        for entry in map.values() {
                            if let Some(q) = quote_from_rest(entry) {
                                self.dispatch(&q);
                            }
                        }
                    }
                }
                Err(error) => {
                    warn!(%error, "Kite quote poll failed");
                    return;
                }
            }
        }
    }
}

async fn send_subscription<S>(ws: &mut S, tokens: &[u32]) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    for chunk in tokens.chunks(500) {
        ws.send(Message::Text(json!({"a": "subscribe", "v": chunk}).to_string())).await?;
        ws.send(Message::Text(json!({"a": "mode", "v": ["full", chunk]}).to_string())).await?;
    }
    Ok(())
}

/// Convert one `/quote` entry into the same shape the socket produces.
pub fn quote_from_rest(entry: &Value) -> Option<TickerQuote> {
    let token = entry.get("instrument_token").and_then(|t| t.as_u64())? as u32;
    let last = entry.get("last_price").and_then(|v| v.as_f64())?;
    let ohlc = entry.get("ohlc");
    let ohlc_field = |name: &str| ohlc.and_then(|o| o.get(name)).and_then(|v| v.as_f64());
    let depth_price = |side: &str| {
        entry
            .get("depth")
            .and_then(|d| d.get(side))
            .and_then(|s| s.as_array())
            .and_then(|levels| levels.first())
            .and_then(|l| l.get("price"))
            .and_then(|p| p.as_f64())
            .filter(|p| *p > 0.0)
    };
    Some(TickerQuote {
        token,
        last,
        bid: depth_price("buy"),
        ask: depth_price("sell"),
        open: ohlc_field("open"),
        high: ohlc_field("high"),
        low: ohlc_field("low"),
        close: ohlc_field("close"),
        volume: entry.get("volume").and_then(|v| v.as_u64()),
        exchange_ts: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_quote_matches_socket_shape() {
        let q = quote_from_rest(&json!({
            "instrument_token": 738561,
            "last_price": 1250.0,
            "volume": 1000,
            "ohlc": {"open": 1240.0, "high": 1255.0, "low": 1238.0, "close": 1230.0},
            "depth": {"buy": [{"price": 1249.9}], "sell": [{"price": 1250.1}]}
        }))
        .unwrap();
        assert_eq!(q.token, 738561);
        assert_eq!(q.bid, Some(1249.9));
        assert_eq!(q.ask, Some(1250.1));
        assert!((q.change_pct().unwrap() - (1250.0 - 1230.0) / 1230.0 * 100.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn dispatch_routes_ticks_by_token_and_drops_closed_subscribers() {
        let session = KiteSession::new("k".into(), None).unwrap();
        let hub = TickerHub::new(session, Arc::new(InstrumentMaster::new()));
        let (tx, mut rx) = mpsc::channel(8);
        let (closed_tx, closed_rx) = mpsc::channel(8);
        drop(closed_rx);
        {
            let mut subs = hub.subscribers.lock().unwrap();
            subs.push(Subscriber {
                symbols: HashMap::from([(1, Symbol("AAA.NS".into()))]),
                tx,
            });
            subs.push(Subscriber {
                symbols: HashMap::from([(1, Symbol("AAA.NS".into()))]),
                tx: closed_tx,
            });
        }
        let quote = |token| TickerQuote {
            token,
            last: 10.0,
            bid: None,
            ask: None,
            open: Some(9.0),
            high: None,
            low: None,
            close: Some(8.0),
            volume: Some(5),
            exchange_ts: None,
        };
        hub.dispatch(&quote(2)); // nobody subscribed to token 2
        hub.dispatch(&quote(1));
        let tick = rx.try_recv().unwrap();
        assert_eq!(tick.symbol.0, "AAA.NS");
        assert_eq!(tick.bid, 10.0);
        assert!(rx.try_recv().is_err());
        assert_eq!(hub.subscribers.lock().unwrap().len(), 1);
    }
}
