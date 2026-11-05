//! Authored Python callbacks for historical replay and bounded paper sessions.
use super::{python_runtime, strategy::normalize_strategy_path};
use crate::state::AppState;
use apex_core::domain::models::*;
use apex_core::ports::execution::ExecutionPort;
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Mutex, OnceLock},
};
use tauri::{Emitter, Manager};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::watch,
};

pub struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Worker {
    pub async fn spawn(paths: &python_runtime::RuntimePaths) -> Result<Self, String> {
        let python =
            python_runtime::resolve_python_executable(paths, "APEX_STRATEGY_PYTHON_PATH", &[])?;
        let mut child = Command::new(python)
            .current_dir(paths.work_root())
            .env("PYTHONPATH", python_runtime::build_python_path(paths)?)
            .arg("-m")
            .arg("runtime.event_bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        })
    }
    pub async fn request(&mut self, value: Value) -> Result<Value, String> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            self.input
                .write_all(format!("{}\n", value).as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            self.input.flush().await.map_err(|e| e.to_string())?;
            let mut line = String::new();
            self.output
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            let result: Value = serde_json::from_str(&line)
                .map_err(|e| format!("Strategy worker response: {e}"))?;
            if let Some(error) = result.get("error") {
                return Err(error.as_str().unwrap_or("Strategy failed").to_string());
            }
            Ok(result)
        })
        .await
        .map_err(|_| "Strategy callback timed out after 15 seconds".to_string())?
    }
    pub async fn finish(&mut self) {
        let _ = self.request(json!({"op":"stop"})).await;
        let _ = self.child.kill().await;
    }
}

fn accepts_quote(
    now: chrono::DateTime<Utc>,
    previous: Option<chrono::DateTime<Utc>>,
    next: chrono::DateTime<Utc>,
) -> bool {
    let age = now.signed_duration_since(next).num_seconds();
    (-5..=30).contains(&age) && previous.map(|last| next > last).unwrap_or(true)
}

pub fn bar_request(bar: &OHLCV, timeframe: &str) -> Value {
    let canonical = match timeframe {
        "d1" => "1d",
        "w1" => "1w",
        "m1" => "1m",
        "m5" => "5m",
        "m15" => "15m",
        "h1" => "1h",
        "h4" => "4h",
        value => value,
    };
    json!({"op":"bar", "bar": {"symbol":bar.symbol.0, "timeframe":canonical,
        "open":bar.open,"high":bar.high,"low":bar.low,"close":bar.close,"volume":bar.volume,
        "timestamp_ns":bar.time.timestamp_millis()*1_000_000}})
}

type Sessions = Mutex<HashMap<String, watch::Sender<bool>>>;
static SESSIONS: OnceLock<Sessions> = OnceLock::new();
fn sessions() -> &'static Sessions {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Snapshot of a strategy session, for the UI and the AI copilot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub path: String,
    pub symbol: String,
    pub timeframe: String,
    pub broker_id: String,
    pub quantity: f64,
    pub started_at: String,
    /// "starting" | "running" | "stopped" | "error"
    pub state: String,
    pub held: bool,
    pub bars_processed: u32,
    pub orders_placed: u32,
    pub last_error: Option<String>,
    /// Newest last. Capped so a long session cannot grow without bound.
    pub recent_logs: Vec<String>,
}

const MAX_RECENT_LOGS: usize = 60;
const MAX_FINISHED_SESSIONS: usize = 20;

static INFO: OnceLock<Mutex<HashMap<String, SessionInfo>>> = OnceLock::new();
fn info_map() -> &'static Mutex<HashMap<String, SessionInfo>> {
    INFO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn update_info(path: &str, f: impl FnOnce(&mut SessionInfo)) {
    if let Ok(mut map) = info_map().lock() {
        if let Some(info) = map.get_mut(path) {
            f(info);
        }
    }
}

/// Record a log line and push it to the UI as it happens.
fn note(app: &tauri::AppHandle, path: &str, line: impl Into<String>) {
    let line = line.into();
    update_info(path, |info| {
        info.recent_logs.push(line.clone());
        if info.recent_logs.len() > MAX_RECENT_LOGS {
            let excess = info.recent_logs.len() - MAX_RECENT_LOGS;
            info.recent_logs.drain(..excess);
        }
    });
    let _ = app.emit("strategy-output", json!({"path": path, "line": line}));
}

fn register_info(info: SessionInfo) {
    if let Ok(mut map) = info_map().lock() {
        map.insert(info.path.clone(), info);
        // Keep the newest finished sessions only.
        let active: std::collections::HashSet<String> = sessions()
            .lock()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default();
        let mut finished: Vec<(String, String)> = map
            .iter()
            .filter(|(k, _)| !active.contains(*k))
            .map(|(k, v)| (v.started_at.clone(), k.clone()))
            .collect();
        if finished.len() > MAX_FINISHED_SESSIONS {
            finished.sort();
            for (_, key) in finished.iter().take(finished.len() - MAX_FINISHED_SESSIONS) {
                map.remove(key);
            }
        }
    }
}

#[tauri::command]
pub async fn stop_strategy_file(path: String) -> Result<(), String> {
    let map = sessions().lock().map_err(|e| e.to_string())?;
    if let Some(tx) = map.get(&path) {
        let _ = tx.send(true);
    }
    Ok(())
}

/// Ask one running session to stop. Returns false if it is not running.
pub fn stop_session(path: &str) -> bool {
    match sessions().lock() {
        Ok(map) => match map.get(path) {
            Some(tx) => {
                let _ = tx.send(true);
                true
            }
            None => false,
        },
        Err(_) => false,
    }
}

/// Ask every running strategy session to stop (used by the kill switch).
/// Returns the paths that were signalled.
pub fn stop_all_sessions() -> Vec<String> {
    let Ok(map) = sessions().lock() else {
        return Vec::new();
    };
    map.iter()
        .filter(|(key, _)| !key.starts_with("symbol:"))
        .map(|(key, tx)| {
            let _ = tx.send(true);
            key.clone()
        })
        .collect()
}

/// Details of running and recently finished sessions, newest first.
pub fn session_details() -> Vec<SessionInfo> {
    let mut all: Vec<SessionInfo> = info_map()
        .lock()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    all
}

/// Parsed, validated session parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionParams {
    pub broker_id: String,
    pub quantity: f64,
    pub duration_secs: u64,
}

pub fn parse_session_params(params: &Value) -> Result<SessionParams, String> {
    let broker_id = params
        .get("broker_id")
        .and_then(Value::as_str)
        .unwrap_or("paper")
        .to_string();
    crate::validation::validate_broker_id(&broker_id)?;
    let quantity = params.get("quantity").and_then(Value::as_f64).unwrap_or(1.0);
    crate::validation::validate_quantity(quantity)?;
    let minutes = params
        .get("duration_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    if !(1..=1440).contains(&minutes) {
        return Err("duration_minutes must be between 1 and 1440".into());
    }
    Ok(SessionParams { broker_id, quantity, duration_secs: minutes * 60 })
}

/// Run a session in the background and return immediately (used by the copilot).
pub fn start_background(
    app: &tauri::AppHandle,
    path: String,
    params: Value,
) -> Result<(), String> {
    parse_session_params(&params)?;
    if sessions().lock().map_err(|e| e.to_string())?.contains_key(&path) {
        return Err(format!("{path} already has a running session"));
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let paths = app.state::<python_runtime::RuntimePaths>();
        if let Err(error) = run(&app, path.clone(), params, state.inner(), paths.inner()).await {
            note(&app, &path, format!("Session ended with an error: {error}"));
        }
    });
    Ok(())
}

pub async fn run(
    app: &tauri::AppHandle,
    path: String,
    params: Value,
    state: &AppState,
    paths: &python_runtime::RuntimePaths,
) -> Result<Vec<String>, String> {
    let session = parse_session_params(&params)?;
    if session.broker_id != "paper" {
        // Live sessions need the config opt-in and a connected broker. Arming,
        // quantity caps and the daily budget are enforced per order by the guard.
        super::automation::check_broker_allowed(state, &session.broker_id)?;
        let connected = state
            .broker_connection(&session.broker_id)
            .map(|c| c.authenticated && c.execution_available)
            .unwrap_or(false);
        if !connected {
            return Err(format!(
                "{} is not connected. Unlock the vault and log in under Settings > Adapters.",
                session.broker_id
            ));
        }
    }
    let full_path = normalize_strategy_path(&path, paths)?;
    let (tx, mut stop) = watch::channel(false);
    {
        let mut map = sessions().lock().map_err(|e| e.to_string())?;
        if map.contains_key(&path) {
            return Err("This strategy already has a running session".into());
        }
        map.insert(path.clone(), tx);
    }
    let result = run_inner(app, &path, full_path, params, &session, state, paths, &mut stop).await;
    sessions().lock().map_err(|e| e.to_string())?.remove(&path);
    update_info(&path, |info| {
        info.state = if result.is_ok() { "stopped" } else { "error" }.into();
        if let Err(error) = &result {
            info.last_error = Some(error.clone());
        }
    });
    result
}

/// Whether an order is still working at the broker.
fn is_working(status: &OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::Pending | OrderStatus::Open | OrderStatus::PartiallyFilled
    )
}

fn tracked_status(state: &AppState, id: &OrderId) -> Option<OrderStatus> {
    state
        .otm
        .open_orders()
        .into_iter()
        .find(|o| &o.id == id)
        .map(|o| o.status)
}

/// Submit through the live-trading guard and wait briefly for a final status.
async fn submit_and_wait(
    state: &AppState,
    broker_id: &str,
    order: NewOrderRequest,
) -> Result<(OrderId, OrderStatus), String> {
    let id = state
        .guarded_submit(order, broker_id, crate::state::LiveAuth::Unattended)
        .await?;
    let mut status = OrderStatus::Pending;
    for _ in 0..20 {
        if let Some(found) = tracked_status(state, &id) {
            status = found;
            if !is_working(&status) {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Ok((id, status))
}

/// Feed recent closed bars to the strategy so indicators are populated before
/// the first live bar. Signals produced here are discarded.
async fn warm_up(
    worker: &mut Worker,
    state: &AppState,
    symbol: &str,
    tf: &str,
    seconds: i64,
) -> Result<usize, String> {
    let bars = super::data::load_bars(state, symbol, Some(tf), 250).await?;
    let current_bucket = Utc::now().timestamp() / seconds * seconds;
    let mut fed = 0;
    for bar in bars.iter().filter(|b| b.time.timestamp() / seconds * seconds < current_bucket) {
        worker.request(bar_request(bar, tf)).await?;
        fed += 1;
    }
    Ok(fed)
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    app: &tauri::AppHandle,
    path: &str,
    full_path: std::path::PathBuf,
    params: Value,
    session: &SessionParams,
    state: &AppState,
    paths: &python_runtime::RuntimePaths,
    stop: &mut watch::Receiver<bool>,
) -> Result<Vec<String>, String> {
    let broker_id = session.broker_id.as_str();
    let is_paper = broker_id == "paper";
    let label = if is_paper { "PAPER" } else { "LIVE" };
    let quantity = session.quantity;

    let mut worker = Worker::spawn(paths).await?;
    let init = worker
        .request(json!({"op":"init","path":full_path,"params":params}))
        .await?;
    let subscriptions = init["subscriptions"]
        .as_array()
        .ok_or("Strategy did not subscribe to bars")?;
    // One symbol per worker keeps ownership and aggregation explicit.
    if subscriptions.len() != 1 || subscriptions[0][0].as_array().map(|a| a.len()) != Some(1) {
        return Err("Sessions require exactly one symbol subscription".into());
    }
    let symbol = subscriptions[0][0][0]
        .as_str()
        .ok_or("Missing subscription symbol")?
        .to_string();
    crate::validation::validate_symbol(&symbol)?;
    let tf = subscriptions[0][1].as_str().ok_or("Missing timeframe")?;
    let seconds: i64 = match tf {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        _ => return Err("Live sessions support 1m, 5m or 15m bars".into()),
    };

    register_info(SessionInfo {
        path: path.to_string(),
        symbol: symbol.clone(),
        timeframe: tf.to_string(),
        broker_id: broker_id.to_string(),
        quantity,
        started_at: Utc::now().to_rfc3339(),
        state: "starting".into(),
        held: false,
        bars_processed: 0,
        orders_placed: 0,
        last_error: None,
        recent_logs: Vec::new(),
    });
    let mut logs: Vec<String> = Vec::new();
    macro_rules! log {
        ($($arg:tt)*) => {{
            let line = format!($($arg)*);
            note(app, path, line.clone());
            logs.push(line);
        }};
    }
    if let Some(lines) = init["logs"].as_array() {
        for line in lines.iter().filter_map(Value::as_str) {
            log!("{line}");
        }
    }

    // Do not share a position with another worker or an existing holding.
    let existing = state.otm.position(broker_id, &symbol).map(|p| p.quantity).unwrap_or(0.0);
    let paper_existing = is_paper
        && state
            .paper
            .get_positions()
            .await
            .map_err(|e| e.to_string())?
            .iter()
            .any(|p| p.symbol.0 == symbol && p.quantity != 0.0);
    if existing != 0.0 || paper_existing {
        return Err(format!(
            "Close the existing {broker_id} position in {symbol} before starting this strategy"
        ));
    }
    let ownership_key = format!("symbol:{broker_id}:{symbol}");
    {
        let mut map = sessions().lock().map_err(|e| e.to_string())?;
        if map.contains_key(&ownership_key) {
            return Err(format!("Another strategy already trades {symbol} on {broker_id}"));
        }
        let (owner, _) = watch::channel(false);
        map.insert(ownership_key.clone(), owner);
    }
    struct Ownership(String);
    impl Drop for Ownership {
        fn drop(&mut self) {
            if let Ok(mut map) = sessions().lock() {
                map.remove(&self.0);
            }
        }
    }
    let _ownership = Ownership(ownership_key);
    state
        .aggregator
        .start(&[Symbol(symbol.clone())])
        .await
        .map_err(|e| e.to_string())?;

    log!(
        "{label} session {path}: {symbol} {tf} on {broker_id}, quantity {quantity}; quote-sampled bars (volume unavailable); auto-stop in {} minutes",
        session.duration_secs / 60
    );
    match warm_up(&mut worker, state, &symbol, tf, seconds).await {
        Ok(0) => log!("No history available to warm up indicators; the first signals need {tf} bars to accumulate"),
        Ok(n) => log!("Warmed up the strategy with {n} historical {tf} bars"),
        Err(error) => log!("Warm-up skipped: {error}"),
    }
    update_info(path, |info| info.state = "running".into());

    let mut held = false;
    // An order that has not reached a final state; no new signals until it does.
    let mut pending: Option<(OrderId, bool)> = None;
    let mut current: Option<OHLCV> = None;
    let mut last_timestamp = None;
    let mut last_fresh = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(session.duration_secs);
    let execution: Result<(), String> = async {
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = tokio::time::sleep_until(deadline) => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
            if last_fresh.elapsed() > std::time::Duration::from_secs(90) {
                return Err(format!("No fresh market data for {symbol} for 90 seconds; session stopped"));
            }
            let Some(quote) = state.aggregator.get_cached_quote(&symbol) else {
                continue;
            };
            if !accepts_quote(Utc::now(), last_timestamp, quote.updated_at) { continue; }
            last_timestamp = Some(quote.updated_at);
            last_fresh = tokio::time::Instant::now();
            let bucket = quote.updated_at.timestamp() / seconds * seconds;
            if let Some(bar) = current.as_mut() {
                if bar.time.timestamp() == bucket {
                    bar.high = bar.high.max(quote.last);
                    bar.low = bar.low.min(quote.last);
                    bar.close = quote.last;
                    continue;
                }
                log!("Quote-sampled {} bar {} close {}", tf, bar.time, bar.close);
                let reply = worker.request(bar_request(bar, tf)).await?;
                update_info(path, |info| info.bars_processed += 1);
                if let Some(lines) = reply["logs"].as_array() {
                    for line in lines.iter().filter_map(Value::as_str) {
                        log!("{line}");
                    }
                }

                // Resolve an order that was still working at the previous bar.
                if let Some((id, long)) = pending.clone() {
                    match tracked_status(state, &id) {
                        Some(OrderStatus::Filled) => {
                            held = long;
                            pending = None;
                            update_info(path, |info| info.held = long);
                            log!("Order {} filled", id.0);
                        }
                        Some(OrderStatus::Rejected) | Some(OrderStatus::Cancelled) => {
                            pending = None;
                            log!("Order {} did not fill; continuing", id.0);
                        }
                        _ => {
                            log!("Order {} is still working; skipping signals this bar", id.0);
                        }
                    }
                }

                if pending.is_none() {
                    if let Some(signals) = reply["signals"].as_array() {
                        for signal in signals {
                            if signal["symbol"].as_str() != Some(&symbol) {
                                return Err(
                                    "Strategy emitted a signal for an unsubscribed symbol".into()
                                );
                            }
                            let long = signal["direction"] == "long";
                            if long == held {
                                continue;
                            }
                            let order = NewOrderRequest {
                                symbol: Symbol(symbol.clone()),
                                side: if long { OrderSide::Buy } else { OrderSide::Sell },
                                order_type: OrderType::Market,
                                quantity,
                                // Market execution still uses the adapter quote; this
                                // estimate makes notional risk checks effective.
                                price: Some(quote.last),
                                stop_price: None,
                                tag: Some(format!("strategy:{path}")),
                                broker_params: None,
                            };
                            let side = if long { "BUY" } else { "SELL" };
                            match submit_and_wait(state, broker_id, order).await {
                                Ok((id, status)) => {
                                    update_info(path, |info| info.orders_placed += 1);
                                    log!("{side} {symbol} {quantity} {broker_id} order {} ({status:?})", id.0);
                                    match status {
                                        OrderStatus::Filled => {
                                            held = long;
                                            update_info(path, |info| info.held = long);
                                        }
                                        OrderStatus::Rejected | OrderStatus::Cancelled => {}
                                        _ => pending = Some((id, long)),
                                    }
                                }
                                Err(error) => {
                                    // A halted risk engine ends the session; any other
                                    // rejection (risk cap, guard) is logged and retried
                                    // on the next signal rather than killing the strategy.
                                    if error.to_lowercase().contains("halted") {
                                        return Err(error);
                                    }
                                    log!("{side} {symbol} rejected: {error}");
                                }
                            }
                        }
                    }
                }
            }
            current = Some(OHLCV {
                symbol: Symbol(symbol.clone()),
                time: chrono::DateTime::from_timestamp(bucket, 0).ok_or("Invalid quote time")?,
                timeframe: Timeframe::M1,
                open: quote.last,
                high: quote.last,
                low: quote.last,
                close: quote.last,
                volume: 0,
            });
        }
        Ok(())
    }
    .await;
    // The strategy's on_stop logs come back with the stop reply.
    if let Ok(reply) = worker.request(json!({"op":"stop"})).await {
        if let Some(lines) = reply["logs"].as_array() {
            for line in lines.iter().filter_map(Value::as_str) {
                log!("{line}");
            }
        }
    }
    worker.finish().await;
    if held {
        let close = NewOrderRequest {
            symbol: Symbol(symbol.clone()),
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity,
            price: None,
            stop_price: None,
            tag: Some(format!("strategy-stop:{path}")),
            broker_params: None,
        };
        match state
            .guarded_submit(close, broker_id, crate::state::LiveAuth::Unattended)
            .await
        {
            Ok(id) => {
                update_info(path, |info| info.held = false);
                log!("{label} position closed by stop: {}", id.0);
            }
            Err(error) if is_paper => {
                return Err(format!("Strategy stopped, but the {symbol} position remains open: {error}. Close it manually when a fresh market price is available."));
            }
            Err(error) => {
                // A disarmed or halted live account must not be traded behind the user's back.
                log!("Strategy stopped; the {broker_id} position in {symbol} was NOT closed ({error}). Close it manually.");
            }
        }
    }
    execution?;
    log!("Strategy stopped; worker terminated");
    Ok(logs)
}

#[tauri::command]
pub fn list_strategy_sessions() -> Result<Vec<String>, String> {
    let map = sessions().lock().map_err(|e| e.to_string())?;
    let mut paths = map
        .keys()
        .filter(|key| !key.starts_with("symbol:"))
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// Running and recently finished sessions with state, position and logs.
#[tauri::command]
pub fn list_strategy_session_details() -> Result<Vec<SessionInfo>, String> {
    Ok(session_details())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_backward_stale_and_future_quotes() {
        let now = Utc::now();
        let previous = now - chrono::Duration::seconds(3);
        assert!(accepts_quote(now, Some(previous), now));
        assert!(!accepts_quote(now, Some(previous), previous));
        assert!(!accepts_quote(
            now,
            Some(previous),
            previous - chrono::Duration::seconds(1)
        ));
        assert!(!accepts_quote(
            now,
            None,
            now - chrono::Duration::seconds(31)
        ));
        assert!(!accepts_quote(
            now,
            None,
            now + chrono::Duration::seconds(6)
        ));
    }

    #[test]
    fn session_params_default_to_a_short_paper_session() {
        let params = parse_session_params(&json!({})).unwrap();
        assert_eq!(
            params,
            SessionParams { broker_id: "paper".into(), quantity: 1.0, duration_secs: 1800 }
        );
    }

    #[test]
    fn session_params_validate_broker_quantity_and_duration() {
        let ok = parse_session_params(
            &json!({"broker_id":"zerodha","quantity":3,"duration_minutes":120}),
        )
        .unwrap();
        assert_eq!(ok.broker_id, "zerodha");
        assert_eq!(ok.duration_secs, 7200);
        assert!(parse_session_params(&json!({"broker_id":"nope"})).is_err());
        assert!(parse_session_params(&json!({"quantity":-1})).is_err());
        assert!(parse_session_params(&json!({"duration_minutes":0})).is_err());
        assert!(parse_session_params(&json!({"duration_minutes":5000})).is_err());
    }

    #[test]
    fn only_unfinished_orders_count_as_working() {
        assert!(is_working(&OrderStatus::Open));
        assert!(is_working(&OrderStatus::PartiallyFilled));
        assert!(!is_working(&OrderStatus::Filled));
        assert!(!is_working(&OrderStatus::Rejected));
    }
}
