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
use apex_core::application::order_trade_manager::{cancel_orphaned_paper_orders_audited, OrderTradeManager};
use apex_core::application::risk_engine::{RiskConfig, RiskEngine};
use apex_core::application::scanner::MarketScanner;
use apex_core::bus::message_bus::{BusMessage, MessageBus, Topic};
use apex_core::ports::market_data::MarketDataPort;
use apex_core::ports::storage::StoragePort;

use crate::order_audit::{AuditContext, OrderSource};
use apex_core::ports::audit::OrderAuditEvent;

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
    /// Why the configured storage backend was not used, if it fell back to SQLite.
    pub storage_notice: Option<String>,
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
    /// Set by the kill switch: the automation scheduler does nothing until the
    /// halt is reset explicitly.
    pub automations_paused: std::sync::atomic::AtomicBool,
    /// Handle for tools that start sessions or push events to the UI. Set once at startup.
    pub app: std::sync::OnceLock<tauri::AppHandle>,
    /// Id of the copilot turn whose approvals and tool results are cached. A new
    /// user message gets a new id, which clears both.
    pub copilot_turn: std::sync::Mutex<Option<String>>,
    /// ChatGPT sign-in state (pending flow, refresh lock).
    pub chatgpt: crate::chatgpt_auth::Runtime,
    /// Serializes changes to the live adapters (apply, reset, lock) so a lock can
    /// never be undone by an apply that was already in flight.
    broker_change_lock: tokio::sync::Mutex<()>,
    /// Strategy files the copilot wrote and the user approved: path -> content hash.
    /// A backtest of one of these runs without a second approval.
    pub copilot_trusted_files: std::sync::Mutex<HashMap<String, String>>,
    market_data_feed: String,
    pub started_at: Instant,
}

struct StorageBootstrap {
    storage: Arc<dyn StoragePort>,
    backend: &'static str,
    target: String,
    /// Set when the configured backend could not start and local SQLite was used instead.
    fallback_reason: Option<String>,
}

/// Open the configured storage. If the Postgres/Timescale backend cannot be reached or
/// authenticated, run on local SQLite and say why, rather than refusing to start. A broken
/// local SQLite file is NOT replaced: that would silently abandon the user's data.
async fn build_storage_or_fallback(
    runtime_paths: &RuntimePaths,
    config: &AppConfig,
) -> Result<StorageBootstrap> {
    match build_storage(runtime_paths, config).await {
        Ok(storage) => Ok(storage),
        Err(error) if matches!(config.storage.backend_kind(), Ok(StorageBackendKind::Timescale)) => {
            // The chain can mention the connection URL; it is redacted before it is kept.
            let reason = format!("{error:#}");
            let reason = match config.storage.postgres_url() {
                Some(url) => reason.replace(&url, &redact_connection_url(&url)),
                None => reason,
            };
            tracing::error!(%reason, "Configured Postgres/Timescale storage failed; falling back to local SQLite");
            let mut sqlite_config = config.clone();
            sqlite_config.storage.backend = "sqlite".to_string();
            let mut bootstrap = build_storage(runtime_paths, &sqlite_config).await?;
            bootstrap.fallback_reason = Some(format!(
                "Postgres/Timescale storage is configured but could not start ({reason}). Running on local SQLite instead. Fix the database, or change the storage backend in Settings > General > Storage & runtime. Changes apply after restart."
            ));
            Ok(bootstrap)
        }
        Err(error) => Err(error),
    }
}

/// Open the immutable audit ledger under `<runtime data dir>/ledger`, install it process-wide and
/// verify it in the background. A problem is logged and recorded as an activity entry; it never
/// stops the app.
///
/// The ledger lives under the RUNTIME data directory (`RuntimePaths::data_dir`), never under the
/// `general.data_dir` setting: model, dataset and training-provenance code, the activity log and the
/// ledger commands all find the ledger there, so a configured data dir must not split the audit
/// trail into two ledgers (one of them never verified or shown). The setting still moves the
/// database; it does not move the audit trail.
fn open_audit_ledger(runtime_paths: &RuntimePaths, configured_data_dir: &std::path::Path) {
    let data_dir = runtime_paths.data_dir();
    if configured_data_dir != data_dir {
        tracing::warn!(
            configured = %configured_data_dir.display(),
            ledger = %data_dir.join("ledger").display(),
            "general.data_dir differs from the runtime data directory; the audit ledger stays in the runtime data directory"
        );
    }
    let ledger = match crate::activity::ledger_for(data_dir) {
        Ok(ledger) => ledger,
        Err(error) => {
            tracing::error!(%error, "could not open the audit ledger; changes will not be versioned");
            return;
        }
    };
    if let Err(error) = crate::ledger::install(ledger.clone()) {
        tracing::warn!(%error, "audit ledger was already installed");
    }
    // Not in unit tests: they start many AppStates on temp folders and write files right away, and a
    // background pass would record those files as outside edits in the middle of the test.
    #[cfg(not(test))]
    let startup_paths = runtime_paths.clone();
    #[cfg(test)]
    let _ = runtime_paths;
    tokio::task::spawn_blocking(move || {
        // Once, before the first verify: finish interrupted saves and record edits made while APEX was
        // not running as `external` versions. Problems are logged and never stop the app.
        #[cfg(not(test))]
        {
            let started = crate::commands::ledger_cmds::startup_reconcile(&ledger, &startup_paths);
            for error in &started.errors {
                tracing::warn!(%error, "startup audit reconcile");
            }
            let f = &started.files;
            tracing::info!(adopted = f.adopted, external_edits = f.external_edits, removed = f.removed, completed_pending = f.completed_pending, datasets = started.datasets, models = started.models, "startup audit reconcile done");
            if started.recorded_changes() {
                let entry = crate::activity::ActivityDto {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts: chrono::Utc::now().to_rfc3339(),
                    actor: "system".into(),
                    action: "ledger.reconcile".into(),
                    kind: "system".into(),
                    target_id: "ledger".into(),
                    summary: format!(
                        "Recorded changes made outside APEX as new versions: {} new file(s), {} edited, {} removed",
                        f.adopted, f.external_edits, f.removed
                    ),
                    ok: true,
                    details: None,
                };
                if let Err(error) = crate::activity::append_to(&ledger, &entry) {
                    tracing::error!(%error, "could not record the startup reconcile");
                }
            }
        }
        let report = ledger.verify();
        if report.ok {
            tracing::info!(records = report.checked_records, events = report.checked_events, blobs = report.checked_blobs, "audit ledger verified");
            return;
        }
        let first = report.problems.first().map(|p| format!("{} at {}: {}", p.kind, p.location, p.detail)).unwrap_or_default();
        tracing::error!(problems = report.problems.len(), %first, "audit ledger verification FAILED");
        let entry = crate::activity::ActivityDto {
            id: uuid::Uuid::new_v4().to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            actor: "system".into(),
            action: "ledger.verify_failed".into(),
            kind: "system".into(),
            target_id: "ledger".into(),
            summary: format!("Audit ledger verification found {} problem(s)", report.problems.len()),
            ok: false,
            details: Some(first),
        };
        if let Err(error) = crate::activity::append_to(&ledger, &entry) {
            tracing::error!(%error, "could not record the ledger verification failure");
        }
    });
}

impl AppState {
    /// Initialize all application services.
    pub async fn init(runtime_paths: RuntimePaths) -> Result<Self> {
        let bus = Arc::new(MessageBus::new());
        let config = AppConfig::load(&runtime_paths)?;
        let resolved_data_dir = config.resolved_data_dir(&runtime_paths)?;
        open_audit_ledger(&runtime_paths, &resolved_data_dir);
        let storage = build_storage_or_fallback(&runtime_paths, &config).await?;
        let startup_sink = crate::order_audit::LedgerOrderSink::global();
        let cancelled = cancel_orphaned_paper_orders_audited(storage.storage.as_ref(), Some(&startup_sink)).await?;
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
        // Every order state becomes an event in the hash-chained "orders" stream of the audit
        // ledger. A live order is refused when its first event cannot be written.
        otm_inner.set_audit_sink(Arc::new(crate::order_audit::LedgerOrderSink::global()));
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
            storage_notice: storage.fallback_reason,
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
            automations_paused: std::sync::atomic::AtomicBool::new(false),
            app: std::sync::OnceLock::new(),
            copilot_turn: std::sync::Mutex::new(None),
            chatgpt: crate::chatgpt_auth::Runtime::default(),
            broker_change_lock: tokio::sync::Mutex::new(()),
            copilot_trusted_files: std::sync::Mutex::new(HashMap::new()),
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
            .map(|(broker_id, broker)| broker.to_dto(broker_id, self.automations_cfg.allow_live_shorting))
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
            .map(|broker| broker.to_dto(broker_id, self.automations_cfg.allow_live_shorting))
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

    /// Make sure a quote for `symbol` is flowing, subscribing it if needed. Paper
    /// market orders fill against the latest quote, so callers use this before
    /// ordering a symbol that may not be on the watchlist.
    pub async fn ensure_quote(&self, symbol: &str) -> std::result::Result<(), String> {
        if self.aggregator.get_cached_quote(symbol).is_some() {
            return Ok(());
        }
        self.aggregator
            .start(&[apex_core::domain::models::Symbol(symbol.to_string())])
            .await
            .map_err(|e| format!("Could not subscribe to {symbol}: {e}"))?;
        for _ in 0..12 {
            if self.aggregator.get_cached_quote(symbol).is_some() {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Err(format!("No live quote for {symbol} yet. Check the symbol and the data feed."))
    }

    /// Submit an order, applying the live-trading safeguards to non-paper brokers.
    /// Paper orders pass straight through. The audit trail names who produced the order from
    /// its tag; callers that know better use [`guarded_submit_with`](Self::guarded_submit_with).
    pub async fn guarded_submit(
        &self,
        order: apex_core::domain::models::NewOrderRequest,
        broker_id: &str,
        auth: LiveAuth<'_>,
    ) -> std::result::Result<apex_core::domain::models::OrderId, String> {
        self.guarded_submit_with(order, broker_id, auth, None).await
    }

    /// Who is placing an order, for the audit trail: the caller's `source`, else what the order's
    /// tag says (Copilot, automation or strategy orders), else "manual" for a confirmed UI order,
    /// else an unattended order of unknown origin.
    fn order_audit_context(
        &self,
        order: &apex_core::domain::models::NewOrderRequest,
        broker_id: &str,
        auth: &LiveAuth<'_>,
        source: Option<OrderSource>,
    ) -> AuditContext {
        let source = source
            .or_else(|| {
                let turn = self.copilot_turn.lock().ok().and_then(|t| t.clone());
                crate::order_audit::source_from_tag(
                    crate::ledger::global(),
                    order.tag.as_deref(),
                    turn.as_deref(),
                    |id| self.automations.get(id).map(|r| r.model_id),
                )
            })
            .or_else(|| match auth {
                LiveAuth::Manual(_) => Some(crate::order_audit::manual_source()),
                LiveAuth::Unattended => Some(OrderSource::new("unattended", "")),
            });
        let actor = source.as_ref().map(crate::order_audit::actor_for).unwrap_or("unattributed");
        let mut ctx = AuditContext::new(actor, source);
        if let (LiveAuth::Manual(confirmation), true) = (auth, broker_id != "paper") {
            ctx = ctx.confirmed(confirmation.is_some());
        }
        ctx
    }

    /// [`guarded_submit`](Self::guarded_submit) with the order's origin stated: pass an
    /// [`OrderSource`] (see `crate::order_audit`: `copilot_source`, `strategy_source`,
    /// `automation_source`, ...) so the trade traces to the exact turn, strategy or rule version.
    pub async fn guarded_submit_with(
        &self,
        order: apex_core::domain::models::NewOrderRequest,
        broker_id: &str,
        auth: LiveAuth<'_>,
        source: Option<OrderSource>,
    ) -> std::result::Result<apex_core::domain::models::OrderId, String> {
        let ctx = self.order_audit_context(&order, broker_id, &auth, source);
        if broker_id == "paper" {
            return self.otm.submit_order_with(order, broker_id, ctx).await.map_err(|e| e.to_string());
        }
        // The live guard's refusals (not armed, over the cap, unconfirmed, daily limit) come before
        // the order manager sees the order, so they are recorded here: an attempted live trade
        // that was blocked still shows in the trail, with who tried it.
        let checked = (|| -> std::result::Result<_, String> {
            self.live_guard.check_order(&order)?;
            if let LiveAuth::Manual(confirmation) = auth {
                let id = confirmation.ok_or_else(|| {
                    "Order was not confirmed. Review and confirm it first.".to_string()
                })?;
                self.live_guard.consume_confirmation(id, broker_id, &order)?;
            }
            self.live_guard.reserve()
        })();
        let slot = match checked {
            Ok(slot) => slot,
            Err(reason) => {
                self.otm.audit_refused_order(&order, broker_id, &ctx, "live_guard", &reason);
                return Err(reason);
            }
        };
        match self.otm.submit_order_with(order, broker_id, ctx).await {
            Ok(id) => {
                // The order was past the checks when the user pulled the plug (kill
                // switch, disarm, or a risk halt). It is now at the broker, so cancel it.
                if !self.live_guard.is_armed() || self.risk.is_halted() {
                    let guard = AuditContext::new("system", Some(OrderSource::new("live_guard", "disarmed_during_placement")));
                    let _ = self.otm.cancel_order_with(&id, broker_id, guard).await;
                    return Err(
                        "Live trading was disarmed while this order was being placed; it was cancelled."
                            .to_string(),
                    );
                }
                Ok(id)
            }
            Err(error) => {
                slot.refund();
                Err(error.to_string())
            }
        }
    }

    /// Change a working order. Live modifications are gated like new orders: they
    /// need live trading armed, stay within the per-order cap, and are risk checked.
    pub async fn guarded_modify(
        &self,
        order_id: &apex_core::domain::models::OrderId,
        broker_id: &str,
        params: &apex_core::domain::models::ModifyParams,
    ) -> std::result::Result<(), String> {
        self.guarded_modify_with(order_id, broker_id, params, None).await
    }

    /// [`guarded_modify`](Self::guarded_modify) with the origin of the change stated.
    pub async fn guarded_modify_with(
        &self,
        order_id: &apex_core::domain::models::OrderId,
        broker_id: &str,
        params: &apex_core::domain::models::ModifyParams,
        source: Option<OrderSource>,
    ) -> std::result::Result<(), String> {
        if broker_id != "paper" {
            self.live_guard.check_modify(params.quantity)?;
        }
        let actor = source.as_ref().map(crate::order_audit::actor_for).unwrap_or("unattributed");
        let ctx = AuditContext::new(actor, source);
        self.otm
            .modify_order_with(order_id, broker_id, params, ctx)
            .await
            .map_err(|e| e.to_string())
    }

    /// Arm or disarm live trading and put it in the audit trail (`live_armed` / `live_disarmed`).
    /// Arming is refused when the event cannot be written: nothing may trade live unrecorded.
    /// Disarming always happens; its event is best effort. No event when nothing changes.
    pub fn set_live_armed_audited(&self, armed: bool, actor: &str, reason: &str) -> std::result::Result<(), String> {
        let was_armed = self.live_guard.is_armed();
        if armed == was_armed {
            return Ok(());
        }
        let event = OrderAuditEvent {
            order_id: String::new(),
            broker_id: String::new(),
            event: if armed { "live_armed" } else { "live_disarmed" }.into(),
            actor: actor.into(),
            source: None,
            data: serde_json::json!({ "reason": reason }),
        };
        if armed {
            self.otm
                .record_audit_event(event)
                .map_err(|e| format!("Live trading was not armed: the audit log could not record it ({e})."))?;
            self.live_guard.set_armed(true);
        } else {
            self.live_guard.set_armed(false);
            if let Err(error) = self.otm.record_audit_event(event) {
                tracing::error!(%error, "could not record live trading being disarmed");
            }
        }
        Ok(())
    }

    /// Emergency stop: disarm live trading, halt the risk engine, pause
    /// automations, stop strategy sessions and cancel every working live order
    /// APEX knows about. Nothing here depends on the side of a position: working orders are
    /// cancelled whether they would open, add to or close a long or a short (closing a short is
    /// a buy), and open positions of either side are left for the user to close.
    pub async fn kill_switch(&self) -> (Vec<String>, Vec<String>) {
        self.kill_switch_as("unattributed").await
    }

    /// [`kill_switch`](Self::kill_switch) naming who pulled it ("user", "copilot", ...). The audit
    /// trail gets a `kill_switch` event when it starts and another with the outcome, and every
    /// cancel it makes carries the source `kill_switch`.
    pub async fn kill_switch_as(&self, actor: &str) -> (Vec<String>, Vec<String>) {
        use apex_core::domain::models::OrderStatus;
        let ctx = AuditContext::new(actor, Some(crate::order_audit::kill_switch_source()));
        let kill_event = |data: serde_json::Value| OrderAuditEvent {
            order_id: String::new(),
            broker_id: String::new(),
            event: "kill_switch".into(),
            actor: ctx.actor.clone(),
            source: ctx.source.clone(),
            data,
        };
        let was_armed = self.live_guard.is_armed();
        if let Err(error) = self.otm.record_audit_event(kill_event(serde_json::json!({"phase": "start", "was_armed": was_armed}))) {
            tracing::error!(%error, "could not record the kill switch start; stopping anyway");
        }
        let _ = self.set_live_armed_audited(false, actor, "kill_switch");
        self.risk.halt();
        // Algos must not keep trading after an emergency stop.
        self.automations_paused
            .store(true, std::sync::atomic::Ordering::SeqCst);
        crate::commands::strategy_worker::stop_all_sessions();
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
        let mut handled = std::collections::HashSet::new();
        for order in working {
            handled.insert((order.broker_id.clone(), order.id.0.clone()));
            match self.otm.cancel_order_with(&order.id, &order.broker_id, ctx.clone()).await {
                Ok(()) => cancelled.push(order.id.0.clone()),
                Err(error) => failed.push(format!("{}: {error}", order.id.0)),
            }
        }
        // Also reach orders APEX never tracked (placed before a restart, or from
        // another app) by asking each connected broker what it has working.
        for order in self.otm.broker_open_orders().await {
            if !handled.insert((order.broker_id.clone(), order.id.0.clone())) {
                continue;
            }
            match self.otm.cancel_order_with(&order.id, &order.broker_id, ctx.clone()).await {
                Ok(()) => cancelled.push(order.id.0.clone()),
                Err(error) => failed.push(format!("{}: {error}", order.id.0)),
            }
        }
        if let Err(error) = self.otm.record_audit_event(kill_event(
            serde_json::json!({"phase": "done", "cancelled": cancelled, "failed": failed}),
        )) {
            tracing::error!(%error, "could not record the kill switch outcome");
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
        let _serialized = self.broker_change_lock.lock().await;
        self.apply_broker_locked(broker_id).await
    }

    async fn apply_broker_locked(&self, broker_id: &str) -> Result<()> {
        if !self.vault.is_unlocked() {
            return Err(anyhow!("The vault is locked."));
        }
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
        // The lock above prevents a lock_vault from interleaving; this catches one that
        // finished while we were decrypting.
        if !self.vault.is_unlocked() {
            return Err(anyhow!("The vault was locked while applying; nothing was installed."));
        }
        self.replace_broker(broker_id, built).await;
        Ok(())
    }

    /// Build every configurable broker from the vault (after unlock).
    pub async fn apply_all_brokers(&self) {
        let _serialized = self.broker_change_lock.lock().await;
        for broker_id in CONFIGURABLE_BROKERS {
            if let Err(error) = self.apply_broker_locked(broker_id).await {
                tracing::warn!(broker = broker_id, %error, "Failed to apply broker configuration");
            }
        }
    }

    /// Reset a broker to its unconfigured form, dropping any held credentials.
    pub async fn reset_broker(&self, broker_id: &str) -> Result<()> {
        let _serialized = self.broker_change_lock.lock().await;
        self.reset_broker_locked(broker_id).await
    }

    async fn reset_broker_locked(&self, broker_id: &str) -> Result<()> {
        let built = build_broker(broker_id, &BrokerCredentials::default(), &self.market_data_feed)?;
        self.replace_broker(broker_id, built).await;
        Ok(())
    }

    /// Lock the vault and drop every in-memory credential. Live trading is
    /// disarmed and live strategy sessions stop: nothing can trade live without
    /// the vault, and it must not come back armed after the next unlock.
    pub async fn lock_vault(&self) {
        let _serialized = self.broker_change_lock.lock().await;
        let _ = self.set_live_armed_audited(false, "system", "vault_locked");
        crate::commands::strategy_worker::stop_live_sessions();
        self.vault.lock();
        for broker_id in CONFIGURABLE_BROKERS {
            if let Err(error) = self.reset_broker_locked(broker_id).await {
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
                fallback_reason: None,
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
                fallback_reason: None,
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

#[cfg(test)]
mod storage_fallback_tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("apex-storage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn unreachable_postgres_falls_back_to_sqlite_without_leaking_the_password() {
        let root = temp_root("fallback");
        let paths = RuntimePaths::for_test(&root);
        let mut config = AppConfig::default();
        config.storage.backend = "timescale".into();
        // Port 1 refuses connections immediately.
        config.storage.postgres_url = Some("postgresql://apex:hunter2secret@127.0.0.1:1/apex".into());
        config.storage.sqlite_path = root.join("fallback.db").to_string_lossy().into_owned();

        let bootstrap = build_storage_or_fallback(&paths, &config).await.expect("falls back");
        assert_eq!(bootstrap.backend, "sqlite");
        let notice = bootstrap.fallback_reason.expect("fallback is reported");
        assert!(notice.contains("local SQLite"), "{notice}");
        assert!(!notice.contains("hunter2secret"), "password leaked: {notice}");
        assert!(root.join("fallback.db").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn plain_sqlite_reports_no_fallback() {
        let root = temp_root("plain");
        let paths = RuntimePaths::for_test(&root);
        let mut config = AppConfig::default();
        config.storage.backend = "sqlite".into();
        config.storage.sqlite_path = root.join("plain.db").to_string_lossy().into_owned();

        let bootstrap = build_storage_or_fallback(&paths, &config).await.unwrap();
        assert_eq!(bootstrap.backend, "sqlite");
        assert!(bootstrap.fallback_reason.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn broken_sqlite_file_is_not_silently_replaced() {
        let root = temp_root("broken");
        let paths = RuntimePaths::for_test(&root);
        let mut config = AppConfig::default();
        config.storage.backend = "sqlite".into();
        let blocker = root.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        config.storage.sqlite_path = blocker.join("apex.db").to_string_lossy().into_owned();

        assert!(build_storage_or_fallback(&paths, &config).await.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_configured_data_dir_does_not_split_the_audit_ledger() {
        let root = temp_root("ledger-split");
        let paths = RuntimePaths::for_test(&root);
        // Write the default config, then point general.data_dir somewhere else.
        let default = AppConfig::load(&paths).unwrap();
        let raw = std::fs::read_to_string(paths.config_file()).unwrap();
        let other = root.join("elsewhere");
        let edited = raw.replacen(
            &format!("data_dir = \"{}\"", default.general.data_dir),
            &format!("data_dir = \"{}\"", other.display()),
            1,
        );
        assert_ne!(raw, edited, "the test must actually change general.data_dir");
        std::fs::write(paths.config_file(), edited).unwrap();
        assert_eq!(AppConfig::load(&paths).unwrap().resolved_data_dir(&paths).unwrap(), other);

        let _state = AppState::init(paths.clone()).await.expect("AppState::init");

        // Models, datasets, provenance and the ledger commands all open the ledger at the runtime data dir.
        assert!(paths.data_dir().join("ledger").is_dir(), "the ledger is at the runtime data directory");
        assert!(!other.join("ledger").exists(), "no second ledger at the configured directory");
        let _ = std::fs::remove_dir_all(&root);
    }
}
