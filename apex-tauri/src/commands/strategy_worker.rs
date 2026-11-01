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
use tauri::Emitter;
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

#[tauri::command]
pub async fn stop_strategy_file(path: String) -> Result<(), String> {
    let map = sessions().lock().map_err(|e| e.to_string())?;
    if let Some(tx) = map.get(&path) {
        let _ = tx.send(true);
    }
    Ok(())
}

pub async fn run(
    app: &tauri::AppHandle,
    path: String,
    params: Value,
    state: &AppState,
    paths: &python_runtime::RuntimePaths,
) -> Result<Vec<String>, String> {
    let full_path = normalize_strategy_path(&path, paths)?;
    let (tx, mut stop) = watch::channel(false);
    {
        let mut map = sessions().lock().map_err(|e| e.to_string())?;
        if map.contains_key(&path) {
            return Err("This strategy already has a paper session".into());
        }
        map.insert(path.clone(), tx);
    }
    let result = run_inner(app, &path, full_path, params, state, paths, &mut stop).await;
    sessions().lock().map_err(|e| e.to_string())?.remove(&path);
    result
}

async fn run_inner(
    app: &tauri::AppHandle,
    path: &str,
    full_path: std::path::PathBuf,
    params: Value,
    state: &AppState,
    paths: &python_runtime::RuntimePaths,
    stop: &mut watch::Receiver<bool>,
) -> Result<Vec<String>, String> {
    let mut worker = Worker::spawn(paths).await?;
    let init = worker
        .request(json!({"op":"init","path":full_path,"params":params}))
        .await?;
    let subscriptions = init["subscriptions"]
        .as_array()
        .ok_or("Strategy did not subscribe to bars")?;
    // One symbol per worker keeps ownership and aggregation explicit.
    if subscriptions.len() != 1 || subscriptions[0][0].as_array().map(|a| a.len()) != Some(1) {
        return Err("Paper sessions require exactly one symbol subscription".into());
    }
    let symbol = subscriptions[0][0][0]
        .as_str()
        .ok_or("Missing subscription symbol")?
        .to_string();
    crate::validation::validate_symbol(&symbol)?;
    let tf = subscriptions[0][1].as_str().ok_or("Missing timeframe")?;
    let seconds = match tf {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        _ => return Err("Live paper sessions support 1m, 5m or 15m bars".into()),
    };
    let quantity = params
        .get("quantity")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    crate::validation::validate_quantity(quantity)?;
    // Avoid sharing a paper position with another worker or existing manual holding.
    if state
        .paper
        .get_positions()
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .any(|p| p.symbol.0 == symbol && p.broker_id == "paper" && p.quantity != 0.0)
    {
        return Err(format!(
            "Close the existing paper position in {symbol} before starting this strategy"
        ));
    }
    let ownership_key = format!("symbol:{symbol}");
    {
        let mut map = sessions().lock().map_err(|e| e.to_string())?;
        if map.contains_key(&ownership_key) {
            return Err(format!("Another paper strategy owns {symbol}"));
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
    let mut logs = vec![format!(
        "PAPER session {path}: {symbol} {tf}, quantity {quantity}; quote-sampled bars (volume unavailable); auto-stop in 30 minutes"
    )];
    let _ = app.emit("strategy-output", json!({"path":path,"line":logs[0]}));
    let mut held = false;
    let mut current: Option<OHLCV> = None;
    let mut last_timestamp = None;
    let mut last_fresh = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1800);
    let execution: Result<(), String> = async {
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = tokio::time::sleep_until(deadline) => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
            if last_fresh.elapsed() > std::time::Duration::from_secs(90) {
                return Err(format!("No fresh market data for {symbol} for 90 seconds; paper session stopped"));
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
                let _ = app.emit("strategy-output", json!({"path":path,"line":format!("Quote-sampled {} bar {} close {}",tf,bar.time,bar.close)}));
                let reply = worker.request(bar_request(bar, tf)).await?;
                if let Some(lines) = reply["logs"].as_array() {
                    logs.extend(lines.iter().filter_map(Value::as_str).map(String::from));
                }
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
                            side: if long {
                                OrderSide::Buy
                            } else {
                                OrderSide::Sell
                            },
                            order_type: OrderType::Market,
                            quantity,
                            // Market execution still uses the adapter quote; this
                            // estimate makes notional risk checks effective.
                            price: Some(quote.last),
                            stop_price: None,
                            tag: Some(format!("strategy:{path}")),
                            broker_params: None,
                        };
                        let id = state
                            .otm
                            .submit_order(order, "paper")
                            .await
                            .map_err(|e| e.to_string())?;
                        logs.push(format!(
                            "{} {} {} paper order {}",
                            if long { "BUY" } else { "SELL" },
                            symbol,
                            quantity,
                            id.0
                        ));
                        let order = state
                            .paper
                            .get_order_status(&id)
                            .await
                            .map_err(|e| e.to_string())?;
                        if order.status != OrderStatus::Filled {
                            return Err(format!(
                                "Paper order {} did not fill: {:?}",
                                id.0, order.status
                            ));
                        }
                        let _ =
                            app.emit("strategy-output", json!({"path":path,"line":logs.last()}));
                        held = long;
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
    worker.finish().await;
    if held {
        let id = state
            .otm
            .submit_order(
                NewOrderRequest {
                    symbol: Symbol(symbol.clone()),
                    side: OrderSide::Sell,
                    order_type: OrderType::Market,
                    quantity,
                    price: None,
                    stop_price: None,
                    tag: Some(format!("strategy-stop:{path}")),
                    broker_params: None,
                },
                "paper",
            )
            .await
            .map_err(|e| format!("Paper strategy stopped, but the {symbol} position remains open: {e}. Close it manually when a fresh market price is available."))?;
        logs.push(format!("Paper position closed by stop: {}", id.0));
    }
    execution?;
    logs.push("Paper strategy stopped; worker terminated".into());
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
}
