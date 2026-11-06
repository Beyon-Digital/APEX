//! Automation commands — persisted rules that run a trained model's signal
//! on an interval and place orders through the normal OTM → RiskEngine path.
//!
//! Safety: rules may only target live brokers when `[automations]
//! allow_live_trading = true`; otherwise everything routes to `paper`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

use apex_core::application::automation::{AutomationEngine, AutomationRule};
use apex_core::domain::models::{NewOrderRequest, OrderSide, OrderType, Symbol};

use crate::commands::{ml::ModelRegistry, python_runtime::RuntimePaths};
use crate::state::AppState;
use crate::validation;

#[derive(Debug, Deserialize)]
pub struct CreateAutomationDto {
    pub name: String,
    /// "model_signal" — the only kind for now.
    pub kind: String,
    pub symbol: String,
    pub model_id: String,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_quantity")]
    pub quantity: f64,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// Defaults to "paper". Live brokers need [automations].allow_live_trading.
    #[serde(default = "default_broker")]
    pub broker_id: String,
}

fn default_interval() -> u64 {
    300
}
fn default_quantity() -> f64 {
    1.0
}
fn default_threshold() -> f64 {
    0.5
}
fn default_broker() -> String {
    "paper".into()
}

#[derive(Debug, Serialize)]
pub struct AutomationDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub symbol: String,
    pub model_id: String,
    pub interval_secs: u64,
    pub quantity: f64,
    pub threshold: f64,
    pub broker_id: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_run_at: Option<String>,
    pub last_result: Option<String>,
    pub orders_today: u32,
}

impl From<&AutomationRule> for AutomationDto {
    fn from(r: &AutomationRule) -> Self {
        Self {
            id: r.id.clone(),
            name: r.name.clone(),
            kind: match r.kind {
                apex_core::application::automation::AutomationKind::ModelSignal => {
                    "model_signal".into()
                }
            },
            symbol: r.symbol.clone(),
            model_id: r.model_id.clone(),
            interval_secs: r.interval_secs,
            quantity: r.quantity,
            threshold: r.threshold,
            broker_id: r.broker_id.clone(),
            enabled: r.enabled,
            created_at: r.created_at.to_rfc3339(),
            last_run_at: r.last_run_at.map(|t| t.to_rfc3339()),
            last_result: r.last_result.clone(),
            orders_today: r.orders_today,
        }
    }
}

/// Reject live-broker rules/orders when the config doesn't opt in.
pub(crate) fn check_broker_allowed(state: &AppState, broker_id: &str) -> Result<(), String> {
    validation::validate_broker_id(broker_id)?;
    if broker_id != "paper" && !state.automations_cfg.allow_live_trading {
        return Err(format!(
            "Broker `{broker_id}` is live — automations/AI orders require \
             [automations] allow_live_trading = true in config"
        ));
    }
    Ok(())
}

/// Validate + register a rule. Shared by the IPC command and copilot tools.
pub(crate) fn create_automation_inner(
    dto: CreateAutomationDto,
    state: &AppState,
) -> Result<AutomationDto, String> {
    if dto.kind != "model_signal" {
        return Err(format!(
            "Unknown automation kind `{}` — expected `model_signal`",
            dto.kind
        ));
    }
    validation::validate_symbol(&dto.symbol)?;
    validation::validate_string_length(&dto.name, "name")?;
    validation::validate_string_length(&dto.model_id, "model_id")?;
    validation::validate_quantity(dto.quantity)?;
    if dto.interval_secs < 30 {
        return Err("interval_secs must be ≥ 30".into());
    }
    check_broker_allowed(state, &dto.broker_id)?;

    let rule = state.automations.add_rule(
        dto.name,
        apex_core::application::automation::AutomationKind::ModelSignal,
        dto.symbol.to_uppercase(),
        dto.model_id,
        dto.interval_secs,
        dto.quantity,
        dto.threshold,
        dto.broker_id,
    );
    Ok(AutomationDto::from(&rule))
}

#[tauri::command]
pub async fn create_automation(
    request: CreateAutomationDto,
    state: State<'_, AppState>,
) -> Result<AutomationDto, String> {
    create_automation_inner(request, &state)
}

#[tauri::command]
pub async fn list_automations(state: State<'_, AppState>) -> Result<Vec<AutomationDto>, String> {
    Ok(state
        .automations
        .list()
        .iter()
        .map(AutomationDto::from)
        .collect())
}

#[tauri::command]
pub async fn delete_automation(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    validation::validate_string_length(&id, "automation id")?;
    Ok(state.automations.remove_rule(&id))
}

#[tauri::command]
pub async fn set_automation_enabled(
    id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    validation::validate_string_length(&id, "automation id")?;
    Ok(state.automations.set_enabled(&id, enabled))
}

/// Rules do not fire for this long after launch.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(45);
/// Daily bars older than this (weekends and holidays included) are too stale to trade on.
const MAX_BAR_AGE_DAYS: i64 = 6;

/// Execute one due rule: model signal → order via OTM (risk-gated).
async fn run_rule(
    rule: &AutomationRule,
    state: &AppState,
    models: &ModelRegistry,
    runtime_paths: &RuntimePaths,
) -> String {
    // Recheck the live-trading gate on every run — the stored rule predates
    // any config change, so a rule created while allow_live_trading was true
    // must stop trading the moment it's turned off.
    if let Err(e) = check_broker_allowed(state, &rule.broker_id) {
        return format!("blocked: {e}");
    }

    // Live rules need a connected broker; say so instead of failing inside the order path.
    if rule.broker_id != "paper" {
        let connected = state
            .broker_connection(&rule.broker_id)
            .map(|c| c.authenticated && c.execution_available)
            .unwrap_or(false);
        if !connected {
            return format!(
                "blocked: {} is not connected (unlock the vault and log in under Settings > Adapters)",
                rule.broker_id
            );
        }
    }

    // Score on fresh daily bars, not whatever happens to be cached.
    match crate::commands::data::load_bars(state, &rule.symbol, Some("d1"), 300).await {
        Ok(bars) => match bars.last() {
            Some(last) if (chrono::Utc::now() - last.time).num_days() <= MAX_BAR_AGE_DAYS => {}
            Some(last) => {
                return format!(
                    "blocked: newest daily bar for {} is from {} (stale); refusing to trade on old data",
                    rule.symbol,
                    last.time.format("%Y-%m-%d")
                )
            }
            None => return format!("blocked: no daily bars for {}", rule.symbol),
        },
        Err(e) => return format!("data error: {e}"),
    }

    let signal = match crate::commands::ml::model_signal_inner(
        &rule.model_id,
        &rule.symbol,
        &models.models_dir,
        runtime_paths,
        &state.storage,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return format!("signal error: {e}"),
    };

    let prob = signal.probability.unwrap_or(0.0);
    if prob < rule.threshold {
        return format!(
            "no-op: signal={} prob={prob:.2} < threshold {:.2}",
            signal.signal, rule.threshold
        );
    }

    let side = match signal.signal {
        1 => OrderSide::Buy,
        0 => OrderSide::Sell,
        other => return format!("no-op: unrecognized signal {other}"),
    };

    // An order from an earlier run may still be working (market closed, partial fill).
    // Sending another would stack them all up to fill together later.
    let order_working = state.otm.open_orders().iter().any(|o| {
        o.broker_id == rule.broker_id
            && o.symbol.0 == rule.symbol
            && matches!(
                o.status,
                apex_core::domain::models::OrderStatus::Pending
                    | apex_core::domain::models::OrderStatus::Open
                    | apex_core::domain::models::OrderStatus::PartiallyFilled
            )
    });
    if order_working {
        return format!("no-op: an earlier {} order is still working", rule.symbol);
    }

    // Long/flat semantics: buy only when flat, sell only to exit a long. This stops
    // a signal that stays "1" from pyramiding on every run and never shorts.
    let held = state
        .otm
        .position(&rule.broker_id, &rule.symbol)
        .filter(|p| p.quantity > 0.0 && p.side == OrderSide::Buy)
        .map(|p| p.quantity)
        .unwrap_or(0.0);
    let quantity = match side {
        OrderSide::Buy if held > 0.0 => {
            return format!("no-op: already long {held} {}", rule.symbol);
        }
        OrderSide::Buy => rule.quantity,
        OrderSide::Sell if held <= 0.0 => {
            return format!("no-op: no {} position to exit", rule.symbol);
        }
        OrderSide::Sell => rule.quantity.min(held),
    };

    let side_label = match side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    };
    let cap = state.automations_cfg.max_orders_per_day;
    if !state.automations.can_place_today(&rule.id, cap) {
        return format!("blocked: daily order cap reached ({cap})");
    }

    if rule.broker_id == "paper" {
        // Paper market orders fill against the latest quote.
        if let Err(e) = state.ensure_quote(&rule.symbol).await {
            return format!("order rejected: {e}");
        }
    }

    let order = NewOrderRequest {
        symbol: Symbol(rule.symbol.clone()),
        side: side.clone(),
        order_type: OrderType::Market,
        quantity,
        price: None,
        stop_price: None,
        tag: Some(format!("automation:{}", rule.id)),
        broker_params: None,
    };

    match state
        .guarded_submit(order, &rule.broker_id, crate::state::LiveAuth::Unattended)
        .await
    {
        Ok(id) => {
            state.automations.record_order(&rule.id);
            format!(
                "placed {side} {qty} {sym} @ mkt (order {oid}) — signal={sig} prob={prob:.2}",
                side = side_label,
                qty = quantity,
                sym = rule.symbol,
                oid = id.0,
                sig = signal.signal,
                prob = prob,
            )
        }
        Err(e) => format!("order rejected: {e}"),
    }
}

/// Background scheduler — every 10s, run all due enabled rules.
/// Spawned once at app setup; no-op when `[automations] enabled = false`.
pub fn spawn_automation_loop(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Give the user time to unlock the vault and look around before rules fire.
        tokio::time::sleep(STARTUP_GRACE).await;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let state = app.state::<AppState>();
            if !state.automations_cfg.enabled
                || state
                    .automations_paused
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                continue;
            }
            let due: Vec<AutomationRule> = state.automations.due_rules();
            if due.is_empty() {
                continue;
            }
            let models = app.state::<ModelRegistry>();
            let paths = app.state::<RuntimePaths>();

            for rule in due {
                let result = run_rule(&rule, &state, &models, &paths).await;
                tracing::info!(rule = %rule.id, %result, "automation ran");
                state.automations.record_run(&rule.id, result.clone());
                // Surface the outcome via the alert channel → in-app banner.
                let _ = state.bus.publish(
                    apex_core::bus::message_bus::Topic::Alert,
                    apex_core::bus::message_bus::BusMessage::AlertFired(
                        apex_core::bus::message_bus::AlertMessage {
                            rule_id: rule.id.clone(),
                            message: format!("Automation `{}`: {result}", rule.name),
                            severity: apex_core::bus::message_bus::AlertSeverity::Info,
                        },
                    ),
                );
            }
        }
    });
}

/// Eagerly construct the engine so rules load at startup.
pub fn new_engine(data_dir: &std::path::Path) -> Arc<AutomationEngine> {
    Arc::new(AutomationEngine::new(data_dir.join("automations.json")))
}
