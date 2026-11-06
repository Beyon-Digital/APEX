use crate::commands::ml::ModelRegistry;
use crate::commands::python_runtime;
use crate::commands::scanner::ScanCriterionDto;
use crate::commands::strategy::StrategyBacktestRequestDto;
use crate::commands::copilot_mode::{self, ChatMode};
use crate::commands::{automation, copilot_more, copilot_tools, data, ml, ml_estimators, quant, scanner, strategy};
use crate::dto::MLTrainingRequestDto;
use crate::state::AppState;
use crate::validation;
use apex_core::application::quant as q;
use apex_core::domain::models::{
    NewOrderRequest, OrderId, OrderQuery, OrderSide, OrderType, Symbol, Timeframe,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

#[derive(Debug, Clone, Deserialize)]
pub struct CopilotMessage {
    /// "user" or "assistant"
    pub role: String,
    pub content: String,
}

/// One step of the agentic tool-call loop, surfaced in the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallTrace {
    pub name: String,
    pub detail: String,
    pub ok: bool,
    /// What this call created or changed that the user can open (strategy file, automation,
    /// model, dataset). Left out of the JSON when there is none, so older readers are unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ToolArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolArtifact {
    /// "strategy" | "model" | "dataset" | "automation"
    pub kind: String,
    pub id: String,
}

/// A write-class tool call paused for user approval ([copilot]
/// require_approval). Approve with `approve_copilot_tools` + resend the turn.
#[derive(Debug, Clone, Serialize)]
pub struct PendingApprovalDto {
    /// Deterministic key: `name:{canonical args}`.
    pub key: String,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CopilotReply {
    pub reply: String,
    pub model: String,
    pub provider: String,
    /// The mode the reply was produced under: "ask", "review" or "agent".
    pub mode: String,
    pub tool_calls: Vec<ToolCallTrace>,
    pub pending_approvals: Vec<PendingApprovalDto>,
}

/// Write-class tools — mutate state or touch the filesystem. When
/// `[copilot] require_approval` is set they pause until the user approves
/// the exact call (name + canonical args).
pub(super) const WRITE_TOOLS: &[&str] = &[
    "place_order",
    "cancel_order",
    "create_automation",
    "set_automation_enabled",
    "delete_automation",
    "save_strategy",
    "export_bars_csv",
    "train_ml_model",
    "create_alert",
    "remove_alert",
];

/// Canonical form of `args` (object keys sorted recursively) so the same
/// logical call always maps to one approval/cache key.
fn canon_args(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let inner = entries
                .iter()
                // Keys are JSON-escaped like values, so two different objects can never
                // produce the same string (a key containing `\":` would otherwise forge one).
                .map(|(k, v)| format!("{}:{}", Value::String((*k).clone()), canon_args(v)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
        Value::Array(items) => {
            let inner = items.iter().map(canon_args).collect::<Vec<_>>().join(",");
            format!("[{inner}]")
        }
        other => other.to_string(),
    }
}

fn tool_approval_key(name: &str, args: &Value) -> String {
    format!("{name}:{}", canon_args(args))
}

/// Dispatch one model-requested tool call under the approval gate.
/// `Some(payload)` = executed (feed back to the model); `None` = the call
/// is pending approval and the loop must stop.
async fn dispatch_tool(
    name: &str,
    args: &Value,
    state: &AppState,
    models: &ModelRegistry,
    runtime_paths: &python_runtime::RuntimePaths,
    mode: ChatMode,
    trace: &mut Vec<ToolCallTrace>,
    pending: &mut Vec<PendingApprovalDto>,
) -> Option<Value> {
    let detail = format!("{}{}", trace_detail(name, args), copilot_tools::estimator_card_note(name, args, runtime_paths));
    let refused = mode == ChatMode::Ask && copilot_mode::refused_in_ask(name, args, state, runtime_paths);
    // Ask mode is read-only. The model is not offered write tools, but a call that arrives
    // anyway (a stale tool list, a hallucinated name) is refused here, not executed.
    if refused {
        trace.push(ToolCallTrace {
            name: name.to_string(),
            detail: format!("{detail} — blocked in Ask mode"),
            ok: false,
            artifact: None,
        });
        return Some(json!({
            "error": "Ask mode is read-only, so this was not done. Tell the user what you would do and that they can switch to Review or Agent mode."
        }));
    }
    let gated = copilot_mode::requires_approval(mode, name, args, state, runtime_paths);

    // A custom-estimator run is approved for the file content the card showed, not for whatever
    // the file holds later.
    let key = format!("{}{}", tool_approval_key(name, args), copilot_tools::approval_key_suffix(name, args, runtime_paths));
    let cached = if gated { state.copilot_tool_cache.lock().unwrap().get(&key).cloned() } else { None };
    let approved = gated && state.copilot_approvals.lock().unwrap().contains(&key);
    let gate = copilot_tools::audit::gate(false, gated, cached.is_some(), approved);

    // Looked up now: a delete removes the rule, and its name belongs in the log.
    let automation_name = if matches!(name, "set_automation_enabled" | "delete_automation") {
        let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
        state.automations.list().iter().find(|r| r.id == id).map(|r| r.name.clone())
    } else {
        None
    };

    let result = match gate {
        copilot_tools::audit::Gate::Replay => {
            // Approved + executed on the first pass — replay returns the
            // recorded result instead of double-firing (e.g. duplicate order).
            let cached = cached.unwrap_or_default();
            Ok(serde_json::from_str(&cached).unwrap_or_else(|_| json!(cached)))
        }
        copilot_tools::audit::Gate::Pending => {
            pending.push(PendingApprovalDto {
                key,
                name: name.to_string(),
                detail: detail.clone(),
            });
            trace.push(ToolCallTrace {
                name: name.to_string(),
                detail: format!("{detail} — awaiting approval"),
                ok: false,
                artifact: None,
            });
            return None;
        }
        // `RefusedInAsk` returned above, so this is `Execute`.
        _ => {
            let r = exec_tool(name, args, state, models, runtime_paths).await;
            if gated {
                // An approval is good for one execution. The result is cached only so the
                // replay of this same turn does not fire the call a second time.
                state.copilot_approvals.lock().unwrap().remove(&key);
                if let Ok(v) = &r {
                    state
                        .copilot_tool_cache
                        .lock()
                        .unwrap()
                        .insert(key, v.to_string());
                }
            }
            r
        }
    };

    // One activity entry per execution. A refused, paused or replayed call never gets here.
    if let Some(entry) = copilot_tools::audit::entry_for(gate, name, args, &result, automation_name.as_deref()) {
        let record = if entry.informational { crate::activity::record_info } else { crate::activity::record };
        record(state, "copilot", entry.action, entry.kind, &entry.target_id, &entry.summary, entry.ok, entry.details);
    }

    let artifact = result
        .as_ref()
        .ok()
        .and_then(|v| copilot_tools::audit::artifact(name, args, v))
        .map(|(kind, id)| ToolArtifact { kind: kind.to_string(), id });
    let (payload, ok) = match result {
        Ok(v) => (v, true),
        Err(e) => (json!({ "error": e }), false),
    };
    trace.push(ToolCallTrace {
        name: name.to_string(),
        detail,
        ok,
        artifact,
    });
    Some(payload)
}

/// Approve pending write-class tool calls — the UI passes the `key`s it
/// showed the user, then re-sends the turn; approved calls execute once
/// and are cached so the replay is idempotent.
#[tauri::command]
pub async fn approve_copilot_tools(
    keys: Vec<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let mut approvals = state.copilot_approvals.lock().unwrap();
    let count = keys.len();
    for k in keys {
        approvals.insert(k);
    }
    Ok(count)
}

// ── Provider resolution ─────────────────────────────────────────────

/// Fully-resolved inference target for this request.
struct ResolvedProvider {
    id: String,
    base_url: String,
    model: String,
    /// "chat" | "responses" | "chatgpt" | "acp"
    api_kind: String,
    api_key: String,
    max_tokens: u32,
}

/// API key for an LLM provider, read from the encrypted vault (`llm:<id>`).
fn vault_key(state: &AppState, provider_id: &str) -> Result<Option<String>, String> {
    if !state.vault.is_unlocked() {
        return Err("Vault is locked. Unlock it to use the LLM provider.".to_string());
    }
    Ok(state
        .vault
        .get_secret(&format!("llm:{provider_id}"), "api_key")?
        .filter(|v| !v.trim().is_empty()))
}

fn resolve_provider(state: &AppState) -> Result<ResolvedProvider, String> {
    // The user's ChatGPT plan, when they signed in and chose it for the copilot.
    let prefs = crate::chatgpt_auth::preferences(&state.vault);
    if prefs.use_for_copilot {
        let status = crate::chatgpt_auth::status(state);
        if !(status.state == "connected" && status.plan_usage) {
            // The user chose their ChatGPT plan; quietly using another provider (and
            // sending their trading data there) would be a surprise.
            return Err(
                "Your ChatGPT sign-in is not active. Sign in again under Settings > AI Copilot, or turn off \"Use my ChatGPT plan\"."
                    .to_string(),
            );
        }
        {
            // Model names differ per account, so never guess one.
            let model = prefs.model.filter(|m| !m.trim().is_empty()).ok_or_else(|| {
                "Choose a ChatGPT model under Settings > AI Copilot > ChatGPT plan.".to_string()
            })?;
            return Ok(ResolvedProvider {
                id: "chatgpt".to_string(),
                base_url: crate::chatgpt_auth::RESOURCE.to_string(),
                model,
                api_kind: "chatgpt".to_string(),
                api_key: String::new(),
                max_tokens: state.copilot.max_tokens,
            });
        }
    }
    // Provider registry takes precedence when an active id is set.
    if !state.llm.active.trim().is_empty() {
        let provider = state
            .llm
            .providers
            .iter()
            .find(|p| p.id == state.llm.active)
            .ok_or_else(|| {
                format!(
                    "Active LLM provider `{}` not found in llm.providers",
                    state.llm.active
                )
            })?;
        let base_url = provider.base_url.trim().trim_end_matches('/').to_string();
        // Enforce at request time too — a hand-edited config file must not
        // bypass settings validation.
        if provider.api_kind != "acp" {
            validation::validate_provider_url(&base_url)?;
        }
        return Ok(ResolvedProvider {
            id: provider.id.clone(),
            base_url,
            model: provider.model.clone(),
            api_kind: provider.api_kind.clone(),
            api_key: vault_key(state, &provider.id)?.unwrap_or_default(),
            // 0 = "use the legacy [copilot] limit" (see LlmProviderConfig docs)
            max_tokens: if provider.max_tokens == 0 {
                state.copilot.max_tokens
            } else {
                provider.max_tokens
            },
        });
    }

    // Legacy fallback: [copilot] section + the `llm:copilot` vault key.
    if !state.copilot.enabled {
        return Err(
            "Copilot is disabled — enable it in settings or configure an LLM provider".to_string(),
        );
    }
    let api_key = vault_key(state, "copilot")?.ok_or_else(|| {
        "No LLM provider configured — add an API key under Settings > Adapters".to_string()
    })?;
    let base_url = state
        .copilot
        .base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    validation::validate_provider_url(&base_url)?;
    Ok(ResolvedProvider {
        id: "copilot".to_string(),
        base_url,
        model: state.copilot.model.clone(),
        api_kind: "chat".to_string(),
        api_key,
        max_tokens: state.copilot.max_tokens,
    })
}

// ── Tool schemas (OpenAI function-calling format) ───────────────────

/// The algorithm ids `train_ml_model` accepts: the built-in ones plus `recipe`.
fn train_algorithm_ids() -> Vec<&'static str> {
    let mut ids = ml_estimators::ALGORITHMS.to_vec();
    ids.push(ml_estimators::RECIPE_ALGORITHM);
    ids
}

fn tool_schemas() -> Value {
    let tools = [
        ("get_quote", "Latest quote for a watchlist symbol", json!({
            "type": "object", "properties": {"symbol": {"type": "string"}},
            "required": ["symbol"] })),
        ("get_ohlcv", "Recent OHLCV bars (last N closes) for a symbol", json!({
            "type": "object", "properties": {
                "symbol": {"type": "string"},
                "timeframe": {"type": "string", "default": "d1"},
                "limit": {"type": "integer", "default": 60} },
            "required": ["symbol"] })),
        ("get_quant_stats", "Quantitative summary (returns, vol, Sharpe, skew, drawdown, autocorr) for a symbol", json!({
            "type": "object", "properties": {
                "symbol": {"type": "string"},
                "timeframe": {"type": "string", "default": "d1"} },
            "required": ["symbol"] })),
        ("get_regression", "OLS regression of y returns on x returns (beta/alpha/R²) — e.g. a stock vs its index", json!({
            "type": "object", "properties": {
                "x_symbol": {"type": "string"},
                "y_symbol": {"type": "string"},
                "timeframe": {"type": "string", "default": "d1"} },
            "required": ["x_symbol", "y_symbol"] })),
        ("run_scan", "Screen symbols against criteria (price_above/below, pct_change_above/below, volume_above, above_sma/below_sma)", json!({
            "type": "object", "properties": {
                "symbols": {"type": "array", "items": {"type": "string"}},
                "criteria": {"type": "array", "items": {"type": "object"}},
                "timeframe": {"type": "string", "default": "d1"} },
            "required": ["criteria"] })),
        ("get_news", "Latest market news headlines", json!({
            "type": "object", "properties": {
                "limit": {"type": "integer", "default": 8},
                "symbol": {"type": "string"} } })),
        ("list_strategies", "List strategy .py files in the strategies directory", json!({
            "type": "object", "properties": {} })),
        ("read_strategy", "Read a strategy .py file's source", json!({
            "type": "object", "properties": {"path": {"type": "string"}},
            "required": ["path"] })),
        ("save_strategy", "Create a strategy .py file (returns its path). Refuses to replace an existing file unless overwrite=true.", json!({
            "type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
                "overwrite": {"type": "boolean", "default": false} },
            "required": ["path", "content"] })),
        ("run_backtest", "Backtest a strategy file on a symbol; returns metrics and equity summary", json!({
            "type": "object", "properties": {
                "path": {"type": "string"},
                "symbol": {"type": "string"},
                "timeframe": {"type": "string", "default": "d1"},
                "from": {"type": "string"}, "to": {"type": "string"},
                "quantity": {"type": "number", "default": 10},
                "initial_capital": {"type": "number", "default": 100000} },
            "required": ["path", "symbol"] })),
        ("export_bars_csv", "Export historical bars (time, open, high, low, close, volume only) to a CSV under data/exports and return its path relative to the app folder and its sha256. The file name carries the first 12 hex characters of the sha256; an export is never overwritten (the same bars reuse the same read-only file, different bars make a new file). The file has no label or feature columns, so it cannot be trained on directly: use build_dataset for training", json!({
            "type": "object", "properties": {
                "symbol": {"type": "string"},
                "timeframe": {"type": "string", "default": "d1"},
                "limit": {"type": "integer", "default": 500} },
            "required": ["symbol"] })),
        ("list_ml_models", "List trained ML models with metrics", json!({
            "type": "object", "properties": {} })),
        ("train_ml_model", "Train an ML model with time-series cross-validation. Best path: build_dataset, then train_ml_model {algorithm, dataset}. Returns model_id, metrics, trading result against buy and hold, and flags for weak spots; read get_model for per-fold detail. Use holdout_pct to also score a final untouched stretch. Alternatively give data_path, target_column and feature_columns for a CSV you already have with a label column. list_algorithms shows every algorithm with its hyperparameters. algorithm `custom` trains the Python estimator file estimator_file and needs the user's approval. algorithm `recipe` trains a saved algorithm recipe (list_recipes) named in `recipe`; its parameters live in the recipe, so hyperparams is ignored, and it needs no extra approval.", json!({
            "type": "object", "properties": {
                "algorithm": {"type": "string", "enum": train_algorithm_ids()},
                "recipe": {"type": "string", "description": "with algorithm recipe: the name of a saved recipe from list_recipes / save_recipe"},
                "dataset": {"type": "string", "description": "name of a dataset from build_dataset; supplies data_path, target and features"},
                "feature_columns": {"type": "array", "items": {"type": "string"}, "description": "optional subset of the dataset's features; required with data_path"},
                "hyperparams": {"type": "object", "description": "constructor parameters of the algorithm, by name (see list_algorithms for the names and defaults); out-of-range values are rejected with the allowed range. For `custom` they are passed to make_estimator(params)"},
                "n_splits": {"type": "integer", "default": 5, "description": "time-series CV folds, 2-20"},
                "trade_mode": {"type": "string", "enum": ["long_flat", "long_short"], "default": "long_flat", "description": "how the evaluation trades the predictions. long_short also shorts the bearish class (direction 0, return_threshold -1); only choose it for a model meant for an automation with allow_short"},
                "holdout_pct": {"type": "number", "description": "0-40: share of the final time steps kept out of cross-validation and scored once"},
                "random_state": {"type": "integer", "description": "seed for estimators that take one (default 42)"},
                "estimator_file": {"type": "string", "description": "with algorithm custom: a file from save_estimator_file, e.g. centroid.py"},
                "lag_periods": {"type": "array", "items": {"type": "integer"}, "description": "add lagged copies of each feature (1-250). Default none with a dataset, [1] with data_path"},
                "return_column": {"type": "string", "description": "column holding each row's forward return, used for the trading result; set automatically for datasets"},
                "data_path": {"type": "string", "description": "CSV path (not needed with dataset)"},
                "target_column": {"type": "string", "description": "label column (not needed with dataset)"} },
            "required": ["algorithm"] })),
        ("get_model_signal", "Score a trained ML model on a symbol's latest bar — returns {signal, probability}. signal is the model's predicted class: 1 = up, 0 = no clear move or down (2-class models), -1 = down (3-class return_threshold models)", json!({
            "type": "object", "properties": {
                "model_id": {"type": "string"},
                "symbol": {"type": "string"} },
            "required": ["model_id", "symbol"] })),
        ("get_positions", "Current open positions (paper + connected brokers)", json!({
            "type": "object", "properties": {} })),
        ("get_open_orders", "Resting (unfilled) orders", json!({
            "type": "object", "properties": {} })),
        ("list_orders", "Recent order history, newest first", json!({
            "type": "object", "properties": {
                "symbol": {"type": "string"},
                "limit": {"type": "integer", "default": 25} } })),
        ("place_order", "Place an order via the risk-gated order manager. Defaults to the paper broker — live brokers are refused unless [automations] allow_live_trading = true", json!({
            "type": "object", "properties": {
                "symbol": {"type": "string"},
                "side": {"type": "string", "enum": ["buy", "sell"]},
                "quantity": {"type": "number"},
                "order_type": {"type": "string", "default": "market"},
                "price": {"type": "number"},
                "stop_price": {"type": "number"},
                "broker_id": {"type": "string", "default": "paper"} },
            "required": ["symbol", "side", "quantity"] })),
        ("cancel_order", "Cancel a resting order", json!({
            "type": "object", "properties": {
                "order_id": {"type": "string"},
                "broker_id": {"type": "string", "default": "paper"} },
            "required": ["order_id"] })),
        ("create_automation", "Create a persisted scheduled rule: run model_id's signal on symbol every interval_secs and place quantity via broker_id when probability >= threshold. Defaults to paper. By default it only goes long or flat. allow_short=true lets it sell short on the bearish class; it is refused when the broker cannot short, and on a live broker it needs the user's approval.", json!({
            "type": "object", "properties": {
                "name": {"type": "string"},
                "symbol": {"type": "string"},
                "model_id": {"type": "string"},
                "interval_secs": {"type": "integer", "default": 300},
                "quantity": {"type": "number", "default": 1},
                "threshold": {"type": "number", "default": 0.5},
                "broker_id": {"type": "string", "default": "paper"},
                "allow_short": {"type": "boolean", "default": false, "description": "let the rule go short (sell what it does not own) on the model's bearish class. Opt-in: leave false unless the user asked for shorting; try paper first"} },
            "required": ["name", "symbol", "model_id"] })),
        ("list_automations", "List all automation rules with last-run status", json!({
            "type": "object", "properties": {} })),
        ("set_automation_enabled", "Enable or pause an automation rule", json!({
            "type": "object", "properties": {
                "id": {"type": "string"},
                "enabled": {"type": "boolean"} },
            "required": ["id", "enabled"] })),
        ("delete_automation", "Permanently delete an automation rule", json!({
            "type": "object", "properties": {
                "id": {"type": "string"} },
            "required": ["id"] })),
        ("create_alert", "Create an in-app alert on a symbol", json!({
            "type": "object", "properties": {
                "symbol": {"type": "string"},
                "condition": {"type": "string", "enum": ["price_above", "price_below", "pct_change", "vwap_cross"]},
                "threshold": {"type": "number"},
                "pct": {"type": "number"},
                "window_secs": {"type": "integer", "default": 300} },
            "required": ["symbol", "condition"] })),
        ("list_alerts", "List configured alert rules", json!({
            "type": "object", "properties": {} })),
        ("remove_alert", "Remove an alert rule", json!({
            "type": "object", "properties": {
                "id": {"type": "string"} },
            "required": ["id"] })),
    ];
    let mut all: Vec<Value> = tools
        .iter()
        .map(|(name, desc, params)| {
            json!({
                "type": "function",
                "function": {"name": name, "description": desc, "parameters": params},
            })
        })
        .collect();
    all.extend(copilot_tools::extra_tool_schemas());
    Value::Array(all)
}

fn parse_timeframe(s: &str) -> Timeframe {
    match s.to_lowercase().as_str() {
        "1m" | "m1" => Timeframe::M1,
        "5m" | "m5" => Timeframe::M5,
        "15m" | "m15" => Timeframe::M15,
        "1h" | "h1" => Timeframe::H1,
        "4h" | "h4" => Timeframe::H4,
        "1w" | "w1" => Timeframe::W1,
        _ => Timeframe::D1,
    }
}

/// An export written by [`write_export`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExportFile {
    pub path: std::path::PathBuf,
    /// SHA-256 of the file content, 64 hex characters.
    pub sha256: String,
    /// The same content was already there, so nothing was written.
    pub reused: bool,
}

fn export_part(text: &str) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).take(64).collect();
    // A name made only of dots would be `.` or `..` once joined.
    if cleaned.chars().all(|c| c == '.') {
        "_".repeat(cleaned.len().max(1))
    } else {
        cleaned
    }
}

/// Write `content` to `<dir>/<symbol>_<timeframe>_<first 12 hex of sha256>.csv`, never replacing a file.
/// Identical content reuses the existing file. A file of that name with different content is an
/// error (a 48-bit prefix collision), never an overwrite. The file is made read-only (0444). The
/// content is written to a temporary file and linked into place, so a reader never sees a partial file.
pub(crate) fn write_export(dir: &std::path::Path, symbol: &str, timeframe: &str, content: &str) -> Result<ExportFile, String> {
    let sha256 = crate::ledger::Ledger::sha256_hex(content.as_bytes());
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create the exports folder: {e}"))?;
    let path = dir.join(format!("{}_{}_{}.csv", export_part(symbol), export_part(timeframe), &sha256[..12]));
    let same_content = |path: &std::path::Path| -> Result<bool, String> {
        std::fs::read(path).map(|existing| existing == content.as_bytes()).map_err(|e| format!("could not read the existing export {}: {e}", path.display()))
    };
    let read_only = |path: &std::path::Path| {
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path, perms)
    };
    let reuse = |path: &std::path::Path| -> Result<ExportFile, String> {
        if !same_content(path)? {
            return Err(format!(
                "{} already exists with different content (a hash-prefix collision); it was not overwritten. Export with a different limit or timeframe.",
                path.display()
            ));
        }
        let _ = read_only(path);
        Ok(ExportFile { path: path.to_path_buf(), sha256: sha256.clone(), reused: true })
    };
    if path.exists() {
        return reuse(&path);
    }
    let tmp = dir.join(format!(".export-{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        read_only(&tmp)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not write the export: {e}"));
    }
    // `hard_link` fails when the name exists, so a file that appeared meanwhile is never replaced.
    let linked = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(ExportFile { path, sha256, reused: false }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => reuse(&path),
        Err(e) => Err(format!("could not store the export {}: {e}", path.display())),
    }
}

fn clamp_limit(v: Option<&Value>, default: usize, max: usize) -> usize {
    v.and_then(|x| x.as_u64())
        .map(|n| (n as usize).clamp(1, max))
        .unwrap_or(default)
}

/// An optional string argument; present but not a string is an error, not silently dropped.
fn optional_str(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn optional_number(args: &Value, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| format!("`{key}` must be a number")),
    }
}

fn optional_integer(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or_else(|| format!("`{key}` must be a whole number")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing `{key}`"))
}

/// Execute one agentic tool against live terminal state. Returns a compact
/// JSON payload (or an `{"error": ...}` payload — tool failures are fed back
/// to the model so it can retry with different arguments).
async fn exec_tool(
    name: &str,
    args: &Value,
    state: &AppState,
    models: &ModelRegistry,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<Value, String> {
    if let Some(result) = copilot_tools::exec_extra(name, args, state, runtime_paths).await {
        return result;
    }
    if let Some(result) = copilot_more::exec_ml(name, args, state, &models.models_dir, runtime_paths).await {
        return result;
    }
    match name {
        "get_quote" => {
            let sym = arg_str(args, "symbol")?.to_uppercase();
            match state.aggregator.get_cached_quote(&sym) {
                Some(q) => Ok(json!({
                    "symbol": sym, "last": q.last, "change_pct": q.change_pct,
                    "volume": q.volume, "high": q.high, "low": q.low,
                })),
                None => Err(format!(
                    "no cached quote for {sym} — add it to the watchlist"
                )),
            }
        }
        "get_ohlcv" => {
            let sym = arg_str(args, "symbol")?;
            let tf = args.get("timeframe").and_then(|v| v.as_str());
            let limit = clamp_limit(args.get("limit"), 60, 300);
            let bars = data::load_bars(state, sym, tf, limit).await?;
            let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
            Ok(json!({
                "symbol": sym, "bars": bars.len(),
                "first": bars.first().map(|b| b.time.to_rfc3339()),
                "last": bars.last().map(|b| b.time.to_rfc3339()),
                "closes": closes.iter().rev().take(limit).rev().collect::<Vec<_>>(),
            }))
        }
        "get_quant_stats" => {
            let sym = arg_str(args, "symbol")?;
            let tf = args.get("timeframe").and_then(|v| v.as_str());
            let bars = data::load_bars(state, sym, tf, 800).await?;
            if bars.len() < 30 {
                return Err(format!("only {} bars for {sym} — need 30+", bars.len()));
            }
            let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
            let s = q::summarize(&closes, quant::periods_per_year(&bars));
            Ok(serde_json::to_value(&s).map_err(|e| e.to_string())?)
        }
        "get_regression" => {
            let x = arg_str(args, "x_symbol")?;
            let y = arg_str(args, "y_symbol")?;
            let tf = args.get("timeframe").and_then(|v| v.as_str());
            let r = quant::regression_inner(x.to_string(), y.to_string(), tf, state).await?;
            Ok(json!({"x": r.x_symbol, "y": r.y_symbol, "n": r.n,
                      "alpha": r.alpha, "beta": r.beta, "r_squared": r.r_squared}))
        }
        "run_scan" => {
            let criteria_raw = args
                .get("criteria")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let criteria: Vec<_> = criteria_raw
                .iter()
                .map(|c| {
                    scanner::to_criterion(
                        &serde_json::from_value::<ScanCriterionDto>(c.clone())
                            .map_err(|e| format!("bad criterion: {e}"))?,
                    )
                })
                .collect::<Result<_, String>>()?;
            if criteria.is_empty() {
                return Err("criteria must be a non-empty array".into());
            }
            let universe: Vec<Symbol> = match args.get("symbols").and_then(|v| v.as_array()) {
                Some(list) if !list.is_empty() => list
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| Symbol(s.to_uppercase())))
                    .collect(),
                _ => state
                    .aggregator
                    .quote_cache()
                    .iter()
                    .map(|e| Symbol(e.key().clone()))
                    .collect(),
            };
            if universe.is_empty() {
                return Err("no symbols in universe — add symbols to the watchlist".into());
            }
            let tf = parse_timeframe(
                args.get("timeframe")
                    .and_then(|v| v.as_str())
                    .unwrap_or("d1"),
            );
            let out = state
                .scanner
                .run_scan(&apex_core::application::scanner::ScanConfig {
                    name: "copilot scan".into(),
                    universe,
                    criteria,
                    timeframe: tf,
                    lookback_bars: 60,
                })
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "matched": out.results.len(),
                "results": out.results.iter().take(25).map(|r| json!({
                    "symbol": r.symbol.0,
                    "last": r.last_price,
                    "change_pct": r.change_pct,
                })).collect::<Vec<_>>(),
            }))
        }
        "get_news" => {
            let limit = clamp_limit(args.get("limit"), 8, 25);
            let items = match args.get("symbol").and_then(|v| v.as_str()) {
                Some(sym) => state
                    .news
                    .get_news_for_symbol(&Symbol(sym.to_uppercase()), limit),
                None => state.news.latest_news(limit),
            };
            Ok(
                json!({"count": items.len(), "items": items.iter().map(|i| json!({
                "headline": i.headline, "source": i.source,
            })).collect::<Vec<_>>()}),
            )
        }
        "list_strategies" => {
            let files = strategy::list_strategy_files_inner(runtime_paths)?;
            Ok(json!({"files": files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()}))
        }
        "read_strategy" => {
            let path = arg_str(args, "path")?;
            let full = strategy::normalize_strategy_path(path, runtime_paths)?;
            let content =
                std::fs::read_to_string(&full).map_err(|e| format!("cannot read {path}: {e}"))?;
            let total = content.chars().count();
            let truncated = total > 12000;
            Ok(json!({
                "path": path,
                "content": content.chars().take(12000).collect::<String>(),
                "total_chars": total,
                "truncated": truncated,
                "note": if truncated {
                    "Only the first 12000 characters are shown. Do NOT overwrite this file with rewritten content: you have not seen all of it."
                } else { "" },
            }))
        }
        "save_strategy" => {
            let path = arg_str(args, "path")?;
            let content = arg_str(args, "content")?;
            let full = strategy::normalize_strategy_path(path, runtime_paths)?;
            let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);
            if full.exists() && !overwrite {
                return Err(format!(
                    "{path} already exists; read it first, then pass overwrite=true to replace it"
                ));
            }
            // Every save is a new version of the same strategy in the audit ledger (actor "copilot"):
            // the previous content stays in the history, so no backup copy is made and nothing is lost.
            strategy::save_strategy_as(runtime_paths, path, content.to_string(), "copilot")
                .map_err(|e| format!("cannot write {path}: {e}"))?;
            // The user approved this exact content, so running it needs no second approval.
            state
                .copilot_trusted_files
                .lock()
                .unwrap()
                .insert(path.to_string(), copilot_tools::content_hash(content));
            Ok(json!({"path": path, "saved": true, "hash": copilot_tools::content_hash(content)}))
        }
        "run_backtest" => {
            let req = StrategyBacktestRequestDto {
                path: arg_str(args, "path")?.to_string(),
                symbol: arg_str(args, "symbol")?.to_uppercase(),
                timeframe: args
                    .get("timeframe")
                    .and_then(|v| v.as_str())
                    .unwrap_or("d1")
                    .to_string(),
                from: args
                    .get("from")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        (chrono::Utc::now() - chrono::Duration::days(730))
                            .format("%Y-%m-%d")
                            .to_string()
                    }),
                to: args
                    .get("to")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string()),
                initial_capital: args.get("initial_capital").and_then(|v| v.as_f64()),
                commission_bps: args.get("commission_bps").and_then(|v| v.as_f64()),
                slippage_bps: args.get("slippage_bps").and_then(|v| v.as_f64()),
                quantity: args.get("quantity").and_then(|v| v.as_f64()),
            };
            let r = strategy::run_backtest_inner(req, state, runtime_paths).await?;
            Ok(json!({
                "strategy": r.strategy_name, "inferred": r.inferred_strategy,
                "symbol": r.symbol, "bars": r.bars_analyzed,
                "metrics": r.metrics,
                "trades": r.trades.len(),
                "equity_final": r.equity_curve.last().map(|p| p.equity),
                "notes": r.notes,
            }))
        }
        "export_bars_csv" => {
            let sym = arg_str(args, "symbol")?;
            let tf = args
                .get("timeframe")
                .and_then(|v| v.as_str())
                .unwrap_or("d1");
            let limit = clamp_limit(args.get("limit"), 500, 2000);
            let bars = data::load_bars(state, sym, Some(tf), limit).await?;
            if bars.is_empty() {
                return Err(format!("no bars for {sym}"));
            }
            let dir = runtime_paths.data_dir().join("exports");
            let mut out = String::from("time,open,high,low,close,volume\n");
            for b in &bars {
                out.push_str(&format!(
                    "{},{},{},{},{},{}\n",
                    b.time.format("%Y-%m-%dT%H:%M:%SZ"),
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    b.volume
                ));
            }
            // Content-addressed and write-once: a model that trained on an earlier export keeps the
            // exact file it saw. The same bars reuse the same file.
            let file = write_export(&dir, sym, tf, &out)?;
            // Relative to the work root, the base the trainer and strategy files resolve against.
            let rel = runtime_paths.to_user_relative_path(&file.path);
            Ok(json!({"path": rel, "sha256": file.sha256, "bars": bars.len(), "reused_existing_file": file.reused}))
        }
        "list_ml_models" => {
            let models = ml::load_registered_models(&models.models_dir)?;
            Ok(
                json!({"count": models.len(), "models": models.iter().map(|m| json!({
                "id": m.model_id, "algorithm": m.algorithm,
                "metrics": m.metrics, "target": m.target_column,
            })).collect::<Vec<_>>()}),
            )
        }
        "train_ml_model" => {
            let dataset = args.get("dataset").and_then(|v| v.as_str()).map(String::from);
            let hyperparams = match args.get("hyperparams") {
                None | Some(Value::Null) => Default::default(),
                Some(v @ Value::Object(_)) => serde_json::from_value(v.clone()).map_err(|e| format!("invalid hyperparams: {e}"))?,
                Some(_) => return Err("`hyperparams` must be an object".into()),
            };
            let strings = |key: &str| -> Vec<String> {
                args.get(key)
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default()
            };
            let text = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let algorithm = arg_str(args, "algorithm")?.to_string();
            let recipe = optional_str(args, "recipe")?;
            if algorithm == ml_estimators::RECIPE_ALGORITHM {
                if recipe.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err("algorithm `recipe` needs `recipe`, the name of a saved recipe; see list_recipes".into());
                }
            } else if recipe.is_some() {
                return Err("`recipe` is only used with algorithm `recipe`".into());
            }
            let hyperparams = if algorithm == ml_estimators::RECIPE_ALGORITHM { Default::default() } else { hyperparams };
            let req = MLTrainingRequestDto {
                algorithm,
                data_path: text("data_path"),
                target_column: text("target_column"),
                feature_columns: strings("feature_columns"),
                hyperparams,
                n_splits: clamp_limit(args.get("n_splits"), 5, 20),
                // A dataset already holds its features; a raw CSV keeps the old one-lag default.
                lag_periods: args
                    .get("lag_periods")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
                    .unwrap_or_else(|| if dataset.is_some() { Vec::new() } else { vec![1] }),
                dataset,
                return_column: args.get("return_column").and_then(|v| v.as_str()).map(String::from),
                trade_mode: optional_str(args, "trade_mode")?,
                holdout_pct: optional_number(args, "holdout_pct")?,
                estimator_file: optional_str(args, "estimator_file")?,
                recipe: recipe.map(|r| r.trim().to_string()),
                random_state: optional_integer(args, "random_state")?,
                ..Default::default()
            };
            if req.algorithm == "custom" {
                // Reaching here means the user approved this run or already approved this exact
                // file content (see `copilot_mode::requires_approval`). Remember it, so the same
                // content does not ask again; an edit changes the hash and asks once more.
                copilot_tools::trust_estimator(state, args, runtime_paths);
            }
            let r =
                ml::train_ml_model_inner(req, &models.models_dir, runtime_paths, state, "copilot")
                    .await?;
            let detail = ml::build_detail(&models.models_dir, &r.model_id, Vec::new()).ok();
            Ok(copilot_more::train_summary(&r, detail.as_ref()))
        }
        "get_model_signal" => {
            ml::ensure_fresh_model_bars(
                state,
                &models.models_dir,
                arg_str(args, "model_id")?,
                arg_str(args, "symbol")?,
            )
            .await?;
            let s = ml::model_signal_inner(
                arg_str(args, "model_id")?,
                arg_str(args, "symbol")?,
                &models.models_dir,
                runtime_paths,
                &state.storage,
            )
            .await?;
            Ok(json!({"model_id": s.model_id, "symbol": s.symbol,
                      "signal": s.signal, "probability": s.probability}))
        }
        "get_positions" => {
            let positions = state.otm.get_positions();
            Ok(
                json!({"count": positions.len(), "positions": positions.iter().map(|p| {
                json!({"symbol": p.symbol.0, "quantity": p.quantity,
                       "avg_price": p.avg_price, "pnl": p.pnl, "pnl_pct": p.pnl_pct})
            }).collect::<Vec<_>>()}),
            )
        }
        "get_open_orders" => {
            let orders = state.otm.open_orders();
            Ok(
                json!({"count": orders.len(), "orders": orders.iter().map(|o| {
                json!({"id": o.id.0, "symbol": o.symbol.0, "side": format!("{:?}", o.side),
                       "quantity": o.quantity, "price": o.price, "status": format!("{:?}", o.status)})
            }).collect::<Vec<_>>()}),
            )
        }
        "list_orders" => {
            let limit = clamp_limit(args.get("limit"), 25, 100);
            let symbol = args.get("symbol").and_then(|v| v.as_str());
            if let Some(s) = symbol {
                validation::validate_symbol(s)?;
            }
            let mut orders = state
                .storage
                .query_orders(OrderQuery {
                    symbol: symbol.map(|s| Symbol(s.to_uppercase())),
                    status: None,
                    broker_id: None,
                    from: None,
                    to: None,
                    limit: Some(limit),
                })
                .await
                .map_err(|e| format!("order query failed: {e}"))?;
            for open in state.otm.open_orders() {
                if !orders.iter().any(|o| o.id == open.id) {
                    orders.push(open);
                }
            }
            orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            orders.truncate(limit);
            Ok(
                json!({"count": orders.len(), "orders": orders.iter().map(|o| {
                json!({"id": o.id.0, "symbol": o.symbol.0, "side": format!("{:?}", o.side),
                       "quantity": o.quantity, "price": o.price,
                       "status": format!("{:?}", o.status),
                       "broker_id": o.broker_id})
            }).collect::<Vec<_>>()}),
            )
        }
        "place_order" => {
            let symbol = arg_str(args, "symbol")?.to_uppercase();
            let side_s = arg_str(args, "side")?.to_lowercase();
            let side = match side_s.as_str() {
                "buy" => OrderSide::Buy,
                "sell" => OrderSide::Sell,
                _ => return Err(format!("invalid side `{side_s}`")),
            };
            let quantity = args
                .get("quantity")
                .and_then(|v| v.as_f64())
                .ok_or("missing `quantity`")?;
            validation::validate_symbol(&symbol)?;
            validation::validate_quantity(quantity)?;
            validation::validate_price(args.get("price").and_then(|v| v.as_f64()))?;
            validation::validate_price(args.get("stop_price").and_then(|v| v.as_f64()))?;

            let broker_id = args
                .get("broker_id")
                .and_then(|v| v.as_str())
                .unwrap_or("paper")
                .to_string();
            automation::check_broker_allowed(state, &broker_id)?;

            let order_type = match args
                .get("order_type")
                .and_then(|v| v.as_str())
                .unwrap_or("market")
                .to_lowercase()
                .as_str()
            {
                "market" => OrderType::Market,
                "limit" => OrderType::Limit,
                "stop" => OrderType::Stop,
                "stoplimit" | "stop_limit" => OrderType::StopLimit,
                other => return Err(format!("invalid order_type `{other}`")),
            };

            if broker_id == "paper" && order_type != OrderType::Market {
                state.aggregator.start(&[Symbol(symbol.clone())]).await
                    .map_err(|e| format!("Could not subscribe to market data for paper order: {e}"))?;
            }
            let id = state
                .guarded_submit_with(
                    NewOrderRequest {
                        symbol: Symbol(symbol.clone()),
                        side,
                        order_type,
                        quantity,
                        price: args.get("price").and_then(|v| v.as_f64()),
                        stop_price: args.get("stop_price").and_then(|v| v.as_f64()),
                        tag: Some("copilot".into()),
                        broker_params: None,
                    },
                    &broker_id,
                    crate::state::LiveAuth::Unattended,
                    // The audit trail ties this order to the Copilot turn that produced it.
                    Some(crate::order_audit::copilot_source(&copilot_tools::current_turn(state))),
                )
                .await?;
            Ok(json!({"order_id": id.0, "symbol": symbol, "broker_id": broker_id}))
        }
        "cancel_order" => {
            let order_id = arg_str(args, "order_id")?;
            let broker_id = args
                .get("broker_id")
                .and_then(|v| v.as_str())
                .unwrap_or("paper");
            validation::validate_broker_id(broker_id)?;
            let source = crate::order_audit::copilot_source(&copilot_tools::current_turn(state));
            state
                .otm
                .cancel_order_with(
                    &OrderId(order_id.to_string()),
                    broker_id,
                    crate::order_audit::AuditContext::new("copilot", Some(source)),
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({"cancelled": order_id}))
        }
        "create_automation" => {
            let dto = automation::CreateAutomationDto {
                name: arg_str(args, "name")?.to_string(),
                kind: "model_signal".into(),
                symbol: arg_str(args, "symbol")?.to_string(),
                model_id: arg_str(args, "model_id")?.to_string(),
                interval_secs: args
                    .get("interval_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300),
                quantity: args.get("quantity").and_then(|v| v.as_f64()).unwrap_or(1.0),
                threshold: args
                    .get("threshold")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5),
                broker_id: args
                    .get("broker_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("paper")
                    .to_string(),
                // Shorting is opt-in: only when the call asks for it. A live broker is gated by
                // `copilot_mode::live_involved`, and `create_automation_inner` refuses a broker
                // that cannot short.
                allow_short: copilot_tools::wants_short(args),
            };
            let created = automation::create_automation_inner(dto, state)?;
            Ok(json!({"created": created.id, "name": created.name,
                      "broker_id": created.broker_id, "interval_secs": created.interval_secs,
                      "allow_short": created.allow_short}))
        }
        "list_automations" => {
            let list: Vec<automation::AutomationDto> = state
                .automations
                .list()
                .iter()
                .map(automation::AutomationDto::from)
                .collect();
            Ok(serde_json::to_value(&list).map_err(|e| e.to_string())?)
        }
        "set_automation_enabled" => {
            let id = arg_str(args, "id")?;
            let enabled = args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .ok_or("missing `enabled`")?;
            // The rule's new version is attributed to the Copilot.
            if !apex_core::application::automation::with_actor("copilot", || state.automations.set_enabled(id, enabled)) {
                return Err(format!("no automation `{id}`"));
            }
            Ok(json!({"id": id, "enabled": enabled}))
        }
        "delete_automation" => {
            let id = arg_str(args, "id")?;
            // Deleting a rule is a tombstone version; the rule's history stays and it can be restored.
            if !apex_core::application::automation::with_actor("copilot", || state.automations.remove_rule(id)) {
                return Err(format!("no automation `{id}`"));
            }
            Ok(json!({"deleted": id}))
        }
        "create_alert" => {
            use apex_core::application::alert_engine::{AlertDelivery, AlertRule, StoredAlert};
            let symbol = arg_str(args, "symbol")?.to_uppercase();
            let rule = match arg_str(args, "condition")? {
                "price_above" => AlertRule::PriceAbove {
                    symbol: symbol.clone(),
                    threshold: args
                        .get("threshold")
                        .and_then(|v| v.as_f64())
                        .ok_or("missing `threshold`")?,
                },
                "price_below" => AlertRule::PriceBelow {
                    symbol: symbol.clone(),
                    threshold: args
                        .get("threshold")
                        .and_then(|v| v.as_f64())
                        .ok_or("missing `threshold`")?,
                },
                "pct_change" => {
                    let window_secs = args
                        .get("window_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(300);
                    if window_secs > apex_core::application::alert_engine::MAX_WINDOW_SECS {
                        return Err(format!(
                            "window_secs exceeds the maximum of {} seconds (~366 days)",
                            apex_core::application::alert_engine::MAX_WINDOW_SECS
                        ));
                    }
                    AlertRule::PctChange {
                        symbol: symbol.clone(),
                        pct: args
                            .get("pct")
                            .and_then(|v| v.as_f64())
                            .ok_or("missing `pct`")?,
                        window_secs,
                    }
                }
                "vwap_cross" => AlertRule::VwapCross {
                    symbol: symbol.clone(),
                },
                other => return Err(format!("invalid condition `{other}`")),
            };
            let id = uuid::Uuid::new_v4().to_string();
            // The same audited path as the UI: versioned in the ledger first, actor "copilot".
            // The activity entry for this call is written by the tool-call logger (`alert.create`).
            state
                .alert_store
                .add(
                    StoredAlert {
                        id: id.clone(),
                        rule,
                        delivery: vec![AlertDelivery::InApp],
                        enabled: true,
                    },
                    "copilot",
                )
                .await?;
            Ok(json!({"alert_id": id, "symbol": symbol}))
        }
        "list_alerts" => {
            let rules = state.alert_store.list().await?;
            Ok(
                json!({"count": rules.len(), "alerts": rules.iter().map(|r| {
                json!({"id": r.id, "enabled": r.enabled,
                       "rule": serde_json::to_value(&r.rule).unwrap_or_default()})
            }).collect::<Vec<_>>()}),
            )
        }
        "remove_alert" => {
            let id = arg_str(args, "id")?;
            // A delete marker in the ledger (actor "copilot"); the tool-call logger writes `alert.delete`.
            if !state.alert_store.remove(id, "copilot").await? {
                return Err(format!("no alert `{id}`"));
            }
            Ok(json!({"removed": id}))
        }
        other => Err(format!("unknown tool `{other}`")),
    }
}

/// Describe a tool call for the approval card and trace. Shows every detail the
/// user is agreeing to (side, quantity, price, broker, ...), not just the symbol.
fn trace_detail(name: &str, args: &Value) -> String {
    copilot_tools::describe_call(name, args)
}

// ── Chat-completions tool loop ──────────────────────────────────────

async fn chat_round(
    http: &reqwest::Client,
    provider: &ResolvedProvider,
    messages: &[Value],
    tools: &Value,
) -> Result<Value, String> {
    let resp = http
        .post(format!("{}/chat/completions", provider.base_url))
        .bearer_auth(&provider.api_key)
        .header("HTTP-Referer", "https://apex.local")
        .header("X-Title", "APEX Terminal")
        .json(&json!({
            "model": provider.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "max_tokens": provider.max_tokens,
        }))
        .send()
        .await
        .map_err(|e| format!("{} request failed: {e}", provider.id))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "{} error {}: {}",
            provider.id,
            status,
            text.chars().take(300).collect::<String>()
        ));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("failed to parse {} response: {e}", provider.id))
}

// ── Responses-API tool loop ─────────────────────────────────────────
// /responses uses flat tool defs and output items instead of messages.

fn chat_tools_to_responses(tools: &Value) -> Value {
    tools
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|t| {
                    let f = &t["function"];
                    json!({"type": "function", "name": f["name"],
                           "description": f["description"], "parameters": f["parameters"]})
                })
                .collect::<Vec<_>>()
        })
        .map(Value::Array)
        .unwrap_or(Value::Array(vec![]))
}

async fn responses_round(
    http: &reqwest::Client,
    provider: &ResolvedProvider,
    input: &[Value],
    tools: &Value,
) -> Result<Value, String> {
    let resp = http
        .post(format!("{}/responses", provider.base_url))
        .bearer_auth(&provider.api_key)
        .header("HTTP-Referer", "https://apex.local")
        .header("X-Title", "APEX Terminal")
        .json(&json!({
            "model": provider.model,
            "input": input,
            "tools": tools,
            "max_output_tokens": provider.max_tokens,
        }))
        .send()
        .await
        .map_err(|e| format!("{} request failed: {e}", provider.id))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "{} error {}: {}",
            provider.id,
            status,
            text.chars().take(300).collect::<String>()
        ));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("failed to parse {} response: {e}", provider.id))
}


/// The text shown for a finished turn. A turn that ran out of rounds says so, because a long
/// Agent task that just stops looks like a hang.
fn final_text(reply_text: String, awaiting_approval: bool, finished: bool, mode: ChatMode) -> String {
    if awaiting_approval {
        return "Paused — approve the pending tool call(s) to continue.".into();
    }
    if finished {
        return if reply_text.is_empty() { "(empty response)".into() } else { reply_text };
    }
    let note = format!(
        "Stopped after {} steps. Send \"continue\" and I will pick up where I left off.",
        mode.max_rounds()
    );
    if reply_text.is_empty() { note } else { format!("{reply_text}\n\n({note})") }
}

/// Chat with the AI copilot. Resolves the configured provider
/// ([[llm.providers]] or legacy [copilot]), then runs an agentic
/// tool-call loop that can read live quotes, run scans/backtests,
/// write strategies, and train models — feeding results back so the
/// model can iterate (build → test → optimise).
#[tauri::command]
pub async fn copilot_chat(
    message: String,
    history: Option<Vec<CopilotMessage>>,
    turn_id: Option<String>,
    mode: Option<String>,
    state: State<'_, AppState>,
    models: State<'_, ModelRegistry>,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
) -> Result<CopilotReply, String> {
    validation::validate_string_length(&message, "message")?;
    // Approvals and cached tool results belong to one user turn. The UI re-sends
    // the same turn id after an approval, so a later message that makes the same
    // call (say, buying again) is a new turn and must not hit the old cache.
    if let Some(turn) = &turn_id {
        let mut current = state.copilot_turn.lock().unwrap();
        if current.as_ref() != Some(turn) {
            *current = Some(turn.clone());
            state.copilot_tool_cache.lock().unwrap().clear();
            state.copilot_approvals.lock().unwrap().clear();
        }
    }
    let mode = ChatMode::resolve(mode.as_deref(), state.copilot.require_approval)?;
    let provider = resolve_provider(&state)?;

    // ACP providers hand the prompt to an external agent process.
    if provider.api_kind == "acp" {
        let reply = crate::commands::acp::acp_prompt(
            state.inner(),
            &message,
            history.as_deref().unwrap_or(&[]),
        )
        .await?;
        return Ok(CopilotReply {
            reply,
            model: provider.model,
            provider: provider.id,
            mode: mode.as_str().to_string(),
            tool_calls: vec![],
            pending_approvals: vec![],
        });
    }

    let system_prompt = copilot_tools::system_prompt(&state, &runtime_paths, mode);

    let mut trace: Vec<ToolCallTrace> = Vec::new();
    let state_ref = state.inner();
    let models_ref = models.inner();
    let paths_ref = runtime_paths.inner();

    match provider.api_kind.as_str() {
        "responses" | "chatgpt" => {
            let tools = chat_tools_to_responses(&copilot_mode::tools_for_mode(tool_schemas(), mode));
            let mut input: Vec<Value> = vec![json!({
                "role": "developer",
                "content": [{"type": "input_text", "text": system_prompt}],
            })];
            if let Some(history) = &history {
                for m in history.iter().rev().take(16).rev() {
                    let role = if m.role == "assistant" {
                        "assistant"
                    } else {
                        "user"
                    };
                    let kind = if role == "assistant" {
                        "output_text"
                    } else {
                        "input_text"
                    };
                    input.push(json!({"role": role,
                        "content": [{"type": kind, "text": m.content}]}));
                }
            }
            input.push(json!({"role": "user",
                "content": [{"type": "input_text", "text": message}]}));

            let mut reply_text = String::new();
            let mut model_name = provider.model.clone();
            let mut pending_approvals: Vec<PendingApprovalDto> = Vec::new();
            let mut finished = false;
            'rounds: for _ in 0..mode.max_rounds() {
                let resp = if provider.api_kind == "chatgpt" {
                    crate::chatgpt_auth::responses_round(&state, &provider.model, &input, &tools).await?
                } else {
                    responses_round(&state.http, &provider, &input, &tools).await?
                };
                if let Some(m) = resp.get("model").and_then(|v| v.as_str()) {
                    model_name = m.to_string();
                }
                let output = resp
                    .get("output")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let calls: Vec<&Value> = output
                    .iter()
                    .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("function_call"))
                    .collect();
                // Collect final text from message/output_text items.
                for o in &output {
                    if o.get("type").and_then(|t| t.as_str()) == Some("message") {
                        if let Some(content) = o.get("content").and_then(|c| c.as_array()) {
                            reply_text = content
                                .iter()
                                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n");
                        }
                    }
                }
                if calls.is_empty() {
                    finished = true;
                    break;
                }
                let mut awaiting = false;
                for call in calls {
                    input.push(call.clone());
                    let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args: Value = call
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(json!({}));
                    let call_id = call
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let Some(payload) = dispatch_tool(
                        name,
                        &args,
                        state_ref,
                        models_ref,
                        paths_ref,
                        mode,
                        &mut trace,
                        &mut pending_approvals,
                    )
                    .await
                    else {
                        // Pending approval: keep going so every gated call in this
                        // round reaches the user, then stop after the round.
                        awaiting = true;
                        continue;
                    };
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": payload.to_string(),
                    }));
                }
                if awaiting {
                    break 'rounds;
                }
            }
            Ok(CopilotReply {
                reply: final_text(reply_text, !pending_approvals.is_empty(), finished, mode),
                model: model_name,
                provider: provider.id,
                mode: mode.as_str().to_string(),
                tool_calls: trace,
                pending_approvals,
            })
        }
        _ => {
            // OpenAI chat/completions loop (default + "chat").
            let tools = copilot_mode::tools_for_mode(tool_schemas(), mode);
            let mut messages = vec![json!({"role": "system", "content": system_prompt})];
            if let Some(history) = &history {
                for m in history.iter().rev().take(16).rev() {
                    let role = if m.role == "assistant" {
                        "assistant"
                    } else {
                        "user"
                    };
                    messages.push(json!({"role": role, "content": m.content}));
                }
            }
            messages.push(json!({"role": "user", "content": message}));

            let mut reply_text = String::new();
            let mut model_name = provider.model.clone();
            let mut pending_approvals: Vec<PendingApprovalDto> = Vec::new();
            let mut finished = false;
            'rounds: for _ in 0..mode.max_rounds() {
                let resp = chat_round(&state.http, &provider, &messages, &tools).await?;
                if let Some(m) = resp.get("model").and_then(|v| v.as_str()) {
                    model_name = m.to_string();
                }
                let msg = resp
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("message"))
                    .cloned()
                    .unwrap_or(json!({}));
                if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                    reply_text = content.to_string();
                }
                let calls = msg
                    .get("tool_calls")
                    .and_then(|t| t.as_array())
                    .cloned()
                    .unwrap_or_default();
                if calls.is_empty() {
                    finished = true;
                    break;
                }
                messages.push(msg);
                let mut awaiting = false;
                for call in calls {
                    let fname = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let args: Value = call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(json!({}));
                    let id = call
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let Some(payload) = dispatch_tool(
                        fname,
                        &args,
                        state_ref,
                        models_ref,
                        paths_ref,
                        mode,
                        &mut trace,
                        &mut pending_approvals,
                    )
                    .await
                    else {
                        // Pending approval: keep going so every gated call in this
                        // round reaches the user, then stop after the round.
                        awaiting = true;
                        continue;
                    };
                    messages.push(json!({
                        "role": "tool", "tool_call_id": id,
                        "content": payload.to_string(),
                    }));
                }
                if awaiting {
                    break 'rounds;
                }
            }
            Ok(CopilotReply {
                reply: final_text(reply_text, !pending_approvals.is_empty(), finished, mode),
                model: model_name,
                provider: provider.id,
                mode: mode.as_str().to_string(),
                tool_calls: trace,
                pending_approvals,
            })
        }
    }
}

#[cfg(test)]
mod mode_reply_tests {
    use super::*;

    #[test]
    fn a_turn_that_ran_out_of_steps_says_so() {
        let text = final_text(String::new(), false, false, ChatMode::Agent);
        assert!(text.contains("Stopped after 25 steps") && text.contains("continue"), "{text}");
        let partial = final_text("Half done.".into(), false, false, ChatMode::Review);
        assert!(partial.starts_with("Half done.") && partial.contains("Stopped after 10 steps"), "{partial}");
    }

    #[test]
    fn finished_and_paused_turns_are_unchanged() {
        assert_eq!(final_text("Done.".into(), false, true, ChatMode::Agent), "Done.");
        assert_eq!(final_text(String::new(), false, true, ChatMode::Ask), "(empty response)");
        assert!(final_text("x".into(), true, false, ChatMode::Review).starts_with("Paused"));
    }
}

#[cfg(test)]
mod approval_key_tests {
    use super::*;

    #[test]
    fn different_arguments_never_share_an_approval_key() {
        // A key crafted to look like `"broker_id":"zerodha"` must not collide with the real one.
        let forged = json!({"symbol":"AAPL","side":"buy","quantity":1,"broker_id\":\"zerodha\",zz":1});
        let real = json!({"symbol":"AAPL","side":"buy","quantity":1,"broker_id":"zerodha","zz":1});
        assert_ne!(canon_args(&forged), canon_args(&real));
        // Key order still does not matter.
        assert_eq!(
            canon_args(&json!({"a":1,"b":{"y":2,"x":1}})),
            canon_args(&json!({"b":{"x":1,"y":2},"a":1}))
        );
    }
}

#[cfg(test)]
mod ml_tool_tests {
    use super::*;

    const ML_READ: &[&str] = &[
        "list_feature_catalog", "list_datasets", "preview_dataset", "get_model", "list_ml_models",
        "check_expression", "list_symbol_data", "list_algorithms", "list_estimator_files",
    ];
    const ML_WRITE: &[&str] = &[
        "build_dataset", "delete_dataset", "update_model", "train_ml_model",
        "fetch_symbol_history", "save_estimator_file", "delete_estimator_file",
    ];

    fn function<'a>(all: &'a Value, name: &str) -> &'a Value {
        all.as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == name)
            .unwrap_or_else(|| panic!("{name} has no schema"))
    }

    fn names(all: &Value) -> Vec<String> {
        all.as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect()
    }

    #[test]
    fn every_ml_tool_has_a_schema_description_and_the_right_required_fields() {
        let all = tool_schemas();
        let required = |name: &str| -> Vec<String> {
            function(&all, name)["function"]["parameters"]["required"]
                .as_array()
                .map(|r| r.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        for name in ML_READ.iter().chain(ML_WRITE) {
            let f = &function(&all, name)["function"];
            assert!(!f["description"].as_str().unwrap_or("").is_empty(), "{name} needs a description");
            assert_eq!(f["parameters"]["type"], "object", "{name}");
            // Every required field must be declared as a property.
            for field in required(name) {
                assert!(f["parameters"]["properties"].get(&field).is_some(), "{name}: required `{field}` is not a property");
            }
        }
        assert_eq!(required("build_dataset"), ["name", "symbols", "features", "label"]);
        assert_eq!(required("get_model"), ["model_id"]);
        assert_eq!(required("update_model"), ["model_id"]);
        assert_eq!(required("preview_dataset"), ["name"]);
        assert_eq!(required("delete_dataset"), ["name"]);
        // A dataset supplies the path, target and features, so only the algorithm is required.
        assert_eq!(required("train_ml_model"), ["algorithm"]);
        let train = &function(&all, "train_ml_model")["function"]["parameters"]["properties"];
        for field in ["dataset", "hyperparams", "return_column", "feature_columns", "n_splits", "lag_periods"] {
            assert!(train.get(field).is_some(), "train_ml_model should accept `{field}`");
        }
        // The algorithm list is the one the backend accepts (the old text offered svm and linear_regression).
        let algos: Vec<&str> = train["algorithm"]["enum"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        for algo in &algos {
            assert!(ml_estimators::validate_algorithm_id(algo).is_ok(), "{algo} is not a valid algorithm");
        }
        let mut expected = ml_estimators::ALGORITHMS.to_vec();
        expected.push("recipe");
        assert_eq!(algos, expected, "the built-in algorithms plus recipe");
        assert!(algos.contains(&"custom") && algos.contains(&"extra_trees") && algos.contains(&"hist_gradient_boosting"));
        for field in ["trade_mode", "holdout_pct", "random_state", "estimator_file", "recipe"] {
            assert!(train.get(field).is_some(), "train_ml_model should accept `{field}`");
        }
        assert_eq!(train["trade_mode"]["enum"], json!(["long_flat", "long_short"]));
    }

    #[test]
    fn build_dataset_offers_dates_formulas_context_fee_and_thresholds() {
        let all = tool_schemas();
        let props = &function(&all, "build_dataset")["function"]["parameters"]["properties"];
        for field in ["from", "to", "custom_features", "context_symbols", "fee_pct"] {
            assert!(props.get(field).is_some(), "build_dataset should accept `{field}`");
        }
        for field in ["up_threshold_pct", "down_threshold_pct", "threshold_pct"] {
            assert!(props["label"]["properties"].get(field).is_some(), "label should accept `{field}`");
        }
        let formula = &props["custom_features"]["items"];
        assert_eq!(formula["required"], json!(["name", "expr"]));
        // What the schema advertises is what the request type reads.
        let req = copilot_more::build_request(&json!({
            "name": "d", "symbols": ["SPY"], "features": [], "from": "2024-01-01", "to": "2024-06-30", "fee_pct": 0.1,
            "custom_features": [{"name": "gap", "expr": "open / shift(close, 1) - 1"}], "context_symbols": ["QQQ"],
            "label": {"horizon_bars": 5, "kind": "return_threshold", "up_threshold_pct": 1.0, "down_threshold_pct": 0.5}
        })).unwrap();
        assert_eq!((req.from.as_deref(), req.to.as_deref(), req.fee_pct), (Some("2024-01-01"), Some("2024-06-30"), 0.1));
        assert_eq!((req.custom_features[0].name.as_str(), req.context_symbols.clone()), ("gap", vec!["QQQ".to_string()]));
        assert_eq!((req.label.up_threshold_pct, req.label.down_threshold_pct), (Some(1.0), Some(0.5)));
    }

    #[test]
    fn the_new_tools_have_the_right_required_fields_and_create_automation_takes_allow_short() {
        let all = tool_schemas();
        let required = |name: &str| -> Vec<String> {
            function(&all, name)["function"]["parameters"]["required"]
                .as_array()
                .map(|r| r.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        assert_eq!(required("check_expression"), ["expr", "symbol"]);
        assert_eq!(required("list_symbol_data"), ["symbols"]);
        assert_eq!(required("fetch_symbol_history"), ["symbol"]);
        assert_eq!(required("save_estimator_file"), ["name", "content"]);
        assert_eq!(required("delete_estimator_file"), ["name"]);
        assert!(required("list_algorithms").is_empty() && required("list_estimator_files").is_empty());
        let auto = &function(&all, "create_automation")["function"];
        assert_eq!(auto["parameters"]["properties"]["allow_short"]["type"], "boolean");
        assert_eq!(auto["parameters"]["properties"]["allow_short"]["default"], false);
        assert!(!required("create_automation").contains(&"allow_short".to_string()), "shorting is never required");
        for name in ML_READ.iter().chain(ML_WRITE) {
            assert!(function(&all, name)["function"]["description"].as_str().unwrap().len() > 20, "{name}");
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let mut all = names(&tool_schemas());
        let count = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), count, "duplicate tool names");
    }

    #[test]
    fn ask_mode_hides_the_ml_write_tools_and_keeps_the_reads() {
        let ask = names(&copilot_mode::tools_for_mode(tool_schemas(), ChatMode::Ask));
        for read in ML_READ {
            assert!(ask.contains(&read.to_string()), "{read} must stay available in Ask");
            assert!(!copilot_mode::is_write_tool(read), "{read} is read-only");
        }
        for write in ML_WRITE {
            assert!(copilot_mode::is_write_tool(write), "{write} is a write tool");
            assert!(!ask.contains(&write.to_string()), "{write} must be hidden in Ask");
        }
        // Review and Agent are offered all of them.
        for mode in [ChatMode::Review, ChatMode::Agent] {
            let all = names(&copilot_mode::tools_for_mode(tool_schemas(), mode));
            for name in ML_READ.iter().chain(ML_WRITE) {
                assert!(all.contains(&name.to_string()), "{name} missing in {mode:?}");
            }
        }
    }

    #[test]
    fn the_new_write_tools_need_approval_in_review() {
        // Review pauses for every tool in these lists (see `copilot_mode::requires_approval`).
        for write in ["build_dataset", "delete_dataset", "update_model"] {
            assert!(copilot_more::WRITE_TOOLS.contains(&write), "{write}");
        }
        assert!(WRITE_TOOLS.contains(&"train_ml_model"));
    }

    #[test]
    fn a_tool_trace_card_names_the_dataset_and_hyperparameters() {
        let text = copilot_tools::describe_call(
            "train_ml_model",
            &json!({"algorithm": "random_forest", "dataset": "spy_5d", "hyperparams": {"max_depth": 4, "n_estimators": 200}}),
        );
        assert_eq!(text, "Train random_forest on dataset spy_5d, max_depth=4 n_estimators=200");
        let csv = copilot_tools::describe_call(
            "train_ml_model",
            &json!({"algorithm": "xgboost", "data_path": "exports/a.csv", "target_column": "signal"}),
        );
        assert!(csv.contains("exports/a.csv") && csv.contains("signal"), "{csv}");
    }
}

#[cfg(test)]
mod approval_flow_tests {
    use super::*;
    use super::super::{ml_datasets, ml_expr, ml_recipes};
    use copilot_tools::test_support::{state as test_state, write_estimator, ESTIMATOR};

    type Run = (Option<Value>, Vec<ToolCallTrace>, Vec<PendingApprovalDto>);

    async fn run(state: &AppState, paths: &python_runtime::RuntimePaths, mode: ChatMode, name: &str, args: Value) -> Run {
        let models = ml::ModelRegistry::new(paths);
        let (mut trace, mut pending) = (Vec::new(), Vec::new());
        let out = dispatch_tool(name, &args, state, &models, paths, mode, &mut trace, &mut pending).await;
        (out, trace, pending)
    }

    fn custom(file: &str) -> Value {
        json!({"algorithm": "custom", "dataset": "d", "estimator_file": file})
    }

    #[tokio::test]
    async fn agent_mode_pauses_a_custom_run_and_the_card_shows_the_code() {
        let (state, paths) = test_state("flow-custom").await;
        write_estimator(&paths, "mine.py", ESTIMATOR);
        let (out, trace, pending) = run(&state, &paths, ChatMode::Agent, "train_ml_model", custom("mine.py")).await;
        assert!(out.is_none(), "nothing may run before approval");
        assert_eq!(pending.len(), 1);
        let hash = copilot_tools::content_hash(ESTIMATOR);
        assert!(pending[0].detail.contains(&hash) && pending[0].detail.contains("make_estimator"), "{}", pending[0].detail);
        assert!(pending[0].detail.contains("custom estimator mine.py"), "{}", pending[0].detail);
        assert!(pending[0].key.contains("#estimator="), "{}", pending[0].key);
        assert!(trace[0].detail.ends_with("awaiting approval"), "{}", trace[0].detail);
    }

    #[tokio::test]
    async fn an_approval_does_not_carry_over_to_edited_estimator_content() {
        let (state, paths) = test_state("flow-edit").await;
        write_estimator(&paths, "mine.py", ESTIMATOR);
        let (_, _, pending) = run(&state, &paths, ChatMode::Review, "train_ml_model", custom("mine.py")).await;
        let approved_key = pending[0].key.clone();
        state.copilot_approvals.lock().unwrap().insert(approved_key.clone());
        // The file changes after the user looked at the card.
        write_estimator(&paths, "mine.py", "import os\ndef make_estimator(params):\n    os.system('x')\n");
        let (out, _, pending) = run(&state, &paths, ChatMode::Review, "train_ml_model", custom("mine.py")).await;
        assert!(out.is_none(), "the old approval must not run the new code");
        assert_eq!(pending.len(), 1);
        assert_ne!(pending[0].key, approved_key);
        assert!(pending[0].detail.contains("import os"), "{}", pending[0].detail);
    }

    #[tokio::test]
    async fn ask_mode_refuses_custom_training_and_every_new_write_tool() {
        let (state, paths) = test_state("flow-ask").await;
        write_estimator(&paths, "mine.py", ESTIMATOR);
        for (name, args) in [
            ("train_ml_model", custom("mine.py")),
            ("save_estimator_file", json!({"name": "x.py", "content": ESTIMATOR})),
            ("delete_estimator_file", json!({"name": "mine.py"})),
            ("fetch_symbol_history", json!({"symbol": "SPY"})),
        ] {
            let (out, trace, pending) = run(&state, &paths, ChatMode::Ask, name, args).await;
            let out = out.unwrap_or_else(|| panic!("{name} should answer, not pause"));
            assert!(out["error"].as_str().unwrap().contains("Ask mode"), "{name}: {out}");
            assert!(pending.is_empty() && !trace[0].ok, "{name}");
        }
        assert!(paths.work_root().join("estimators/mine.py").exists(), "refused calls change nothing");
        assert!(!paths.work_root().join("estimators/x.py").exists());
    }

    #[tokio::test]
    async fn a_live_short_automation_pauses_in_agent_mode_with_short_leading_the_card() {
        let (state, paths) = test_state("flow-short").await;
        let args = json!({"name": "m", "symbol": "TCS.NS", "model_id": "m1", "allow_short": true, "broker_id": "zerodha"});
        for mode in [ChatMode::Agent, ChatMode::Review] {
            let (out, _, pending) = run(&state, &paths, mode, "create_automation", args.clone()).await;
            assert!(out.is_none(), "{mode:?}: nothing runs before approval");
            assert!(pending[0].detail.starts_with("[LIVE zerodha] [SHORT] "), "{}", pending[0].detail);
        }
        assert!(state.automations.list().is_empty(), "no rule was created");
    }

    #[tokio::test]
    async fn a_paper_short_automation_is_created_with_the_flag_only_when_asked() {
        let (state, paths) = test_state("flow-paper-short").await;
        let make = |name: &str, extra: Value| {
            let mut args = json!({"name": name, "symbol": "SPY", "model_id": "m1"});
            if !extra.is_null() {
                args["allow_short"] = extra;
            }
            args
        };
        // (name, allow_short argument, expected flag on the saved rule)
        for (name, flag, expected) in [
            ("asked", json!(true), true),
            ("declined", json!(false), false),
            ("silent", Value::Null, false),
            ("sloppy", json!("true"), false),
        ] {
            let (out, _, pending) = run(&state, &paths, ChatMode::Agent, "create_automation", make(name, flag)).await;
            assert!(pending.is_empty(), "paper actions run in Agent mode");
            let out = out.unwrap();
            assert_eq!(out["allow_short"], expected, "{name}: {out}");
            let rule = state.automations.list().into_iter().find(|r| r.name == name).unwrap();
            assert_eq!(rule.allow_short, expected, "{name}");
        }
    }

    #[tokio::test]
    async fn estimator_files_are_saved_listed_and_deleted_and_the_copilot_save_is_not_an_approval() {
        let (state, paths) = test_state("flow-files").await;
        let (out, trace, _) = run(&state, &paths, ChatMode::Agent, "save_estimator_file", json!({"name": "mine.py", "content": ESTIMATOR})).await;
        let out = out.unwrap();
        assert_eq!(out["saved"], true, "{out}");
        assert_eq!(out["approved_for_training"], false);
        assert!(trace[0].ok);
        assert!(paths.work_root().join("estimators/mine.py").exists());
        let recorded = state.copilot_trusted_files.lock().unwrap().get("estimators/mine.py").cloned().unwrap();
        assert!(recorded.starts_with("authored:"), "{recorded}");

        let (listed, _, _) = run(&state, &paths, ChatMode::Ask, "list_estimator_files", json!({})).await;
        let listed = listed.unwrap();
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["files"][0]["name"], "mine.py");
        assert_eq!(listed["files"][0]["approved_for_training"], false);
        assert!(listed["files"][0].get("content").is_none(), "the list stays small");
        let (one, _, _) = run(&state, &paths, ChatMode::Ask, "list_estimator_files", json!({"name": "mine.py"})).await;
        assert!(one.unwrap()["content"].as_str().unwrap().contains("make_estimator"));

        copilot_tools::trust_estimator(&state, &custom("mine.py"), &paths);
        let (listed, _, _) = run(&state, &paths, ChatMode::Ask, "list_estimator_files", json!({})).await;
        assert_eq!(listed.unwrap()["files"][0]["approved_for_training"], true);

        // A bad file is rejected with the reason; nothing is written or trusted.
        let (bad, trace, _) = run(&state, &paths, ChatMode::Agent, "save_estimator_file", json!({"name": "../evil.py", "content": ESTIMATOR})).await;
        assert!(bad.unwrap()["error"].is_string() && !trace[0].ok);
        let (bad, _, _) = run(&state, &paths, ChatMode::Agent, "save_estimator_file", json!({"name": "nomake.py", "content": "x = 1\n"})).await;
        assert!(bad.unwrap()["error"].as_str().unwrap().contains("make_estimator"));

        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_estimator_file", json!({"name": "mine.py"})).await;
        assert_eq!(gone.unwrap()["deleted"], "mine.py");
        assert!(!paths.work_root().join("estimators/mine.py").exists());
        assert!(state.copilot_trusted_files.lock().unwrap().get("estimators/mine.py").is_none(), "approval goes with the file");
        let (again, _, _) = run(&state, &paths, ChatMode::Agent, "delete_estimator_file", json!({"name": "mine.py"})).await;
        assert!(again.unwrap()["error"].as_str().unwrap().contains("no estimator file"));
    }

    #[tokio::test]
    async fn the_formula_and_data_tools_read_the_same_core_as_the_ui() {
        let (state, paths) = test_state("flow-data").await;
        let (bad, _, _) = run(&state, &paths, ChatMode::Ask, "check_expression", json!({"expr": "sma(close, ", "symbol": "SPY"})).await;
        let bad = bad.unwrap();
        assert_eq!(bad["ok"], false, "{bad}");
        assert!(bad["error"].is_string() && bad["error_pos"].is_number(), "{bad}");
        let (good, _, _) = run(&state, &paths, ChatMode::Ask, "check_expression", json!({"expr": "sma(close, 20) / close - 1", "symbol": "SPY"})).await;
        let good = good.unwrap();
        assert_eq!(good["ok"], true, "{good}");
        assert_eq!(good["warmup_bars"], 19);
        let (missing, _, _) = run(&state, &paths, ChatMode::Ask, "check_expression", json!({"symbol": "SPY"})).await;
        assert!(missing.unwrap()["error"].as_str().unwrap().contains("expr"));

        let (listed, _, _) = run(&state, &paths, ChatMode::Ask, "list_symbol_data", json!({"symbols": ["spy", "QQQ"], "timeframe": "1h"})).await;
        let listed = listed.unwrap();
        assert_eq!(listed["symbols"][0]["symbol"], "SPY");
        assert_eq!(listed["symbols"][0]["bars"], 0);
        assert_eq!(listed["symbols"][1]["timeframe"], "1h");
        let (none, _, _) = run(&state, &paths, ChatMode::Ask, "list_symbol_data", json!({"symbols": []})).await;
        assert!(none.unwrap()["error"].is_string());

        // Argument errors are caught before any download starts.
        for args in [json!({"symbol": "SPY", "bars": 0}), json!({"symbol": "SPY", "bars": "many"}), json!({"symbol": "SPY", "timeframe": "3m"}), json!({})] {
            let (out, trace, _) = run(&state, &paths, ChatMode::Agent, "fetch_symbol_history", args.clone()).await;
            assert!(out.unwrap()["error"].is_string() && !trace[0].ok, "{args}");
        }
    }

    #[tokio::test]
    async fn an_executed_custom_run_trusts_exactly_the_content_it_ran() {
        let (state, paths) = test_state("flow-trust-on-run").await;
        write_estimator(&paths, "mine.py", ESTIMATOR);
        let args = custom("mine.py");
        assert!(copilot_tools::needs_approval("train_ml_model", &args, &state, &paths));
        // The dataset does not exist, so training stops early; the run was still approved.
        let models = ml::ModelRegistry::new(&paths);
        let err = exec_tool("train_ml_model", &args, &state, &models, &paths).await.unwrap_err();
        assert!(!err.is_empty());
        assert!(!copilot_tools::needs_approval("train_ml_model", &args, &state, &paths), "approved content is remembered");
        // A run with another algorithm touches nothing.
        let (state2, paths2) = test_state("flow-trust-other").await;
        write_estimator(&paths2, "mine.py", ESTIMATOR);
        let _ = exec_tool("train_ml_model", &json!({"algorithm": "random_forest", "dataset": "d", "estimator_file": "mine.py"}), &state2, &ml::ModelRegistry::new(&paths2), &paths2).await;
        assert!(state2.copilot_trusted_files.lock().unwrap().is_empty());
    }

    fn voting_recipe(name: &str) -> Value {
        json!({
            "name": name, "label": "Calm vote", "description": "scaled forest and linear model",
            "steps": [{"kind": "standard_scaler", "params": {}}],
            "model": {"kind": "voting", "voting": "soft", "members": [
                {"algorithm": "random_forest", "params": {"n_estimators": 10, "max_depth": 3}},
                {"algorithm": "logistic_regression", "params": {}}]},
            "calibrate": "none"
        })
    }

    /// A two-symbol dataset built by the real builder, so the CSV and its sidecar agree.
    fn write_dataset(paths: &python_runtime::RuntimePaths, name: &str) {
        use super::super::ml_features::tests::synthetic_bars;
        let req: crate::dto::BuildDatasetRequestDto = serde_json::from_value(json!({
            "name": name, "symbols": ["AAA", "BBB"], "timeframe": "1d", "bars": 700,
            "features": ["ret_1", "ret_5", "rsi_14", "vol_10", "macd_hist"],
            "label": {"horizon_bars": 5, "kind": "direction", "threshold_pct": 0.0}
        }))
        .unwrap();
        let req = ml_datasets::normalize_request(&req).unwrap();
        let series = vec![("AAA".to_string(), synthetic_bars(700)), ("BBB".to_string(), synthetic_bars(685))];
        let rel = ml_datasets::relative_csv_path(paths, name);
        let (csv, info) = ml_datasets::assemble_with(&req, &series, &ml_expr::Context::new(), "user", &rel, chrono::Utc::now()).unwrap();
        ml_datasets::write_dataset(&ml_datasets::datasets_dir(paths), &csv, &info).unwrap();
    }

    #[tokio::test]
    async fn ask_mode_refuses_recipe_writes_and_training_but_answers_the_reads() {
        let (state, paths) = test_state("flow-recipe-ask").await;
        for (name, args) in [
            ("save_recipe", voting_recipe("calm_vote")),
            ("delete_recipe", json!({"name": "calm_vote"})),
            ("train_ml_model", json!({"algorithm": "recipe", "recipe": "calm_vote", "dataset": "d"})),
        ] {
            let (out, trace, pending) = run(&state, &paths, ChatMode::Ask, name, args).await;
            let out = out.unwrap_or_else(|| panic!("{name} should answer, not pause"));
            assert!(out["error"].as_str().unwrap().contains("Ask mode"), "{name}: {out}");
            assert!(pending.is_empty() && !trace[0].ok, "{name}");
        }
        assert!(!paths.work_root().join("algorithms").exists(), "refused calls write nothing");

        let (listed, trace, pending) = run(&state, &paths, ChatMode::Ask, "list_recipes", json!({})).await;
        let listed = listed.unwrap();
        assert_eq!((listed["count"].as_u64(), pending.len(), trace[0].ok), (Some(0), 0, true), "{listed}");
        let (missing, trace, _) = run(&state, &paths, ChatMode::Ask, "get_recipe", json!({"name": "nope"})).await;
        assert!(missing.unwrap()["error"].as_str().unwrap().contains("not found") && !trace[0].ok);
        // Checks that need no Python still answer in Ask: a bad name is reported with its path.
        let mut bad = voting_recipe("Bad Name");
        bad["label"] = json!("x");
        let (checked, _, pending) = run(&state, &paths, ChatMode::Ask, "validate_recipe", bad).await;
        let checked = checked.unwrap();
        assert_eq!(checked["ok"], false, "{checked}");
        assert!(checked["errors"].as_array().unwrap().iter().any(|e| e["path"] == "name"), "{checked}");
        assert!(pending.is_empty());
        let (junk, _, _) = run(&state, &paths, ChatMode::Ask, "validate_recipe", json!({"name": "x"})).await;
        assert!(junk.unwrap()["error"].as_str().unwrap().contains("invalid recipe"));
    }

    #[tokio::test]
    async fn review_mode_asks_before_saving_deleting_or_training_a_recipe_and_the_cards_say_what_happens() {
        let (state, paths) = test_state("flow-recipe-review").await;
        let (out, trace, pending) = run(&state, &paths, ChatMode::Review, "save_recipe", voting_recipe("calm_vote")).await;
        assert!(out.is_none() && pending.len() == 1, "nothing is saved before approval");
        assert!(pending[0].detail.starts_with("Save algorithm recipe calm_vote"), "{}", pending[0].detail);
        assert!(pending[0].detail.contains("standard_scaler then soft voting of random_forest, logistic_regression"), "{}", pending[0].detail);
        assert!(pending[0].detail.contains("no code runs"), "{}", pending[0].detail);
        assert!(!pending[0].key.contains("#estimator"), "recipes use the plain approval key: {}", pending[0].key);
        assert!(trace[0].detail.ends_with("awaiting approval"), "{}", trace[0].detail);
        assert!(!paths.work_root().join("algorithms").exists());

        let (out, _, pending) = run(&state, &paths, ChatMode::Review, "delete_recipe", json!({"name": "calm_vote"})).await;
        assert!(out.is_none() && pending[0].detail.starts_with("Delete algorithm recipe calm_vote"), "{:?}", pending);

        let train = json!({"algorithm": "recipe", "recipe": "calm_vote", "dataset": "d", "hyperparams": {"max_depth": 9}, "holdout_pct": 20});
        let (out, _, pending) = run(&state, &paths, ChatMode::Review, "train_ml_model", train.clone()).await;
        assert!(out.is_none() && pending.len() == 1);
        assert_eq!(pending[0].detail, "Train algorithm recipe calm_vote on dataset d [20% hold-out]");
        assert!(!pending[0].detail.contains("custom") && !pending[0].detail.contains("Python"), "no code-approval wording: {}", pending[0].detail);

        // Approved once, the call runs; the missing recipe is reported as an error, not a crash.
        state.copilot_approvals.lock().unwrap().insert(pending[0].key.clone());
        let (out, trace, pending) = run(&state, &paths, ChatMode::Review, "train_ml_model", train).await;
        assert!(pending.is_empty());
        let err = out.unwrap()["error"].as_str().unwrap().to_string();
        assert!(err.contains("calm_vote") && err.contains("not found"), "{err}");
        assert!(!trace[0].ok);
    }

    #[tokio::test]
    async fn recipe_training_arguments_are_checked_before_anything_runs() {
        let (state, paths) = test_state("flow-recipe-args").await;
        let models = ml::ModelRegistry::new(&paths);
        for (args, needle) in [
            (json!({"algorithm": "recipe", "dataset": "d"}), "needs `recipe`"),
            (json!({"algorithm": "recipe", "recipe": "  ", "dataset": "d"}), "needs `recipe`"),
            (json!({"algorithm": "random_forest", "recipe": "calm_vote", "dataset": "d"}), "only used with algorithm `recipe`"),
            (json!({"algorithm": "recipe", "recipe": 5, "dataset": "d"}), "must be a string"),
        ] {
            let err = exec_tool("train_ml_model", &args, &state, &models, &paths).await.unwrap_err();
            assert!(err.contains(needle), "{args}: {err}");
        }
        // Agent mode runs a recipe training without asking; only the missing recipe stops it.
        let (out, _, pending) = run(&state, &paths, ChatMode::Agent, "train_ml_model", json!({"algorithm": "recipe", "recipe": "gone", "dataset": "d"})).await;
        assert!(pending.is_empty(), "no approval for a declarative recipe");
        assert!(out.unwrap()["error"].as_str().unwrap().contains("not found"));
        assert!(state.copilot_trusted_files.lock().unwrap().is_empty(), "recipes never touch the estimator trust list");
    }

    #[tokio::test]
    async fn invalid_recipes_are_refused_without_writing_anything() {
        let (state, paths) = test_state("flow-recipe-invalid").await;
        // Structural problems are found before Python is needed.
        let mut too_few = voting_recipe("calm_vote");
        too_few["model"]["members"] = json!([{"algorithm": "random_forest"}]);
        let mut custom_member = voting_recipe("calm_vote");
        custom_member["model"]["members"][0]["algorithm"] = json!("custom");
        let mut bad_name = voting_recipe("../evil");
        bad_name["label"] = json!("evil");
        for (args, needle) in [(too_few, "model.members"), (custom_member, "algorithm"), (bad_name, "name")] {
            let (out, trace, _) = run(&state, &paths, ChatMode::Agent, "save_recipe", args.clone()).await;
            let err = out.unwrap()["error"].as_str().unwrap().to_string();
            assert!(err.contains(needle), "{args}: {err}");
            assert!(!trace[0].ok);
        }
        assert!(ml_recipes::list_recipe_files(paths.work_root()).is_empty());
        assert!(!paths.work_root().join("algorithms").join("evil.json").exists());
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_recipe", json!({"name": "calm_vote"})).await;
        assert!(gone.unwrap()["error"].as_str().unwrap().contains("no algorithm recipe"));
    }

    #[tokio::test]
    async fn agent_mode_never_silently_replaces_a_saved_recipe() {
        let (state, paths) = test_state("flow-recipe-collision").await;
        let mine: crate::dto::RecipeDto = serde_json::from_value(voting_recipe("calm_vote")).unwrap();
        ml_recipes::write_recipe(paths.work_root(), &mine).unwrap();
        let before = std::fs::read_to_string(paths.work_root().join("algorithms/calm_vote.json")).unwrap();

        let mut other = voting_recipe("calm_vote");
        other["label"] = json!("Agent version");
        let (out, trace, pending) = run(&state, &paths, ChatMode::Agent, "save_recipe", other).await;
        let err = out.unwrap()["error"].as_str().unwrap().to_string();
        assert!(pending.is_empty() && !trace[0].ok);
        assert!(err.contains("already saved") && err.contains("overwrite"), "{err}");
        let after = std::fs::read_to_string(paths.work_root().join("algorithms/calm_vote.json")).unwrap();
        assert_eq!(before, after, "the user's recipe is untouched");
        assert!(ml_recipes::recipe_exists(paths.work_root(), "calm_vote"));
    }

    #[tokio::test]
    async fn a_recipe_is_built_checked_saved_trained_and_deleted_through_the_tools() {
        let Some((state, paths)) = copilot_tools::test_support::state_with_python("flow-recipe-e2e").await else { return };
        let (blocks, _, _) = run(&state, &paths, ChatMode::Ask, "recipe_building_blocks", json!({})).await;
        let blocks = blocks.unwrap();
        assert!(blocks["steps"].as_array().unwrap().iter().any(|s| s["kind"] == "clip_outliers" && s["params"]["upper_pct"] == 99), "{blocks}");
        assert!(blocks["members"].as_array().unwrap().iter().any(|m| m == "random_forest"));
        assert!(blocks["limits"]["max_members"].is_number());
        assert!(blocks.to_string().len() < 6000, "trimmed for the model");

        // A typo in a member's parameter is caught by the same validator as normal training.
        let mut typo = voting_recipe("calm_vote");
        typo["model"]["members"][0]["params"] = json!({"n_estimatorz": 5});
        let (checked, _, _) = run(&state, &paths, ChatMode::Ask, "validate_recipe", typo.clone()).await;
        let checked = checked.unwrap();
        assert_eq!(checked["ok"], false, "{checked}");
        assert!(checked.to_string().contains("n_estimatorz"), "{checked}");
        let (refused, trace, _) = run(&state, &paths, ChatMode::Agent, "save_recipe", typo).await;
        assert!(refused.unwrap()["error"].as_str().unwrap().contains("n_estimatorz") && !trace[0].ok);
        assert!(ml_recipes::list_recipe_files(paths.work_root()).is_empty(), "an invalid recipe is not saved");

        let (checked, _, _) = run(&state, &paths, ChatMode::Ask, "validate_recipe", voting_recipe("calm_vote")).await;
        let checked = checked.unwrap();
        assert_eq!((checked["ok"].as_bool(), checked["models"].as_u64()), (Some(true), Some(2)), "{checked}");
        assert!(checked["next"].as_str().unwrap().contains("save_recipe"));
        assert!(ml_recipes::list_recipe_files(paths.work_root()).is_empty(), "validate never writes");

        let (saved, trace, pending) = run(&state, &paths, ChatMode::Agent, "save_recipe", voting_recipe("calm_vote")).await;
        let saved = saved.unwrap();
        assert!(pending.is_empty() && trace[0].ok, "{saved}");
        assert_eq!((saved["saved"].as_bool(), saved["model"].as_str(), saved["steps"].as_u64()), (Some(true), Some("voting"), Some(1)), "{saved}");
        assert!(paths.work_root().join("algorithms/calm_vote.json").exists());
        assert_eq!(saved["replaced"], false, "{saved}");
        let mut again = voting_recipe("calm_vote");
        again["label"] = json!("Calm vote v2");
        let (refused, _, _) = run(&state, &paths, ChatMode::Agent, "save_recipe", again.clone()).await;
        assert!(refused.unwrap()["error"].as_str().unwrap().contains("already saved"));
        again["overwrite"] = json!(true);
        let (replaced, _, _) = run(&state, &paths, ChatMode::Agent, "save_recipe", again).await;
        let replaced = replaced.unwrap();
        assert_eq!((replaced["saved"].as_bool(), replaced["replaced"].as_bool()), (Some(true), Some(true)), "{replaced}");

        let (listed, _, _) = run(&state, &paths, ChatMode::Ask, "list_recipes", json!({})).await;
        let listed = listed.unwrap();
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["recipes"][0]["algorithms"], json!(["random_forest", "logistic_regression"]));
        assert!(listed["recipes"][0].get("model_params").is_none() && listed["recipes"][0]["steps"][0] == "standard_scaler");
        let (full, _, _) = run(&state, &paths, ChatMode::Ask, "get_recipe", json!({"name": "calm_vote"})).await;
        let full = full.unwrap();
        assert_eq!(full["model"]["members"][0]["params"]["n_estimators"], 10, "{full}");
        assert!(full["created_at"].is_string());

        // Train it: hyperparams are ignored for a recipe, and no approval is asked in Agent mode.
        write_dataset(&paths, "rcp");
        let train = json!({"algorithm": "recipe", "recipe": "calm_vote", "dataset": "rcp", "n_splits": 3, "holdout_pct": 15,
                           "hyperparams": {"not_a_param": 1}});
        let (trained, trace, pending) = run(&state, &paths, ChatMode::Agent, "train_ml_model", train).await;
        let trained = trained.unwrap();
        assert!(pending.is_empty() && trace[0].ok, "{trained}");
        let model_id = trained["model_id"].as_str().unwrap_or_else(|| panic!("no model_id: {trained}")).to_string();
        assert!(model_id.starts_with("recipe_"), "{model_id}");
        assert!(trained["holdout"].is_object() && trained["metrics"]["accuracy"].is_number(), "{trained}");

        let (model, _, _) = run(&state, &paths, ChatMode::Ask, "get_model", json!({"model_id": model_id})).await;
        let model = model.unwrap();
        assert_eq!(model["algorithm"], "recipe", "{model}");
        assert_eq!(model["recipe"]["name"], "calm_vote");
        assert_eq!(model["recipe"]["model"]["members"][1]["algorithm"], "logistic_regression");
        assert_eq!(model["hyperparams"], json!({}), "{model}");

        // Deleting the recipe leaves the model and its copy of the spec alone.
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_recipe", json!({"name": "calm_vote"})).await;
        assert_eq!(gone.unwrap()["deleted"], "calm_vote");
        assert!(!paths.work_root().join("algorithms/calm_vote.json").exists());
        let (model, _, _) = run(&state, &paths, ChatMode::Ask, "get_model", json!({"model_id": model_id})).await;
        assert_eq!(model.unwrap()["recipe"]["name"], "calm_vote");
        let (again, _, _) = run(&state, &paths, ChatMode::Agent, "delete_recipe", json!({"name": "calm_vote"})).await;
        assert!(again.unwrap()["error"].as_str().unwrap().contains("no algorithm recipe"));
    }

    #[test]
    fn extended_training_arguments_must_have_the_right_types() {
        assert!(optional_str(&json!({"trade_mode": 3}), "trade_mode").is_err());
        assert_eq!(optional_str(&json!({"trade_mode": "long_short"}), "trade_mode").unwrap().as_deref(), Some("long_short"));
        assert_eq!(optional_str(&json!({}), "trade_mode").unwrap(), None);
        assert!(optional_number(&json!({"holdout_pct": "20"}), "holdout_pct").is_err());
        assert_eq!(optional_number(&json!({"holdout_pct": 20}), "holdout_pct").unwrap(), Some(20.0));
        assert!(optional_integer(&json!({"random_state": 1.5}), "random_state").is_err());
        assert_eq!(optional_integer(&json!({"random_state": null}), "random_state").unwrap(), None);
    }
}

/// The audit ledger as the Copilot sees it: its saves and deletes are versions, its orders carry the
/// turn id, and no tool can erase history.
#[cfg(test)]
mod ledger_flow_tests {
    use super::*;
    use crate::ledger::{self, Kind, Ledger};
    use copilot_tools::test_support::{state as test_state, write_estimator, ESTIMATOR};
    use std::sync::Arc;

    type Run = (Option<Value>, Vec<ToolCallTrace>, Vec<PendingApprovalDto>);

    async fn run(state: &AppState, paths: &python_runtime::RuntimePaths, mode: ChatMode, name: &str, args: Value) -> Run {
        let models = ml::ModelRegistry::new(paths);
        let (mut trace, mut pending) = (Vec::new(), Vec::new());
        let out = dispatch_tool(name, &args, state, &models, paths, mode, &mut trace, &mut pending).await;
        (out, trace, pending)
    }

    /// (version, action, actor) of every version.
    fn versions(ledger: &Ledger, kind: Kind, id: &str) -> Vec<(u32, String, String)> {
        ledger.history(kind, id).into_iter().map(|r| (r.version, r.action, r.actor)).collect()
    }

    fn tool_names() -> Vec<String> {
        tool_schemas().as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect()
    }

    #[tokio::test]
    async fn two_tool_driven_saves_of_a_strategy_leave_two_versions_by_the_copilot_and_no_backup_file() {
        let (state, paths) = test_state("ledger-flow-save").await;
        let ledger = crate::commands::versioned_files::ledger().unwrap();
        let id = "strategies/ledger_flow_a.py";
        let file = paths.work_root().join(id);

        let first = "def on_bar(ctx, bar):\n    return None\n";
        let (out, _, _) = run(&state, &paths, ChatMode::Agent, "save_strategy", json!({"path": "ledger_flow_a.py", "content": first})).await;
        assert_eq!(out.unwrap()["saved"], true);
        // Replacing needs the explicit flag, and then it is a second version, not an overwrite.
        let (refused, _, _) = run(&state, &paths, ChatMode::Agent, "save_strategy", json!({"path": "ledger_flow_a.py", "content": "x = 1\n"})).await;
        assert!(refused.unwrap()["error"].as_str().unwrap().contains("already exists"));
        let second = "def on_bar(ctx, bar):\n    return 'buy'\n";
        let (out, _, _) = run(&state, &paths, ChatMode::Agent, "save_strategy", json!({"path": "ledger_flow_a.py", "content": second, "overwrite": true})).await;
        assert_eq!(out.unwrap()["saved"], true);

        assert_eq!(
            versions(&ledger, Kind::Strategy, id),
            vec![(1, "create".to_string(), "copilot".to_string()), (2, "update".to_string(), "copilot".to_string())]
        );
        assert_eq!(ledger.content(Kind::Strategy, id, 1).unwrap(), first.as_bytes(), "version 1 is still the first text");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), second);
        assert!(!file.with_extension("py.bak").exists(), "history replaces the backup copy");
        // Saving identical content again adds nothing.
        run(&state, &paths, ChatMode::Agent, "save_strategy", json!({"path": "ledger_flow_a.py", "content": second, "overwrite": true})).await;
        assert_eq!(ledger.history(Kind::Strategy, id).len(), 2, "{:#?}", ledger.history(Kind::Strategy, id).iter().map(|r| (r.version, &r.action, &r.actor, &r.summary)).collect::<Vec<_>>());

        // The Copilot reads its own history, and restores version 1 as a third version.
        let (hist, _, _) = run(&state, &paths, ChatMode::Ask, "ledger_history", json!({"kind": "strategy", "id": id})).await;
        let hist = hist.unwrap();
        assert_eq!(hist["total_versions"], 2);
        assert_eq!(hist["versions"][1]["actor"], "copilot");
        let (v1, _, _) = run(&state, &paths, ChatMode::Ask, "ledger_version", json!({"kind": "strategy", "id": id, "version": 1})).await;
        assert_eq!(v1.unwrap()["text"], first);
        let (restored, _, _) = run(&state, &paths, ChatMode::Agent, "ledger_restore", json!({"kind": "strategy", "id": id, "version": 1})).await;
        assert_eq!(restored.unwrap()["new_version"], 3);
        let history = ledger.history(Kind::Strategy, id);
        assert_eq!((history[2].action.as_str(), history[2].actor.as_str(), history[2].restored_from), ("restore", "copilot", Some(1)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
        assert_eq!(ledger.content(Kind::Strategy, id, 2).unwrap(), second.as_bytes(), "the replaced text is still in version 2");
    }

    #[tokio::test]
    async fn the_copilots_remove_alert_tool_deletes_through_the_ledger_as_the_copilot() {
        use apex_core::application::alert_engine::{AlertDelivery, AlertRule, StoredAlert};
        let (state, paths) = test_state("ledger-flow-alert").await;
        let ledger = crate::commands::versioned_files::ledger().unwrap();
        let id = "copilot-flow-alert-1";
        let rule = StoredAlert { id: id.into(), rule: AlertRule::DailyPnl { threshold: -50.0 }, delivery: vec![AlertDelivery::InApp], enabled: true };
        state.alert_store.add(rule, "user").await.unwrap();
        let (out, trace, _) = run(&state, &paths, ChatMode::Agent, "remove_alert", json!({"id": id})).await;
        assert_eq!(out.unwrap()["removed"], id);
        assert_eq!(
            versions(&ledger, Kind::Alert, id),
            vec![(1, "create".to_string(), "user".to_string()), (2, "delete".to_string(), "copilot".to_string())]
        );
        // The tool-call log entry names the deletion like the UI path does.
        assert!(trace.iter().any(|t| t.name == "remove_alert"));
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "remove_alert", json!({"id": id})).await;
        assert!(gone.unwrap()["error"].as_str().unwrap().contains("no alert"));
        assert_eq!(ledger.history(Kind::Alert, id).len(), 2, "a refused delete adds no version");
    }

    #[tokio::test]
    async fn the_copilots_delete_tools_leave_a_tombstone_and_the_history() {
        let (state, paths) = test_state("ledger-flow-delete").await;
        let ledger = crate::commands::versioned_files::ledger().unwrap();

        // Estimator file
        let (saved, _, _) = run(&state, &paths, ChatMode::Agent, "save_estimator_file", json!({"name": "ledger_flow_e.py", "content": ESTIMATOR})).await;
        assert!(saved.unwrap().get("error").is_none());
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_estimator_file", json!({"name": "ledger_flow_e.py"})).await;
        assert_eq!(gone.unwrap()["deleted"], "ledger_flow_e.py");
        let e = versions(&ledger, Kind::Estimator, "ledger_flow_e.py");
        assert_eq!(e.iter().map(|v| v.1.as_str()).collect::<Vec<_>>(), ["create", "delete"]);
        assert!(e.iter().all(|v| v.2 == "copilot"));
        assert!(ledger.head(Kind::Estimator, "ledger_flow_e.py").unwrap().is_tombstone());
        assert_eq!(ledger.content(Kind::Estimator, "ledger_flow_e.py", 1).unwrap(), ESTIMATOR.as_bytes(), "the deleted text is still readable");
        assert!(!paths.work_root().join("estimators/ledger_flow_e.py").exists());
        // Writing the same name again continues the same object.
        run(&state, &paths, ChatMode::Agent, "save_estimator_file", json!({"name": "ledger_flow_e.py", "content": ESTIMATOR})).await;
        assert_eq!(ledger.history(Kind::Estimator, "ledger_flow_e.py").len(), 3);

        // Recipe
        let recipe_file = paths.work_root().join("algorithms").join("ledger_flow_r.json");
        std::fs::create_dir_all(recipe_file.parent().unwrap()).unwrap();
        crate::commands::versioned_files::save(&ledger, Kind::Recipe, "ledger_flow_r", &recipe_file, b"{\"name\":\"ledger_flow_r\"}", "user").unwrap();
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_recipe", json!({"name": "ledger_flow_r"})).await;
        assert_eq!(gone.unwrap()["deleted"], "ledger_flow_r");
        assert!(ledger.head(Kind::Recipe, "ledger_flow_r").unwrap().is_tombstone());
        assert_eq!(ledger.history(Kind::Recipe, "ledger_flow_r").len(), 2);

        // Dataset (its files stay, the object is marked deleted)
        let dir = ml_datasets_dir(&paths);
        let info = crate::dto::DatasetInfoDto { name: "ledger_flow_d".into(), path: "data/datasets/ledger_flow_d.csv".into(), rows: 1, timeframe: "1d".into(), created_at: "2026-01-01T00:00:00Z".into(), source: "copilot".into(), ..Default::default() };
        crate::commands::ml_datasets::write_dataset(&dir, "a\n1\n", &info).unwrap();
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_dataset", json!({"name": "ledger_flow_d"})).await;
        assert_eq!(gone.unwrap()["deleted"], "ledger_flow_d");
        let dataset_ledger = crate::activity::ledger_for(paths.data_dir()).unwrap();
        assert!(dataset_ledger.head(Kind::Dataset, "ledger_flow_d").unwrap().is_tombstone());
        assert!(dir.join("ledger_flow_d").join("v1.csv").is_file(), "the data file is kept");

        // Automation
        let (made, _, _) = run(&state, &paths, ChatMode::Agent, "create_automation", json!({"name": "flow rule", "symbol": "TCS.NS", "model_id": "m1"})).await;
        let id = made.unwrap()["created"].as_str().unwrap().to_string();
        let (off, _, _) = run(&state, &paths, ChatMode::Agent, "set_automation_enabled", json!({"id": id, "enabled": false})).await;
        assert!(off.unwrap().get("error").is_none());
        let (gone, _, _) = run(&state, &paths, ChatMode::Agent, "delete_automation", json!({"id": id})).await;
        assert_eq!(gone.unwrap()["deleted"], id.as_str());
        let a = versions(&ledger::global().unwrap().clone(), Kind::Automation, &id);
        assert!(a.len() >= 3, "{a:?}");
        assert!(a.iter().all(|v| v.2 == "copilot"), "{a:?}");
        assert_eq!(a.last().unwrap().1, "delete");
    }

    fn ml_datasets_dir(paths: &python_runtime::RuntimePaths) -> std::path::PathBuf {
        crate::commands::ml_datasets::datasets_dir(paths)
    }

    #[tokio::test]
    async fn ledger_restore_follows_the_mode_rules_and_the_reads_stay_in_ask() {
        let (state, paths) = test_state("ledger-flow-modes").await;
        let args = json!({"kind": "strategy", "id": "strategies/x.py", "version": 1});
        // Ask hides it and refuses it if it arrives anyway.
        assert!(copilot_mode::is_write_tool("ledger_restore"));
        let ask_names: Vec<String> = copilot_mode::tools_for_mode(tool_schemas(), ChatMode::Ask)
            .as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect();
        assert!(!ask_names.contains(&"ledger_restore".to_string()));
        for read in ["ledger_history", "ledger_version", "ledger_verify", "list_order_events"] {
            assert!(ask_names.contains(&read.to_string()), "{read} must stay available in Ask");
            assert!(!copilot_mode::is_write_tool(read));
            assert!(!copilot_mode::requires_approval(ChatMode::Review, read, &json!({}), &state, &paths), "{read} only reads");
        }
        let (refused, trace, _) = run(&state, &paths, ChatMode::Ask, "ledger_restore", args.clone()).await;
        assert!(refused.unwrap()["error"].as_str().unwrap().contains("read-only"));
        assert!(!trace[0].ok);
        // Review asks, and the card says a new version is created.
        let (out, _, pending) = run(&state, &paths, ChatMode::Review, "ledger_restore", args.clone()).await;
        assert!(out.is_none(), "nothing runs before approval");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].detail.contains("NEW version") && pending[0].detail.contains("history are kept"), "{}", pending[0].detail);
        // Agent runs a strategy restore on its own (the error here is only that the strategy does not exist).
        assert!(!copilot_mode::requires_approval(ChatMode::Agent, "ledger_restore", &args, &state, &paths));
        let (out, _, pending) = run(&state, &paths, ChatMode::Agent, "ledger_restore", args).await;
        assert!(pending.is_empty());
        assert!(out.unwrap()["error"].is_string());
    }

    #[tokio::test]
    async fn restoring_an_automation_that_trades_live_needs_approval_even_in_agent_mode() {
        use apex_core::application::automation::AutomationRule;
        let (state, paths) = test_state("ledger-flow-live").await;
        let rule: AutomationRule = serde_json::from_value(json!({
            "id": "live-restore-rule", "name": "live", "kind": "model_signal", "symbol": "TCS.NS", "model_id": "m",
            "interval_secs": 300, "quantity": 1.0, "threshold": 0.5, "broker_id": "zerodha",
            "enabled": false, "allow_short": false, "created_at": "2026-01-01T00:00:00Z",
            "last_run_at": null, "last_result": null
        })).unwrap();
        state.automations.apply_restored(rule);
        let live = json!({"kind": "automation", "id": "live-restore-rule", "version": 1});
        let paper = json!({"kind": "automation", "id": "no-such-rule", "version": 1});
        assert!(copilot_mode::requires_approval(ChatMode::Agent, "ledger_restore", &live, &state, &paths));
        assert!(copilot_mode::requires_approval(ChatMode::Review, "ledger_restore", &live, &state, &paths));
        assert!(!copilot_mode::requires_approval(ChatMode::Agent, "ledger_restore", &paper, &state, &paths));
        // A pending call executes nothing.
        let (out, _, pending) = run(&state, &paths, ChatMode::Agent, "ledger_restore", live).await;
        assert!(out.is_none() && pending.len() == 1);
        assert!(copilot_mode::live_involved("ledger_restore", &json!({"kind": "automation", "id": "x"}), &|_| Some("zerodha".into())));
        assert!(!copilot_mode::live_involved("ledger_restore", &json!({"kind": "strategy", "id": "x"}), &|_| Some("zerodha".into())));
    }

    #[test]
    fn no_copilot_tool_can_erase_history() {
        let names = tool_names();
        // Every tool that mentions the ledger is one of the known reads or the restore.
        for name in &names {
            if name.contains("ledger") || name.contains("audit") || name.contains("history") && name.starts_with("ledger") {
                assert!(
                    super::super::ledger_cmds::READ_TOOLS.contains(&name.as_str()) || super::super::ledger_cmds::WRITE_TOOLS.contains(&name.as_str()),
                    "unexpected ledger tool {name}"
                );
            }
            for word in ["purge", "erase", "wipe", "truncate", "compact", "rewrite", "prune", "vacuum", "forget", "overwrite_history"] {
                assert!(!name.contains(word), "tool {name} sounds like it destroys history");
            }
        }
        // The only delete-like tools are the ones that leave a tombstone version (tested above).
        let known: &[&str] = &[
            "delete_automation", "delete_dataset", "delete_estimator_file", "delete_recipe", "remove_alert", "remove_from_watchlist",
        ];
        for name in &names {
            if name.contains("delete") || name.starts_with("remove") {
                assert!(known.contains(&name.as_str()), "new delete-like tool {name}: make it a tombstone and list it here");
            }
        }
        for forbidden in ["ledger_delete", "ledger_purge", "ledger_edit", "ledger_clear", "ledger_truncate", "ledger_write", "ledger_put", "ledger_reset"] {
            assert!(!names.contains(&forbidden.to_string()));
            assert!(!copilot_mode::is_write_tool(forbidden));
        }
        // The one ledger tool that changes anything is the restore, which only appends.
        assert_eq!(super::super::ledger_cmds::WRITE_TOOLS, &["ledger_restore"]);
        assert!(copilot_mode::is_write_tool("ledger_restore"));
        // The executor does not know any other ledger tool either.
        let src = include_str!("ledger_cmds.rs");
        let exec = src.split("async fn exec_inner").nth(1).unwrap().split("/// [`verify_and_record`] for a named actor").next().unwrap();
        assert!(!exec.contains("tombstone(") && !exec.contains("append_event") && !exec.contains(".put("), "the tool executor must not write to the ledger directly");
    }

    #[tokio::test]
    async fn an_unknown_ledger_tool_is_not_executed() {
        let (state, paths) = test_state("ledger-flow-unknown").await;
        for name in ["ledger_delete", "ledger_purge", "ledger_edit"] {
            assert!(super::super::ledger_cmds::exec_tool(name, &json!({}), &state, &paths).await.is_none(), "{name}");
        }
    }

    #[tokio::test]
    async fn the_system_prompt_says_everything_is_versioned() {
        let (state, paths) = test_state("ledger-flow-prompt").await;
        for mode in [ChatMode::Ask, ChatMode::Review, ChatMode::Agent] {
            let prompt = copilot_tools::system_prompt(&state, &paths, mode);
            assert!(prompt.contains("AUDIT HISTORY") && prompt.contains("Nothing can be erased") && prompt.contains("ledger_history"));
            assert!(prompt.contains("Prefer a new version over deleting"));
        }
    }

    #[tokio::test]
    async fn verify_and_the_order_trail_are_readable_by_the_copilot() {
        let (state, paths) = test_state("ledger-flow-verify").await;
        let (out, _, _) = run(&state, &paths, ChatMode::Ask, "ledger_verify", json!({})).await;
        let out = out.unwrap();
        assert!(out["ok"].is_boolean() && out["checked_records"].is_number(), "{out}");
        assert!(out["limit"].as_str().unwrap().contains("middle of a log"));
        let (events, _, _) = run(&state, &paths, ChatMode::Ask, "list_order_events", json!({"limit": 500})).await;
        assert!(events.unwrap()["count"].as_u64().unwrap() <= 50);
        let (bad, _, _) = run(&state, &paths, ChatMode::Ask, "ledger_history", json!({"kind": "nonsense", "id": "x"})).await;
        assert!(bad.unwrap()["error"].as_str().unwrap().contains("unknown ledger kind"));
    }

    /// Orders and kill switches the Copilot triggers name the turn that produced them.
    #[tokio::test]
    async fn orders_the_copilot_places_changes_and_cancels_carry_its_turn_id() {
        use apex_core::domain::models::{Quote, Symbol};
        let (state, paths) = test_state("ledger-flow-orders").await;
        let dir = std::env::temp_dir().join(format!("apex-ledger-flow-orders-{}", uuid::Uuid::new_v4()));
        let ledger = Ledger::open(&dir).unwrap();
        state.otm.set_audit_sink(Arc::new(crate::order_audit::LedgerOrderSink::with_ledger(ledger.clone())));
        state.paper.update_quote(&Quote {
            symbol: Symbol("AAPL".into()), bid: 99.9, ask: 100.1, last: 100.0, open: 100.0, high: 100.0, low: 100.0,
            volume: 1, change_pct: 0.0, vwap: 100.0, updated_at: chrono::Utc::now(),
        });
        *state.copilot_turn.lock().unwrap() = Some("turn-ledger-7".to_string());

        // place_order (market, fills at once on paper)
        let (placed, _, _) = run(&state, &paths, ChatMode::Agent, "place_order", json!({"symbol": "AAPL", "side": "buy", "quantity": 2})).await;
        let placed = placed.unwrap();
        assert!(placed.get("error").is_none(), "{placed}");
        // A resting order the Copilot then modifies and cancels.
        let working = state
            .guarded_submit_with(
                apex_core::domain::models::NewOrderRequest {
                    symbol: Symbol("AAPL".into()),
                    side: apex_core::domain::models::OrderSide::Buy,
                    order_type: apex_core::domain::models::OrderType::Limit,
                    quantity: 1.0,
                    price: Some(50.0),
                    stop_price: None,
                    tag: None,
                    broker_params: None,
                },
                "paper",
                crate::state::LiveAuth::Unattended,
                Some(crate::order_audit::manual_source()),
            )
            .await
            .unwrap();
        let (modified, _, _) = run(&state, &paths, ChatMode::Agent, "modify_order", json!({"order_id": working.0, "price": 49.0})).await;
        assert!(modified.unwrap().get("error").is_none());
        let (cancelled, _, _) = run(&state, &paths, ChatMode::Agent, "cancel_order", json!({"order_id": working.0})).await;
        assert!(cancelled.unwrap().get("error").is_none());
        let (killed, _, _) = run(&state, &paths, ChatMode::Ask, "kill_switch", json!({})).await;
        assert_eq!(killed.unwrap()["halted"], true);

        let events = ledger.read_events("orders", &|_| true, 1000);
        let find = |event: &str, actor: &str| {
            events.iter().find(|e| e.value["event"] == event && e.value["actor"] == actor).map(|e| e.value.clone())
        };
        let submitted = events.iter().rev().find(|e| e.value["event"] == "submitted" && e.value["actor"] == "copilot").expect("the copilot's order").value.clone();
        assert_eq!((submitted["source"]["kind"].as_str(), submitted["source"]["id"].as_str()), (Some("copilot"), Some("turn-ledger-7")));
        for event in ["modified", "cancel_requested"] {
            let e = find(event, "copilot").unwrap_or_else(|| panic!("no {event} by the copilot"));
            assert_eq!(e["source"]["id"], "turn-ledger-7", "{event}");
            assert_eq!(e["order_id"], working.0.as_str());
        }
        let kill = find("kill_switch", "copilot").expect("the kill switch names the copilot");
        assert_eq!(kill["source"]["kind"], "kill_switch");
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(dir);
        let _ = write_estimator; // shared helper import used by sibling modules
    }
}

/// `export_bars_csv` writes content-addressed, write-once, read-only files.
#[cfg(test)]
mod export_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("apex-export-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn csv(close: f64) -> String {
        format!("time,open,high,low,close,volume\n2026-01-02T00:00:00Z,1,2,0.5,{close},100\n")
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn different_bars_make_different_files_and_the_first_file_never_changes() {
        let d = dir("two");
        let (a, b) = (csv(1.5), csv(1.6));
        let first = write_export(&d, "AAPL", "d1", &a).unwrap();
        let first_bytes = std::fs::read(&first.path).unwrap();
        let second = write_export(&d, "AAPL", "d1", &b).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!((first.reused, second.reused), (false, false));
        assert_eq!(std::fs::read(&first.path).unwrap(), first_bytes, "the earlier export is byte for byte what it was");
        assert_eq!(std::fs::read(&first.path).unwrap(), a.as_bytes());
        assert_eq!(std::fs::read(&second.path).unwrap(), b.as_bytes());
        assert_eq!(first.sha256, crate::ledger::Ledger::sha256_hex(a.as_bytes()));
        let name = first.path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, format!("AAPL_d1_{}.csv", &first.sha256[..12]));
        assert_eq!(names(&d).len(), 2, "no temporary files are left behind");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_same_bars_reuse_one_file_and_files_are_read_only() {
        let d = dir("same");
        let a = csv(2.5);
        let first = write_export(&d, "MSFT", "h1", &a).unwrap();
        let again = write_export(&d, "MSFT", "h1", &a).unwrap();
        assert_eq!((first.path.clone(), first.reused, again.reused), (again.path.clone(), false, true));
        assert_eq!(first.sha256, again.sha256);
        assert_eq!(names(&d).len(), 1);
        assert_eq!(std::fs::metadata(&first.path).unwrap().permissions().mode() & 0o777, 0o444);
        // Someone made it writable: reusing it puts the protection back, content untouched.
        std::fs::set_permissions(&first.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_export(&d, "MSFT", "h1", &a).unwrap();
        assert_eq!(std::fs::metadata(&first.path).unwrap().permissions().mode() & 0o777, 0o444);
        assert!(std::fs::OpenOptions::new().write(true).open(&first.path).is_err() || std::fs::metadata(&first.path).unwrap().permissions().readonly());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_name_clash_with_different_content_is_an_error_and_nothing_is_overwritten() {
        let d = dir("clash");
        let a = csv(3.5);
        let sha = crate::ledger::Ledger::sha256_hex(a.as_bytes());
        let clashing = d.join(format!("AAPL_d1_{}.csv", &sha[..12]));
        std::fs::write(&clashing, b"someone else's data").unwrap();
        let error = write_export(&d, "AAPL", "d1", &a).unwrap_err();
        assert!(error.contains("different content") && error.contains("not overwritten"), "{error}");
        assert_eq!(std::fs::read(&clashing).unwrap(), b"someone else's data");
        assert_eq!(names(&d).len(), 1, "no temporary file is left");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn older_fixed_name_exports_stay_where_they_are() {
        let d = dir("legacy");
        let legacy = d.join("AAPL_d1.csv");
        std::fs::write(&legacy, b"old export").unwrap();
        let file = write_export(&d, "AAPL", "d1", &csv(4.5)).unwrap();
        assert_ne!(file.path, legacy);
        assert_eq!(std::fs::read(&legacy).unwrap(), b"old export");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn symbol_and_timeframe_cannot_leave_the_exports_folder() {
        let d = dir("names");
        for (symbol, tf) in [("../../etc/passwd", "d1"), ("..", ".."), ("AAPL", "../x"), ("a/b\\c", "d 1")] {
            let file = write_export(&d, symbol, tf, &csv(5.5)).unwrap();
            assert_eq!(file.path.parent().unwrap(), d.as_path(), "{symbol} {tf}");
        }
        let _ = std::fs::remove_dir_all(d);
    }
}
