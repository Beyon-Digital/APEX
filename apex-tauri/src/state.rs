use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::commands::python_runtime::RuntimePaths;
use crate::broker_runtime::{
    build_broker, paper_entry, BrokerCredentials, BrokerRuntimeEntry, BuiltBroker,
    CONFIGURABLE_BROKERS,
};
use crate::config::CopilotConfig;
use crate::live_guard::LiveTradingGuard;
use crate::vault::Vault;
use crate::config::{AppConfig, StorageBackendKind};
use anyhow::{anyhow, Result};
use apex_adapters::execution::paper_trading::PaperTradingAdapter;
use apex_adapters::market_data::yahoo_finance::YahooFinanceAdapter;
use apex_adapters::storage::sqlite_storage::SqliteStorage;
use apex_adapters::storage::timescale::TimescaleAdapter;
use apex_core::application::alert_engine::AlertEngine;
use apex_core::application::automation::AutomationEngine;
use apex_core::application::circuit_breaker::reconcile_on_startup;
use apex_core::application::graph_engine::GraphEngine;
use apex_core::application::market_data_aggregator::MarketDataAggregator;
use apex_core::application::metrics::Metrics;
use apex_core::application::news_engine::{FeedType, NewsEngine, NewsFeed};
use apex_core::application::order_trade_manager::{cancel_orphaned_paper_orders, OrderTradeManager};
use apex_core::application::risk_engine::{RiskConfig, RiskEngine};
use apex_core::application::scanner::MarketScanner;
use apex_core::bus::message_bus::{BusMessage, MessageBus, Topic};
use apex_core::ports::market_data::MarketDataPort;
use apex_core::ports::storage::StoragePort;

/// How a live order was authorised.
pub enum LiveAuth<'a> {
    /// A person confirmed it in the UI; carries the preview's confirmation id.
    Manual(Option<&'a str>),
    /// Placed by an automation rule or the AI copilot (gated separately by config).
    Unattended,
}

/// Shared application state — initialised once at startup, shared across all IPC handlers.
pub struct AppState {
    pub aggregator: Arc<MarketDataAggregator>,
    pub otm: Arc<OrderTradeManager>,
    pub alerts: Arc<AlertEngine>,
    pub alert_store: Arc<crate::commands::persistent_alerts::PersistentAlerts>,
    pub risk: Arc<RiskEngine>,
    pub bus: Arc<MessageBus>,
    pub storage: Arc<dyn StoragePort>,
    pub storage_backend: String,
    pub storage_target: String,
    pub metrics: Arc<Metrics>,
    pub news: Arc<NewsEngine>,
    pub graph: Arc<tokio::sync::RwLock<GraphEngine>>,
    pub scanner: Arc<MarketScanner>,
    pub history_source: Arc<dyn MarketDataPort>,
    pub copilot: CopilotConfig,
    pub llm: crate::config::LlmConfig,
    pub acp: crate::config::AcpConfig,
    pub automations_cfg: crate::config::AutomationsConfig,
    pub automations: Arc<AutomationEngine>,
    /// Copilot tool approvals: keys of `name:{canonical args}` the user has
    /// approved this session. Guarded by [copilot] require_approval.
    pub copilot_approvals: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Results of approved write-tool executions, keyed like approvals —
    /// replays return the cached result so an approved call can't double-fire
    /// when the UI re-sends the turn after an approval.
    pub copilot_tool_cache: std::sync::Mutex<HashMap<String, String>>,
    pub http: reqwest::Client,
    pub paper: Arc<PaperTradingAdapter>,
    brokers: RwLock<HashMap<String, BrokerRuntimeEntry>>,
    pub vault: Arc<Vault>,
    pub live_guard: LiveTradingGuard,
    market_data_feed: String,
    pub started_at: Instant,
}

struct StorageBootstrap {
    storage: Arc<dyn StoragePort>,
    backend: &'static str,
    target: String,
}

impl AppState {
    /// Initialize all application services.
    pub async fn init(runtime_paths: RuntimePaths) -> Result<Self> {
        let bus = Arc::new(MessageBus::new());
        let config = AppConfig::load(&runtime_paths)?;
        let resolved_data_dir = config.resolved_data_dir(&runtime_paths)?;
        let storage = build_storage(&runtime_paths, &config).await?;
        let cancelled = cancel_orphaned_paper_orders(storage.storage.as_ref()).await?;
        if cancelled > 0 { tracing::info!(cancelled, "Cancelled paper orders from the previous session"); }

        tracing::info!(
            bundled_runtime = runtime_paths.is_bundled(),
            config_path = %runtime_paths.config_file().display(),
            config_dir = %runtime_paths.config_dir().display(),
            data_dir = %resolved_data_dir.display(),
            storage_backend = storage.backend,
            storage_target = %storage.target,
            python_root = %runtime_paths.python_root().display(),
            work_root = %runtime_paths.work_root().display(),
            resource_dir = ?runtime_paths.resource_dir().map(|path| path.display().to_string()),
            "Resolved application runtime paths"
        );

        let risk_config: RiskConfig = config.risk_config();
        let risk = Arc::new(RiskEngine::new(risk_config));

        let mut aggregator_inner = MarketDataAggregator::new(bus.clone());
        aggregator_inner.set_storage(storage.storage.clone());
        let otm_inner = OrderTradeManager::new(risk.clone(), bus.clone());
        let mut brokers = HashMap::new();

        let yahoo_adapter = Arc::new(YahooFinanceAdapter::new());
        aggregator_inner.add_adapter(yahoo_adapter.clone());
        let scanner_source: Arc<dyn MarketDataPort> = yahoo_adapter;

        // Register paper trading adapter — always available as default execution.
        let paper = Arc::new(PaperTradingAdapter::new());
        otm_inner.register_execution("paper".to_string(), paper.clone());
        brokers.insert("paper".to_string(), paper_entry());

        // Live brokers start unconfigured; their adapters are built from the
        // encrypted vault once it is unlocked (see `apply_all_brokers`).
        let market_data_feed = config.market_data.adapter.clone();
        for broker_id in CONFIGURABLE_BROKERS {
            let built = build_broker(broker_id, &BrokerCredentials::default(), &market_data_feed)?;
            if let Some(execution) = &built.execution {
                otm_inner.register_execution(broker_id.to_string(), execution.clone());
            }
            if let Some(market_data) = &built.market_data {
                aggregator_inner.add_adapter(market_data.clone());
            }
            brokers.insert(broker_id.to_string(), built.entry);
        }

        let vault = Arc::new(
            Vault::open(&resolved_data_dir.join("vault.db")).map_err(|e| anyhow!(e))?,
        );

        otm_inner.set_quote_cache(aggregator_inner.quote_cache());
        let aggregator = Arc::new(aggregator_inner);
        let otm = Arc::new(otm_inner);
        // Pick up fills/cancels on live orders even without a broker push stream.
        OrderTradeManager::start_live_order_sync(otm.clone(), std::time::Duration::from_secs(3));

        let alerts = Arc::new(AlertEngine::new(bus.clone()));
        let alert_store = Arc::new(crate::commands::persistent_alerts::PersistentAlerts::load(
            alerts.clone(),
            aggregator.clone(),
            resolved_data_dir.join("alerts.json"),
        ).await);
        alerts.start();

        // Evaluate DailyPnl alert rules against the live session P&L whenever a
        // position update lands on the bus.
        {
            let alerts = alerts.clone();
            let risk = risk.clone();
            let bus = bus.clone();
            tokio::spawn(async move {
                let mut rx = bus.subscribe(Topic::PositionUpdate);
                loop {
                    match rx.recv().await {
                        Ok(_) => alerts.evaluate_pnl(risk.session_pnl()).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        // News engine — publishes items onto the bus for the `news-item` push.
        let (news_tx, mut news_rx) = tokio::sync::mpsc::unbounded_channel();
        let news_engine = Arc::new(NewsEngine::new(news_tx));
        if config.news.enabled {
            for feed in &config.news.feeds {
                if !feed.enabled {
                    continue;
                }
                news_engine.add_feed(NewsFeed {
                    name: feed.name.clone(),
                    url: feed.url.clone(),
                    feed_type: if feed.feed_type.eq_ignore_ascii_case("atom") {
                        FeedType::Atom
                    } else {
                        FeedType::Rss
                    },
                    priority: feed.priority,
                    enabled: feed.enabled,
                });
            }
            news_engine
                .clone()
                .start_polling_loop(config.news.poll_interval_secs)
                .await;
        }
        {
            let bus = bus.clone();
            let alerts = alerts.clone();
            tokio::spawn(async move {
                while let Some(item) = news_rx.recv().await {
                    alerts.evaluate_news(&item).await;
                    bus.publish(Topic::NewsItem, BusMessage::News(item));
                }
            });
        }

        // Order journal — persist every order update so the blotter survives
        // restarts (the storage write is an upsert keyed on order id).
        {
            let storage = storage.storage.clone();
            let bus = bus.clone();
            tokio::spawn(async move {
                let mut rx = bus.subscribe(Topic::OrderUpdate("*".into()));
                loop {
                    match rx.recv().await {
                        Ok(BusMessage::OrderData(order)) => {
                            if let Err(e) = storage.write_order(&order).await {
                                tracing::warn!(error = %e, order = %order.id.0, "Failed to journal order");
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        let graph = Arc::new(tokio::sync::RwLock::new(GraphEngine::new()));
        let scanner = Arc::new(MarketScanner::new(scanner_source.clone()));

        // Crash recovery — reconcile stale orders and positions on startup
        let broker_ids = otm.authenticated_broker_ids();
        if !broker_ids.is_empty() {
            match reconcile_on_startup(&otm, &broker_ids).await {
                Ok(report) => {
                    tracing::info!(
                        pending = report.pending_orders,
                        reconciled = report.brokers_reconciled,
                        "Crash recovery completed"
                    );
                    if !report.errors.is_empty() {
                        tracing::warn!("Reconciliation errors: {:?}", report.errors);
                    }
                }
                Err(e) => {
                    tracing::warn!("Crash recovery failed (non-fatal): {}", e);
                }
            }
        }

        // Start periodic position reconciliation (every 30 seconds)
        OrderTradeManager::start_reconciliation_loop(
            otm.clone(),
            std::time::Duration::from_secs(30),
        );

        let metrics = Arc::new(Metrics::new());

        Ok(Self {
            aggregator,
            otm,
            alerts,
            alert_store,
            risk,
            bus,
            storage: storage.storage,
            storage_backend: storage.backend.to_string(),
            storage_target: storage.target,
            metrics,
            news: news_engine,
            graph,
            scanner,
            history_source: scanner_source,
            copilot: config.copilot.clone(),
            llm: config.llm.clone(),
            acp: config.acp.clone(),
            automations_cfg: config.automations.clone(),
            automations: crate::commands::automation::new_engine(&resolved_data_dir),
            copilot_approvals: std::sync::Mutex::new(std::collections::HashSet::new()),
            copilot_tool_cache: std::sync::Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            paper,
            brokers: RwLock::new(brokers),
            vault,
            live_guard: LiveTradingGuard::new(config.live.clone()),
            market_data_feed,
            started_at: Instant::now(),
        })
    }

    pub fn broker_connections(&self) -> Vec<crate::dto::BrokerConnectionDto> {
        let mut connections: Vec<_> = self
            .brokers
            .read()
            .expect("brokers lock poisoned")
            .iter()
            .map(|(broker_id, broker)| broker.to_dto(broker_id))
            .collect();

        connections.sort_by_key(|broker| {
            (
                if broker.mode == "paper" { 0 } else { 1 },
                broker.display_name.clone(),
            )
        });

        connections
    }

    pub fn broker_connection(&self, broker_id: &str) -> Option<crate::dto::BrokerConnectionDto> {
        self.brokers
            .read()
            .expect("brokers lock poisoned")
            .get(broker_id)
            .map(|broker| broker.to_dto(broker_id))
    }

    pub fn set_broker_session(&self, broker_id: &str, session_token: &str) -> Result<()> {
        let brokers = self.brokers.read().expect("brokers lock poisoned");
        let broker = brokers
            .get(broker_id)
            .ok_or_else(|| anyhow!("Unknown broker: {}", broker_id))?;

        broker.set_session_token(session_token)
    }

    pub fn clear_broker_session(&self, broker_id: &str) -> Result<()> {
        let brokers = self.brokers.read().expect("brokers lock poisoned");
        let broker = brokers
            .get(broker_id)
            .ok_or_else(|| anyhow!("Unknown broker: {}", broker_id))?;

        broker.clear_session_token()
    }

    /// Persist a broker session token (encrypted) so a restart doesn't need a new login.
    /// No-op when the vault is locked or the broker has no session token.
    pub fn persist_session_token(
        &self,
        broker_id: &str,
        token: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let Some(field) = crate::broker_runtime::session_token_field(broker_id) else {
            return Ok(());
        };
        if !self.vault.is_unlocked() {
            return Ok(());
        }
        self.vault.set_secret(broker_id, field, token).map_err(|e| anyhow!(e))?;
        self.set_token_expiry(broker_id, field, expires_at)
    }

    /// Remove a persisted session token.
    pub fn forget_session_token(&self, broker_id: &str) -> Result<()> {
        let Some(field) = crate::broker_runtime::session_token_field(broker_id) else {
            return Ok(());
        };
        if !self.vault.is_unlocked() {
            return Ok(());
        }
        self.vault.delete_secret(broker_id, field).map_err(|e| anyhow!(e))?;
        self.set_token_expiry(broker_id, field, None)
    }

    fn set_token_expiry(
        &self,
        broker_id: &str,
        field: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let config = self.vault.get_config(broker_id).map_err(|e| anyhow!(e))?;
        let enabled = config.as_ref().map(|c| c.enabled).unwrap_or(true);
        let mut settings: serde_json::Map<String, serde_json::Value> = config
            .and_then(|c| serde_json::from_str(&c.settings_json).ok())
            .unwrap_or_default();
        let key = format!("{field}_expires_at");
        match expires_at {
            Some(at) => settings.insert(key, serde_json::Value::String(at.to_rfc3339())),
            None => settings.remove(&key),
        };
        self.vault
            .set_config(broker_id, enabled, &serde_json::Value::Object(settings).to_string())
            .map_err(|e| anyhow!(e))
    }

    /// Search Zerodha instruments (loading the master first if needed). Empty
    /// when Zerodha is not connected.
    pub async fn search_zerodha_instruments(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<apex_adapters::kite::instruments::Instrument> {
        let (instruments, session) = {
            let brokers = self.brokers.read().expect("brokers lock poisoned");
            let handle = brokers.get("zerodha").and_then(|b| b.market_data.as_ref());
            (
                handle.and_then(|h| h.instruments()),
                handle.and_then(|h| h.kite_session()),
            )
        };
        let (Some(instruments), Some(session)) = (instruments, session) else {
            return Vec::new();
        };
        if session.is_authenticated() {
            if let Err(error) = instruments.ensure_loaded(&session).await {
                tracing::debug!(%error, "Could not load Kite instruments for search");
            }
        }
        instruments.search(query, limit)
    }

    /// Submit an order, applying the live-trading safeguards to non-paper brokers.
    /// Paper orders pass straight through.
    pub async fn guarded_submit(
        &self,
        order: apex_core::domain::models::NewOrderRequest,
        broker_id: &str,
        auth: LiveAuth<'_>,
    ) -> std::result::Result<apex_core::domain::models::OrderId, String> {
        if broker_id == "paper" {
            return self.otm.submit_order(order, broker_id).await.map_err(|e| e.to_string());
        }
        self.live_guard.check_order(&order)?;
        if let LiveAuth::Manual(confirmation) = auth {
            let id = confirmation.ok_or_else(|| {
                "Order was not confirmed. Review and confirm it first.".to_string()
            })?;
            self.live_guard.consume_confirmation(id, broker_id, &order)?;
        }
        let slot = self.live_guard.reserve()?;
        match self.otm.submit_order(order, broker_id).await {
            Ok(id) => Ok(id),
            Err(error) => {
                slot.refund();
                Err(error.to_string())
            }
        }
    }

    /// Emergency stop: disarm live trading, halt the risk engine and cancel
    /// every working live order APEX knows about.
    pub async fn kill_switch(&self) -> (Vec<String>, Vec<String>) {
        use apex_core::domain::models::OrderStatus;
        self.live_guard.set_armed(false);
        self.risk.halt();
        let working: Vec<_> = self
            .otm
            .open_orders()
            .into_iter()
            .filter(|o| {
                o.broker_id != "paper"
                    && matches!(
                        o.status,
                        OrderStatus::Pending | OrderStatus::Open | OrderStatus::PartiallyFilled
                    )
            })
            .collect();
        let (mut cancelled, mut failed) = (Vec::new(), Vec::new());
        for order in working {
            match self.otm.cancel_order(&order.id, &order.broker_id).await {
                Ok(()) => cancelled.push(order.id.0.clone()),
                Err(error) => failed.push(format!("{}: {error}", order.id.0)),
            }
        }
        (cancelled, failed)
    }

    /// Decrypt one adapter's stored settings and secrets. Requires an unlocked vault.
    pub fn load_broker_credentials(&self, broker_id: &str) -> Result<BrokerCredentials> {
        let mut creds = BrokerCredentials::default();
        if let Some(config) = self.vault.get_config(broker_id).map_err(|e| anyhow!(e))? {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(&config.settings_json)
            {
                for (key, value) in map {
                    let text = match value {
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Null => continue,
                        other => other.to_string(),
                    };
                    creds.settings.insert(key, text);
                }
            }
        }
        for field in self.vault.secret_fields(broker_id).map_err(|e| anyhow!(e))? {
            if let Some(value) = self.vault.get_secret(broker_id, &field).map_err(|e| anyhow!(e))? {
                creds.secrets.insert(field, value);
            }
        }
        Ok(creds)
    }

    /// (Re)build one broker from the vault and swap it into the running registries.
    /// A disabled adapter is replaced by its unconfigured form.
    pub async fn apply_broker(&self, broker_id: &str) -> Result<()> {
        if !CONFIGURABLE_BROKERS.contains(&broker_id) {
            return Err(anyhow!("Unknown broker: {}", broker_id));
        }
        let enabled = self
            .vault
            .get_config(broker_id)
            .map_err(|e| anyhow!(e))?
            .map(|config| config.enabled)
            .unwrap_or(true);
        let creds = if enabled {
            self.load_broker_credentials(broker_id)?
        } else {
            BrokerCredentials::default()
        };
        let built = build_broker(broker_id, &creds, &self.market_data_feed)?;
        self.replace_broker(broker_id, built).await;
        Ok(())
    }

    /// Build every configurable broker from the vault (after unlock).
    pub async fn apply_all_brokers(&self) {
        for broker_id in CONFIGURABLE_BROKERS {
            if let Err(error) = self.apply_broker(broker_id).await {
                tracing::warn!(broker = broker_id, %error, "Failed to apply broker configuration");
            }
        }
    }

    /// Reset a broker to its unconfigured form, dropping any held credentials.
    pub async fn reset_broker(&self, broker_id: &str) -> Result<()> {
        let built = build_broker(broker_id, &BrokerCredentials::default(), &self.market_data_feed)?;
        self.replace_broker(broker_id, built).await;
        Ok(())
    }

    /// Lock the vault and drop every in-memory credential.
    pub async fn lock_vault(&self) {
        self.vault.lock();
        for broker_id in CONFIGURABLE_BROKERS {
            if let Err(error) = self.reset_broker(broker_id).await {
                tracing::warn!(broker = broker_id, %error, "Failed to reset broker on lock");
            }
        }
    }

    async fn replace_broker(&self, broker_id: &str, built: BuiltBroker) {
        let old_market_data_id = self
            .brokers
            .read()
            .expect("brokers lock poisoned")
            .get(broker_id)
            .and_then(|entry| entry.market_data.as_ref().map(|handle| handle.adapter_id()));

        self.otm.unregister_execution(broker_id);
        if let Some(adapter_id) = old_market_data_id {
            if let Some(old) = self.aggregator.remove_adapter(&adapter_id) {
                let symbols = self.aggregator.requested_symbols();
                if let Err(error) = old.unsubscribe(&symbols).await {
                    tracing::debug!(adapter = %adapter_id, %error, "Unsubscribe on replace failed");
                }
            }
        }

        if let Some(execution) = built.execution {
            self.otm.register_execution(broker_id.to_string(), execution);
            self.otm.attach_order_stream(broker_id);
        }
        if let Some(handle) = &built.entry.market_data {
            handle.start_background();
        }
        let has_market_data = built.market_data.is_some();
        if let Some(market_data) = built.market_data {
            self.aggregator.add_adapter(market_data);
        }
        self.brokers
            .write()
            .expect("brokers lock poisoned")
            .insert(broker_id.to_string(), built.entry);

        if has_market_data {
            if let Err(error) = self.aggregator.sync_subscriptions().await {
                tracing::warn!(broker = broker_id, %error, "Failed to subscribe symbols on new adapter");
            }
        }
    }

    /// Start real-time event push to the frontend.
    /// Subscribes to message bus topics and forwards events via Tauri app handle.
    pub fn start_event_push(&self, app_handle: tauri::AppHandle) {
        use tauri::Emitter;

        // Forward quote updates
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        let otm = self.otm.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::Quote("*".into()));
            loop {
                match rx.recv().await {
                  Ok(BusMessage::QuoteData(quote)) => {
                    // Matching and lifecycle updates are serialized by the order manager.
                    if let Err(error) = otm.process_paper_quote(&quote).await {
                        tracing::warn!(%error, "Paper quote processing failed");
                    }
                    let dto = crate::dto::QuoteDto::from(&quote);
                    let _ = handle.emit("quote-update", &dto);
                  }
                  Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                  Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Forward order updates
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::OrderUpdate("*".into()));
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::OrderData(order) = msg {
                    let dto = crate::dto::OrderDto::from(&order);
                    let _ = handle.emit("order-update", &dto);
                }
            }
        });

        // Forward position updates
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::PositionUpdate);
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::PositionData(pos) = msg {
                    let dto = crate::dto::PositionDto::from(&pos);
                    let _ = handle.emit("position-update", &dto);
                }
            }
        });

        // Forward news items
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::NewsItem);
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::News(item) = msg {
                    let dto = crate::dto::NewsItemDto::from(&item);
                    let _ = handle.emit("news-item", &dto);
                }
            }
        });

        // Forward alert events
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::Alert);
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::AlertFired(alert) = msg {
                    let dto = crate::dto::AlertDto {
                        rule_id: alert.rule_id,
                        message: alert.message,
                        severity: format!("{:?}", alert.severity),
                    };
                    let _ = handle.emit("alert-fired", &dto);
                }
            }
        });

        // Forward strategy signals
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::StrategySignal("*".into()));
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::Signal(signal) = msg {
                    let _ = handle.emit("strategy-signal", &signal);
                }
            }
        });

        // Forward adapter health
        let bus = self.bus.clone();
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe(Topic::SystemHealth);
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::Health(health) = msg {
                    let _ = handle.emit("adapter-health", &health);
                }
            }
        });
    }
}

async fn build_storage(
    runtime_paths: &RuntimePaths,
    config: &AppConfig,
) -> Result<StorageBootstrap> {
    match config.storage.backend_kind()? {
        StorageBackendKind::Sqlite => {
            let sqlite_path = config.resolved_sqlite_path(runtime_paths)?;
            let sqlite_path_text = sqlite_path.to_string_lossy().to_string();
            let sqlite =
                SqliteStorage::new_with_options(&sqlite_path_text, config.storage.wal_mode)?;
            sqlite.init_schema().await?;

            Ok(StorageBootstrap {
                storage: Arc::new(sqlite),
                backend: "sqlite",
                target: sqlite_path.display().to_string(),
            })
        }
        StorageBackendKind::Timescale => {
            let connection_url = config.storage.postgres_url().ok_or_else(|| {
                anyhow!(
                    "Storage backend is set to Timescale/Postgres but no connection URL was provided. Set `storage.postgres_url` in {} or define `APEX_POSTGRES_URL`.",
                    runtime_paths.config_file().display()
                )
            })?;

            let timescale = TimescaleAdapter::new(&connection_url).await?;

            Ok(StorageBootstrap {
                storage: Arc::new(timescale),
                backend: "timescale",
                target: redact_connection_url(&connection_url),
            })
        }
    }
}

fn redact_connection_url(connection_url: &str) -> String {
    let Some((scheme, rest)) = connection_url.split_once("://") else {
        return "configured via postgres_url".to_string();
    };

    let host_part = rest.rsplit('@').next().unwrap_or(rest);
    format!("{scheme}://{}", host_part)
}
