use super::python_runtime;
use crate::state::AppState;
use crate::validation;
use apex_adapters::market_data::yahoo_finance::YahooFinanceAdapter;
use apex_core::application::backtest_engine::{
    BacktestConfig, BacktestEngine, BacktestMetrics, BacktestSignal, BacktestTrade, EquityPoint,
};
use apex_core::domain::models::{OHLCVQuery, Symbol, Timeframe, OHLCV};
use apex_core::ports::market_data::MarketDataPort;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{command, State};
use uuid::Uuid;

const DEFAULT_STRATEGY_TEMPLATE: &str = r#""""
APEX Strategy Template
Subclass Strategy and override on_bar / on_tick.
"""
from apex_sdk import Strategy, Bar, Signal, Timeframe


class MyStrategy(Strategy):
    def on_init(self, params: dict) -> None:
        self.subscribe(["RELIANCE.NS"], Timeframe.M5)
        self.log("Strategy initialized")

    def on_bar(self, symbol: str, bar: Bar) -> None:
        sma = self.indicator("sma", symbol, 20)
        if bar.close > sma:
            self.emit(Signal(
                symbol=symbol,
                direction="long",
                strength=0.8,
                metadata={"reason": "price_above_sma"},
            ))

    def on_stop(self) -> None:
        self.log("Strategy stopped")
"#;

#[derive(Debug, Clone, Serialize)]
pub struct StrategyFileDto {
    pub name: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyExecutionResultDto {
    pub success: bool,
    pub output: Vec<String>,
    pub error: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyBacktestRequestDto {
    pub path: String,
    pub symbol: String,
    pub timeframe: String,
    pub from: String,
    pub to: String,
    pub initial_capital: Option<f64>,
    pub commission_bps: Option<f64>,
    pub slippage_bps: Option<f64>,
    pub quantity: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyBacktestResultDto {
    pub strategy_name: String,
    pub strategy_path: String,
    pub inferred_strategy: String,
    pub symbol: String,
    pub timeframe: String,
    pub bars_analyzed: usize,
    pub metrics: BacktestMetrics,
    pub trades: Vec<BacktestTrade>,
    pub equity_curve: Vec<EquityPoint>,
    pub notes: Vec<String>,
}

pub(crate) fn strategy_root(
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<PathBuf, String> {
    let root = runtime_paths.strategies_dir().to_path_buf();
    fs::create_dir_all(&root).map_err(|e| format!("Failed to create strategies directory: {e}"))?;
    Ok(root)
}

pub(crate) fn make_strategy_path(root: &Path, full_path: &Path) -> String {
    let relative = full_path
        .strip_prefix(root)
        .unwrap_or(full_path)
        .to_string_lossy()
        .replace('\\', "/");
    format!("strategies/{relative}")
}

pub(crate) fn strategy_name(full_path: &Path) -> Result<String, String> {
    full_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string)
        .ok_or_else(|| format!("Invalid strategy file name: {:?}", full_path))
}

fn collect_strategy_files(
    directory: &Path,
    root: &Path,
    files: &mut Vec<StrategyFileDto>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|e| format!("Failed to read strategy directory {:?}: {e}", directory))?
    {
        let entry = entry.map_err(|e| format!("Failed to inspect strategy entry: {e}"))?;
        let path = entry.path();

        if path.is_dir() {
            collect_strategy_files(&path, root, files)?;
            continue;
        }

        if path.extension() != Some(OsStr::new("py")) {
            continue;
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read strategy file {:?}: {e}", path))?;
        files.push(StrategyFileDto {
            name: strategy_name(&path)?,
            path: make_strategy_path(root, &path),
            content,
        });
    }

    Ok(())
}

fn ensure_default_strategy(root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_strategy_files(root, root, &mut files)?;

    if files.is_empty() {
        fs::write(root.join("my_strategy.py"), DEFAULT_STRATEGY_TEMPLATE)
            .map_err(|e| format!("Failed to create starter strategy: {e}"))?;
    }

    Ok(())
}

pub(crate) fn normalize_strategy_path(
    path: &str,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<PathBuf, String> {
    validation::validate_path(path)?;

    let trimmed = path.trim().trim_start_matches("strategies/");
    if trimmed.is_empty() {
        return Err("Strategy path must not be empty".into());
    }
    // Containment: every component must be a plain name — no `..`, `.`,
    // absolute roots, or drive prefixes can escape the strategy root.
    let rel = Path::new(trimmed);
    if !rel
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err("Strategy path must stay inside the strategies directory".into());
    }
    if rel.extension() != Some(OsStr::new("py")) {
        return Err("Strategy files must end in .py".into());
    }

    Ok(strategy_root(runtime_paths)?.join(trimmed))
}

fn parse_timeframe(value: &str) -> Result<Timeframe, String> {
    match value.trim().to_lowercase().as_str() {
        "1m" | "m1" => Ok(Timeframe::M1),
        "5m" | "m5" => Ok(Timeframe::M5),
        "15m" | "m15" => Ok(Timeframe::M15),
        "1h" | "h1" => Ok(Timeframe::H1),
        "4h" | "h4" => Ok(Timeframe::H4),
        "1d" | "d1" => Ok(Timeframe::D1),
        "1w" | "w1" => Ok(Timeframe::W1),
        other => Err(format!("Unsupported timeframe: {other}")),
    }
}

fn parse_date_input(value: &str, end_of_day: bool) -> Result<DateTime<Utc>, String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Ok(dt.with_timezone(&Utc));
    }

    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|e| format!("Invalid date '{value}': {e}"))?;
    let naive = if end_of_day {
        date.and_hms_opt(23, 59, 59)
    } else {
        date.and_hms_opt(0, 0, 0)
    }
    .ok_or_else(|| format!("Invalid date boundary for {value}"))?;

    Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

fn currency_for_symbol(symbol: &str) -> &'static str {
    if symbol.ends_with(".NS") || symbol.ends_with(".BO") {
        "INR"
    } else {
        "USD"
    }
}

async fn load_backtest_bars(
    state: &AppState,
    symbol: &Symbol,
    timeframe: Timeframe,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<(Vec<OHLCV>, String), String> {
    let query = OHLCVQuery {
        symbol: symbol.clone(),
        timeframe: timeframe.clone(),
        from,
        to,
        limit: Some(5_000),
    };

    match state.storage.query_ohlcv(query).await {
        Ok(bars) if !bars.is_empty() => {
            return Ok((
                bars,
                "Loaded historical bars from local storage cache.".to_string(),
            ));
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                ?error,
                "Local historical cache lookup failed; falling back to Yahoo Finance"
            );
        }
    }

    let adapter = YahooFinanceAdapter::new();
    let bars = adapter
        .get_historical_ohlcv(symbol, timeframe, from, to)
        .await
        .map_err(|e| format!("Failed to fetch historical bars for {}: {e}", symbol.0))?;

    if bars.is_empty() {
        return Err(format!(
            "No historical bars were available for {} in the requested date range.",
            symbol.0
        ));
    }

    Ok((
        bars,
        "Fetched historical bars live from Yahoo Finance for this backtest run.".to_string(),
    ))
}

#[command]
pub async fn list_strategy_files(
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
) -> Result<Vec<StrategyFileDto>, String> {
    list_strategy_files_inner(runtime_paths.inner())
}

pub(crate) fn list_strategy_files_inner(
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<Vec<StrategyFileDto>, String> {
    let root = strategy_root(runtime_paths)?;
    ensure_default_strategy(&root)?;

    let mut files = Vec::new();
    collect_strategy_files(&root, &root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

#[command]
pub async fn create_strategy_file(
    path: String,
    content: Option<String>,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
    state: State<'_, AppState>,
) -> Result<StrategyFileDto, String> {
    let result = create_strategy_file_inner(&path, content, runtime_paths.inner());
    record_file_change(state.inner(), "strategy.create", "Created", "create", &path, &result.as_ref().map(|f| f.path.clone()).map_err(|e| e.clone()));
    result
}

fn create_strategy_file_inner(
    path: &str,
    content: Option<String>,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<StrategyFileDto, String> {
    let path = path.to_string();
    let root = strategy_root(runtime_paths)?;
    let full_path = normalize_strategy_path(&path, runtime_paths)?;

    if full_path.exists() {
        return Err(format!("Strategy file already exists: {}", path));
    }

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create strategy subdirectory {:?}: {e}", parent))?;
    }

    let strategy_content = content.unwrap_or_else(|| DEFAULT_STRATEGY_TEMPLATE.to_string());
    fs::write(&full_path, &strategy_content)
        .map_err(|e| format!("Failed to create strategy file {:?}: {e}", full_path))?;

    Ok(StrategyFileDto {
        name: strategy_name(&full_path)?,
        path: make_strategy_path(&root, &full_path),
        content: strategy_content,
    })
}

#[command]
pub async fn save_strategy_file(
    path: String,
    content: String,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
    state: State<'_, AppState>,
) -> Result<StrategyFileDto, String> {
    let result = save_strategy_file_inner(&path, content, runtime_paths.inner());
    record_file_change(state.inner(), "strategy.save", "Saved", "save", &path, &result.as_ref().map(|f| f.path.clone()).map_err(|e| e.clone()));
    result
}

fn save_strategy_file_inner(
    path: &str,
    content: String,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<StrategyFileDto, String> {
    let root = strategy_root(runtime_paths)?;
    let full_path = normalize_strategy_path(&path, runtime_paths)?;

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create strategy subdirectory {:?}: {e}", parent))?;
    }

    fs::write(&full_path, &content)
        .map_err(|e| format!("Failed to save strategy file {:?}: {e}", full_path))?;

    Ok(StrategyFileDto {
        name: strategy_name(&full_path)?,
        path: make_strategy_path(&root, &full_path),
        content,
    })
}

#[command]
pub async fn delete_strategy_file(
    path: String,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let result = delete_strategy_file_inner(&path, runtime_paths.inner());
    // Deleting a file that was already gone changed nothing, so it is not logged.
    if !matches!(result, Ok(false)) {
        record_file_change(
            state.inner(),
            "strategy.delete",
            "Deleted",
            "delete",
            &path,
            &result.as_ref().map(|_| super::copilot_tools::audit::strategy_target(&path)).map_err(|e| e.clone()),
        );
    }
    result
}

fn delete_strategy_file_inner(
    path: &str,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<bool, String> {
    let full_path = normalize_strategy_path(path, runtime_paths)?;

    if !full_path.exists() {
        return Ok(false);
    }

    fs::remove_file(&full_path)
        .map_err(|e| format!("Failed to delete strategy file {:?}: {e}", full_path))?;
    Ok(true)
}

#[command]
pub async fn run_strategy_file(
    app: tauri::AppHandle,
    path: String,
    params_json: Option<String>,
    state: State<'_, AppState>,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
) -> Result<StrategyExecutionResultDto, String> {
    let runtime_paths = runtime_paths.inner();
    let full_path = normalize_strategy_path(&path, runtime_paths)?;
    if !full_path.exists() {
        return Err(format!("Strategy file not found: {}", path));
    }

    let params_value = match params_json.as_deref() {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|e| format!("Invalid strategy params JSON: {e}"))?,
        _ => serde_json::json!({}),
    };

    if !params_value.is_object() {
        return Err("Strategy params must be a JSON object".into());
    }

    let started_at = Utc::now();
    let result =
        super::strategy_worker::run(&app, path, params_value, state.inner(), runtime_paths, "user").await;
    let finished_at = Utc::now();
    let (success, output, error) = match result {
        Ok(lines) => (true, lines, None),
        Err(error) => (false, Vec::new(), Some(error)),
    };
    Ok(StrategyExecutionResultDto {
        success,
        output,
        error,
        exit_code: Some(if success { 0 } else { 1 }),
        started_at: started_at.to_rfc3339(),
        finished_at: finished_at.to_rfc3339(),
    })
}

#[command]
pub async fn run_strategy_backtest(
    request: StrategyBacktestRequestDto,
    state: State<'_, AppState>,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
) -> Result<StrategyBacktestResultDto, String> {
    let target = super::copilot_tools::audit::strategy_target(&request.path);
    let symbol = request.symbol.trim().to_uppercase();
    let result = run_backtest_inner(request, state.inner(), runtime_paths.inner()).await;
    // A backtest changes nothing, so it is logged without telling panels to reload.
    let (summary, details) = match &result {
        Ok(r) => (backtest_summary(&target, &symbol, &r.metrics, r.trades.len()), None),
        Err(error) => (format!("Backtest of {target} on {symbol} failed"), Some(error.clone())),
    };
    crate::activity::record_info(state.inner(), "user", "strategy.backtest", "strategy", &target, &summary, result.is_ok(), details);
    result
}

fn backtest_summary(target: &str, symbol: &str, metrics: &BacktestMetrics, trades: usize) -> String {
    format!(
        "Backtested {target} on {symbol}: return {:.2}%, Sharpe {:.2}, {trades} trades",
        metrics.total_return_pct, metrics.sharpe_ratio
    )
}

/// Log a user-made change to a strategy file. `path` is what the caller asked for; on success
/// `saved_path` is the canonical `strategies/...` path the UI knows the file by.
fn record_file_change(
    state: &AppState,
    action: &str,
    past: &str,
    infinitive: &str,
    path: &str,
    saved_path: &Result<String, String>,
) {
    let target = saved_path
        .as_ref()
        .map(String::clone)
        .unwrap_or_else(|_| super::copilot_tools::audit::strategy_target(path));
    let (summary, details) = match saved_path {
        Ok(_) => (format!("{past} {target}"), None),
        Err(error) => (format!("Could not {infinitive} {target}"), Some(error.clone())),
    };
    crate::activity::record(state, "user", action, "strategy", &target, &summary, saved_path.is_ok(), details);
}

/// Shared backtest implementation — also used by the copilot agent loop.
pub(crate) async fn run_backtest_inner(
    request: StrategyBacktestRequestDto,
    state: &AppState,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<StrategyBacktestResultDto, String> {
    validation::validate_symbol(&request.symbol)?;
    validation::validate_path(&request.path)?;

    let full_path = normalize_strategy_path(&request.path, runtime_paths)?;
    if !full_path.exists() {
        return Err(format!("Strategy file not found: {}", request.path));
    }

    let timeframe = parse_timeframe(&request.timeframe)?;
    let from = parse_date_input(&request.from, false)?;
    let to = parse_date_input(&request.to, true)?;
    if from >= to {
        return Err("Backtest start date must be earlier than the end date".into());
    }

    let initial_capital = request.initial_capital.unwrap_or(100_000.0);
    if !initial_capital.is_finite() || initial_capital <= 0.0 {
        return Err("Initial capital must be a positive finite number".into());
    }

    let commission_bps = request.commission_bps.unwrap_or(3.0);
    if !commission_bps.is_finite() || commission_bps < 0.0 {
        return Err("Commission must be a non-negative finite number".into());
    }

    let slippage_bps = request.slippage_bps.unwrap_or(2.0);
    if !slippage_bps.is_finite() || slippage_bps < 0.0 {
        return Err("Slippage must be a non-negative finite number".into());
    }

    let quantity = request.quantity.unwrap_or(10.0);
    validation::validate_quantity(quantity)?;

    let symbol_text = request.symbol.trim().to_uppercase();
    let symbol = Symbol(symbol_text.clone());
    let mut notes = vec!["Executed the authored Python on_bar callbacks; long/flat signals only. Signals fill at the current bar close with configured costs.".to_string()];
    let (bars, data_source_note) =
        load_backtest_bars(state, &symbol, timeframe.clone(), from, to).await?;
    let bars_analyzed = bars.len();
    if bars_analyzed < 2 {
        return Err("Backtest requires at least two historical bars".into());
    }

    notes.push(data_source_note);
    notes.push(format!(
        "Configured trade size: {quantity:.2} shares/contracts with {:.2} bps commission and {:.2} bps slippage.",
        commission_bps,
        slippage_bps,
    ));

    let mut worker = super::strategy_worker::Worker::spawn(runtime_paths).await?;
    worker
        .request(serde_json::json!({"op":"init","path":full_path,"params":{"symbol":symbol_text}}))
        .await?;
    let mut signals = HashMap::new();
    for bar in &bars {
        let reply = worker
            .request(super::strategy_worker::bar_request(bar, &request.timeframe))
            .await?;
        let emitted = reply["signals"]
            .as_array()
            .ok_or("Invalid strategy signal response")?;
        if emitted.len() > 1 {
            return Err("Backtests support at most one signal per bar".into());
        }
        if let Some(signal) = emitted.first() {
            if signal["symbol"].as_str() != Some(&symbol_text) {
                return Err("Strategy emitted a signal for a different backtest symbol".into());
            }
            signals.insert(
                bar.time.timestamp(),
                signal["direction"].as_str().unwrap_or("").to_string(),
            );
        }
    }
    worker.finish().await;
    let mut data = HashMap::new();
    data.insert(symbol_text.clone(), bars);

    let config = BacktestConfig {
        run_id: format!("bt-{}", Uuid::new_v4()),
        symbols: vec![symbol.clone()],
        start: from,
        end: to,
        initial_capital,
        currency: currency_for_symbol(&symbol_text).to_string(),
        commission_bps,
        slippage_bps,
    };
    let mut engine = BacktestEngine::new(config);
    let backtest = engine
        .run(&data, move |symbol, bar, positions, _cash| {
            match signals.get(&bar.time.timestamp()).map(String::as_str) {
                Some("long") if !positions.contains_key(symbol) => Some(BacktestSignal::Buy {
                    symbol: Symbol(symbol.to_string()),
                    quantity,
                }),
                Some("flat") if positions.contains_key(symbol) => Some(BacktestSignal::Close {
                    symbol: Symbol(symbol.to_string()),
                }),
                _ => None,
            }
        })
        .map_err(|e| format!("Backtest execution failed: {e}"))?;

    Ok(StrategyBacktestResultDto {
        strategy_name: strategy_name(&full_path)?,
        strategy_path: request.path,
        inferred_strategy: "Authored Python strategy".into(),
        symbol: symbol_text,
        timeframe: request.timeframe.to_lowercase(),
        bars_analyzed,
        metrics: backtest.metrics,
        trades: backtest.trades,
        equity_curve: backtest.equity_curve,
        notes,
    })
}
