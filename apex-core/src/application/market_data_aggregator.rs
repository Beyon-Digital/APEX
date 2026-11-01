use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::{info, info_span, warn};

use crate::application::data_quality::DataQualityChecker;
use crate::bus::message_bus::{BusMessage, MessageBus, Topic};
use crate::domain::models::*;
use crate::ports::market_data::AdapterHealth;
use crate::ports::market_data::MarketDataPort;
use crate::ports::storage::StoragePort;

/// How long a feed keeps a symbol after its last tick before lower-priority
/// feeds may take over again.
const SOURCE_HOLD: Duration = Duration::from_secs(10);

/// Decide whether a tick from a feed of `priority` may update `symbol`.
/// A higher-priority feed that ticked within [`SOURCE_HOLD`] blocks lower ones.
fn claim_symbol(
    owners: &DashMap<String, (u8, std::time::Instant)>,
    symbol: &str,
    priority: u8,
) -> bool {
    let now = std::time::Instant::now();
    let mut entry = owners.entry(symbol.to_string()).or_insert((priority, now));
    let (owner_priority, last_seen) = *entry;
    if owner_priority > priority && now.duration_since(last_seen) < SOURCE_HOLD {
        return false;
    }
    *entry = (priority, now);
    true
}

/// Market Data Aggregator — the system's sensory cortex
pub struct MarketDataAggregator {
    adapters: RwLock<Vec<Arc<dyn MarketDataPort>>>,
    /// Every symbol ever passed to `start`, so adapters registered later
    /// can be brought up to date via `sync_subscriptions`.
    requested_symbols: Arc<RwLock<Vec<Symbol>>>,
    bus: Arc<MessageBus>,
    storage: Option<Arc<dyn StoragePort>>,
    quote_cache: Arc<DashMap<String, Quote>>,
    /// Aged market closes for display only; never published or used to fill orders.
    reference_quote_cache: Arc<DashMap<String, Quote>>,
    tick_buffer: Arc<Mutex<Vec<Tick>>>,
    symbol_map: HashMap<String, String>,
    /// Symbols each adapter is already streaming — repeat `start` calls only
    /// forward the delta so adapters don't stack duplicate sockets/pollers.
    subscribed_symbols: Arc<DashMap<String, HashSet<String>>>,
    flush_task_started: Arc<AtomicBool>,
    /// Highest-priority feed seen per symbol and when, so a delayed fallback
    /// feed cannot overwrite quotes from a live broker feed.
    source_priority: Arc<DashMap<String, (u8, std::time::Instant)>>,
    data_quality_checker: Arc<DataQualityChecker>,
}

impl MarketDataAggregator {
    /// Create a new Market Data Aggregator
    pub fn new(bus: Arc<MessageBus>) -> Self {
        Self {
            adapters: RwLock::new(Vec::new()),
            requested_symbols: Arc::new(RwLock::new(Vec::new())),
            bus,
            storage: None,
            quote_cache: Arc::new(DashMap::new()),
            reference_quote_cache: Arc::new(DashMap::new()),
            tick_buffer: Arc::new(Mutex::new(Vec::new())),
            symbol_map: HashMap::new(),
            subscribed_symbols: Arc::new(DashMap::new()),
            flush_task_started: Arc::new(AtomicBool::new(false)),
            source_priority: Arc::new(DashMap::new()),
            data_quality_checker: Arc::new(DataQualityChecker::new()),
        }
    }

    /// Add a market data adapter. Replaces any adapter with the same id.
    /// Safe to call while the aggregator is running; follow with
    /// `sync_subscriptions` to stream already-requested symbols from it.
    pub fn add_adapter(&self, adapter: Arc<dyn MarketDataPort>) {
        info!("Registering market data adapter: {}", adapter.adapter_id());
        let id = adapter.adapter_id().to_string();
        let mut adapters = self.adapters.write().expect("adapters lock poisoned");
        adapters.retain(|existing| existing.adapter_id() != id);
        // A replaced adapter starts with a clean subscription slate.
        self.subscribed_symbols.remove(&id);
        adapters.push(adapter);
    }

    /// Remove an adapter by id, returning it so the caller can shut it down.
    pub fn remove_adapter(&self, adapter_id: &str) -> Option<Arc<dyn MarketDataPort>> {
        let mut adapters = self.adapters.write().expect("adapters lock poisoned");
        let index = adapters
            .iter()
            .position(|adapter| adapter.adapter_id() == adapter_id)?;
        self.subscribed_symbols.remove(adapter_id);
        info!("Removed market data adapter: {adapter_id}");
        Some(adapters.remove(index))
    }

    /// Re-run `start` for every symbol requested so far (idempotent per adapter).
    pub async fn sync_subscriptions(&self) -> Result<()> {
        let symbols = self.requested_symbols();
        if symbols.is_empty() {
            return Ok(());
        }
        self.start(&symbols).await
    }

    /// Stop tracking symbols: they are no longer re-subscribed on new adapters and
    /// polling adapters stop fetching them.
    pub async fn unsubscribe(&self, symbols: &[Symbol]) {
        self.requested_symbols
            .write()
            .expect("requested symbols lock poisoned")
            .retain(|s| !symbols.contains(s));
        let adapters: Vec<Arc<dyn MarketDataPort>> =
            self.adapters.read().expect("adapters lock poisoned").clone();
        for adapter in adapters {
            let id = adapter.adapter_id().to_string();
            let owned: Vec<Symbol> = match self.subscribed_symbols.get_mut(&id) {
                Some(mut known) => symbols.iter().filter(|s| known.remove(&s.0)).cloned().collect(),
                None => continue,
            };
            if owned.is_empty() {
                continue;
            }
            if let Err(error) = adapter.unsubscribe(&owned).await {
                warn!(adapter = %id, %error, "Unsubscribe failed");
            }
        }
    }

    /// Symbols requested through `start` so far.
    pub fn requested_symbols(&self) -> Vec<Symbol> {
        self.requested_symbols
            .read()
            .expect("requested symbols lock poisoned")
            .clone()
    }

    /// Register a symbol alias mapping
    pub fn add_symbol_mapping(&mut self, alias: String, canonical: String) {
        self.symbol_map.insert(alias, canonical);
    }

    /// Attach a storage backend for durable tick persistence.
    pub fn set_storage(&mut self, storage: Arc<dyn StoragePort>) {
        self.storage = Some(storage);
    }

    /// Get the shared quote cache
    pub fn quote_cache(&self) -> Arc<DashMap<String, Quote>> {
        self.quote_cache.clone()
    }

    /// Resolve a symbol to its canonical form
    pub fn resolve_symbol(&self, symbol: &str) -> String {
        self.symbol_map
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| symbol.to_string())
    }

    /// Subscribe to symbols across all adapters and start processing
    #[tracing::instrument(skip(self, symbols))]
    pub async fn start(&self, symbols: &[Symbol]) -> Result<()> {
        let mut subscribed_any = false;
        let mut last_error = None;

        {
            let mut requested = self
                .requested_symbols
                .write()
                .expect("requested symbols lock poisoned");
            for symbol in symbols {
                if !requested.contains(symbol) {
                    requested.push(symbol.clone());
                }
            }
        }

        // Snapshot so the lock is never held across an await point.
        let adapters: Vec<Arc<dyn MarketDataPort>> =
            self.adapters.read().expect("adapters lock poisoned").clone();

        for adapter in &adapters {
            if !adapter.subscription_ready() {
                continue;
            }
            let adapter_id = adapter.adapter_id().to_string();
            let eligible: Vec<Symbol> = symbols
                .iter()
                .filter(|s| adapter.supports_symbol(s))
                .cloned()
                .collect();
            if eligible.is_empty() {
                continue;
            }

            // Forward only symbols this adapter hasn't seen yet — polling
            // adapters extend their shared set, WS adapters get one stream per
            // distinct call; resubscribing the same universe would leak tasks.
            let new_symbols: Vec<Symbol> = {
                let mut known = self
                    .subscribed_symbols
                    .entry(adapter_id.clone())
                    .or_default();
                eligible
                    .iter()
                    .filter(|s| known.insert(s.0.clone()))
                    .cloned()
                    .collect()
            };
            if new_symbols.is_empty() {
                // Existing successful subscriptions satisfy an idempotent start.
                subscribed_any = true;
                continue;
            }

            let mut tick_stream = match adapter.subscribe(&new_symbols).await {
                Ok(stream) => stream,
                Err(err) => {
                    warn!(adapter = %adapter_id, error = %err, "Market data adapter subscription failed");
                    // Unmark so a later `start` can retry this adapter.
                    let mut known = self
                        .subscribed_symbols
                        .entry(adapter_id.clone())
                        .or_default();
                    for s in &new_symbols {
                        known.remove(&s.0);
                    }
                    last_error = Some(err);
                    continue;
                }
            };

            subscribed_any = true;
            let bus = self.bus.clone();
            let quote_cache = self.quote_cache.clone();
            let reference_quote_cache = self.reference_quote_cache.clone();
            let tick_buffer = self.tick_buffer.clone();
            let data_quality_checker = self.data_quality_checker.clone();
            let subscribed_symbols = self.subscribed_symbols.clone();
            let source_priority = self.source_priority.clone();
            let priority = adapter.priority();

            // Spawn a task per adapter to read from its tick stream
            tokio::spawn(async move {
                while let Some(tick) = tick_stream.recv().await {
                    let symbol_key = tick.symbol.0.clone();
                    if !claim_symbol(&source_priority, &symbol_key, priority) {
                        continue; // a higher-priority feed is live for this symbol
                    }
                    let span =
                        info_span!("tick_pipeline", symbol = %symbol_key, source = %adapter_id);

                    // Validate tick with data quality checker
                    if data_quality_checker.validate_tick(&tick).is_err()
                        || tick.time > chrono::Utc::now() + chrono::Duration::seconds(5)
                    {
                        // Retain a well-formed historical close for the UI only. It
                        // must not enter the bus, trading cache, or strategy stream.
                        if tick.time <= chrono::Utc::now()
                            && data_quality_checker.validate_tick_values(&tick).is_ok()
                        {
                            let reference = Quote {
                                symbol: tick.symbol.clone(),
                                bid: tick.bid,
                                ask: tick.ask,
                                last: tick.last,
                                open: tick.open.unwrap_or(tick.last),
                                high: tick.last,
                                low: tick.last,
                                volume: tick.volume,
                                change_pct: tick.change_pct.unwrap_or(0.0),
                                vwap: tick.last,
                                updated_at: tick.time,
                            };
                            let mut entry = reference_quote_cache
                                .entry(symbol_key)
                                .or_insert_with(|| reference.clone());
                            if reference.updated_at > entry.updated_at {
                                *entry = reference;
                            }
                        }
                        continue;
                    }

                    let tick_clone = span.in_scope(|| {
                        // Update quote cache; accumulate day OHLC across ticks so
                        // WS feeds that only report last-trades still show a real
                        // open/high/low and the change % reported by poll adapters.
                        let prev = quote_cache.get(&symbol_key).map(|q| q.clone());
                        let quote = Quote {
                            symbol: tick.symbol.clone(),
                            bid: tick.bid,
                            ask: tick.ask,
                            last: tick.last,
                            open: tick
                                .open
                                .or_else(|| prev.as_ref().map(|p| p.open))
                                .unwrap_or(tick.last),
                            high: prev
                                .as_ref()
                                .map(|p| p.high.max(tick.last))
                                .unwrap_or(tick.last),
                            low: prev
                                .as_ref()
                                .map(|p| p.low.min(tick.last))
                                .unwrap_or(tick.last),
                            volume: tick.volume,
                            change_pct: tick
                                .change_pct
                                .or_else(|| prev.as_ref().map(|p| p.change_pct))
                                .unwrap_or(0.0),
                            vwap: tick.last,
                            updated_at: tick.time,
                        };
                        quote_cache.insert(symbol_key.clone(), quote.clone());

                        // Publish to message bus
                        bus.publish(
                            Topic::Tick(symbol_key.clone()),
                            BusMessage::TickData(tick.clone()),
                        );
                        bus.publish(Topic::Quote(symbol_key), BusMessage::QuoteData(quote));

                        tick
                    });

                    // Add to tick buffer for batch write
                    tick_buffer.lock().await.push(tick_clone);
                }
                if let Some(mut known) = subscribed_symbols.get_mut(&adapter_id) {
                    for symbol in &new_symbols {
                        known.remove(&symbol.0);
                    }
                }
                warn!("Tick stream ended for adapter: {}", adapter_id);
            });
        }

        if !subscribed_any && !adapters.is_empty() {
            if let Some(err) = last_error {
                return Err(err);
            }
        }

        // Start tick buffer flush task (every 100ms)
        if !self.flush_task_started.swap(true, Ordering::SeqCst) {
            let tick_buffer = self.tick_buffer.clone();
            let storage = self.storage.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(100));
                loop {
                    interval.tick().await;
                    let ticks: Vec<Tick> = {
                        let mut buffer = tick_buffer.lock().await;
                        if buffer.is_empty() {
                            continue;
                        }
                        buffer.drain(..).collect()
                    };

                    if let Some(storage) = storage.as_ref() {
                        if let Err(error) = storage.write_ticks(&ticks).await {
                            warn!(error = %error, tick_count = ticks.len(), "Failed to persist tick batch");
                        }
                    }
                }
            });
        }

        Ok(())
    }

    /// Get a snapshot quote for a symbol from cache
    pub fn get_cached_quote(&self, symbol: &str) -> Option<Quote> {
        self.quote_cache.get(symbol).map(|q| q.clone())
    }

    /// Return the latest available quote for display, including a dated close.
    /// Callers executing orders or strategies must use `get_cached_quote`.
    pub fn get_display_quote(&self, symbol: &str) -> Option<Quote> {
        let live = self.get_cached_quote(symbol);
        let reference = self.reference_quote_cache.get(symbol).map(|q| q.clone());
        match (live, reference) {
            (Some(a), Some(b)) => Some(if a.updated_at >= b.updated_at { a } else { b }),
            (Some(q), None) | (None, Some(q)) => Some(q),
            (None, None) => None,
        }
    }

    /// Get the number of registered adapters
    pub fn adapter_count(&self) -> usize {
        self.adapters.read().expect("adapters lock poisoned").len()
    }

    /// Snapshot market data adapter health by adapter id.
    pub fn adapter_health(&self) -> Vec<(String, AdapterHealth)> {
        self.adapters
            .read()
            .expect("adapters lock poisoned")
            .iter()
            .filter(|adapter| adapter.subscription_ready())
            .map(|adapter| (adapter.adapter_id().to_string(), adapter.health()))
            .collect()
    }

    /// Number of actively cached subscriptions/quotes.
    pub fn active_subscription_count(&self) -> usize {
        self.quote_cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    struct RoutedFeed {
        ready: Arc<AtomicBool>,
        requests: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }
    #[async_trait::async_trait]
    impl MarketDataPort for RoutedFeed {
        fn subscription_ready(&self) -> bool {
            self.ready.load(Ordering::SeqCst)
        }
        fn supports_symbol(&self, symbol: &Symbol) -> bool {
            symbol.0.contains('/')
        }
        async fn subscribe(
            &self,
            symbols: &[Symbol],
        ) -> Result<crate::ports::market_data::TickStream> {
            self.requests
                .lock()
                .unwrap()
                .push(symbols.iter().map(|s| s.0.clone()).collect());
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }
        async fn unsubscribe(&self, _: &[Symbol]) -> Result<()> {
            Ok(())
        }
        async fn get_snapshot(&self, _: &Symbol) -> Result<Quote> {
            anyhow::bail!("unused")
        }
        async fn get_historical_ohlcv(
            &self,
            _: &Symbol,
            _: Timeframe,
            _: chrono::DateTime<Utc>,
            _: chrono::DateTime<Utc>,
        ) -> Result<Vec<OHLCV>> {
            Ok(vec![])
        }
        fn adapter_id(&self) -> &'static str {
            "routed"
        }
        fn health(&self) -> AdapterHealth {
            AdapterHealth::Healthy
        }
    }

    #[tokio::test]
    async fn skips_unready_feed_routes_symbols_and_retries_ended_streams() {
        let ready = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut agg = MarketDataAggregator::new(Arc::new(MessageBus::new()));
        agg.add_adapter(Arc::new(RoutedFeed {
            ready: ready.clone(),
            requests: requests.clone(),
        }));
        let symbols = [Symbol("AAPL".into()), Symbol("BTC/USDT".into())];
        agg.start(&symbols).await.unwrap();
        assert!(requests.lock().unwrap().is_empty());
        assert!(agg.adapter_health().is_empty());
        ready.store(true, Ordering::SeqCst);
        agg.start(&symbols).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        agg.start(&symbols).await.unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            vec![vec!["BTC/USDT".to_string()], vec!["BTC/USDT".to_string()]]
        );
    }

    struct SubscriptionAdapter {
        fail: bool,
    }
    #[async_trait::async_trait]
    impl MarketDataPort for SubscriptionAdapter {
        async fn subscribe(&self, _: &[Symbol]) -> Result<crate::ports::market_data::TickStream> {
            if self.fail {
                anyhow::bail!("not authenticated");
            }
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }
        async fn unsubscribe(&self, _: &[Symbol]) -> Result<()> {
            Ok(())
        }
        async fn get_snapshot(&self, _: &Symbol) -> Result<Quote> {
            anyhow::bail!("unused")
        }
        async fn get_historical_ohlcv(
            &self,
            _: &Symbol,
            _: Timeframe,
            _: chrono::DateTime<Utc>,
            _: chrono::DateTime<Utc>,
        ) -> Result<Vec<OHLCV>> {
            Ok(vec![])
        }
        fn adapter_id(&self) -> &'static str {
            if self.fail {
                "failed"
            } else {
                "good"
            }
        }
        fn health(&self) -> AdapterHealth {
            AdapterHealth::Healthy
        }
    }
    #[tokio::test]
    async fn repeated_subscription_survives_unrelated_adapter_failure() {
        let mut agg = MarketDataAggregator::new(Arc::new(MessageBus::new()));
        agg.add_adapter(Arc::new(SubscriptionAdapter { fail: false }));
        agg.add_adapter(Arc::new(SubscriptionAdapter { fail: true }));
        let symbols = [Symbol("BTC-USD".into())];
        assert!(agg.start(&symbols).await.is_ok());
        assert!(agg.start(&symbols).await.is_ok());
    }

    struct TickAdapter(Vec<Tick>);
    #[async_trait::async_trait]
    impl MarketDataPort for TickAdapter {
        async fn subscribe(&self, _: &[Symbol]) -> Result<crate::ports::market_data::TickStream> {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            for tick in &self.0 {
                tx.send(tick.clone()).await?;
            }
            Ok(rx)
        }
        async fn unsubscribe(&self, _: &[Symbol]) -> Result<()> {
            Ok(())
        }
        async fn get_snapshot(&self, _: &Symbol) -> Result<Quote> {
            anyhow::bail!("unused")
        }
        async fn get_historical_ohlcv(
            &self,
            _: &Symbol,
            _: Timeframe,
            _: chrono::DateTime<Utc>,
            _: chrono::DateTime<Utc>,
        ) -> Result<Vec<OHLCV>> {
            Ok(vec![])
        }
        fn adapter_id(&self) -> &'static str {
            "test-ticks"
        }
        fn health(&self) -> AdapterHealth {
            AdapterHealth::Healthy
        }
    }

    #[tokio::test]
    async fn old_close_is_display_only_and_invalid_ticks_are_excluded() {
        let now = Utc::now();
        let tick = Tick {
            time: now - chrono::Duration::hours(5),
            symbol: Symbol("AAPL".into()),
            bid: 150.0,
            ask: 151.0,
            last: 150.5,
            volume: 12,
            source: "test".into(),
            open: Some(149.0),
            change_pct: Some(1.0),
        };
        let mut invalid = tick.clone();
        invalid.time = now - chrono::Duration::hours(4);
        invalid.last = f64::NAN;
        let mut future = tick.clone();
        future.time = now + chrono::Duration::seconds(30);
        future.last = 160.0;
        let bus = Arc::new(MessageBus::new());
        let mut events = bus.subscribe(Topic::Quote("*".into()));
        let mut agg = MarketDataAggregator::new(bus);
        agg.add_adapter(Arc::new(TickAdapter(vec![tick, invalid, future])));
        agg.start(&[Symbol("AAPL".into())]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(agg.get_cached_quote("AAPL").is_none());
        assert_eq!(agg.get_display_quote("AAPL").unwrap().last, 150.5);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn test_create_aggregator() {
        let bus = Arc::new(MessageBus::new());
        let agg = MarketDataAggregator::new(bus);
        assert_eq!(agg.adapter_count(), 0);
    }

    #[test]
    fn test_symbol_mapping() {
        let bus = Arc::new(MessageBus::new());
        let mut agg = MarketDataAggregator::new(bus);
        agg.add_symbol_mapping("REL".into(), "RELIANCE.NS".into());
        assert_eq!(agg.resolve_symbol("REL"), "RELIANCE.NS");
        assert_eq!(agg.resolve_symbol("AAPL"), "AAPL");
    }

    #[tokio::test]
    async fn test_quote_cache() {
        let bus = Arc::new(MessageBus::new());
        let agg = MarketDataAggregator::new(bus);

        // Initially empty
        assert!(agg.get_cached_quote("AAPL").is_none());

        // Manually insert a quote
        agg.quote_cache.insert(
            "AAPL".into(),
            Quote {
                symbol: Symbol("AAPL".into()),
                bid: 150.0,
                ask: 150.05,
                last: 150.02,
                open: 149.0,
                high: 151.0,
                low: 148.5,
                volume: 10000,
                change_pct: 0.5,
                vwap: 149.8,
                updated_at: Utc::now(),
            },
        );

        let quote = agg.get_cached_quote("AAPL").unwrap();
        assert_eq!(quote.symbol.0, "AAPL");
        assert!((quote.last - 150.02).abs() < 0.001);
    }

    #[test]
    fn higher_priority_feed_blocks_fallback_until_it_goes_quiet() {
        let owners = DashMap::new();
        assert!(claim_symbol(&owners, "RELIANCE.NS", 0), "fallback may tick first");
        assert!(claim_symbol(&owners, "RELIANCE.NS", 1), "broker feed takes over");
        assert!(!claim_symbol(&owners, "RELIANCE.NS", 0), "fallback is now blocked");
        assert!(claim_symbol(&owners, "RELIANCE.NS", 1));
        assert!(claim_symbol(&owners, "AAPL", 0), "other symbols are unaffected");

        // Once the broker feed has been quiet past the hold time, the fallback resumes.
        owners.insert(
            "RELIANCE.NS".to_string(),
            (1, std::time::Instant::now() - SOURCE_HOLD - Duration::from_secs(1)),
        );
        assert!(claim_symbol(&owners, "RELIANCE.NS", 0));
    }
}
