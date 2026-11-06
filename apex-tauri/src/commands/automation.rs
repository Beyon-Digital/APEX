//! Automation commands — persisted rules that run a trained model's signal
//! on an interval and place orders through the normal OTM → RiskEngine path.
//!
//! Safety: rules may only target live brokers when `[automations]
//! allow_live_trading = true`; otherwise everything routes to `paper`. Shorting is opt-in per
//! rule (`allow_short`) and, on a live broker, also needs `[automations] allow_live_shorting`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

use apex_core::application::automation::{AutomationEngine, AutomationRule};
use apex_core::domain::models::{NewOrderRequest, OrderSide, OrderType, Position, Symbol};

use crate::commands::signal_policy::{self, Plan};
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
    /// Opt in to short selling. Needs a broker that can short (see `BrokerConnectionDto.can_short`).
    #[serde(default)]
    pub allow_short: bool,
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
    pub allow_short: bool,
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
            allow_short: r.allow_short,
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

/// Refuse to enable shorting on a broker that cannot do it, with the reason.
pub(crate) fn check_broker_can_short(state: &AppState, broker_id: &str) -> Result<(), String> {
    let live_shorting = state.automations_cfg.allow_live_shorting;
    if signal_policy::broker_can_short(broker_id, live_shorting) {
        return Ok(());
    }
    Err(if signal_policy::adapter_supports_shorting(broker_id) {
        format!(
            "Broker `{broker_id}` cannot short: live shorting is disabled. Set \
             [automations] allow_live_shorting = true in config to allow it"
        )
    } else {
        format!("Broker `{broker_id}` cannot short: its adapter does not support short selling")
    })
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
    if dto.allow_short {
        check_broker_can_short(state, &dto.broker_id)?;
    }

    let rule = state.automations.add_rule(
        dto.name,
        apex_core::application::automation::AutomationKind::ModelSignal,
        dto.symbol.to_uppercase(),
        dto.model_id,
        dto.interval_secs,
        dto.quantity,
        dto.threshold,
        dto.broker_id,
        dto.allow_short,
    );
    Ok(AutomationDto::from(&rule))
}

#[tauri::command]
pub async fn create_automation(
    request: CreateAutomationDto,
    state: State<'_, AppState>,
) -> Result<AutomationDto, String> {
    let name = request.name.clone();
    let result = create_automation_inner(request, &state);
    match &result {
        Ok(created) => crate::activity::record(
            &state,
            "user",
            "automation.create",
            "automation",
            &created.id,
            &format!(
                "Created automation {name}: model {} on {} ({})",
                created.model_id, created.symbol, created.broker_id
            ),
            true,
            None,
        ),
        Err(error) => crate::activity::record(
            &state,
            "user",
            "automation.create",
            "automation",
            "",
            &format!("Could not create automation {name}"),
            false,
            Some(error.clone()),
        ),
    }
    result
}

/// Display name of an automation, looked up before a change that may remove it.
fn automation_label(state: &AppState, id: &str) -> String {
    state
        .automations
        .list()
        .iter()
        .find(|r| r.id == id)
        .map(|r| format!("{} ({id})", r.name))
        .unwrap_or_else(|| id.to_string())
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
    let label = automation_label(&state, &id);
    let removed = state.automations.remove_rule(&id);
    // Deleting a rule that was already gone changed nothing, so it is not logged.
    if removed {
        crate::activity::record(&state, "user", "automation.delete", "automation", &id, &format!("Deleted automation {label}"), true, None);
    }
    Ok(removed)
}

#[tauri::command]
pub async fn set_automation_enabled(
    id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    validation::validate_string_length(&id, "automation id")?;
    let label = automation_label(&state, &id);
    let changed = state.automations.set_enabled(&id, enabled);
    if changed {
        let verb = if enabled { "Enabled" } else { "Disabled" };
        crate::activity::record(&state, "user", "automation.toggle", "automation", &id, &format!("{verb} automation {label}"), true, None);
    }
    Ok(changed)
}

/// Allow or forbid short selling for a rule. Allowing it is refused when the rule's broker cannot
/// short; forbidding it always works. An open short is not touched: the rule closes it on its
/// next run.
#[tauri::command]
pub async fn set_automation_short(
    id: String,
    allow: bool,
    state: State<'_, AppState>,
) -> Result<AutomationDto, String> {
    validation::validate_string_length(&id, "automation id")?;
    let label = automation_label(&state, &id);
    let rule = state
        .automations
        .get(&id)
        .ok_or_else(|| format!("Unknown automation {id}"))?;
    if allow {
        if let Err(error) = check_broker_can_short(&state, &rule.broker_id) {
            crate::activity::record(
                &state,
                "user",
                "automation.toggle",
                "automation",
                &id,
                &format!("Could not allow shorting for {label}"),
                false,
                Some(error.clone()),
            );
            return Err(error);
        }
    }
    if !state.automations.set_allow_short(&id, allow) {
        return Err(format!("Unknown automation {id}"));
    }
    let verb = if allow { "Allowed" } else { "Disallowed" };
    crate::activity::record(
        &state,
        "user",
        "automation.toggle",
        "automation",
        &id,
        &format!("{verb} shorting for {label}"),
        true,
        None,
    );
    state
        .automations
        .get(&id)
        .map(|r| AutomationDto::from(&r))
        .ok_or_else(|| format!("Unknown automation {id}"))
}

/// Rules do not fire for this long after launch.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(45);

/// The rule's signed position at its broker: positive when long, negative when short.
/// A short is held as a `Sell`-side position, so this reads both paper and live brokers.
fn signed_position(position: Option<&Position>) -> f64 {
    match position {
        Some(p) if p.quantity > 0.0 && p.quantity.is_finite() => match p.side {
            OrderSide::Buy => p.quantity,
            OrderSide::Sell => -p.quantity,
        },
        _ => 0.0,
    }
}

/// The rule's signed position, read from the broker itself for a live account.
///
/// The cached position can be stale: a fill is stored before the position refresh finishes
/// (and that refresh can fail), and the user can square off in the broker's own app. A
/// closing order sized from a stale short is a BUY that opens a long, so a live account is
/// re-read first and the run stops if it cannot be.
async fn current_signed_position(
    otm: &apex_core::application::order_trade_manager::OrderTradeManager,
    broker_id: &str,
    symbol: &str,
) -> Result<f64, String> {
    if broker_id != "paper" {
        otm.reconcile_positions(broker_id).await.map_err(|e| {
            format!("blocked: could not read the {broker_id} position ({e}); no order was sent")
        })?;
    }
    Ok(signed_position(otm.position(broker_id, symbol).as_ref()))
}

/// Shares of `symbol` held as delivery (demat) at a live broker. Brokers such as Zerodha keep
/// these apart from intraday positions, so the position view reads flat while they exist.
async fn delivery_holdings(
    otm: &apex_core::application::order_trade_manager::OrderTradeManager,
    broker_id: &str,
    symbol: &str,
) -> Result<f64, String> {
    let holdings = otm.get_holdings(broker_id).await.map_err(|e| {
        format!("blocked: could not read {broker_id} holdings ({e}); no short was sent")
    })?;
    Ok(holdings
        .iter()
        .filter(|h| h.symbol.0.eq_ignore_ascii_case(symbol) && h.side == OrderSide::Buy)
        .map(|h| h.quantity)
        .filter(|q| q.is_finite() && *q > 0.0)
        .sum())
}

/// Opening a short on top of delivery shares leaves the account long and short at once.
fn short_over_holdings_message(symbol: &str, held: f64) -> Option<String> {
    (held > 0.0).then(|| {
        format!(
            "blocked: the account holds {held} {symbol} as delivery shares, so a short would leave it \
             long and short at once; no order was sent"
        )
    })
}

/// What one run does, given the model's wish and the account state. The arguments from the rule
/// and the config are passed through here so a test can see them reach the policy.
fn plan_rule_order(
    rule: &AutomationRule,
    desired: signal_policy::Target,
    held: f64,
    order_working: bool,
    allow_live_shorting: bool,
) -> Plan {
    let can_short = signal_policy::broker_can_short(&rule.broker_id, allow_live_shorting);
    signal_policy::plan_action(
        desired,
        held,
        order_working,
        rule.allow_short,
        can_short,
        rule.quantity,
        &rule.symbol,
    )
}

/// The order a rule sends for a planned intent.
fn rule_order_request(rule: &AutomationRule, intent: signal_policy::Intent, quantity: f64) -> NewOrderRequest {
    NewOrderRequest {
        symbol: Symbol(rule.symbol.clone()),
        side: intent.side(),
        order_type: OrderType::Market,
        quantity,
        price: None,
        stop_price: None,
        tag: Some(format!("automation:{}", rule.id)),
        // Zerodha shorts and their covers must both be MIS: CNC (delivery) cannot be shorted.
        broker_params: signal_policy::broker_params_for(&rule.broker_id, intent),
    }
}

/// An order an automation tried to place, for the activity log.
struct OrderAttempt {
    ok: bool,
    summary: String,
    details: Option<String>,
}

/// Execute one due rule and report what it did. An order the rule tried to place also comes back
/// as an [`OrderAttempt`] so the caller can log it once the rule's own counters are saved.
async fn run_rule(
    rule: &AutomationRule,
    state: &AppState,
    models: &ModelRegistry,
    runtime_paths: &RuntimePaths,
) -> (String, Option<OrderAttempt>) {
    let mut attempt = None;
    let text = run_rule_inner(rule, state, models, runtime_paths, &mut attempt).await;
    (text, attempt)
}

/// Execute one due rule: model signal → order via OTM (risk-gated).
async fn run_rule_inner(
    rule: &AutomationRule,
    state: &AppState,
    models: &ModelRegistry,
    runtime_paths: &RuntimePaths,
    attempt: &mut Option<OrderAttempt>,
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

    // Score on fresh bars of the model's own timeframe, not whatever happens to be cached.
    if let Err(e) = crate::commands::ml::ensure_fresh_model_bars(
        state,
        &models.models_dir,
        &rule.model_id,
        &rule.symbol,
    )
    .await
    {
        return format!("blocked: {e}");
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

    let label_kind = signal_policy::read_label_kind(&models.models_dir, &rule.model_id);
    let desired = match signal_policy::target_position(
        label_kind.as_deref(),
        signal.signal,
        rule.allow_short,
    ) {
        Some(target) => target,
        None => return format!("no-op: unrecognized signal {}", signal.signal),
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

    // Position state machine on the signed position (long > 0, short < 0): at most one order,
    // and never one that reverses through zero. Closing a short is a BUY (a cover).
    let held = match current_signed_position(&state.otm, &rule.broker_id, &rule.symbol).await {
        Ok(held) => held,
        Err(message) => return message,
    };
    let (intent, quantity) = match plan_rule_order(
        rule,
        desired,
        held,
        order_working,
        state.automations_cfg.allow_live_shorting,
    ) {
        Plan::NoOp(message) | Plan::Blocked(message) => return message,
        Plan::Order { intent, quantity } => (intent, quantity),
    };
    if intent == signal_policy::Intent::Short && rule.broker_id != "paper" {
        match delivery_holdings(&state.otm, &rule.broker_id, &rule.symbol).await {
            Ok(delivery) => {
                if let Some(message) = short_over_holdings_message(&rule.symbol, delivery) {
                    return message;
                }
            }
            Err(message) => return message,
        }
    }
    let side_label = intent.label();
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

    let order = rule_order_request(rule, intent, quantity);

    match state
        .guarded_submit(order, &rule.broker_id, crate::state::LiveAuth::Unattended)
        .await
    {
        Ok(id) => {
            state.automations.record_order(&rule.id);
            *attempt = Some(OrderAttempt {
                ok: true,
                summary: format!("{side_label} {quantity} {} {} via {} (order {})", rule.symbol, rule.broker_id, rule.name, id.0),
                details: None,
            });
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
        Err(e) => {
            *attempt = Some(OrderAttempt {
                ok: false,
                summary: format!("Order rejected: {side_label} {quantity} {} {} via {}", rule.symbol, rule.broker_id, rule.name),
                details: Some(e.to_string()),
            });
            format!("order rejected: {e}")
        }
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
                let (result, attempt) = run_rule(&rule, &state, &models, &paths).await;
                tracing::info!(rule = %rule.id, %result, "automation ran");
                state.automations.record_run(&rule.id, result.clone());
                // Logged after the rule's counters are saved, so the panel that reloads on this
                // entry shows the new order count and result together.
                if let Some(order) = attempt {
                    crate::activity::record(
                        &state,
                        "automation",
                        "automation.order",
                        "automation",
                        &rule.id,
                        &order.summary,
                        order.ok,
                        order.details,
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::signal_policy::Intent;

    fn position(side: OrderSide, quantity: f64) -> Position {
        Position {
            symbol: Symbol("AAPL".into()),
            quantity,
            avg_price: 100.0,
            side,
            pnl: 0.0,
            pnl_pct: 0.0,
            broker_id: "paper".into(),
        }
    }

    #[test]
    fn short_positions_are_negative_and_long_positions_positive() {
        assert_eq!(signed_position(None), 0.0);
        assert_eq!(signed_position(Some(&position(OrderSide::Buy, 7.0))), 7.0);
        assert_eq!(signed_position(Some(&position(OrderSide::Sell, 7.0))), -7.0);
        assert_eq!(signed_position(Some(&position(OrderSide::Sell, 0.0))), 0.0);
        assert_eq!(signed_position(Some(&position(OrderSide::Buy, f64::NAN))), 0.0);
    }

    #[test]
    fn create_request_without_allow_short_defaults_to_false() {
        let dto: CreateAutomationDto = serde_json::from_str(
            r#"{"name":"n","kind":"model_signal","symbol":"AAPL","model_id":"m"}"#,
        )
        .unwrap();
        assert!(!dto.allow_short);
        assert_eq!(dto.broker_id, "paper");
        let dto: CreateAutomationDto = serde_json::from_str(
            r#"{"name":"n","kind":"model_signal","symbol":"AAPL","model_id":"m","allow_short":true}"#,
        )
        .unwrap();
        assert!(dto.allow_short);
    }

    #[test]
    fn automation_dto_reports_allow_short() {
        let engine = AutomationEngine::new(
            std::env::temp_dir().join(format!("apex-auto-dto-{}", uuid::Uuid::new_v4())).join("a.json"),
        );
        let rule = engine.add_rule(
            "r".into(),
            apex_core::application::automation::AutomationKind::ModelSignal,
            "AAPL".into(),
            "m".into(),
            60,
            1.0,
            0.5,
            "paper".into(),
            true,
        );
        let json = serde_json::to_value(AutomationDto::from(&rule)).unwrap();
        assert_eq!(json["allow_short"], serde_json::json!(true));
    }

    #[test]
    fn config_defaults_to_no_live_shorting_and_old_files_still_parse() {
        assert!(!crate::config::AutomationsConfig::default().allow_live_shorting);
        // An [automations] table written before the key existed.
        let old: crate::config::AutomationsConfig =
            toml::from_str("enabled = true\nallow_live_trading = true\nmax_orders_per_day = 5\n").unwrap();
        assert!(old.allow_live_trading);
        assert!(!old.allow_live_shorting);
        let empty: crate::config::AutomationsConfig = toml::from_str("").unwrap();
        assert!(!empty.allow_live_shorting && !empty.allow_live_trading);
        let on: crate::config::AutomationsConfig = toml::from_str("allow_live_shorting = true").unwrap();
        assert!(on.allow_live_shorting);
    }

    // ---- the glue in run_rule_inner ----

    use crate::commands::signal_policy::Target;
    use apex_core::application::order_trade_manager::OrderTradeManager;
    use std::sync::Mutex;

    fn rule(broker: &str, allow_short: bool) -> AutomationRule {
        AutomationEngine::new(
            std::env::temp_dir().join(format!("apex-auto-glue-{}", uuid::Uuid::new_v4())).join("a.json"),
        )
        .add_rule(
            "r".into(),
            apex_core::application::automation::AutomationKind::ModelSignal,
            "RELIANCE.NS".into(),
            "m".into(),
            60,
            10.0,
            0.5,
            broker.into(),
            allow_short,
        )
    }

    fn pos(side: OrderSide, quantity: f64) -> Position {
        Position {
            symbol: Symbol("RELIANCE.NS".into()),
            quantity,
            avg_price: 100.0,
            side,
            pnl: 0.0,
            pnl_pct: 0.0,
            broker_id: "zerodha".into(),
        }
    }

    /// A broker whose reported positions and holdings the test controls.
    struct Scripted {
        positions: Mutex<Result<Vec<Position>, String>>,
        holdings: Mutex<Result<Vec<Position>, String>>,
    }

    impl Scripted {
        fn new(positions: Vec<Position>) -> Arc<Self> {
            Arc::new(Self { positions: Mutex::new(Ok(positions)), holdings: Mutex::new(Ok(Vec::new())) })
        }
    }

    // `async_trait` is not a dependency of this crate, so the trait's boxed-future methods are
    // written out by hand.
    type Fut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<T>> + Send + 'a>>;

    impl apex_core::ports::execution::ExecutionPort for Scripted {
        fn place_order<'a, 'b, 'c>(&'a self, _: &'b NewOrderRequest) -> Fut<'c, apex_core::domain::models::OrderId>
        where
            'a: 'c,
            'b: 'c,
            Self: 'c,
        {
            unreachable!()
        }
        fn cancel_order<'a, 'b, 'c>(&'a self, _: &'b apex_core::domain::models::OrderId) -> Fut<'c, ()>
        where
            'a: 'c,
            'b: 'c,
            Self: 'c,
        {
            unreachable!()
        }
        fn modify_order<'a, 'b, 'c, 'd>(
            &'a self,
            _: &'b apex_core::domain::models::OrderId,
            _: &'c apex_core::domain::models::ModifyParams,
        ) -> Fut<'d, ()>
        where
            'a: 'd,
            'b: 'd,
            'c: 'd,
            Self: 'd,
        {
            unreachable!()
        }
        fn get_order_status<'a, 'b, 'c>(
            &'a self,
            _: &'b apex_core::domain::models::OrderId,
        ) -> Fut<'c, apex_core::domain::models::Order>
        where
            'a: 'c,
            'b: 'c,
            Self: 'c,
        {
            unreachable!()
        }
        fn get_positions<'a, 'b>(&'a self) -> Fut<'b, Vec<Position>>
        where
            'a: 'b,
            Self: 'b,
        {
            let result = self.positions.lock().unwrap().clone().map_err(|e| anyhow::anyhow!(e));
            Box::pin(async move { result })
        }
        fn get_holdings<'a, 'b>(&'a self) -> Fut<'b, Vec<Position>>
        where
            'a: 'b,
            Self: 'b,
        {
            let result = self.holdings.lock().unwrap().clone().map_err(|e| anyhow::anyhow!(e));
            Box::pin(async move { result })
        }
        fn get_account_balance<'a, 'b>(&'a self) -> Fut<'b, apex_core::domain::models::AccountBalance>
        where
            'a: 'b,
            Self: 'b,
        {
            unreachable!()
        }
        fn broker_id(&self) -> &'static str {
            "zerodha"
        }
        fn supported_order_types(&self) -> &[OrderType] {
            &[OrderType::Market]
        }
        fn health(&self) -> apex_core::ports::market_data::AdapterHealth {
            apex_core::ports::market_data::AdapterHealth::Healthy
        }
    }

    fn otm_with(broker_id: &str, broker: Arc<Scripted>) -> OrderTradeManager {
        let bus = Arc::new(apex_core::bus::message_bus::MessageBus::new());
        let risk = Arc::new(apex_core::application::risk_engine::RiskEngine::new(Default::default()));
        let otm = OrderTradeManager::new(risk, bus);
        otm.register_execution(broker_id.into(), broker);
        otm
    }

    #[tokio::test]
    async fn a_live_account_is_re_read_so_a_stale_short_cannot_send_a_cover_buy() {
        let broker = Scripted::new(vec![pos(OrderSide::Sell, 10.0)]);
        let otm = otm_with("zerodha", broker.clone());
        // The fill that closed the short is stored, but the position refresh has not landed:
        // the cache still says short 10.
        otm.reconcile_positions("zerodha").await.unwrap();
        assert_eq!(signed_position(otm.position("zerodha", "RELIANCE.NS").as_ref()), -10.0);
        // The broker is in fact flat now.
        *broker.positions.lock().unwrap() = Ok(Vec::new());

        let held = current_signed_position(&otm, "zerodha", "RELIANCE.NS").await.unwrap();
        assert_eq!(held, 0.0);
        // Flat and bearish: nothing to close. Planned from the stale cache this was a 10-share
        // cover BUY that would open a long.
        let plan = plan_rule_order(&rule("zerodha", true), Target::Flat, held, false, true);
        assert!(matches!(plan, Plan::NoOp(_)), "{plan:?}");
    }

    #[tokio::test]
    async fn a_live_run_stops_when_the_broker_position_cannot_be_read() {
        let broker = Scripted::new(vec![pos(OrderSide::Sell, 10.0)]);
        let otm = otm_with("zerodha", broker.clone());
        otm.reconcile_positions("zerodha").await.unwrap();
        *broker.positions.lock().unwrap() = Err("network down".into());

        let message = current_signed_position(&otm, "zerodha", "RELIANCE.NS").await.unwrap_err();
        assert!(message.starts_with("blocked:") && message.contains("network down"), "{message}");
    }

    #[tokio::test]
    async fn paper_positions_are_read_from_the_manager_without_a_broker_call() {
        // A paper broker that would fail if asked; the paper account is the manager's own state.
        let broker = Scripted::new(Vec::new());
        *broker.positions.lock().unwrap() = Err("must not be called".into());
        let otm = otm_with("paper", broker);
        assert_eq!(current_signed_position(&otm, "paper", "RELIANCE.NS").await.unwrap(), 0.0);
    }

    #[tokio::test]
    async fn delivery_holdings_of_the_symbol_are_summed() {
        let broker = Scripted::new(Vec::new());
        let mut other = pos(OrderSide::Buy, 99.0);
        other.symbol = Symbol("TCS.NS".into());
        *broker.holdings.lock().unwrap() = Ok(vec![pos(OrderSide::Buy, 4.0), pos(OrderSide::Buy, 3.0), other]);
        let otm = otm_with("zerodha", broker.clone());
        assert_eq!(delivery_holdings(&otm, "zerodha", "RELIANCE.NS").await.unwrap(), 7.0);
        assert_eq!(delivery_holdings(&otm, "zerodha", "INFY.NS").await.unwrap(), 0.0);

        *broker.holdings.lock().unwrap() = Err("down".into());
        let message = delivery_holdings(&otm, "zerodha", "RELIANCE.NS").await.unwrap_err();
        assert!(message.starts_with("blocked:"), "{message}");
    }

    #[test]
    fn a_short_over_delivery_shares_is_blocked() {
        assert!(short_over_holdings_message("RELIANCE.NS", 7.0).unwrap().starts_with("blocked:"));
        assert!(short_over_holdings_message("RELIANCE.NS", 0.0).is_none());
    }

    #[test]
    fn the_rules_allow_short_flag_reaches_the_policy() {
        let off = plan_rule_order(&rule("paper", false), Target::Short, 0.0, false, false);
        assert!(matches!(off, Plan::NoOp(_)), "{off:?}");
        let on = plan_rule_order(&rule("paper", true), Target::Short, 0.0, false, false);
        assert!(matches!(on, Plan::Order { intent: Intent::Short, quantity } if quantity == 10.0), "{on:?}");
    }

    #[test]
    fn live_shorting_needs_the_config_flag_and_a_capable_broker() {
        let r = rule("zerodha", true);
        assert!(matches!(plan_rule_order(&r, Target::Short, 0.0, false, false), Plan::Blocked(_)));
        assert!(matches!(
            plan_rule_order(&r, Target::Short, 0.0, false, true),
            Plan::Order { intent: Intent::Short, .. }
        ));
        // A broker that cannot short stays blocked even with the flag on.
        let angel = rule("angel_one", true);
        assert!(matches!(plan_rule_order(&angel, Target::Short, 0.0, false, true), Plan::Blocked(_)));
    }

    #[test]
    fn a_cover_is_sized_to_the_short_and_never_exceeds_it() {
        let r = rule("paper", true);
        let plan = plan_rule_order(&r, Target::Flat, -4.0, false, false);
        assert!(matches!(plan, Plan::Order { intent: Intent::Cover, quantity } if quantity == 4.0), "{plan:?}");
    }

    #[test]
    fn a_zerodha_short_and_its_cover_go_out_as_mis_and_other_orders_do_not() {
        let r = rule("zerodha", true);
        for intent in [Intent::Short, Intent::Cover] {
            let order = rule_order_request(&r, intent, 3.0);
            let params = order.broker_params.expect("MIS needed");
            assert_eq!(params.product.as_deref(), Some("MIS"));
            assert_eq!(order.quantity, 3.0);
            assert_eq!(order.tag.as_deref(), Some(format!("automation:{}", r.id).as_str()));
        }
        assert_eq!(rule_order_request(&r, Intent::Short, 1.0).side, OrderSide::Sell);
        assert_eq!(rule_order_request(&r, Intent::Cover, 1.0).side, OrderSide::Buy);
        assert!(rule_order_request(&r, Intent::Buy, 1.0).broker_params.is_none());
        assert!(rule_order_request(&r, Intent::Sell, 1.0).broker_params.is_none());
        assert!(rule_order_request(&rule("paper", true), Intent::Short, 1.0).broker_params.is_none());
    }
}
