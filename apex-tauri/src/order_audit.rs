//! The order audit trail: every state an order passes through is a new event in the hash-chained
//! "orders" stream of the audit ledger. An earlier event is never updated or removed.
//!
//! * [`LedgerOrderSink`] implements the core's `OrderAuditSink` over the ledger. `seq`, `ts` and the
//!   hashes come from the ledger; the rest is the event as the order manager reported it.
//! * The `*_source` functions say who or what produced an order and look up the exact version (and
//!   content hash) of the automation, strategy or model involved, so every trade traces back to the
//!   code that produced it.
//! * [`list_order_events`] is the Tauri command behind the UI's order trail.
//!
//! HONEST LIMIT. A hash chain detects an edit or a deletion in the middle of the stream. Someone with
//! full write access to the data directory could rewrite the whole stream consistently, which is why
//! the stream's head hash is exposed by the ledger status so it can be copied somewhere else.

use crate::ledger::{self, EventRecord, Kind, Ledger};
use apex_core::ports::audit::{OrderAuditEvent, OrderAuditSink};
use serde_json::{json, Value};
use std::collections::HashSet;

pub use apex_core::ports::audit::{AuditContext, OrderAuditSource as OrderSource};

pub const ORDERS_STREAM: &str = "orders";
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 5_000;

/// Writes order events to the "orders" stream. Built without a ledger it uses the process-wide one at
/// the time of each event, so a ledger that failed to open is a clear error rather than a silent skip.
pub struct LedgerOrderSink {
    ledger: Option<Ledger>,
}

impl LedgerOrderSink {
    /// A sink over the process-wide ledger ([`ledger::global`]).
    pub fn global() -> Self {
        Self { ledger: None }
    }

    /// A sink over a specific ledger (tests).
    #[allow(dead_code)]
    pub fn with_ledger(ledger: Ledger) -> Self {
        Self { ledger: Some(ledger) }
    }

    fn ledger(&self) -> Result<&Ledger, String> {
        self.resolve(ledger::global())
    }

    /// The sink's own ledger, else `process_wide`; an error when neither exists.
    fn resolve<'a>(&'a self, process_wide: Option<&'a Ledger>) -> Result<&'a Ledger, String> {
        self.ledger
            .as_ref()
            .or(process_wide)
            .ok_or_else(|| "the audit ledger is not open".to_string())
    }
}

impl OrderAuditSink for LedgerOrderSink {
    fn record(&self, event: OrderAuditEvent) -> Result<(), String> {
        let ledger = self.ledger()?;
        let source = match &event.source {
            Some(source) => serde_json::to_value(source).map_err(|e| e.to_string())?,
            None => Value::Null,
        };
        ledger
            .append_event(
                ORDERS_STREAM,
                json!({
                    "order_id": event.order_id,
                    "broker_id": event.broker_id,
                    "event": event.event,
                    "actor": event.actor,
                    "source": source,
                    "data": event.data,
                }),
            )
            .map(|_| ())
    }
}

// ── Who produced an order ─────────────────────────────────────────────

fn head_of(ledger: Option<&Ledger>, kind: Kind, id: &str) -> (Option<u32>, Option<String>) {
    match ledger.or_else(|| ledger::global()).and_then(|l| l.head(kind, id)) {
        Some(head) => (Some(head.version), head.content_hash),
        None => (None, None),
    }
}

/// An order a person placed in the UI.
pub fn manual_source() -> OrderSource {
    OrderSource::new("manual", "")
}

/// An order the Copilot placed during the turn `turn_id`.
pub fn copilot_source(turn_id: &str) -> OrderSource {
    OrderSource::new("copilot", turn_id)
}

/// The emergency stop.
pub fn kill_switch_source() -> OrderSource {
    OrderSource::new("kill_switch", "")
}

/// A running strategy at `path` (the ledger id of the strategy), with the version and content hash it
/// had at order time.
pub fn strategy_source(ledger: Option<&Ledger>, path: &str) -> OrderSource {
    let (version, hash) = head_of(ledger, Kind::Strategy, path);
    OrderSource { kind: "strategy".into(), id: path.into(), version, hash, ..Default::default() }
}

/// An automation rule: the rule's current ledger version and hash, and the model it runs (with the
/// model record's version).
pub fn automation_source(ledger: Option<&Ledger>, rule_id: &str, model_id: &str) -> OrderSource {
    let (version, hash) = head_of(ledger, Kind::Automation, rule_id);
    let (model_version, _) = head_of(ledger, Kind::Model, model_id);
    OrderSource {
        kind: "automation".into(),
        id: rule_id.into(),
        version,
        hash,
        model_id: (!model_id.is_empty()).then(|| model_id.to_string()),
        model_version,
    }
}

/// Work out the source from the order's own `tag`, for callers that do not pass one: the Copilot
/// tags its orders "copilot", automations "automation:<rule id>", strategies "strategy:<path>" and
/// "strategy-stop:<path>". `copilot_turn` is the turn that is running, if any.
pub fn source_from_tag(
    ledger: Option<&Ledger>,
    tag: Option<&str>,
    copilot_turn: Option<&str>,
    automation_model: impl Fn(&str) -> Option<String>,
) -> Option<OrderSource> {
    let tag = tag?;
    if tag == "copilot" {
        return Some(copilot_source(copilot_turn.unwrap_or("")));
    }
    if let Some(id) = tag.strip_prefix("automation:") {
        let model = automation_model(id).unwrap_or_default();
        return Some(automation_source(ledger, id, &model));
    }
    if let Some(path) = tag.strip_prefix("strategy-stop:").or_else(|| tag.strip_prefix("strategy:")) {
        return Some(strategy_source(ledger, path));
    }
    None
}

/// The actor name for a source kind.
pub fn actor_for(source: &OrderSource) -> &'static str {
    match source.kind.as_str() {
        "manual" | "kill_switch" => "user",
        "copilot" => "copilot",
        "automation" => "automation",
        "strategy" => "strategy",
        _ => "unattributed",
    }
}

// ── Reading the trail ─────────────────────────────────────────────────

/// Order events, newest first. `order_id` also returns the events that came before the broker gave the
/// order an id (submitted, risk_accepted, risk_rejected), which are tied to it by `data.request_id`.
pub fn query_order_events(
    ledger: &Ledger,
    order_id: Option<&str>,
    broker_id: Option<&str>,
    limit: Option<usize>,
) -> Vec<EventRecord> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let order_id = order_id.filter(|id| !id.is_empty());
    let broker_id = broker_id.filter(|id| !id.is_empty());
    let broker_ok = |line: &Value| broker_id.map_or(true, |b| line["broker_id"].as_str() == Some(b));

    let mut requests: HashSet<String> = HashSet::new();
    if let Some(order) = order_id {
        let by_order = |line: &Value| broker_ok(line) && line["order_id"].as_str() == Some(order);
        for event in ledger.read_events(ORDERS_STREAM, &by_order, usize::MAX) {
            if let Some(request) = event.value["data"]["request_id"].as_str() {
                requests.insert(request.to_string());
            }
        }
    }
    let wanted = |line: &Value| {
        if !broker_ok(line) {
            return false;
        }
        match order_id {
            None => true,
            Some(order) => {
                line["order_id"].as_str() == Some(order)
                    || line["data"]["request_id"].as_str().is_some_and(|r| requests.contains(r))
            }
        }
    };
    ledger.read_events(ORDERS_STREAM, &wanted, limit)
}

/// The order trail, newest first: every state each order passed through, who produced it, and the
/// hash chain links. `order_id` and `broker_id` narrow it; `limit` defaults to 200 (at most 5000).
#[tauri::command]
pub async fn list_order_events(
    order_id: Option<String>,
    broker_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<EventRecord>, String> {
    let ledger = ledger::global().ok_or_else(|| "The audit ledger is not open.".to_string())?.clone();
    tauri::async_runtime::spawn_blocking(move || {
        query_order_events(&ledger, order_id.as_deref(), broker_id.as_deref(), limit)
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_core::ports::audit::OrderAuditSource;

    fn temp_ledger() -> (Ledger, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("apex-order-audit-{}", uuid::Uuid::new_v4()));
        (Ledger::open(&dir).unwrap(), dir)
    }

    fn event(order: &str, broker: &str, name: &str, request: Option<&str>) -> OrderAuditEvent {
        OrderAuditEvent {
            order_id: order.into(),
            broker_id: broker.into(),
            event: name.into(),
            actor: "user".into(),
            source: Some(OrderAuditSource::new("manual", "")),
            data: match request {
                Some(r) => json!({"request_id": r}),
                None => json!({}),
            },
        }
    }

    #[test]
    fn the_sink_writes_flat_events_the_ui_can_read_and_the_chain_verifies() {
        let (ledger, dir) = temp_ledger();
        let sink = LedgerOrderSink::with_ledger(ledger.clone());
        sink.record(event("", "paper", "submitted", Some("r1"))).unwrap();
        sink.record(event("o-1", "paper", "broker_ack", Some("r1"))).unwrap();
        let events = ledger.read_events(ORDERS_STREAM, &|_| true, 10);
        assert_eq!(events.len(), 2);
        let line = serde_json::to_value(&events[0]).unwrap(); // newest first
        for key in ["seq", "ts", "order_id", "broker_id", "event", "actor", "source", "data", "hash", "prev_hash"] {
            assert!(line.get(key).is_some(), "missing {key}");
        }
        assert_eq!(line["event"], "broker_ack");
        assert_eq!(line["source"]["kind"], "manual");
        assert_eq!(events[0].prev_hash, events[1].hash);
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_sink_without_any_open_ledger_reports_an_error_instead_of_dropping_the_event() {
        // Other tests in this binary install the process-wide ledger, so the "no ledger anywhere"
        // case is tested through `resolve` with nothing passed, which does not depend on test order.
        let error = LedgerOrderSink::global().resolve(None).err().expect("no ledger anywhere is an error");
        assert!(error.contains("not open"), "{error}");
        // With a process-wide ledger available the global sink uses it; a sink's own ledger wins.
        let (ledger, dir) = temp_ledger();
        assert!(LedgerOrderSink::global().resolve(Some(&ledger)).is_ok());
        let (own, own_dir) = temp_ledger();
        own.put(Kind::Model, "marker", b"x", "user", "create").unwrap();
        let sink = LedgerOrderSink::with_ledger(own);
        assert!(sink.resolve(Some(&ledger)).unwrap().head(Kind::Model, "marker").is_some());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(own_dir);
    }

    #[test]
    fn list_filters_by_order_broker_and_limit_and_includes_the_events_before_the_ack() {
        let (ledger, dir) = temp_ledger();
        let sink = LedgerOrderSink::with_ledger(ledger.clone());
        sink.record(event("", "paper", "submitted", Some("r1"))).unwrap();
        sink.record(event("", "paper", "risk_accepted", Some("r1"))).unwrap();
        sink.record(event("o-1", "paper", "broker_ack", Some("r1"))).unwrap();
        sink.record(event("", "zerodha", "submitted", Some("r2"))).unwrap();
        sink.record(event("o-2", "zerodha", "broker_ack", Some("r2"))).unwrap();
        sink.record(event("o-1", "paper", "fill", None)).unwrap();
        sink.record(event("", "", "kill_switch", None)).unwrap();

        let names = |events: Vec<EventRecord>| -> Vec<String> {
            events.iter().map(|e| e.value["event"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(names(query_order_events(&ledger, None, None, None)).len(), 7);
        assert_eq!(names(query_order_events(&ledger, Some("o-1"), None, None)), ["fill", "broker_ack", "risk_accepted", "submitted"]);
        assert_eq!(names(query_order_events(&ledger, Some("o-2"), None, None)), ["broker_ack", "submitted"]);
        assert_eq!(names(query_order_events(&ledger, None, Some("zerodha"), None)), ["broker_ack", "submitted"]);
        assert_eq!(names(query_order_events(&ledger, Some("o-1"), Some("zerodha"), None)).len(), 0);
        assert_eq!(names(query_order_events(&ledger, None, None, Some(2))), ["kill_switch", "fill"]);
        assert_eq!(names(query_order_events(&ledger, Some("nope"), None, None)).len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sources_carry_the_exact_version_and_hash_of_what_produced_the_order() {
        let (ledger, dir) = temp_ledger();
        let v1 = ledger.put(Kind::Automation, "rule-1", b"{\"v\":1}", "user", "create").unwrap();
        let v2 = ledger.put(Kind::Automation, "rule-1", b"{\"v\":2}", "user", "edit").unwrap();
        ledger.put(Kind::Model, "m-1", b"{\"notes\":\"a\"}", "user", "create").unwrap();
        ledger.put(Kind::Strategy, "strategies/s.py", b"print(1)", "user", "create").unwrap();

        let source = automation_source(Some(&ledger), "rule-1", "m-1");
        assert_eq!((source.version, source.hash.clone()), (Some(2), v2.content_hash.clone()));
        assert_ne!(v1.content_hash, v2.content_hash);
        assert_eq!(source.model_id.as_deref(), Some("m-1"));
        assert_eq!(source.model_version, Some(1));

        let strategy = strategy_source(Some(&ledger), "strategies/s.py");
        assert_eq!(strategy.version, Some(1));
        assert_eq!(strategy.hash, Some(Ledger::sha256_hex(b"print(1)")));

        // Unknown objects give a source without a version rather than an invented one.
        let unknown = automation_source(Some(&ledger), "gone", "");
        assert_eq!((unknown.version, unknown.hash, unknown.model_id), (None, None, None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_source_is_recovered_from_the_order_tag_for_callers_that_pass_none() {
        let (ledger, dir) = temp_ledger();
        ledger.put(Kind::Automation, "rule-1", b"{}", "user", "create").unwrap();
        ledger.put(Kind::Strategy, "strategies/s.py", b"x", "user", "create").unwrap();
        let models = |id: &str| (id == "rule-1").then(|| "m-9".to_string());

        let copilot = source_from_tag(Some(&ledger), Some("copilot"), Some("turn-3"), models).unwrap();
        assert_eq!((copilot.kind.as_str(), copilot.id.as_str()), ("copilot", "turn-3"));
        let auto = source_from_tag(Some(&ledger), Some("automation:rule-1"), None, models).unwrap();
        assert_eq!((auto.kind.as_str(), auto.version, auto.model_id.as_deref()), ("automation", Some(1), Some("m-9")));
        let strategy = source_from_tag(Some(&ledger), Some("strategy:strategies/s.py"), None, models).unwrap();
        assert_eq!((strategy.kind.as_str(), strategy.version), ("strategy", Some(1)));
        let stop = source_from_tag(Some(&ledger), Some("strategy-stop:strategies/s.py"), None, models).unwrap();
        assert_eq!(stop.id, "strategies/s.py");
        assert!(source_from_tag(Some(&ledger), Some("hand-typed"), None, models).is_none());
        assert!(source_from_tag(Some(&ledger), None, None, models).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── the whole path, on a real paper adapter and a real (temporary) ledger ──

    use crate::commands::copilot_tools::test_support;
    use crate::state::{AppState, LiveAuth};
    use apex_core::domain::models::*;
    use std::sync::Arc;

    async fn audited_state(name: &str) -> (AppState, Ledger, std::path::PathBuf) {
        let (state, _paths) = test_support::state(name).await;
        let dir = std::env::temp_dir().join(format!("apex-order-trail-{name}-{}", uuid::Uuid::new_v4()));
        let ledger = Ledger::open(&dir).unwrap();
        state.otm.set_audit_sink(Arc::new(LedgerOrderSink::with_ledger(ledger.clone())));
        state.paper.update_quote(&Quote {
            symbol: Symbol("AAPL".into()),
            bid: 99.9,
            ask: 100.1,
            last: 100.0,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            volume: 1,
            change_pct: 0.0,
            vwap: 100.0,
            updated_at: chrono::Utc::now(),
        });
        (state, ledger, dir)
    }

    fn order(kind: OrderType, quantity: f64, price: Option<f64>) -> NewOrderRequest {
        NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: kind,
            quantity,
            price,
            stop_price: None,
            tag: None,
            broker_params: None,
        }
    }

    /// Events in the order they happened, as (event, order id) with the whole line for detail.
    fn trail(ledger: &Ledger) -> Vec<(String, Value)> {
        let mut events = ledger.read_events(ORDERS_STREAM, &|_| true, 10_000);
        events.reverse();
        events
            .into_iter()
            .map(|e| (e.value["event"].as_str().unwrap_or("").to_string(), serde_json::to_value(&e).unwrap()))
            .collect()
    }

    fn names(ledger: &Ledger) -> Vec<String> {
        trail(ledger).into_iter().map(|(name, _)| name).collect()
    }

    #[tokio::test]
    async fn a_paper_market_order_leaves_submitted_risk_accepted_ack_and_fill() {
        let (state, ledger, dir) = audited_state("trail-market").await;
        let id = state
            .guarded_submit_with(order(OrderType::Market, 2.0, None), "paper", LiveAuth::Manual(None), Some(manual_source()))
            .await
            .unwrap();
        let events = trail(&ledger);
        let order_events: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(order_events, ["submitted", "risk_accepted", "broker_ack", "fill"]);
        let (submitted, ack, fill) = (&events[0].1, &events[2].1, &events[3].1);
        assert_eq!(submitted["actor"], "user");
        assert_eq!(submitted["source"]["kind"], "manual");
        assert_eq!(submitted["data"]["live"], false);
        assert_eq!(submitted["data"]["request"]["quantity"], 2.0);
        assert_eq!(ack["order_id"], id.0.as_str());
        assert_eq!(ack["data"]["request_id"], submitted["data"]["request_id"]);
        assert_eq!(fill["order_id"], id.0.as_str());
        assert_eq!(fill["data"]["status"], "filled");
        assert_eq!(fill["data"]["filled_qty"], 2.0);
        assert!(fill["data"]["avg_price"].as_f64().unwrap() > 99.0);
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_risk_rejected_paper_order_records_submitted_then_risk_rejected_with_the_reason() {
        let (state, ledger, dir) = audited_state("trail-risk").await;
        let error = state
            .guarded_submit_with(order(OrderType::Market, 1_000_000_000.0, None), "paper", LiveAuth::Manual(None), Some(manual_source()))
            .await
            .unwrap_err();
        assert!(error.contains("Risk check failed"), "{error}");
        let events = trail(&ledger);
        assert_eq!(names(&ledger), ["submitted", "risk_rejected"]);
        assert!(events[1].1["data"]["reason"].as_str().unwrap().len() > 5);
        assert_eq!(events[1].1["order_id"], "");
        assert!(state.otm.open_orders().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_cancel_and_a_modify_each_append_events_and_change_nothing_before_them() {
        let (state, ledger, dir) = audited_state("trail-cancel").await;
        // A buy limit far below the market rests.
        let id = state
            .guarded_submit_with(order(OrderType::Limit, 1.0, Some(50.0)), "paper", LiveAuth::Manual(None), Some(manual_source()))
            .await
            .unwrap();
        assert_eq!(names(&ledger), ["submitted", "risk_accepted", "broker_ack", "status"]);
        let before = trail(&ledger);

        let params = ModifyParams { quantity: Some(3.0), price: Some(55.0), stop_price: None };
        state.guarded_modify_with(&id, "paper", &params, Some(manual_source())).await.unwrap();
        let ctx = AuditContext::new("user", Some(manual_source()));
        state.otm.cancel_order_with(&id, "paper", ctx).await.unwrap();

        let after = trail(&ledger);
        let all: Vec<&str> = after.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            all,
            ["submitted", "risk_accepted", "broker_ack", "status", "modified", "cancel_requested", "cancelled"]
        );
        // The earlier lines are byte for byte what they were.
        assert_eq!(&after[..before.len()], &before[..]);
        let modified = &after[4].1;
        assert_eq!(modified["data"]["requested"]["price"], 55.0);
        assert_eq!(modified["data"]["quantity"], 3.0);
        assert_eq!(after[6].1["data"]["status"], "cancelled");
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_order_from_an_automation_names_the_rule_version_hash_and_model() {
        let (state, ledger, dir) = audited_state("trail-source").await;
        ledger.put(Kind::Automation, "rule-1", b"{\"v\":1}", "user", "create").unwrap();
        let head = ledger.put(Kind::Automation, "rule-1", b"{\"v\":2}", "user", "edit").unwrap();
        ledger.put(Kind::Model, "model-7", b"{}", "user", "create").unwrap();
        let source = automation_source(Some(&ledger), "rule-1", "model-7");
        state
            .guarded_submit_with(order(OrderType::Market, 1.0, None), "paper", LiveAuth::Unattended, Some(source))
            .await
            .unwrap();
        for (_, line) in trail(&ledger) {
            assert_eq!(line["actor"], "automation");
            assert_eq!(line["source"]["kind"], "automation");
            assert_eq!(line["source"]["id"], "rule-1");
            assert_eq!(line["source"]["version"], 2);
            assert_eq!(line["source"]["hash"], head.content_hash.clone().unwrap());
            assert_eq!(line["source"]["model_id"], "model-7");
            assert_eq!(line["source"]["model_version"], 1);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_caller_that_names_no_source_is_attributed_from_the_tag_or_marked_unattributed() {
        let (state, ledger, dir) = audited_state("trail-tag").await;
        let mut tagged = order(OrderType::Market, 1.0, None);
        tagged.tag = Some("copilot".into());
        state.guarded_submit(tagged, "paper", LiveAuth::Unattended).await.unwrap();
        // A different size: the risk engine refuses a repeat of the same order within 500 ms.
        state.guarded_submit(order(OrderType::Market, 1.5, None), "paper", LiveAuth::Unattended).await.unwrap();
        let events = trail(&ledger);
        let submitted: Vec<&Value> = events.iter().filter(|(n, _)| n == "submitted").map(|(_, l)| l).collect();
        assert_eq!((submitted[0]["actor"].as_str(), submitted[0]["source"]["kind"].as_str()), (Some("copilot"), Some("copilot")));
        assert_eq!(submitted[1]["source"]["kind"], "unattended");
        assert_eq!(submitted[1]["actor"], "unattributed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn arming_disarming_and_the_kill_switch_are_events_and_the_cancels_say_kill_switch() {
        let (state, ledger, dir) = audited_state("trail-kill").await;
        state.set_live_armed_audited(true, "user", "ui").unwrap();
        state.set_live_armed_audited(true, "user", "ui").unwrap(); // no change, no event
        state.kill_switch_as("user").await;
        assert_eq!(names(&ledger), ["live_armed", "kill_switch", "live_disarmed", "kill_switch"]);
        let events = trail(&ledger);
        assert_eq!(events[0].1["actor"], "user");
        assert_eq!(events[0].1["order_id"], "");
        assert_eq!(events[1].1["data"]["phase"], "start");
        assert_eq!(events[1].1["data"]["was_armed"], true);
        assert_eq!(events[1].1["source"]["kind"], "kill_switch");
        assert_eq!(events[2].1["data"]["reason"], "kill_switch");
        assert_eq!(events[3].1["data"]["phase"], "done");
        // Disarming when already disarmed adds nothing.
        state.set_live_armed_audited(false, "user", "ui").unwrap();
        assert_eq!(names(&ledger).len(), 4);
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_chain_stays_valid_after_a_burst_of_concurrent_orders() {
        let (state, ledger, dir) = audited_state("trail-burst").await;
        let state = Arc::new(state);
        let mut tasks = Vec::new();
        for n in 0..24 {
            let state = state.clone();
            tasks.push(tokio::spawn(async move {
                // Distinct sizes: the risk engine refuses a repeat of the same order within 500 ms.
                let quantity = 1.0 + n as f64 * 0.01;
                state
                    .guarded_submit_with(order(OrderType::Market, quantity, None), "paper", LiveAuth::Manual(None), Some(manual_source()))
                    .await
            }));
        }
        // Writers from plain threads at the same time.
        let sink = Arc::new(LedgerOrderSink::with_ledger(ledger.clone()));
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    for n in 0..25 {
                        sink.record(event("", "none", "kill_switch", Some(&format!("t{t}-{n}")))).unwrap();
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let events = trail(&ledger);
        // 24 orders x (submitted, risk_accepted, broker_ack, fill) + 100 thread events.
        assert_eq!(events.len(), 24 * 4 + 100);
        let seqs: Vec<u64> = events.iter().map(|(_, l)| l["seq"].as_u64().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "sequence numbers must be gap free");
        for pair in events.windows(2) {
            assert_eq!(pair[1].1["prev_hash"], pair[0].1["hash"]);
        }
        let report = ledger.verify();
        assert!(report.ok, "{:?}", report.problems);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── fail closed, on the real sink ──

    #[cfg(unix)]
    fn broken_ledger() -> (Ledger, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let (ledger, dir) = temp_ledger();
        LedgerOrderSink::with_ledger(ledger.clone()).record(event("", "x", "submitted", None)).unwrap();
        // Make the orders stream unwritable, like a read-only or full disk.
        let file = dir.join("streams").join("orders.jsonl");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        (ledger, dir)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_live_order_is_not_sent_when_the_ledger_cannot_write_but_a_paper_order_is() {
        use apex_core::application::order_trade_manager::OrderTradeManager;
        use apex_core::application::risk_engine::{RiskConfig, RiskEngine};
        use apex_core::bus::message_bus::MessageBus;
        use std::os::unix::fs::PermissionsExt;

        let (ledger, dir) = broken_ledger();
        // Skip when the process can write anyway (running as root).
        if LedgerOrderSink::with_ledger(ledger.clone()).record(event("", "x", "probe", None)).is_ok() {
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let otm = OrderTradeManager::new(Arc::new(RiskEngine::new(RiskConfig::default())), Arc::new(MessageBus::new()));
        let paper = Arc::new(apex_adapters::execution::paper_trading::PaperTradingAdapter::new());
        paper.update_quote(&Quote {
            symbol: Symbol("AAPL".into()),
            bid: 100.0,
            ask: 100.0,
            last: 100.0,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            volume: 1,
            change_pct: 0.0,
            vwap: 100.0,
            updated_at: chrono::Utc::now(),
        });
        // The same adapter registered under a live-looking id and under "paper".
        otm.register_execution("paper".into(), paper.clone());
        otm.register_execution("livepaper".into(), paper.clone());
        otm.set_audit_sink(Arc::new(LedgerOrderSink::with_ledger(ledger.clone())));
        let ctx = || AuditContext::new("user", Some(manual_source()));

        let refused = otm.submit_order_with(order(OrderType::Market, 1.0, None), "livepaper", ctx()).await.unwrap_err();
        assert!(refused.to_string().contains("was not sent"), "{refused}");
        assert!(otm.open_orders().is_empty(), "the live order must not have been placed");

        let accepted = otm.submit_order_with(order(OrderType::Market, 1.0, None), "paper", ctx()).await;
        assert!(accepted.is_ok(), "paper proceeds when the audit write fails: {accepted:?}");

        let file = dir.join("streams").join("orders.jsonl");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── strategy sessions pin the version they run ──

    fn pinned(version: u32) -> OrderSource {
        OrderSource {
            kind: "strategy".into(),
            id: "strategies/pinned_mom.py".into(),
            version: Some(version),
            hash: Some(format!("hash-of-v{version}")),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn strategy_session_orders_carry_the_pinned_version_not_the_head_at_order_time() {
        let (state, ledger, dir) = audited_state("trail-pinned").await;
        let mut market = order(OrderType::Market, 1.0, None);
        market.tag = Some("strategy:strategies/pinned_mom.py".into());
        crate::commands::strategy_worker::submit_and_wait(&state, "paper", market, &pinned(3)).await.unwrap();
        let submitted = trail(&ledger).into_iter().find(|(n, _)| n == "submitted").unwrap().1;
        assert_eq!(submitted["actor"], "strategy");
        assert_eq!(submitted["source"]["kind"], "strategy");
        assert_eq!(submitted["source"]["id"], "strategies/pinned_mom.py");
        assert_eq!(submitted["source"]["version"], 3);
        assert_eq!(submitted["source"]["hash"], "hash-of-v3");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_pinned_source_uses_the_canonical_ledger_id_whatever_spelling_started_the_session() {
        let root = std::env::temp_dir().join(format!("apex-pinned-id-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = crate::commands::python_runtime::RuntimePaths::for_test(&root);
        let provenance = crate::commands::versioned_files::Provenance { version: 4, hash: "h4".into() };
        for spelling in ["foo.py", "strategies/foo.py"] {
            let full = crate::commands::strategy::normalize_strategy_path(spelling, &paths).unwrap();
            let source = crate::commands::strategy_worker::pinned_strategy_source(&paths, &full, Some(&provenance));
            assert_eq!((source.id.as_str(), source.version, source.hash.as_deref()), ("strategies/foo.py", Some(4), Some("h4")));
        }
        // A paper session that could not be pinned says so (no invented version).
        let full = crate::commands::strategy::normalize_strategy_path("foo.py", &paths).unwrap();
        let none = crate::commands::strategy_worker::pinned_strategy_source(&paths, &full, None);
        assert_eq!((none.version, none.hash), (None, None));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_strategy_cancel_of_its_working_order_names_the_strategy_and_its_version() {
        let (state, ledger, dir) = audited_state("trail-strategy-cancel").await;
        let mut limit = order(OrderType::Limit, 1.0, Some(50.0));
        limit.tag = Some("strategy:strategies/pinned_mom.py".into());
        let id = state.guarded_submit_with(limit, "paper", LiveAuth::Unattended, Some(pinned(3))).await.unwrap();
        crate::commands::strategy_worker::cancel_working_order(&state, &id, "paper", &pinned(3)).await.unwrap();
        let events = trail(&ledger);
        let cancels: Vec<&Value> = events.iter().filter(|(n, _)| n.starts_with("cancel")).map(|(_, v)| v).collect();
        assert_eq!(cancels.len(), 2);
        for cancel in cancels {
            assert_eq!(cancel["actor"], "strategy", "{cancel}");
            assert_eq!(cancel["source"]["id"], "strategies/pinned_mom.py");
            assert_eq!(cancel["source"]["version"], 3);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── live orders the live guard refuses still leave a trail ──

    #[tokio::test]
    async fn live_orders_refused_by_the_live_guard_are_recorded_with_the_reason_and_source() {
        let (state, ledger, dir) = audited_state("trail-live-guard").await;
        state.otm.register_execution("livepaper".into(), state.paper.clone());
        let over_cap = state.live_guard.max_qty_per_order() + 1.0;

        // 1. Not armed (an automation tried).
        let rule = OrderSource { kind: "automation".into(), id: "rule-9".into(), version: Some(2), ..Default::default() };
        let error = state
            .guarded_submit_with(order(OrderType::Market, 1.0, None), "livepaper", LiveAuth::Unattended, Some(rule))
            .await
            .unwrap_err();
        assert!(error.contains("not armed"), "{error}");
        // 2. Armed, but a manual order nobody confirmed.
        state.live_guard.set_armed(true);
        let error = state
            .guarded_submit_with(order(OrderType::Market, 1.0, None), "livepaper", LiveAuth::Manual(None), Some(manual_source()))
            .await
            .unwrap_err();
        assert!(error.contains("not confirmed"), "{error}");
        // 3. A confirmation id that was never issued.
        state
            .guarded_submit_with(order(OrderType::Market, 1.0, None), "livepaper", LiveAuth::Manual(Some("bogus")), Some(manual_source()))
            .await
            .unwrap_err();
        // 4. Over the per-order cap.
        let error = state
            .guarded_submit_with(order(OrderType::Market, over_cap, None), "livepaper", LiveAuth::Unattended, Some(copilot_source("turn-1")))
            .await
            .unwrap_err();
        assert!(error.contains("exceeds"), "{error}");

        assert!(state.otm.open_orders().is_empty(), "nothing was sent");
        assert_eq!(
            names(&ledger),
            ["submitted", "risk_rejected", "submitted", "risk_rejected", "submitted", "risk_rejected", "submitted", "risk_rejected"]
        );
        let events = trail(&ledger);
        let rejected: Vec<&Value> = events.iter().filter(|(n, _)| n == "risk_rejected").map(|(_, v)| v).collect();
        assert!(rejected[0]["data"]["reason"].as_str().unwrap().contains("not armed"));
        assert_eq!(rejected[0]["data"]["stage"], "live_guard");
        assert_eq!(rejected[0]["source"]["kind"], "automation");
        assert_eq!(rejected[0]["source"]["id"], "rule-9");
        assert_eq!(rejected[0]["actor"], "automation");
        assert!(rejected[1]["data"]["reason"].as_str().unwrap().contains("not confirmed"));
        assert_eq!(rejected[1]["actor"], "user");
        assert_eq!(rejected[3]["source"]["id"], "turn-1");
        // The refusal is tied to its own `submitted` event, which holds the full request.
        let submitted: Vec<&Value> = events.iter().filter(|(n, _)| n == "submitted").map(|(_, v)| v).collect();
        for (sub, rej) in submitted.iter().zip(&rejected) {
            assert_eq!(sub["data"]["request_id"], rej["data"]["request_id"]);
            assert_eq!(sub["data"]["live"], true);
        }
        assert_eq!(submitted[3]["data"]["request"]["quantity"], over_cap);
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn orphaned_paper_orders_cancelled_at_startup_get_a_cancelled_event_in_the_trail() {
        use apex_adapters::storage::sqlite_storage::SqliteStorage;
        use apex_core::application::order_trade_manager::cancel_orphaned_paper_orders_audited;
        use apex_core::ports::storage::StoragePort;

        let (state, ledger, dir) = audited_state("trail-orphan").await;
        // A resting paper limit order, as the previous session left it.
        let id = state
            .guarded_submit_with(order(OrderType::Limit, 1.0, Some(50.0)), "paper", LiveAuth::Manual(None), Some(manual_source()))
            .await
            .unwrap();
        assert_eq!(names(&ledger).last().map(String::as_str), Some("status"));
        let resting = state.otm.open_orders().into_iter().find(|o| o.id == id).unwrap();
        let storage = SqliteStorage::new(":memory:").unwrap();
        storage.init_schema().await.unwrap();
        storage.write_order(&resting).await.unwrap();

        let sink = LedgerOrderSink::with_ledger(ledger.clone());
        assert_eq!(cancel_orphaned_paper_orders_audited(&storage, Some(&sink)).await.unwrap(), 1);

        let events = trail(&ledger);
        let (name, last) = events.last().unwrap();
        assert_eq!(name, "cancelled");
        assert_eq!(last["order_id"], id.0.as_str());
        assert_eq!(last["actor"], "system");
        assert_eq!(last["source"]["kind"], "startup_cleanup");
        assert_eq!(last["data"]["status"], "cancelled");
        assert!(last["data"]["reason"].as_str().unwrap().contains("previous session"));
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
    }
}
