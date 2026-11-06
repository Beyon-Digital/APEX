use super::python_runtime;
use super::versioned_files::{self, Provenance};
use crate::ledger::{Kind, Ledger};
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
    /// The ledger version of the strategy file this run used (absent if it could not be resolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_hash: Option<String>,
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
    /// The ledger version and content hash of the strategy file that was backtested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_hash: Option<String>,
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

fn ensure_default_strategy(root: &Path, ledger: Option<&Ledger>) -> Result<(), String> {
    let mut files = Vec::new();
    collect_strategy_files(root, root, &mut files)?;

    if files.is_empty() {
        let target = root.join("my_strategy.py");
        match ledger {
            Some(ledger) => {
                versioned_files::save(ledger, Kind::Strategy, "strategies/my_strategy.py", &target, DEFAULT_STRATEGY_TEMPLATE.as_bytes(), "system")?;
            }
            // No ledger: the file is still created (listing must work); the next reconcile records it.
            None => fs::write(&target, DEFAULT_STRATEGY_TEMPLATE).map_err(|e| format!("Failed to create starter strategy: {e}"))?,
        }
    }

    Ok(())
}

/// The ledger object id of a strategy file: the path the UI knows it by (`strategies/sub/mom.py`).
pub(crate) fn strategy_id(runtime_paths: &python_runtime::RuntimePaths, full_path: &Path) -> String {
    make_strategy_path(runtime_paths.strategies_dir(), full_path)
}

/// Save a strategy file as `actor` through the audit ledger (a new version; identical content is a
/// no-op). The Copilot's `save_strategy` tool writes through this.
#[allow(dead_code)]
pub(crate) fn save_strategy_as(
    runtime_paths: &python_runtime::RuntimePaths,
    path: &str,
    content: String,
    actor: &str,
) -> Result<StrategyFileDto, String> {
    save_strategy_file_inner(versioned_files::ledger()?, path, content, runtime_paths, actor)
}

/// Restore an older version of a strategy as a new version and rewrite the file.
#[allow(dead_code)]
pub(crate) fn restore_strategy(
    ledger: &Ledger,
    runtime_paths: &python_runtime::RuntimePaths,
    id: &str,
    version: u32,
    actor: &str,
) -> Result<crate::ledger::VersionRecord, String> {
    let full_path = normalize_strategy_path(id, runtime_paths)?;
    let id = strategy_id(runtime_paths, &full_path);
    versioned_files::restore(ledger, Kind::Strategy, &id, &full_path, version, actor)
}

/// The strategy's current ledger version and content hash, after recording any unsaved external edit.
pub(crate) fn strategy_provenance(
    ledger: &Ledger,
    runtime_paths: &python_runtime::RuntimePaths,
    path: &str,
) -> Result<Option<Provenance>, String> {
    let full_path = normalize_strategy_path(path, runtime_paths)?;
    versioned_files::resolve_head(ledger, Kind::Strategy, &strategy_id(runtime_paths, &full_path), &full_path)
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

    let root = strategy_root(runtime_paths)?;
    Ok(match_disk_names(&root, rel))
}

/// `rel` under `root`, spelled the way the entries on disk are spelled. macOS (APFS) and Windows
/// treat `Foo.py` and `foo.py` as one file, but the ledger id is the path as spelled, so a save
/// under another spelling would put one file's history in two objects (and the next reconcile
/// would record the app's own save as an "external" edit). A component that does not exist yet
/// keeps the spelling given. The same holds for Unicode forms that the filesystem treats alike.
fn match_disk_names(root: &Path, rel: &Path) -> PathBuf {
    let mut current = root.to_path_buf();
    let mut exists = true;
    for component in rel.components() {
        let name = component.as_os_str();
        if exists {
            if let Some(found) = on_disk_name(&current, name) {
                current.push(found);
                continue;
            }
            exists = false;
        }
        current.push(name);
    }
    current
}

/// The entry of `dir` that is the same file as `dir/name` (exact spelling first), if there is one.
fn on_disk_name(dir: &Path, name: &OsStr) -> Option<std::ffi::OsString> {
    let entries: Vec<std::ffi::OsString> = fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
    if entries.iter().any(|e| e == name) {
        return Some(name.to_os_string());
    }
    let wanted = fs::metadata(dir.join(name)).ok()?; // the filesystem says it is there under this spelling
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        entries.into_iter().find(|e| {
            fs::metadata(dir.join(e)).map(|m| m.dev() == wanted.dev() && m.ino() == wanted.ino()).unwrap_or(false)
        })
    }
    #[cfg(not(unix))]
    {
        let _ = wanted;
        let lower = name.to_string_lossy().to_lowercase();
        entries.into_iter().find(|e| e.to_string_lossy().to_lowercase() == lower)
    }
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
    // An edit made outside the app becomes a version the next time the user looks.
    versioned_files::reconcile_for_listing(runtime_paths, Kind::Strategy);
    ensure_default_strategy(&root, versioned_files::try_ledger())?;

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
    let result = versioned_files::ledger().and_then(|ledger| create_strategy_file_inner(ledger, &path, content, runtime_paths.inner(), "user"));
    record_file_change(state.inner(), "strategy.create", "Created", "create", &path, &result.as_ref().map(|f| f.path.clone()).map_err(|e| e.clone()));
    result
}

fn create_strategy_file_inner(
    ledger: &Ledger,
    path: &str,
    content: Option<String>,
    runtime_paths: &python_runtime::RuntimePaths,
    actor: &str,
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
    versioned_files::save(ledger, Kind::Strategy, &strategy_id(runtime_paths, &full_path), &full_path, strategy_content.as_bytes(), actor)
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
    let result = versioned_files::ledger().and_then(|ledger| save_strategy_file_inner(ledger, &path, content, runtime_paths.inner(), "user"));
    record_file_change(state.inner(), "strategy.save", "Saved", "save", &path, &result.as_ref().map(|f| f.path.clone()).map_err(|e| e.clone()));
    result
}

pub(crate) fn save_strategy_file_inner(
    ledger: &Ledger,
    path: &str,
    content: String,
    runtime_paths: &python_runtime::RuntimePaths,
    actor: &str,
) -> Result<StrategyFileDto, String> {
    let root = strategy_root(runtime_paths)?;
    let full_path = normalize_strategy_path(&path, runtime_paths)?;

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create strategy subdirectory {:?}: {e}", parent))?;
    }

    versioned_files::save(ledger, Kind::Strategy, &strategy_id(runtime_paths, &full_path), &full_path, content.as_bytes(), actor)
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
    let result = versioned_files::ledger().and_then(|ledger| delete_strategy_file_inner(ledger, &path, runtime_paths.inner(), "user"));
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

/// A delete is a tombstone in the ledger (the history stays and the file can be restored or
/// re-created under the same id), then the working file is removed.
fn delete_strategy_file_inner(
    ledger: &Ledger,
    path: &str,
    runtime_paths: &python_runtime::RuntimePaths,
    actor: &str,
) -> Result<bool, String> {
    let full_path = normalize_strategy_path(path, runtime_paths)?;

    if full_path.is_dir() {
        return Err(format!("Failed to delete strategy file {:?}: it is a directory", full_path));
    }
    versioned_files::delete(ledger, Kind::Strategy, &strategy_id(runtime_paths, &full_path), &full_path, actor)
        .map_err(|e| format!("Failed to delete strategy file {:?}: {e}", full_path))
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
    let started_text = started_at.to_rfc3339();
    let session_path = path.clone();
    let result =
        super::strategy_worker::run(&app, path, params_value, state.inner(), runtime_paths, "user").await;
    let finished_at = Utc::now();
    // The version the session ran (recorded by the worker when it started).
    let provenance = super::strategy_worker::session_provenance(&session_path, &started_text);
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
        strategy_version: provenance.as_ref().map(|p| p.version),
        strategy_hash: provenance.map(|p| p.hash),
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
        Ok(r) => (
            backtest_summary(&target, &symbol, &r.metrics, r.trades.len()),
            r.strategy_version.zip(r.strategy_hash.as_ref()).map(|(version, hash)| Provenance { version, hash: hash.clone() }.details("strategy")),
        ),
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

    // Pin the exact version being tested (an unsaved external edit becomes a version first). A backtest
    // trades nothing, so a missing ledger only leaves the version out of the result.
    let provenance = match versioned_files::ledger().and_then(|ledger| strategy_provenance(ledger, runtime_paths, &request.path)) {
        Ok(p) => p,
        Err(error) => {
            tracing::warn!(path = %request.path, %error, "could not resolve the strategy version for a backtest");
            None
        }
    };

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
    if let Some(p) = &provenance {
        notes.push(format!("Strategy version {} (sha256 {}).", p.version, p.hash));
    }
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
        strategy_version: provenance.as_ref().map(|p| p.version),
        strategy_hash: provenance.map(|p| p.hash),
    })
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use crate::commands::versioned_files::testutil::{open, tmp, versions};
    use std::fs;

    #[test]
    fn create_save_delete_and_recreate_go_through_the_ledger() {
        let root = tmp("strat-crud");
        let paths = python_runtime::RuntimePaths::for_test(&root.0);
        let l = open(&root);
        let id = "strategies/sub/mom.py";

        let created = create_strategy_file_inner(&l, "sub/mom.py", Some("a = 1\n".into()), &paths, "user").unwrap();
        assert_eq!((created.path.as_str(), created.content.as_str()), (id, "a = 1\n"));
        assert!(create_strategy_file_inner(&l, "sub/mom.py", None, &paths, "user").unwrap_err().contains("already exists"));

        save_strategy_file_inner(&l, id, "a = 2\n".into(), &paths, "copilot").unwrap();
        save_strategy_file_inner(&l, id, "a = 2\n".into(), &paths, "user").unwrap();
        assert_eq!(fs::read_to_string(root.0.join("strategies/sub/mom.py")).unwrap(), "a = 2\n");
        assert_eq!(versions(&l, Kind::Strategy, id), [(1, "create".into(), "user".into()), (2, "update".into(), "copilot".into())]);

        assert!(delete_strategy_file_inner(&l, id, &paths, "user").unwrap());
        assert!(!delete_strategy_file_inner(&l, id, &paths, "user").unwrap());
        assert!(!root.0.join("strategies/sub/mom.py").exists());
        assert_eq!(l.history(Kind::Strategy, id).len(), 3);

        let again = create_strategy_file_inner(&l, "sub/mom.py", Some("a = 3\n".into()), &paths, "user").unwrap();
        assert_eq!(again.content, "a = 3\n");
        assert_eq!(l.head(Kind::Strategy, id).unwrap().version, 4, "the same object continues");
        assert_eq!(l.content(Kind::Strategy, id, 2).unwrap(), b"a = 2\n");

        let restored = restore_strategy(&l, &paths, id, 1, "user").unwrap();
        assert_eq!((restored.version, restored.restored_from), (5, Some(1)));
        assert_eq!(fs::read_to_string(root.0.join("strategies/sub/mom.py")).unwrap(), "a = 1\n");
        // The id may be given the way the UI sends it or without the folder prefix.
        assert_eq!(restore_strategy(&l, &paths, "sub/mom.py", 2, "user").unwrap().version, 6);
    }

    /// Whether the temp filesystem treats `A` and `a` as one file (macOS APFS by default, Windows).
    fn case_insensitive(dir: &Path) -> bool {
        let probe = dir.join("CaseProbe.tmp");
        fs::write(&probe, b"x").unwrap();
        let same = dir.join("caseprobe.tmp").exists();
        let _ = fs::remove_file(&probe);
        same
    }

    #[test]
    fn a_save_under_another_letter_case_continues_the_same_object() {
        let root = tmp("strat-case");
        let paths = python_runtime::RuntimePaths::for_test(&root.0);
        let l = open(&root);
        fs::create_dir_all(root.0.join("strategies")).unwrap();
        if !case_insensitive(&root.0.join("strategies")) {
            eprintln!("skipping: this filesystem is case sensitive, so foo.py and Foo.py are different files");
            return;
        }
        save_strategy_file_inner(&l, "Sub/Foo.py", "v1".into(), &paths, "user").unwrap();
        let again = save_strategy_file_inner(&l, "sub/foo.py", "v2".into(), &paths, "user").unwrap();
        assert_eq!(again.path, "strategies/Sub/Foo.py", "the on-disk spelling is the id");
        assert_eq!(versions(&l, Kind::Strategy, "strategies/Sub/Foo.py").len(), 2);
        assert!(l.head(Kind::Strategy, "strategies/sub/foo.py").is_none(), "no second object");
        assert_eq!(strategy_provenance(&l, &paths, "SUB/FOO.py").unwrap().map(|p| p.version), Some(2));
        assert_eq!(strategy_provenance(&l, &paths, "sub/foo.py").unwrap().map(|p| p.version), Some(2));
        // Nothing for the next reconcile to record as an outside edit.
        let summary = versioned_files::reconcile_all(&l, &paths);
        assert_eq!((summary.adopted, summary.external_edits, summary.removed), (0, 0, 0), "{summary:?}");
        assert_eq!(l.objects(Kind::Strategy, true).len(), 1);
    }

    #[test]
    fn a_new_file_keeps_the_spelling_it_was_given() {
        let root = tmp("strat-case-new");
        let paths = python_runtime::RuntimePaths::for_test(&root.0);
        let l = open(&root);
        let dto = save_strategy_file_inner(&l, "NewDir/MixedCase.py", "x".into(), &paths, "user").unwrap();
        assert_eq!(dto.path, "strategies/NewDir/MixedCase.py");
    }

    #[test]
    fn path_escapes_are_rejected_as_before_and_record_nothing() {
        let root = tmp("strat-escape");
        let paths = python_runtime::RuntimePaths::for_test(&root.0);
        let l = open(&root);
        for bad in ["../evil.py", "strategies/../../evil.py", "/tmp/evil.py", "a/../../evil.py", "notes.txt", "", "strategies/"] {
            assert!(create_strategy_file_inner(&l, bad, None, &paths, "user").is_err(), "create {bad:?}");
            assert!(save_strategy_file_inner(&l, bad, "x".into(), &paths, "user").is_err(), "save {bad:?}");
            assert!(delete_strategy_file_inner(&l, bad, &paths, "user").is_err(), "delete {bad:?}");
            assert!(restore_strategy(&l, &paths, bad, 1, "user").is_err(), "restore {bad:?}");
        }
        assert!(l.objects(Kind::Strategy, true).is_empty());
        assert!(!root.0.join("evil.py").exists() && !std::env::temp_dir().join("evil.py").exists());
    }

    #[test]
    fn provenance_and_result_fields_carry_the_version_and_hash() {
        let root = tmp("strat-prov");
        let paths = python_runtime::RuntimePaths::for_test(&root.0);
        let l = open(&root);
        save_strategy_file_inner(&l, "mom.py", "v1".into(), &paths, "user").unwrap();
        let p = strategy_provenance(&l, &paths, "mom.py").unwrap().unwrap();
        assert_eq!((p.version, p.hash.clone()), (1, crate::ledger::Ledger::sha256_hex(b"v1")));
        assert_eq!(strategy_provenance(&l, &paths, "strategies/mom.py").unwrap(), Some(p.clone()));
        assert!(strategy_provenance(&l, &paths, "../x.py").is_err());
        assert_eq!(strategy_provenance(&l, &paths, "other.py").unwrap(), None);

        let dto = StrategyExecutionResultDto {
            success: true, output: vec![], error: None, exit_code: Some(0), started_at: String::new(), finished_at: String::new(),
            strategy_version: Some(p.version), strategy_hash: Some(p.hash.clone()),
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!((json["strategy_version"].as_u64(), json["strategy_hash"].as_str()), (Some(1), Some(p.hash.as_str())));
        let bare = StrategyExecutionResultDto { strategy_version: None, strategy_hash: None, ..dto };
        assert!(serde_json::to_value(&bare).unwrap().get("strategy_version").is_none(), "absent when unknown");
    }
}
