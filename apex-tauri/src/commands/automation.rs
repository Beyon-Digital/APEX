//! Automation commands — persisted rules that run a trained model's signal
//! on an interval and place orders through the normal OTM → RiskEngine path.
//!
//! Safety: rules may only target live brokers when `[automations]
//! allow_live_trading = true`; otherwise everything routes to `paper`. Shorting is opt-in per
//! rule (`allow_short`) and, on a live broker, also needs `[automations] allow_live_shorting`.
//!
//! Audit: `automations.json` is the working copy. Every change to a rule's definition (create,
//! enable/disable, allow_short, delete, restore) appends a version of that rule to the audit ledger
//! (kind `automation`, object id = rule id). A delete is a tombstone, never an erase. The versioned
//! content leaves out the run counters (`orders_today`, `last_run_at`, `last_result`), which change on
//! every run and are recorded by the order event stream instead. Every order a rule places names the
//! rule's version and content hash, and its model, as its source.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

use apex_core::application::automation::{
    with_actor, AutomationEngine, AutomationRule, RuleChange, RuleVersionHook,
};
use apex_core::domain::models::{NewOrderRequest, OrderSide, OrderType, Position, Symbol};

use crate::commands::signal_policy::{self, Plan};
use crate::commands::{ml::ModelRegistry, python_runtime::RuntimePaths};
use crate::ledger::{Kind, Ledger};
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
///
/// The audit trail names the Copilot as the author (its tool is the only other caller); the IPC
/// command uses [`create_automation_as`] with the user.
pub(crate) fn create_automation_inner(
    dto: CreateAutomationDto,
    state: &AppState,
) -> Result<AutomationDto, String> {
    create_automation_as(dto, state, "copilot")
}

/// [`create_automation_inner`] naming who created the rule in the audit trail.
pub(crate) fn create_automation_as(
    dto: CreateAutomationDto,
    state: &AppState,
    actor: &str,
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

    let rule = with_actor(actor, || {
        state.automations.add_rule(
            dto.name,
            apex_core::application::automation::AutomationKind::ModelSignal,
            dto.symbol.to_uppercase(),
            dto.model_id,
            dto.interval_secs,
            dto.quantity,
            dto.threshold,
            dto.broker_id,
            dto.allow_short,
        )
    });
    Ok(AutomationDto::from(&rule))
}

#[tauri::command]
pub async fn create_automation(
    request: CreateAutomationDto,
    state: State<'_, AppState>,
) -> Result<AutomationDto, String> {
    let name = request.name.clone();
    let result = create_automation_as(request, &state, "user");
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
    let removed = with_actor("user", || state.automations.remove_rule(&id));
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
    let changed = with_actor("user", || state.automations.set_enabled(&id, enabled));
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
    if !with_actor("user", || state.automations.set_allow_short(&id, allow)) {
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

    // The rule's current ledger version and hash, and its model, travel with the order.
    let source = crate::order_audit::automation_source(None, &rule.id, &rule.model_id);
    match state
        .guarded_submit_with(order, &rule.broker_id, crate::state::LiveAuth::Unattended, Some(source))
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

/// Eagerly construct the engine so rules load at startup. When the audit ledger is open, rules that
/// have no history yet are registered as version 1 and every later change is versioned.
pub fn new_engine(data_dir: &std::path::Path) -> Arc<AutomationEngine> {
    new_engine_with(data_dir, crate::ledger::global().cloned())
}

pub(crate) fn new_engine_with(data_dir: &std::path::Path, ledger: Option<Ledger>) -> Arc<AutomationEngine> {
    let engine = Arc::new(AutomationEngine::new(data_dir.join("automations.json")));
    match ledger {
        Some(ledger) => {
            let recorded = register_existing_rules(&engine, &ledger);
            if recorded > 0 {
                tracing::info!(recorded, "automation rules recorded in the audit ledger");
            }
            engine.set_version_hook(Arc::new(LedgerRuleHook { ledger }));
        }
        None => tracing::error!("the audit ledger is not open; automation changes will not be versioned"),
    }
    engine
}

/// Appends a version of the rule to the audit ledger for every change to its definition.
pub struct LedgerRuleHook {
    ledger: Ledger,
}

impl LedgerRuleHook {
    #[allow(dead_code)] // used by tests; the app builds the hook in `new_engine_with`
    pub fn new(ledger: Ledger) -> Self {
        Self { ledger }
    }
}

fn change_summary(change: &RuleChange<'_>) -> String {
    let rule = change.rule;
    match change.action {
        "create" => format!("Created automation {}: model {} on {} ({})", rule.name, rule.model_id, rule.symbol, rule.broker_id),
        "enable" => format!("Enabled automation {}", rule.name),
        "disable" => format!("Disabled automation {}", rule.name),
        "allow_short" => format!("Set shorting {} for automation {}", if rule.allow_short { "on" } else { "off" }, rule.name),
        "delete" => format!("Deleted automation {}", rule.name),
        "restore" => format!("Restored automation {}", rule.name),
        other => format!("Changed automation {} ({other})", rule.name),
    }
}

impl RuleVersionHook for LedgerRuleHook {
    fn on_change(&self, change: &RuleChange<'_>) -> Result<(), String> {
        let id = change.rule.id.as_str();
        let content = change.rule.audit_content();
        let summary = change_summary(change);
        if change.deleted {
            // A rule that never reached the ledger still gets its history before the delete marker.
            if self.ledger.head(Kind::Automation, id).is_none() {
                self.ledger.put(Kind::Automation, id, content.as_bytes(), "system", "Recorded before deletion")?;
            }
            return self.ledger.tombstone(Kind::Automation, id, &change.actor, &summary).map(|_| ());
        }
        self.ledger.put(Kind::Automation, id, content.as_bytes(), &change.actor, &summary).map(|_| ())
    }
}

/// Bring the ledger in step with `automations.json` at startup. A rule with no history becomes
/// version 1 (actor "system"). A rule whose definition differs from its ledger head, or that exists
/// again after a delete marker, was changed outside APEX: that is recorded as a new version with
/// actor "external" and action "external" (never lost, never silently overwritten). A rule that is
/// in the ledger but missing from the file is not touched: an unreadable file must not be able to
/// mark every rule deleted. Returns how many versions were appended.
pub fn register_existing_rules(engine: &AutomationEngine, ledger: &Ledger) -> usize {
    let mut appended = 0;
    for rule in engine.list() {
        let content = rule.audit_content();
        let hash = Ledger::sha256_hex(content.as_bytes());
        let result = match ledger.head(Kind::Automation, &rule.id) {
            None => ledger
                .put(Kind::Automation, &rule.id, content.as_bytes(), "system", "Registered an existing automation")
                .map(|_| true),
            Some(head) if !head.is_tombstone() && head.content_hash.as_deref() == Some(hash.as_str()) => Ok(false),
            Some(_) => ledger
                .put_as(
                    Kind::Automation,
                    &rule.id,
                    content.as_bytes(),
                    "external",
                    "external",
                    "Rule file differs from the last recorded version",
                )
                .map(|_| true),
        };
        match result {
            Ok(true) => appended += 1,
            Ok(false) => {}
            Err(error) => tracing::error!(rule = %rule.id, %error, "could not record an automation in the audit ledger"),
        }
    }
    appended
}

/// Put a rule back to an older version. The ledger gets a new version (action "restore") whose
/// content equals the old one, and the rule is rewritten from it. The rule keeps its current run
/// counters; a deleted rule comes back with fresh ones. For the history panel's restore button.
#[allow(dead_code)] // called by the history panel's restore command
pub fn restore_automation_with(
    engine: &AutomationEngine,
    ledger: &Ledger,
    id: &str,
    version: u32,
    actor: &str,
) -> Result<AutomationRule, String> {
    let (_, bytes) = ledger.restore(Kind::Automation, id, version, actor)?;
    let restored: AutomationRule = serde_json::from_slice(&bytes)
        .map_err(|e| format!("version {version} of automation {id} could not be read: {e}"))?;
    if restored.id != id {
        return Err(format!("version {version} of automation {id} belongs to a different rule"));
    }
    Ok(with_actor(actor, || engine.apply_restored(restored)))
}

/// [`restore_automation_with`] on the running app's engine and ledger, logged in the activity log.
#[allow(dead_code)] // called by the history panel's restore command
pub fn restore_automation(state: &AppState, id: &str, version: u32, actor: &str) -> Result<AutomationDto, String> {
    validation::validate_string_length(id, "automation id")?;
    let ledger = crate::ledger::global().ok_or_else(|| "The audit ledger is not open.".to_string())?;
    let label = automation_label(state, id);
    match restore_automation_with(&state.automations, ledger, id, version, actor) {
        Ok(rule) => {
            crate::activity::record(
                state,
                actor,
                "automation.restore",
                "automation",
                id,
                &format!("Restored automation {label} to version {version}"),
                true,
                None,
            );
            Ok(AutomationDto::from(&rule))
        }
        Err(error) => {
            crate::activity::record(
                state,
                actor,
                "automation.restore",
                "automation",
                id,
                &format!("Could not restore automation {label} to version {version}"),
                false,
                Some(error.clone()),
            );
            Err(error)
        }
    }
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

    // ── audit versions of the rules ──

    fn audited_engine() -> (Arc<AutomationEngine>, Ledger, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("apex-auto-ledger-{}", uuid::Uuid::new_v4()));
        let ledger = Ledger::open(&dir.join("ledger")).unwrap();
        let engine = new_engine_with(&dir, Some(ledger.clone()));
        (engine, ledger, dir)
    }

    fn add(engine: &AutomationEngine, actor: &str) -> AutomationRule {
        with_actor(actor, || {
            engine.add_rule(
                "r".into(),
                apex_core::application::automation::AutomationKind::ModelSignal,
                "AAPL".into(),
                "m".into(),
                60,
                1.0,
                0.5,
                "paper".into(),
                false,
            )
        })
    }

    fn versions(ledger: &Ledger, id: &str) -> Vec<(u32, String, String)> {
        ledger
            .history(Kind::Automation, id)
            .into_iter()
            .map(|v| (v.version, v.action, v.actor))
            .collect()
    }

    #[test]
    fn every_change_to_a_rule_is_a_new_version_of_the_same_object() {
        let (engine, ledger, dir) = audited_engine();
        let rule = add(&engine, "user");
        with_actor("user", || engine.set_enabled(&rule.id, false));
        with_actor("user", || engine.set_enabled(&rule.id, false)); // already off: nothing changed
        with_actor("copilot", || engine.set_allow_short(&rule.id, true));
        with_actor("user", || engine.set_enabled(&rule.id, true));
        with_actor("user", || engine.remove_rule(&rule.id));

        assert_eq!(
            versions(&ledger, &rule.id),
            [
                (1, "create".to_string(), "user".to_string()),
                (2, "update".to_string(), "user".to_string()),
                (3, "update".to_string(), "copilot".to_string()),
                (4, "update".to_string(), "user".to_string()),
                (5, "delete".to_string(), "user".to_string()),
            ]
        );
        // The delete is a marker: every earlier version is still readable.
        let first: serde_json::Value = serde_json::from_slice(&ledger.content(Kind::Automation, &rule.id, 1).unwrap()).unwrap();
        assert_eq!(first["enabled"], true);
        let second: serde_json::Value = serde_json::from_slice(&ledger.content(Kind::Automation, &rule.id, 2).unwrap()).unwrap();
        assert_eq!(second["enabled"], false);
        assert!(ledger.head(Kind::Automation, &rule.id).unwrap().is_tombstone());
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn run_counters_never_create_a_version() {
        let (engine, ledger, dir) = audited_engine();
        let rule = add(&engine, "user");
        let before = ledger.head(Kind::Automation, &rule.id).unwrap();
        engine.record_order(&rule.id);
        engine.record_order(&rule.id);
        engine.record_run(&rule.id, "placed BUY".into());
        engine.record_run(&rule.id, "no-op".into());
        let after = ledger.head(Kind::Automation, &rule.id).unwrap();
        assert_eq!(before.version, after.version);
        assert_eq!(ledger.history(Kind::Automation, &rule.id).len(), 1);
        // The stored content has none of the volatile fields.
        let stored: serde_json::Value = serde_json::from_slice(&ledger.content(Kind::Automation, &rule.id, 1).unwrap()).unwrap();
        for volatile in ["orders_today", "orders_day", "last_run_at", "last_result"] {
            assert!(stored.get(volatile).is_none(), "{volatile} must not be versioned");
        }
        assert_eq!(stored["model_id"], "m");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_change_without_a_named_actor_is_visibly_unattributed() {
        let (engine, ledger, dir) = audited_engine();
        let rule = engine.add_rule(
            "r".into(),
            apex_core::application::automation::AutomationKind::ModelSignal,
            "AAPL".into(),
            "m".into(),
            60,
            1.0,
            0.5,
            "paper".into(),
            false,
        );
        assert_eq!(versions(&ledger, &rule.id)[0].2, "unattributed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restoring_appends_a_new_version_keeps_the_counters_and_brings_back_a_deleted_rule() {
        let (engine, ledger, dir) = audited_engine();
        let rule = add(&engine, "user");
        engine.record_order(&rule.id);
        with_actor("user", || engine.set_enabled(&rule.id, false));

        // Back to version 1 (enabled): counters are the current ones.
        let restored = restore_automation_with(&engine, &ledger, &rule.id, 1, "user").unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.orders_today, 1);
        assert!(engine.get(&rule.id).unwrap().enabled);
        let history = versions(&ledger, &rule.id);
        assert_eq!(history.len(), 3);
        assert_eq!(history[2], (3, "restore".to_string(), "user".to_string()));
        assert_eq!(ledger.history(Kind::Automation, &rule.id)[2].restored_from, Some(1));

        // A deleted rule comes back, with fresh counters, as the next version of the same object.
        with_actor("user", || engine.remove_rule(&rule.id));
        assert!(engine.get(&rule.id).is_none());
        let back = restore_automation_with(&engine, &ledger, &rule.id, 2, "user").unwrap();
        assert!(!back.enabled);
        assert_eq!(back.orders_today, 0);
        assert!(engine.get(&rule.id).is_some());
        let history = ledger.history(Kind::Automation, &rule.id);
        assert_eq!(history.len(), 5);
        assert_eq!(history[4].action, "restore");
        assert_eq!(history[3].action, "delete");
        // Nothing was overwritten: version 1 still has its original content.
        let first: serde_json::Value = serde_json::from_slice(&ledger.content(Kind::Automation, &rule.id, 1).unwrap()).unwrap();
        assert_eq!(first["enabled"], true);

        assert!(restore_automation_with(&engine, &ledger, &rule.id, 99, "user").is_err());
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_rules_are_registered_as_version_one_and_outside_edits_are_recorded_as_external() {
        let dir = std::env::temp_dir().join(format!("apex-auto-register-{}", uuid::Uuid::new_v4()));
        // A rule persisted before the ledger existed.
        let before = new_engine_with(&dir, None);
        let rule = add(&before, "user");
        drop(before);

        let ledger = Ledger::open(&dir.join("ledger")).unwrap();
        let engine = new_engine_with(&dir, Some(ledger.clone()));
        assert_eq!(versions(&ledger, &rule.id), [(1, "create".to_string(), "system".to_string())]);

        // A second start with nothing changed adds nothing.
        drop(engine);
        let _ = new_engine_with(&dir, Some(ledger.clone()));
        assert_eq!(ledger.history(Kind::Automation, &rule.id).len(), 1);

        // Someone edits automations.json by hand.
        let path = dir.join("automations.json");
        let text = std::fs::read_to_string(&path).unwrap().replace("\"quantity\": 1.0", "\"quantity\": 500.0");
        assert!(text.contains("500.0"));
        std::fs::write(&path, text).unwrap();
        let _ = new_engine_with(&dir, Some(ledger.clone()));
        let history = versions(&ledger, &rule.id);
        assert_eq!(history[1], (2, "external".to_string(), "external".to_string()));
        let old: serde_json::Value = serde_json::from_slice(&ledger.content(Kind::Automation, &rule.id, 1).unwrap()).unwrap();
        assert_eq!(old["quantity"], 1.0, "the earlier version keeps its content");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleting_a_rule_that_never_reached_the_ledger_still_leaves_its_history() {
        let (engine, ledger, dir) = audited_engine();
        // Simulate a rule created while the ledger could not record it.
        let rule = {
            let bare = AutomationEngine::new(dir.join("other.json"));
            bare.add_rule(
                "x".into(),
                apex_core::application::automation::AutomationKind::ModelSignal,
                "AAPL".into(),
                "m".into(),
                60,
                1.0,
                0.5,
                "paper".into(),
                false,
            )
        };
        let hook = LedgerRuleHook::new(ledger.clone());
        hook.on_change(&RuleChange { action: "delete", rule: &rule, deleted: true, actor: "user".into() }).unwrap();
        let history = versions(&ledger, &rule.id);
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].1, "delete");
        drop(engine);
        let _ = std::fs::remove_dir_all(dir);
    }
}
