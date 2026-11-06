//! Commands and Copilot tools for the immutable audit ledger.
//!
//! Everything here READS the ledger, except restore. A restore never rewrites history: it appends a
//! NEW version whose content equals an older one (action "restore", `restored_from` set) and puts
//! the working copy back in step. There is deliberately no command and no Copilot tool that deletes,
//! purges or edits a record; `no_copilot_tool_can_erase_history` pins that down.
//!
//! HONEST LIMIT (also shown in the UI): the hash chain detects an edit or a deleted line in the
//! middle of a log. Someone with full write access to the data directory can rewrite the whole
//! thing consistently, and deleting the newest lines cannot be detected. That is why
//! [`ledger_status`] exposes the head hashes: copy them somewhere else to notice a later rewrite.
//!
//! The commands are thin. The `*_inner` functions take the ledger and paths explicitly so the tests
//! can use a temporary ledger.

use super::automation as automation_cmds;
use super::python_runtime::RuntimePaths;
use super::{ml, ml_datasets, ml_estimators, ml_recipes, notebook, settings, strategy, versioned_files};
use crate::dto::DatasetInfoDto;
use crate::ledger::{self, Kind, Ledger, LedgerStatus, ObjectSummary, VerifyReport, VersionRecord};
use crate::state::AppState;
use apex_core::application::automation::AutomationEngine;
use serde::Serialize;
use serde_json::{json, Value};
use std::str::FromStr;
use tauri::State;

/// Text of a version sent to the UI is cut at this many bytes (the stored copy is complete).
pub const MAX_TEXT_BYTES: usize = 512 * 1024;
/// The Copilot sees at most this many characters of a version.
pub const COPILOT_TEXT_CHARS: usize = 8000;
/// The Copilot sees at most this many versions / events per call.
pub const COPILOT_MAX_ITEMS: usize = 50;

/// Serializes to the TypeScript `VersionContentDto`.
#[derive(Debug, Clone, Serialize)]
pub struct VersionContentDto {
    pub record: VersionRecord,
    /// The content as text; null when it is binary, deleted or not retained.
    pub text: Option<String>,
    /// True when the text was cut for display.
    pub truncated: bool,
    pub binary: bool,
}

fn parse_kind(kind: &str) -> Result<Kind, String> {
    Kind::from_str(kind.trim())
}

fn check_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("an object id is required".into());
    }
    crate::validation::validate_string_length(id, "id")
}

/// The process-wide ledger. Everything in the app writes to this one.
fn app_ledger() -> Result<Ledger, String> {
    ledger::global().cloned().ok_or_else(|| "The audit ledger is not open. Restart APEX; if this keeps happening check that the data folder is writable.".to_string())
}

// ── Reading ──────────────────────────────────────────────────────────

/// All versions of one object, oldest first.
pub fn history_inner(ledger: &Ledger, kind: Kind, id: &str) -> Vec<VersionRecord> {
    ledger.history(kind, id)
}

/// Split content into display text. Up to [`MAX_TEXT_BYTES`] of valid UTF-8 is text; anything else
/// (invalid UTF-8, NUL bytes) is binary.
fn text_of(bytes: &[u8]) -> (Option<String>, bool, bool) {
    let truncated = bytes.len() > MAX_TEXT_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_TEXT_BYTES)];
    match std::str::from_utf8(slice) {
        Ok(s) if !s.contains('\0') => (Some(s.to_string()), truncated, false),
        // The cut landed inside a multi-byte character: keep what is complete.
        Err(e) if truncated && e.error_len().is_none() => {
            let s = std::str::from_utf8(&slice[..e.valid_up_to()]).unwrap_or_default();
            if s.contains('\0') {
                (None, truncated, true)
            } else {
                (Some(s.to_string()), true, false)
            }
        }
        _ => (None, truncated, true),
    }
}

/// One version with its content. A delete marker and a version whose content the ledger does not
/// hold (a large write-once file referenced by hash) have `text: null`.
pub fn version_inner(ledger: &Ledger, kind: Kind, id: &str, version: u32) -> Result<VersionContentDto, String> {
    let record = ledger
        .history(kind, id)
        .into_iter()
        .find(|r| r.version == version)
        .ok_or_else(|| format!("{} {id} has no version {version}", kind.as_str()))?;
    if record.is_tombstone() || !record.retained {
        return Ok(VersionContentDto { record, text: None, truncated: false, binary: false });
    }
    let bytes = ledger.content(kind, id, version)?;
    let (text, truncated, binary) = text_of(&bytes);
    Ok(VersionContentDto { record, text, truncated, binary })
}

pub fn objects_inner(ledger: &Ledger, kind: Kind, include_deleted: bool) -> Vec<ObjectSummary> {
    ledger.objects(kind, include_deleted)
}

// ── Restore ──────────────────────────────────────────────────────────

/// Bring a dataset version back as a NEW version. The restored record has the old version's content,
/// so it points at that version's write-once CSV; nothing is copied. Refused when that file is gone
/// or no longer matches its recorded hash, because a restore must not point at different data.
pub fn restore_dataset(ledger: &Ledger, paths: &RuntimePaths, name: &str, version: u32, actor: &str) -> Result<VersionRecord, String> {
    ml_datasets::validate_dataset_name(name)?;
    let dir = ml_datasets::datasets_dir(paths);
    let _guard = ml_datasets::lock_writes();
    let bytes = ledger.content(Kind::Dataset, name, version)?;
    let info: DatasetInfoDto =
        serde_json::from_slice(&bytes).map_err(|e| format!("version {version} of dataset {name} could not be read: {e}"))?;
    let csv = ml_datasets::csv_of(&dir, &info);
    let (hash, _) = ml_datasets::hash_file(&csv)
        .map_err(|e| format!("the data file of dataset {name} version {version} ({}) is not readable: {e}", csv.display()))?;
    if !info.csv_sha256.is_empty() && hash != info.csv_sha256 {
        return Err(format!(
            "the data file of dataset {name} version {version} no longer matches its recorded hash, so it was not restored"
        ));
    }
    ledger.restore(Kind::Dataset, name, version, actor).map(|(record, _)| record)
}

/// Why a settings version cannot be restored.
pub const SETTINGS_VIEW_ONLY: &str = "settings history is view-only; redacted values cannot be restored";

/// Restore `version` of an object as a new version and put the working copy back. Dispatches to the
/// per-kind restore of the module that owns the kind. Does not write an activity entry.
pub fn restore_dispatch(
    ledger: &Ledger,
    engine: &AutomationEngine,
    paths: &RuntimePaths,
    kind: Kind,
    id: &str,
    version: u32,
    actor: &str,
) -> Result<VersionRecord, String> {
    check_id(id)?;
    match kind {
        Kind::Strategy => strategy::restore_strategy(ledger, paths, id, version, actor),
        Kind::Estimator => ml_estimators::restore_estimator(ledger, paths.work_root(), id, version, actor),
        Kind::Recipe => ml_recipes::restore_recipe(ledger, paths.work_root(), id, version, actor),
        Kind::Notebook => notebook::restore_notebook(ledger, paths, id, version, actor),
        Kind::Dataset => restore_dataset(ledger, paths, id, version, actor),
        Kind::Model => ml::restore_model_files(paths.models_dir(), id, Some(version), actor),
        Kind::Automation => {
            automation_cmds::restore_automation_with(engine, ledger, id, version, actor)?;
            // The rule's new head is the restore record `restore_automation_with` just appended.
            ledger
                .head(Kind::Automation, id)
                .ok_or_else(|| format!("automation {id} has no record after the restore"))
        }
        Kind::Settings => Err(SETTINGS_VIEW_ONLY.to_string()),
        // An alert is restored into the running alert store, which only the app state holds
        // (see `restore_and_record`).
        Kind::Alert => Err("alerts are restored through the running alert store (restore_and_record)".to_string()),
    }
}

/// The activity kind for a restore: the four kinds the UI refreshes on keep their own name; the
/// rest are logged as "system" like their save and delete entries.
fn activity_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Strategy => "strategy",
        Kind::Model => "model",
        Kind::Automation => "automation",
        Kind::Dataset => "dataset",
        Kind::Estimator | Kind::Recipe | Kind::Notebook | Kind::Settings | Kind::Alert => "system",
    }
}

/// [`restore_dispatch`] plus the activity entry (`<kind>.restore`). The entry fires
/// `artifacts-changed` for strategies, models, automations and datasets, so those panels refresh.
pub async fn restore_and_record(
    state: &AppState,
    paths: &RuntimePaths,
    kind: Kind,
    id: &str,
    version: u32,
    actor: &str,
) -> Result<VersionRecord, String> {
    let ledger = app_ledger()?;
    let result = match kind {
        // Refused before anything is touched; recorded below like any other failed restore.
        Kind::Settings => Err(SETTINGS_VIEW_ONLY.to_string()),
        Kind::Alert => state.alert_store.restore(&ledger, id, version, actor).await,
        _ => {
            let (engine, paths_owned, id_owned, actor_owned) = (state.automations.clone(), paths.clone(), id.to_string(), actor.to_string());
            tauri::async_runtime::spawn_blocking(move || {
                restore_dispatch(&ledger, &engine, &paths_owned, kind, &id_owned, version, &actor_owned)
            })
            .await
            .map_err(|e| format!("the restore task failed: {e}"))
            .and_then(|r| r)
        }
    };
    let action = format!("{}.restore", kind.as_str());
    match &result {
        Ok(record) => crate::activity::record(
            state,
            actor,
            &action,
            activity_kind(kind),
            id,
            &format!(
                "Restored {} {id} to version {version} (saved as new version {}; nothing was erased)",
                kind.as_str(),
                record.version
            ),
            true,
            Some(format!("restored_from={version} new_version={} hash={}", record.version, record.content_hash.as_deref().unwrap_or(""))),
        ),
        Err(error) => crate::activity::record(
            state,
            actor,
            &action,
            activity_kind(kind),
            id,
            &format!("Restoring {} {id} to version {version} failed", kind.as_str()),
            false,
            Some(error.clone()),
        ),
    }
    result
}

// ── Verify ───────────────────────────────────────────────────────────

/// Recompute every hash on a blocking thread and keep the result for [`ledger_status`] (the ledger
/// stores it). Writes an activity entry (`ledger.verify`, kind system); `ok` is false when problems
/// were found. The entry does not announce an artifact change.
pub async fn verify_and_record(state: &AppState, ledger: Ledger) -> Result<VerifyReport, String> {
    let report = tauri::async_runtime::spawn_blocking(move || ledger.verify())
        .await
        .map_err(|e| format!("the verification task failed: {e}"))?;
    let summary = if report.ok {
        format!(
            "Audit ledger verified: {} records, {} files, {} events, no problems",
            report.checked_records, report.checked_blobs, report.checked_events
        )
    } else {
        format!("Audit ledger verification found {} problem(s)", report.problems.len())
    };
    let details = report
        .problems
        .first()
        .map(|p| format!("{} at {}: {}", p.kind, p.location, p.detail))
        .or_else(|| report.head_hash.as_ref().map(|h| format!("head_hash={h}")));
    crate::activity::record_info(state, "user", "ledger.verify", "system", "ledger", &summary, report.ok, details);
    Ok(report)
}

// ── Commands ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn ledger_history(kind: String, id: String) -> Result<Vec<VersionRecord>, String> {
    let kind = parse_kind(&kind)?;
    check_id(&id)?;
    Ok(history_inner(&app_ledger()?, kind, &id))
}

#[tauri::command]
pub async fn ledger_version(kind: String, id: String, version: u32) -> Result<VersionContentDto, String> {
    let kind = parse_kind(&kind)?;
    check_id(&id)?;
    let ledger = app_ledger()?;
    tauri::async_runtime::spawn_blocking(move || version_inner(&ledger, kind, &id, version))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn ledger_objects(kind: String, include_deleted: Option<bool>) -> Result<Vec<ObjectSummary>, String> {
    let kind = parse_kind(&kind)?;
    Ok(objects_inner(&app_ledger()?, kind, include_deleted.unwrap_or(true)))
}

/// Restore an older version as a NEW version (the older record and everything after it stay).
#[tauri::command]
pub async fn ledger_restore(
    kind: String,
    id: String,
    version: u32,
    state: State<'_, AppState>,
    paths: State<'_, RuntimePaths>,
) -> Result<VersionRecord, String> {
    let kind = parse_kind(&kind)?;
    check_id(&id)?;
    restore_and_record(state.inner(), paths.inner(), kind, &id, version, "user").await
}

/// Recompute every record, file and event hash from disk. Takes a while on a large ledger, so it
/// runs on a blocking thread.
#[tauri::command]
pub async fn ledger_verify(state: State<'_, AppState>) -> Result<VerifyReport, String> {
    verify_and_record(state.inner(), app_ledger()?).await
}

#[tauri::command]
pub async fn ledger_status() -> Result<LedgerStatus, String> {
    Ok(app_ledger()?.status())
}

// ── Startup ──────────────────────────────────────────────────────────

/// What the startup pass found.
#[derive(Debug, Default, Clone)]
pub struct StartupReconcile {
    pub files: versioned_files::ReconcileSummary,
    pub datasets: usize,
    pub models: usize,
    /// The settings file was registered (first run) or found changed outside APEX and recorded.
    pub settings_recorded: bool,
    pub errors: Vec<String>,
}

impl StartupReconcile {
    /// Whether anything outside the app's own writes was recorded.
    pub fn recorded_changes(&self) -> bool {
        self.files.changed()
    }
}

/// Bring the ledger in step with what is on disk, once at startup: finish interrupted saves and
/// record edits, additions and removals made outside APEX as `external` versions (strategies,
/// estimators, recipes, notebooks), and register datasets and models that predate the ledger.
/// Automations register when their engine is built. Failures are collected, never fatal.
pub fn startup_reconcile(ledger: &Ledger, paths: &RuntimePaths) -> StartupReconcile {
    let mut out = StartupReconcile { files: versioned_files::reconcile_all(ledger, paths), ..Default::default() };
    out.errors.extend(out.files.errors.iter().cloned());
    // The settings file: version 1 on the first run, an `external` version when it changed while
    // APEX was not running. An out-of-app edit also gets an activity entry (nothing else would show it).
    match settings::reconcile_settings(ledger, paths.config_file()) {
        Ok(Some(found)) => {
            out.settings_recorded = true;
            if found.record.actor == "external" {
                if let Err(error) = crate::activity::append_to(ledger, &settings::external_edit_entry(&found)) {
                    out.errors.push(format!("settings: could not record the outside edit: {error}"));
                }
            }
        }
        Ok(None) => {}
        Err(error) => out.errors.push(format!("settings: {error}")),
    }
    match ml_datasets::list_datasets_checked(&ml_datasets::datasets_dir(paths)) {
        Ok(list) => out.datasets = list.len(),
        Err(error) => out.errors.push(format!("datasets: {error}")),
    }
    match ml::load_registered_models(paths.models_dir()) {
        Ok(list) => out.models = list.len(),
        Err(error) => out.errors.push(format!("models: {error}")),
    }
    out
}

// ── Copilot tools ────────────────────────────────────────────────────
//
// Read tools: ledger_history, ledger_version, ledger_verify, list_order_events.
// Write-class: ledger_restore (follows the mode rules; see copilot_mode). There is no tool that
// deletes, purges or edits the ledger.

/// The Copilot's ledger tools that only read.
pub const READ_TOOLS: &[&str] = &["ledger_history", "ledger_version", "ledger_verify", "list_order_events"];
/// The Copilot's ledger tool that changes state: it appends a new version.
pub const WRITE_TOOLS: &[&str] = &["ledger_restore"];

const KINDS: [&str; 9] = ["strategy", "estimator", "recipe", "dataset", "model", "automation", "notebook", "settings", "alert"];

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}})
}

/// OpenAI function-calling definitions for the ledger tools.
pub fn tool_schemas() -> Vec<Value> {
    let kind = json!({"type": "string", "enum": KINDS, "description": "what kind of object"});
    let id = json!({"type": "string", "description": "strategy path (strategies/x.py), estimator file name, recipe name, dataset name, model id, automation id, notebook path, alert rule id, or config/apex.toml for the settings (secrets are redacted and settings cannot be restored)"});
    vec![
        tool(
            "ledger_history",
            "The full version history of one object in the immutable audit ledger, oldest first: version, action (create/update/delete/restore/external), actor, time, size, hashes and a summary. \
             Use it whenever the user asks what changed, who changed it or when. At most 50 versions are returned (the newest). Nothing in the ledger can be edited or erased.",
            json!({"type": "object", "properties": {"kind": kind, "id": id,
                "limit": {"type": "integer", "default": 20, "description": "1-50 newest versions"}},
                "required": ["kind", "id"]}),
        ),
        tool(
            "ledger_version",
            "The content of one version of an object (text cut at 8000 characters). Use it to compare what an older version held with the current one.",
            json!({"type": "object", "properties": {"kind": kind, "id": id, "version": {"type": "integer", "description": "version number, 1 is the first"}},
                "required": ["kind", "id", "version"]}),
        ),
        tool(
            "ledger_verify",
            "Recompute every hash in the audit ledger (records, stored files, activity and order logs) and report whether anything was altered. \
             A hash chain detects edits and deletions in the middle of a log; it cannot prove the newest entries are all still there.",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "list_order_events",
            "The order audit trail, newest first: every state an order passed through (submitted, risk accepted or rejected, broker ack, fills, cancels, modifies) with who produced it \
             (manual, copilot turn, automation and its version, strategy and its version). At most 50 events.",
            json!({"type": "object", "properties": {
                "order_id": {"type": "string"}, "broker_id": {"type": "string"},
                "limit": {"type": "integer", "default": 20, "description": "1-50"}}}),
        ),
        tool(
            "ledger_restore",
            "Bring an older version of an object back. This adds a NEW version whose content equals the older one; the current content and every earlier version stay in the history, so nothing is erased. \
             Prefer this over deleting or overwriting. Restoring an automation can resume it on its saved broker.",
            json!({"type": "object", "properties": {"kind": kind, "id": id, "version": {"type": "integer", "description": "the older version to bring back"}},
                "required": ["kind", "id", "version"]}),
        ),
    ]
}

/// The card text for a ledger tool call.
pub fn describe(name: &str, args: &Value) -> Option<String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("?").to_string();
    let version = args.get("version").and_then(|v| v.as_u64()).map(|v| v.to_string()).unwrap_or_else(|| "?".into());
    Some(match name {
        "ledger_history" => format!("Read the version history of {} {}", s("kind"), s("id")),
        "ledger_version" => format!("Read version {version} of {} {}", s("kind"), s("id")),
        "ledger_verify" => "Verify the audit ledger (recomputes every hash)".to_string(),
        "list_order_events" => match args.get("order_id").and_then(|v| v.as_str()) {
            Some(order) => format!("Read the audit trail of order {order}"),
            None => "Read the latest order audit events".to_string(),
        },
        "ledger_restore" => format!(
            "Restore {} {} to version {version}. This creates a NEW version with the content of version {version}; the current content and all history are kept.{}",
            s("kind"),
            s("id"),
            if args.get("kind").and_then(|v| v.as_str()) == Some("automation") {
                " The automation can resume on its saved broker, which may be live."
            } else {
                ""
            }
        ),
        _ => return None,
    })
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

fn short(hash: &Option<String>) -> Value {
    json!(hash.as_deref().map(|h| h.chars().take(12).collect::<String>()))
}

fn limit_arg(args: &Value, default: usize) -> usize {
    args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(default).clamp(1, COPILOT_MAX_ITEMS)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(|v| v.as_str()).ok_or_else(|| format!("missing `{key}`"))
}

fn version_arg(args: &Value) -> Result<u32, String> {
    args.get("version")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v >= 1)
        .ok_or_else(|| "`version` must be a whole number, 1 or more".to_string())
}

/// A trimmed history for the model: the newest `limit` versions, oldest of those first.
pub fn history_for_copilot(ledger: &Ledger, kind: Kind, id: &str, limit: usize) -> Value {
    let all = ledger.history(kind, id);
    let total = all.len();
    let shown: Vec<Value> = all
        .iter()
        .skip(total.saturating_sub(limit))
        .map(|r| {
            json!({
                "version": r.version, "action": r.action, "actor": r.actor, "ts": r.ts, "size": r.size,
                "summary": clip(&r.summary, 160), "content_hash": short(&r.content_hash),
                "restored_from": r.restored_from, "deleted": r.is_tombstone(),
            })
        })
        .collect();
    json!({"kind": kind.as_str(), "id": id, "total_versions": total, "shown": shown.len(), "versions": shown,
           "note": if total == 0 { "No history: the ledger has never seen this object. Check the id with list tools." } else { "" }})
}

/// One version for the model, text capped at [`COPILOT_TEXT_CHARS`].
pub fn version_for_copilot(ledger: &Ledger, kind: Kind, id: &str, version: u32) -> Result<Value, String> {
    let v = version_inner(ledger, kind, id, version)?;
    let (text, cut) = match &v.text {
        Some(t) => (Some(clip(t, COPILOT_TEXT_CHARS)), v.truncated || t.chars().count() > COPILOT_TEXT_CHARS),
        None => (None, false),
    };
    Ok(json!({
        "kind": kind.as_str(), "id": id, "version": v.record.version, "action": v.record.action, "actor": v.record.actor,
        "ts": v.record.ts, "size": v.record.size, "content_hash": short(&v.record.content_hash),
        "deleted": v.record.is_tombstone(), "binary": v.binary, "truncated": cut, "text": text,
    }))
}

/// Order events for the model, newest first, trimmed to what helps reasoning about the trade.
pub fn order_events_for_copilot(ledger: &Ledger, order_id: Option<&str>, broker_id: Option<&str>, limit: usize) -> Value {
    let events = crate::order_audit::query_order_events(ledger, order_id, broker_id, Some(limit.clamp(1, COPILOT_MAX_ITEMS)));
    let rows: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "seq": e.seq, "ts": e.ts, "order_id": e.value["order_id"], "broker_id": e.value["broker_id"],
                "event": e.value["event"], "actor": e.value["actor"], "source": e.value["source"],
                "data": clip(&e.value["data"].to_string(), 300),
            })
        })
        .collect();
    json!({"count": rows.len(), "events": rows})
}

fn verify_for_copilot(report: &VerifyReport) -> Value {
    json!({
        "ok": report.ok, "checked_records": report.checked_records, "checked_blobs": report.checked_blobs,
        "checked_events": report.checked_events, "problem_count": report.problems.len(),
        "problems": report.problems.iter().take(10).map(|p| json!({"kind": p.kind, "where": p.location, "detail": clip(&p.detail, 200)})).collect::<Vec<_>>(),
        "head_hash": report.head_hash, "verified_at": report.verified_at,
        "limit": "A hash chain detects edits and deletions in the middle of a log; it cannot show that the newest entries are all still there.",
    })
}

/// Run a ledger tool. `None` means `name` is not one of them.
pub async fn exec_tool(name: &str, args: &Value, state: &AppState, paths: &RuntimePaths) -> Option<Result<Value, String>> {
    if !READ_TOOLS.contains(&name) && !WRITE_TOOLS.contains(&name) {
        return None;
    }
    Some(exec_inner(name, args, state, paths).await)
}

async fn exec_inner(name: &str, args: &Value, state: &AppState, paths: &RuntimePaths) -> Result<Value, String> {
    let ledger = app_ledger()?;
    match name {
        "ledger_history" => {
            let kind = parse_kind(str_arg(args, "kind")?)?;
            let id = str_arg(args, "id")?;
            check_id(id)?;
            Ok(history_for_copilot(&ledger, kind, id, limit_arg(args, 20)))
        }
        "ledger_version" => {
            let kind = parse_kind(str_arg(args, "kind")?)?;
            let id = str_arg(args, "id")?;
            check_id(id)?;
            let version = version_arg(args)?;
            tauri::async_runtime::spawn_blocking({
                let (ledger, id) = (ledger.clone(), id.to_string());
                move || version_for_copilot(&ledger, kind, &id, version)
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "ledger_verify" => verify_and_record_as(state, ledger, "copilot").await.map(|r| verify_for_copilot(&r)),
        "list_order_events" => {
            let order = args.get("order_id").and_then(|v| v.as_str());
            let broker = args.get("broker_id").and_then(|v| v.as_str());
            Ok(order_events_for_copilot(&ledger, order, broker, limit_arg(args, 20)))
        }
        "ledger_restore" => {
            let kind = parse_kind(str_arg(args, "kind")?)?;
            let id = str_arg(args, "id")?;
            let version = version_arg(args)?;
            let record = restore_and_record(state, paths, kind, id, version, "copilot").await?;
            Ok(json!({
                "restored": true, "kind": kind.as_str(), "id": id, "restored_from": version, "new_version": record.version,
                "note": "A new version was added; the earlier versions are unchanged and still in ledger_history.",
            }))
        }
        _ => Err(format!("unknown ledger tool `{name}`")),
    }
}

/// [`verify_and_record`] for a named actor.
async fn verify_and_record_as(state: &AppState, ledger: Ledger, actor: &str) -> Result<VerifyReport, String> {
    if actor == "user" {
        return verify_and_record(state, ledger).await;
    }
    let report = tauri::async_runtime::spawn_blocking(move || ledger.verify())
        .await
        .map_err(|e| format!("the verification task failed: {e}"))?;
    let summary = if report.ok {
        format!("Audit ledger verified: {} records, {} events, no problems", report.checked_records, report.checked_events)
    } else {
        format!("Audit ledger verification found {} problem(s)", report.problems.len())
    };
    let details = report.problems.first().map(|p| format!("{} at {}: {}", p.kind, p.location, p.detail));
    crate::activity::record_info(state, actor, "ledger.verify", "system", "ledger", &summary, report.ok, details);
    Ok(report)
}

/// Whether restoring this automation version involves a live (non-paper) broker: the rule as it is
/// now, or the version being restored (a deleted rule has no "now"). Used by Agent mode to decide
/// whether the restore needs the user's approval.
pub fn restore_involves_live_broker(ledger: Option<&Ledger>, engine: &AutomationEngine, args: &Value) -> bool {
    if args.get("kind").and_then(|v| v.as_str()) != Some("automation") {
        return false;
    }
    let Some(id) = args.get("id").and_then(|v| v.as_str()) else { return false };
    let live = |broker: &str| broker != "paper";
    if engine.list().iter().any(|r| r.id == id && live(&r.broker_id)) {
        return true;
    }
    let Some(ledger) = ledger else {
        // Without the ledger the target version cannot be inspected: be careful.
        return true;
    };
    let Some(version) = args.get("version").and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok()) else {
        return false;
    };
    match ledger.content(Kind::Automation, id, version) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("broker_id").and_then(|b| b.as_str()).map(|b| live(b)))
            // A version that cannot be read as a rule restores nothing useful; the restore itself fails.
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// `restore_involves_live_broker` on the running app.
pub fn restore_needs_live_approval(state: &AppState, args: &Value) -> bool {
    restore_involves_live_broker(ledger::global(), &state.automations, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::copilot_tools::test_support;
    use crate::commands::versioned_files::testutil::{open, tmp};

    #[test]
    fn text_of_splits_text_binary_and_truncated_content() {
        assert_eq!(text_of(b"hello"), (Some("hello".into()), false, false));
        assert_eq!(text_of(&[0xff, 0xfe, 0x00]), (None, false, true));
        assert_eq!(text_of(b"a\0b"), (None, false, true));
        // A multi-byte character cut by the limit keeps the complete part and is marked truncated.
        let mut big = "x".repeat(MAX_TEXT_BYTES - 1).into_bytes();
        big.extend("é".as_bytes()); // 2 bytes, straddles the limit
        big.extend(b"tail");
        let (text, truncated, binary) = text_of(&big);
        assert!(truncated && !binary);
        assert_eq!(text.unwrap().len(), MAX_TEXT_BYTES - 1);
        let (text, truncated, _) = text_of(&vec![b'y'; MAX_TEXT_BYTES + 10]);
        assert!(truncated);
        assert_eq!(text.unwrap().len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn kinds_and_ids_are_validated() {
        assert!(parse_kind("strategy").is_ok() && parse_kind("model").is_ok());
        assert!(parse_kind("blob").is_err() && parse_kind("").is_err());
        assert!(check_id("").is_err() && check_id("   ").is_err());
        assert!(check_id(&"x".repeat(100_000)).is_err());
        assert!(check_id("strategies/a.py").is_ok());
    }

    #[test]
    fn a_version_comes_back_with_its_text_and_a_delete_marker_has_none() {
        let dir = tmp("lc-version");
        let ledger = open(&dir);
        ledger.put(Kind::Strategy, "strategies/a.py", b"v1 code\n", "user", "first").unwrap();
        ledger.put(Kind::Strategy, "strategies/a.py", b"v2 code\n", "copilot", "second").unwrap();
        ledger.tombstone(Kind::Strategy, "strategies/a.py", "user", "gone").unwrap();

        let v1 = version_inner(&ledger, Kind::Strategy, "strategies/a.py", 1).unwrap();
        assert_eq!(v1.text.as_deref(), Some("v1 code\n"));
        assert!(!v1.truncated && !v1.binary);
        assert_eq!(v1.record.actor, "user");
        let gone = version_inner(&ledger, Kind::Strategy, "strategies/a.py", 3).unwrap();
        assert!(gone.text.is_none() && !gone.binary && gone.record.action == "delete");
        assert!(version_inner(&ledger, Kind::Strategy, "strategies/a.py", 9).is_err());
        assert!(version_inner(&ledger, Kind::Strategy, "strategies/none.py", 1).is_err());

        // History and the object list are readable after the delete.
        assert_eq!(history_inner(&ledger, Kind::Strategy, "strategies/a.py").len(), 3);
        assert_eq!(objects_inner(&ledger, Kind::Strategy, false).len(), 0);
        let all = objects_inner(&ledger, Kind::Strategy, true);
        assert_eq!((all.len(), all[0].deleted, all[0].head_version), (1, true, 3));
    }

    #[test]
    fn binary_and_large_content_is_flagged_not_dumped() {
        let dir = tmp("lc-binary");
        let ledger = open(&dir);
        ledger.put(Kind::Notebook, "notebooks/bin", &[0xff, 0x00, 0xfe, 0x10], "user", "bytes").unwrap();
        let v = version_inner(&ledger, Kind::Notebook, "notebooks/bin", 1).unwrap();
        assert!(v.binary && v.text.is_none());

        let big = "line\n".repeat(MAX_TEXT_BYTES / 5 + 100);
        ledger.put(Kind::Estimator, "big.py", big.as_bytes(), "user", "large").unwrap();
        let v = version_inner(&ledger, Kind::Estimator, "big.py", 1).unwrap();
        assert!(v.truncated && !v.binary);
        assert_eq!(v.text.unwrap().len(), MAX_TEXT_BYTES);
        // The stored copy is complete.
        assert_eq!(ledger.content(Kind::Estimator, "big.py", 1).unwrap().len(), big.len());
    }

    #[test]
    fn a_version_the_ledger_does_not_hold_has_no_text() {
        let dir = tmp("lc-ref");
        let ledger = open(&dir);
        let hash = Ledger::sha256_hex(b"huge file");
        ledger.put_reference(Kind::Dataset, "big", &hash, 9, "user", "write-once file").unwrap();
        let v = version_inner(&ledger, Kind::Dataset, "big", 1).unwrap();
        assert!(!v.record.retained && v.text.is_none() && !v.binary && !v.truncated);
        assert_eq!(v.record.content_hash.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn the_tools_and_commands_have_no_way_to_erase() {
        // Command source: the only mutating function is the restore path.
        let src = include_str!("ledger_cmds.rs");
        let prod = src.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in ["fn ledger_delete", "fn ledger_purge", "fn ledger_edit", "fn ledger_clear", "fn ledger_truncate", ".tombstone(", "remove_dir_all", "fs::remove_file"] {
            assert!(!prod.contains(forbidden), "ledger_cmds must not contain `{forbidden}`");
        }
    }

    #[test]
    fn copilot_ledger_tools_are_well_formed_and_split_into_read_and_write() {
        let schemas = tool_schemas();
        let names: Vec<&str> = schemas.iter().filter_map(|s| s["function"]["name"].as_str()).collect();
        assert_eq!(names.len(), READ_TOOLS.len() + WRITE_TOOLS.len());
        for n in READ_TOOLS.iter().chain(WRITE_TOOLS) {
            assert!(names.contains(n), "{n} has no schema");
        }
        for s in &schemas {
            assert_eq!(s["type"], "function");
            assert_eq!(s["function"]["parameters"]["type"], "object");
            assert!(s["function"]["description"].as_str().unwrap().len() > 20);
            let props = s["function"]["parameters"]["properties"].as_object().unwrap();
            for required in s["function"]["parameters"]["required"].as_array().cloned().unwrap_or_default() {
                assert!(props.contains_key(required.as_str().unwrap()), "{s}");
            }
        }
        for write in WRITE_TOOLS {
            assert!(!READ_TOOLS.contains(write));
        }
        // The cards explain what a restore does.
        let card = describe("ledger_restore", &json!({"kind": "strategy", "id": "strategies/a.py", "version": 2})).unwrap();
        assert!(card.contains("NEW version") && card.contains("version 2") && card.contains("history are kept"), "{card}");
        assert!(describe("ledger_restore", &json!({"kind": "automation", "id": "a", "version": 1})).unwrap().contains("live"));
        for read in READ_TOOLS {
            assert!(describe(read, &json!({})).is_some(), "{read}");
        }
    }

    #[test]
    fn the_copilot_sees_a_trimmed_history_and_capped_text() {
        let dir = tmp("lc-copilot");
        let ledger = open(&dir);
        for i in 1..=60 {
            ledger.put(Kind::Recipe, "calm", format!("body {i}").as_bytes(), "copilot", &"s".repeat(400)).unwrap();
        }
        assert_eq!(limit_arg(&json!({"limit": 500}), 20), COPILOT_MAX_ITEMS);
        assert_eq!(limit_arg(&json!({}), 20), 20);
        assert_eq!(limit_arg(&json!({"limit": 0}), 20), 1);
        let h = history_for_copilot(&ledger, Kind::Recipe, "calm", limit_arg(&json!({"limit": 500}), 20));
        let versions = h["versions"].as_array().unwrap();
        assert_eq!(h["total_versions"], 60);
        assert_eq!(versions.len(), 50);
        assert_eq!(versions.last().unwrap()["version"], 60);
        assert_eq!(versions[0]["version"], 11, "the newest 50 are shown, oldest of those first");
        assert!(versions[0]["summary"].as_str().unwrap().chars().count() <= 161);
        assert_eq!(versions[0]["content_hash"].as_str().unwrap().len(), 12);
        assert_eq!(history_for_copilot(&ledger, Kind::Recipe, "none", 5)["total_versions"], 0);

        ledger.put(Kind::Estimator, "long.py", "z".repeat(20_000).as_bytes(), "user", "long").unwrap();
        let v = version_for_copilot(&ledger, Kind::Estimator, "long.py", 1).unwrap();
        assert_eq!(v["text"].as_str().unwrap().chars().count(), COPILOT_TEXT_CHARS + 1); // + the ellipsis
        assert_eq!(v["truncated"], true);
        ledger.put(Kind::Estimator, "short.py", b"x = 1", "user", "short").unwrap();
        let v = version_for_copilot(&ledger, Kind::Estimator, "short.py", 1).unwrap();
        assert_eq!((v["text"].as_str(), v["truncated"].as_bool()), (Some("x = 1"), Some(false)));
    }

    #[test]
    fn arguments_are_checked() {
        assert_eq!(version_arg(&json!({"version": 3})).unwrap(), 3);
        for bad in [json!({}), json!({"version": 0}), json!({"version": -1}), json!({"version": "2"}), json!({"version": 1.5})] {
            assert!(version_arg(&bad).is_err(), "{bad}");
        }
        assert!(str_arg(&json!({}), "kind").is_err());
    }

    #[test]
    fn order_events_reach_the_copilot_newest_first_and_capped() {
        let dir = tmp("lc-orders");
        let ledger = open(&dir);
        for i in 0..60 {
            ledger
                .append_event(
                    "orders",
                    json!({"order_id": format!("o{}", i % 2), "broker_id": "paper", "event": "status", "actor": "copilot",
                           "source": {"kind": "copilot", "id": "turn-1", "version": null, "hash": null}, "data": {"n": i}}),
                )
                .unwrap();
        }
        let all = order_events_for_copilot(&ledger, None, None, 500);
        assert_eq!(all["count"], 50);
        assert_eq!(all["events"][0]["data"].as_str().unwrap(), "{\"n\":59}");
        let one = order_events_for_copilot(&ledger, Some("o1"), Some("paper"), 5);
        assert_eq!(one["count"], 5);
        assert!(one["events"].as_array().unwrap().iter().all(|e| e["order_id"] == "o1"));
        assert_eq!(one["events"][0]["source"]["id"], "turn-1");
    }

    #[test]
    fn the_verify_report_for_the_copilot_is_short_and_states_the_limit() {
        let dir = tmp("lc-verify-report");
        let ledger = open(&dir);
        ledger.put(Kind::Strategy, "strategies/a.py", b"x", "user", "s").unwrap();
        let report = ledger.verify();
        let v = verify_for_copilot(&report);
        assert_eq!(v["ok"], true);
        assert_eq!(v["problem_count"], 0);
        assert!(v["limit"].as_str().unwrap().contains("middle of a log"));
    }

    // ── Restore dispatch, one test per kind ──

    /// A state whose files and ledger are private to the test. The process-wide ledger is shared by
    /// all tests, so these tests call the dispatch with the state's own ledger.
    async fn fixture(name: &str) -> (AppState, RuntimePaths, Ledger) {
        let (state, paths) = test_support::state(name).await;
        let ledger = crate::activity::ledger_for(paths.data_dir()).unwrap();
        (state, paths, ledger)
    }

    fn restore(state: &AppState, ledger: &Ledger, paths: &RuntimePaths, kind: Kind, id: &str, version: u32) -> Result<VersionRecord, String> {
        restore_dispatch(ledger, &state.automations, paths, kind, id, version, "user")
    }

    fn actions(ledger: &Ledger, kind: Kind, id: &str) -> Vec<(u32, String, String)> {
        ledger.history(kind, id).into_iter().map(|r| (r.version, r.action, r.actor)).collect()
    }

    #[tokio::test]
    async fn restoring_settings_is_refused_with_a_clear_message_and_nothing_changes() {
        let (state, paths, ledger) = fixture("lc-r-settings").await;
        ledger.put(Kind::Settings, "config/apex.toml", b"[risk]\nmax_daily_loss = 1.0\n", "system", "v1").unwrap();
        ledger.put(Kind::Settings, "config/apex.toml", b"[risk]\nmax_daily_loss = 2.0\n", "user", "v2").unwrap();
        let error = restore(&state, &ledger, &paths, Kind::Settings, "config/apex.toml", 1).unwrap_err();
        assert_eq!(error, "settings history is view-only; redacted values cannot be restored");
        assert_eq!(ledger.history(Kind::Settings, "config/apex.toml").len(), 2);
        // Viewing an old version still works.
        let v1 = version_inner(&ledger, Kind::Settings, "config/apex.toml", 1).unwrap();
        assert_eq!(v1.text.as_deref(), Some("[risk]\nmax_daily_loss = 1.0\n"));
        // An alert needs the running alert store, so the plain dispatch says so instead of guessing.
        assert!(restore(&state, &ledger, &paths, Kind::Alert, "a", 1).unwrap_err().contains("alert store"));
    }

    #[tokio::test]
    async fn restoring_a_strategy_appends_a_version_and_rewrites_the_file() {
        let (state, paths, ledger) = fixture("lc-r-strategy").await;
        let id = "strategies/restore_me.py";
        let file = paths.work_root().join(id);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        strategy::save_strategy_file_inner(&ledger, id, "def on_bar(): return 1\n".to_string(), &paths, "user").unwrap();
        strategy::save_strategy_file_inner(&ledger, id, "def on_bar(): return 2\n".to_string(), &paths, "copilot").unwrap();

        let rec = restore(&state, &ledger, &paths, Kind::Strategy, id, 1).unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.restored_from), (3, "restore", Some(1)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def on_bar(): return 1\n");
        // Versions 1 and 2 are untouched.
        assert_eq!(ledger.content(Kind::Strategy, id, 2).unwrap(), b"def on_bar(): return 2\n");
        assert_eq!(actions(&ledger, Kind::Strategy, id).len(), 3);
        // Version 99 does not exist.
        assert!(restore(&state, &ledger, &paths, Kind::Strategy, id, 99).is_err());
        assert_eq!(actions(&ledger, Kind::Strategy, id).len(), 3, "a failed restore adds nothing");
    }

    #[tokio::test]
    async fn restoring_an_estimator_a_recipe_and_a_notebook_each_appends_a_version() {
        let (state, paths, ledger) = fixture("lc-r-files").await;
        // Estimator
        let est = paths.work_root().join("estimators").join("mine.py");
        ml_estimators::write_file_audited(&ledger, paths.work_root(), "mine.py", "def make_estimator(params):\n    return 1\n", "user").unwrap();
        ml_estimators::write_file_audited(&ledger, paths.work_root(), "mine.py", "def make_estimator(params):\n    return 2\n", "user").unwrap();
        let rec = restore(&state, &ledger, &paths, Kind::Estimator, "mine.py", 1).unwrap();
        assert_eq!((rec.version, rec.action.as_str()), (3, "restore"));
        assert!(std::fs::read_to_string(&est).unwrap().contains("return 1"));

        // Notebook: written through the audited path, then restored.
        let nb_path = paths.work_root().join("notebooks").join("n.apexnb.json");
        std::fs::create_dir_all(nb_path.parent().unwrap()).unwrap();
        let id = "notebooks/n.apexnb.json".to_string();
        versioned_files::save(&ledger, Kind::Notebook, &id, &nb_path, b"{\"v\":1}", "user").unwrap();
        versioned_files::save(&ledger, Kind::Notebook, &id, &nb_path, b"{\"v\":2}", "user").unwrap();
        let rec = restore(&state, &ledger, &paths, Kind::Notebook, &id, 1).unwrap();
        assert_eq!((rec.version, rec.action.as_str()), (3, "restore"));
        assert_eq!(std::fs::read(&nb_path).unwrap(), b"{\"v\":1}");

        // Recipe
        let rp = paths.work_root().join("algorithms").join("calm.json");
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        versioned_files::save(&ledger, Kind::Recipe, "calm", &rp, b"{\"name\":\"calm\",\"n\":1}", "user").unwrap();
        versioned_files::save(&ledger, Kind::Recipe, "calm", &rp, b"{\"name\":\"calm\",\"n\":2}", "user").unwrap();
        let rec = restore(&state, &ledger, &paths, Kind::Recipe, "calm", 1).unwrap();
        assert_eq!((rec.version, rec.action.as_str()), (3, "restore"));
        assert!(std::fs::read_to_string(&rp).unwrap().contains("\"n\":1"));
    }

    #[tokio::test]
    async fn restoring_a_deleted_dataset_points_at_the_old_write_once_files() {
        let (state, paths, ledger) = fixture("lc-r-dataset").await;
        let dir = ml_datasets::datasets_dir(&paths);
        let info = |rows: usize| DatasetInfoDto {
            name: "ds".into(),
            path: "data/datasets/ds.csv".into(),
            rows,
            symbols: vec!["AAPL".into()],
            timeframe: "1d".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            source: "user".into(),
            ..Default::default()
        };
        let v1 = ml_datasets::write_dataset(&dir, "a,b\n1,2\n", &info(1)).unwrap();
        let v2 = ml_datasets::write_dataset(&dir, "a,b\n1,2\n3,4\n", &info(2)).unwrap();
        assert_eq!((v1.version, v2.version), (1, 2));
        assert!(ml_datasets::delete_in_as(&dir, "ds", "user").unwrap());

        let rec = restore(&state, &ledger, &paths, Kind::Dataset, "ds", 1).unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.restored_from), (4, "restore", Some(1)));
        let head = ml_datasets::read_info(&dir, "ds").unwrap();
        assert_eq!((head.rows, head.version), (1, 1), "the dataset reads as version 1 again");
        // Both files of both builds are still there and unchanged.
        assert_eq!(std::fs::read_to_string(dir.join("ds").join("v1.csv")).unwrap(), "a,b\n1,2\n");
        assert_eq!(std::fs::read_to_string(dir.join("ds").join("v2.csv")).unwrap(), "a,b\n1,2\n3,4\n");
        // The next build continues the numbering after the restore record.
        let v5 = ml_datasets::write_dataset(&dir, "a,b\n9,9\n", &info(5)).unwrap();
        assert_eq!(v5.version, 5);
    }

    #[tokio::test]
    async fn a_dataset_whose_file_changed_or_vanished_is_not_restored() {
        let (state, paths, ledger) = fixture("lc-r-dataset-bad").await;
        let dir = ml_datasets::datasets_dir(&paths);
        let info = DatasetInfoDto { name: "ds2".into(), path: "data/datasets/ds2.csv".into(), rows: 1, timeframe: "1d".into(), created_at: "2026-01-01T00:00:00Z".into(), source: "user".into(), ..Default::default() };
        ml_datasets::write_dataset(&dir, "a\n1\n", &info).unwrap();
        ml_datasets::write_dataset(&dir, "a\n2\n", &info).unwrap();
        let csv = dir.join("ds2").join("v1.csv");
        // Make it writable and change it behind the ledger's back.
        let mut perm = std::fs::metadata(&csv).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&csv, perm).unwrap();
        std::fs::write(&csv, "a\n999\n").unwrap();
        let err = restore(&state, &ledger, &paths, Kind::Dataset, "ds2", 1).unwrap_err();
        assert!(err.contains("no longer matches"), "{err}");
        std::fs::remove_file(&csv).unwrap();
        let err = restore(&state, &ledger, &paths, Kind::Dataset, "ds2", 1).unwrap_err();
        assert!(err.contains("not readable"), "{err}");
        assert_eq!(ledger.history(Kind::Dataset, "ds2").len(), 2, "nothing was added");
    }

    #[tokio::test]
    async fn restoring_a_deleted_model_brings_it_back_as_a_new_version() {
        let (state, paths, ledger) = fixture("lc-r-model").await;
        std::fs::create_dir_all(paths.models_dir()).unwrap();
        let id = "model_restore_1";
        let meta = serde_json::json!({
            "model_id": id, "algorithm": "random_forest", "display_name": "first", "feature_names": ["f1"],
            "target_column": "label", "data_path": "data/d.csv", "metrics": {"accuracy": 0.5},
            "model_file": format!("{id}.joblib"), "created_utc": "2026-01-01T00:00:00Z"
        });
        std::fs::write(paths.models_dir().join(format!("{id}.json")), meta.to_string()).unwrap();
        std::fs::write(paths.models_dir().join(format!("{id}.joblib")), b"model bytes").unwrap();
        // Lazily registered as version 1 by the listing.
        ml::load_registered_models(paths.models_dir()).unwrap();
        assert_eq!(ledger.history(Kind::Model, id).len(), 1);
        ml::delete_model_files(paths.models_dir(), id, "user").unwrap();
        assert!(ledger.head(Kind::Model, id).unwrap().is_tombstone());

        let rec = restore(&state, &ledger, &paths, Kind::Model, id, 1).unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.restored_from), (3, "restore", Some(1)));
        assert!(paths.models_dir().join(format!("{id}.json")).is_file());
        assert_eq!(std::fs::read(paths.models_dir().join(format!("{id}.joblib"))).unwrap(), b"model bytes", "the artifact came back unchanged");
    }

    #[tokio::test]
    async fn restoring_an_automation_puts_the_rule_back_and_keeps_every_version() {
        let (state, paths, ledger) = fixture("lc-r-automation").await;
        let dto = automation_cmds::CreateAutomationDto {
            name: "restore rule".into(),
            kind: "model_signal".into(),
            symbol: "TCS.NS".into(),
            model_id: "m1".into(),
            interval_secs: 300,
            quantity: 1.0,
            threshold: 0.5,
            broker_id: "paper".into(),
            allow_short: false,
        };
        let created = automation_cmds::create_automation_inner(dto, &state).unwrap();
        let id = created.id.clone();
        // The engine's hook writes to the process-wide ledger, which may not be this fixture's.
        let hooked = ledger::global().unwrap_or(&ledger).clone();
        assert!(hooked.head(Kind::Automation, &id).is_some(), "creating a rule is recorded");
        state.automations.set_enabled(&id, !created.enabled);
        assert!(hooked.history(Kind::Automation, &id).len() >= 2);

        let rec = restore_dispatch(&hooked, &state.automations, &paths, Kind::Automation, &id, 1, "user").unwrap();
        assert_eq!(rec.action, "restore");
        assert_eq!(rec.restored_from, Some(1));
        let rule = state.automations.list().into_iter().find(|r| r.id == id).unwrap();
        assert_eq!(rule.enabled, created.enabled, "the rule is back to the state of version 1");
        assert!(restore_dispatch(&hooked, &state.automations, &paths, Kind::Automation, &id, 77, "user").is_err());
    }

    #[test]
    fn a_restore_of_a_live_automation_is_flagged_by_the_current_rule_or_the_target_version() {
        use apex_core::application::automation::AutomationRule;
        let dir = tmp("lc-live");
        let ledger = open(&dir);
        let engine = AutomationEngine::new(dir.0.join("automations.json"));
        let rule = |id: &str, broker: &str| {
            let mut r: AutomationRule = serde_json::from_value(json!({
                "id": id, "name": id, "kind": "model_signal", "symbol": "TCS.NS", "model_id": "m",
                "interval_secs": 300, "quantity": 1.0, "threshold": 0.5, "broker_id": broker,
                "enabled": false, "allow_short": false, "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap();
            r.id = id.into();
            r
        };
        let paper_now = rule("paper-now", "paper");
        let live_now = rule("live-now", "zerodha");
        engine.apply_restored(paper_now.clone());
        engine.apply_restored(live_now);
        let old_live = rule("gone", "zerodha");
        ledger.put(Kind::Automation, "gone", serde_json::to_string(&old_live).unwrap().as_bytes(), "user", "v1").unwrap();
        ledger.put(Kind::Automation, "paper-now", serde_json::to_string(&paper_now).unwrap().as_bytes(), "user", "v1").unwrap();

        let live = |id: &str, v: u32| restore_involves_live_broker(Some(&ledger), &engine, &json!({"kind": "automation", "id": id, "version": v}));
        assert!(live("live-now", 1), "the rule trades live now");
        assert!(live("gone", 1), "a deleted rule whose version trades live");
        assert!(!live("paper-now", 1));
        assert!(!restore_involves_live_broker(Some(&ledger), &engine, &json!({"kind": "strategy", "id": "x", "version": 1})));
        assert!(restore_involves_live_broker(None, &engine, &json!({"kind": "automation", "id": "unknown", "version": 1})), "no ledger: be careful");
    }

    #[test]
    fn startup_reconcile_records_outside_edits_and_registers_old_objects() {
        let root = std::env::temp_dir().join(format!("apex-lc-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let paths = RuntimePaths::for_test(&root);
        let ledger = crate::activity::ledger_for(paths.data_dir()).unwrap();
        // A strategy that exists on disk and was never recorded, and an estimator edited later.
        let sdir = paths.work_root().join("strategies");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(sdir.join("old.py"), "print('old')\n").unwrap();
        let edir = paths.work_root().join("estimators");
        std::fs::create_dir_all(&edir).unwrap();
        ml_estimators::write_file_audited(&ledger, paths.work_root(), "e.py", "def make_estimator(params):\n    return 1\n", "user").unwrap();
        std::fs::write(edir.join("e.py"), "def make_estimator(params):\n    return 2\n").unwrap();

        let first = startup_reconcile(&ledger, &paths);
        assert!(first.recorded_changes() && first.files.adopted >= 1 && first.files.external_edits >= 1, "{first:?}");
        let old = actions(&ledger, Kind::Strategy, "strategies/old.py");
        assert_eq!(old.len(), 1);
        assert_eq!((old[0].1.as_str(), old[0].2.as_str()), ("external", "external"));
        let e = actions(&ledger, Kind::Estimator, "e.py");
        assert_eq!(e.iter().map(|(v, a, _)| (*v, a.as_str())).collect::<Vec<_>>(), vec![(1, "create"), (2, "external")]);
        // Running it again changes nothing.
        let again = startup_reconcile(&ledger, &paths);
        assert!(!again.recorded_changes(), "{again:?}");
        assert_eq!(actions(&ledger, Kind::Estimator, "e.py").len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The control plane (settings, credentials, vault, brokers, risk, live arming, alerts, ChatGPT) is in
/// the audit trail, and no secret reaches any ledger file, activity entry or log line.
#[cfg(test)]
mod control_plane_audit_tests {
    use super::*;
    use crate::activity::ActivityDto;
    use crate::commands::copilot_tools::test_support;
    use crate::commands::{adapters, alerts, brokers, chatgpt, live, risk, vault as vault_cmds};
    use crate::dto::{AppSettingsUpdateDto, SaveAdapterConfigRequest};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    const PASSPHRASE: &str = "CANARY-passphrase-one";
    const NEW_PASSPHRASE: &str = "CANARY-passphrase-two";
    const WRONG_PASSPHRASE: &str = "CANARY-wrong-passphrase";
    const API_KEY: &str = "CANARY-api-key-0001";
    const CLIENT_SECRET: &str = "CANARY-client-secret-0002";
    const ACCESS_TOKEN: &str = "CANARY-access-token-0003";
    const REQUEST_TOKEN: &str = "CANARY-request-token-0004";
    const PG_PASSWORD: &str = "CANARY-pg-password-0005";
    const URL_PASSWORD: &str = "CANARY-url-password-0006";
    const QUERY_KEY: &str = "CANARY-query-key-0007";
    const OAUTH_CODE: &str = "CANARY-oauth-code-0008";
    const EMAIL_LOCAL: &str = "canary.person.0009";
    const ALL: [&str; 11] = [
        PASSPHRASE, NEW_PASSPHRASE, WRONG_PASSPHRASE, API_KEY, CLIENT_SECRET, ACCESS_TOKEN, REQUEST_TOKEN, PG_PASSWORD, URL_PASSWORD, QUERY_KEY, OAUTH_CODE,
    ];

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    fn files_text(dir: &std::path::Path, out: &mut String) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                files_text(&path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }

    fn entries(paths: &RuntimePaths) -> Vec<ActivityDto> {
        crate::activity::read_from(ledger::global(), paths.data_dir(), 1000, None, None)
    }

    fn found<'a>(all: &'a [ActivityDto], action: &str, ok: bool) -> Vec<&'a ActivityDto> {
        all.iter().filter(|e| e.action == action && e.ok == ok).collect()
    }

    fn settings_request(paths: &RuntimePaths, edit: impl FnOnce(&mut serde_json::Value)) -> AppSettingsUpdateDto {
        let cfg = crate::config::AppConfig::load(paths).unwrap();
        let mut value = serde_json::json!({
            "general": { "data_dir": cfg.general.data_dir },
            "market_data": { "adapter": cfg.market_data.adapter },
            "execution": { "adapter": cfg.execution.adapter },
            "risk": { "max_daily_loss": cfg.risk.max_daily_loss, "max_order_value": cfg.risk.max_order_value },
            "storage": {
                "backend": "sqlite", "sqlite_path": cfg.storage.sqlite_path, "postgres_url": cfg.storage.postgres_url.clone().unwrap_or_default(),
                "wal_mode": cfg.storage.wal_mode, "pool_size": cfg.storage.pool_size,
            },
        });
        edit(&mut value);
        serde_json::from_value(value).unwrap()
    }

    fn adapter_request(settings: &[(&str, &str)], secrets: &[(&str, &str)], clear: &[&str]) -> SaveAdapterConfigRequest {
        let map = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>();
        SaveAdapterConfigRequest {
            adapter_id: "binance".into(),
            enabled: true,
            settings: map(settings),
            secrets: map(secrets),
            clear_secrets: clear.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn every_control_plane_action_is_recorded_and_no_secret_reaches_any_ledger_file_or_log() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt().with_writer(capture.clone()).with_max_level(tracing::Level::TRACE).with_ansi(false).finish();
        let _logs = tracing::subscriber::set_default(subscriber);

        let (state, paths) = test_support::state("cp-canary").await;
        let ledger = ledger::global().expect("the test state installs the process-wide ledger").clone();
        // Never reach the network from a test: sign-out talks to a closed local port.
        state.chatgpt.set_endpoints(crate::chatgpt_auth::AuthEndpoints {
            issuer: "http://127.0.0.1:1".into(),
            authorize: "http://127.0.0.1:1/a".into(),
            token: "http://127.0.0.1:1/t".into(),
            discovery: "http://127.0.0.1:1/d".into(),
        });

        // Vault: a failed unlock before it exists, create it, lock, two failures, unlock, change.
        assert!(vault_cmds::vault_unlock_as(&state, WRONG_PASSPHRASE, "user").await.is_err());
        vault_cmds::vault_init_as(&state, PASSPHRASE, "user").await.unwrap();
        vault_cmds::vault_lock_as(&state, "user").await.unwrap();
        assert!(vault_cmds::vault_unlock_as(&state, WRONG_PASSPHRASE, "user").await.is_err());
        assert!(vault_cmds::vault_unlock_as(&state, WRONG_PASSPHRASE, "user").await.is_err());
        // Locked: the Zerodha login is refused before any network call.
        assert!(brokers::zerodha_login_as(&state, REQUEST_TOKEN.to_string(), "user").await.is_err());
        vault_cmds::vault_unlock_as(&state, PASSPHRASE, "user").await.unwrap();
        assert!(vault_cmds::vault_change_passphrase_as(&state, WRONG_PASSPHRASE, NEW_PASSPHRASE, "user").await.is_err());
        vault_cmds::vault_change_passphrase_as(&state, PASSPHRASE, NEW_PASSPHRASE, "user").await.unwrap();

        // Adapters: save with secrets, a refused save, disable, delete.
        adapters::save_adapter_config_as(&state, adapter_request(&[("testnet", "true")], &[("api_key", API_KEY), ("api_secret", CLIENT_SECRET)], &[]), "user").await.unwrap();
        assert!(adapters::save_adapter_config_as(&state, adapter_request(&[], &[("not_a_field", API_KEY)], &[]), "user").await.is_err());
        adapters::save_adapter_config_as(&state, adapter_request(&[], &[], &["api_secret"]), "user").await.unwrap();
        adapters::set_adapter_enabled_as(&state, "binance".into(), false, "user").await.unwrap();
        adapters::delete_adapter_config_as(&state, "binance".into(), "user").await.unwrap();

        // Brokers: no API key stored for Zerodha, so the login stops before the network.
        let _ = brokers::set_broker_session_as(&state, "zerodha".into(), ACCESS_TOKEN.into(), "user").await;
        assert!(brokers::zerodha_login_as(&state, format!("http://127.0.0.1/?request_token={REQUEST_TOKEN}&action=login"), "user").await.is_err());
        let _ = brokers::clear_broker_session_as(&state, "zerodha".into(), "user").await;

        // Settings: a save with a database password and an API key in a URL query, and two refusals.
        let saved = crate::commands::settings::save_settings_audited(
            &state,
            &paths,
            settings_request(&paths, |v| {
                v["risk"]["max_daily_loss"] = serde_json::json!(12345.0);
                v["storage"]["postgres_url"] = serde_json::json!(format!("postgresql://apex:{PG_PASSWORD}@db.example.com:5432/apex"));
                v["llm"] = serde_json::json!({"active": "canary", "providers": [{
                    "id": "canary", "name": "Canary", "base_url": format!("https://llm.example/v1?api_key={QUERY_KEY}"),
                    "model": "m", "api_kind": "chat", "api_key_env": "CANARY_ENV", "max_tokens": 77 }]});
            }),
            "user",
        );
        saved.unwrap();
        let refused = crate::commands::settings::save_settings_audited(
            &state,
            &paths,
            settings_request(&paths, |v| {
                v["llm"] = serde_json::json!({"active": "", "providers": [{"id": "bad", "base_url": format!("http://apex:{URL_PASSWORD}@[bad"), "model": "m", "api_kind": "chat"}]});
            }),
            "user",
        );
        assert!(refused.is_err());
        assert!(crate::commands::settings::save_settings_audited(&state, &paths, settings_request(&paths, |v| v["risk"]["max_daily_loss"] = serde_json::json!(0.0)), "user").is_err());

        // Risk and live arming.
        risk::reset_halt_as(&state, "user");
        live::set_live_armed_as(&state, true, "user").unwrap();
        live::set_live_armed_as(&state, false, "user").unwrap();

        // Alerts, and a refused restore of a settings version.
        let rule = r#"{"DailyPnl":{"threshold":-100.0}}"#;
        alerts::add_alert_as(&state, "cp-canary-alert".into(), rule.into(), "user").await.unwrap();
        assert!(alerts::add_alert_as(&state, "cp-canary-alert-2".into(), "{not json".into(), "user").await.is_err());
        assert!(alerts::remove_alert_as(&state, "cp-canary-alert", "user").await.unwrap());
        restore_and_record(&state, &paths, Kind::Alert, "cp-canary-alert", 1, "user").await.unwrap();
        let view_only = restore_and_record(&state, &paths, Kind::Settings, "config/apex.toml", 1, "user").await.unwrap_err();
        assert_eq!(view_only, "settings history is view-only; redacted values cannot be restored");

        // ChatGPT: a stored account, sign-out, and the background sign-in outcomes.
        crate::chatgpt_auth::save_signed_in(
            &state.vault,
            "client",
            &crate::chatgpt_auth::IdentityClaims { sub: "sub".into(), email: Some(format!("{EMAIL_LOCAL}@Canary-Domain.example")) },
            &crate::chatgpt_auth::TokenSet { access_token: ACCESS_TOKEN.into(), refresh_token: Some(API_KEY.into()), id_token: None, expires_in: Some(3600), scope: None },
        )
        .unwrap();
        chatgpt::sign_out_audited(&state, "user").await.unwrap();
        let status = |state: &str, email: Option<&str>, message: Option<String>| crate::chatgpt_auth::ChatGptStatus { state: state.into(), email: email.map(String::from), plan_usage: true, message };
        chatgpt::SIGN_IN_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
        chatgpt::note_sign_in_outcome(&state, &status("connected", Some(&format!("{EMAIL_LOCAL}@Canary-Domain.example")), None));
        chatgpt::SIGN_IN_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
        chatgpt::note_sign_in_outcome(&state, &status("error", None, Some(format!("exchange failed: https://auth.openai.com/cb?code={OAUTH_CODE}&state=s"))));

        // ── What the trail says ──
        let all = entries(&paths);
        let one = |action: &str, ok: bool| {
            let list = found(&all, action, ok);
            assert!(!list.is_empty(), "no {action} entry with ok={ok}");
            list[0].clone()
        };
        for action in [
            "vault.init", "vault.lock", "vault.unlock", "vault.change_passphrase", "adapter.save", "adapter.enable", "adapter.delete", "settings.save", "risk.reset_halt",
            "live.arm", "live.disarm", "alert.create", "alert.delete", "alert.restore", "chatgpt.sign_out", "chatgpt.sign_in",
        ] {
            let e = one(action, true);
            assert_eq!(e.actor, "user", "{action}");
            assert_eq!(e.kind, "system", "{action}");
        }
        for action in ["vault.unlock", "vault.change_passphrase", "adapter.save", "settings.save", "alert.create", "settings.restore", "broker.login", "chatgpt.sign_in"] {
            one(action, false);
        }
        // Failed unlocks carry a count and nothing else.
        let failed_unlocks: Vec<&ActivityDto> = found(&all, "vault.unlock", false);
        assert!(failed_unlocks.len() >= 3);
        for e in &failed_unlocks {
            assert!(e.details.as_deref().unwrap_or("").starts_with("failed_attempts="), "{e:?}");
        }
        assert!(failed_unlocks.iter().any(|e| e.details.as_deref() == Some("failed_attempts=3")), "the count goes up with each failure in a row");
        assert_eq!(one("vault.unlock", true).details.as_deref(), Some("failed_attempts_before=3"));
        // Adapter entries name fields, not values.
        let saved_adapter = found(&all, "adapter.save", true).into_iter().find(|e| e.details.as_deref().unwrap_or("").contains("secrets_set=api_key,api_secret")).expect("field names are listed");
        assert_eq!(saved_adapter.target_id, "binance");
        assert!(saved_adapter.details.as_deref().unwrap().contains("settings_set=testnet"));
        assert!(found(&all, "adapter.save", true).iter().any(|e| e.details.as_deref().unwrap_or("").contains("secrets_cleared=api_secret")));
        assert!(one("adapter.delete", true).details.unwrap().contains("secrets_removed=api_key"));
        // The settings entry lists the changed keys and old -> new for plain values, never a secret.
        let settings_entry = found(&all, "settings.save", true).into_iter().find(|e| e.summary.contains("risk.max_daily_loss")).expect("settings entry");
        assert!(settings_entry.summary.contains("storage.postgres_url"), "{}", settings_entry.summary);
        let details = settings_entry.details.clone().unwrap();
        assert!(details.contains("risk.max_daily_loss: 50000.0 -> 12345.0"), "{details}");
        assert!(details.contains("storage.postgres_url: (empty) -> postgresql://apex:<redacted>@db.example.com:5432/apex"), "{details}");
        assert!(details.contains("llm.providers[canary].max_tokens: added 77"), "{details}");
        assert!(details.contains("?<query omitted>"), "{details}");
        // The settings object in the ledger is redacted.
        let head = ledger.head(Kind::Settings, "config/apex.toml").expect("settings are versioned");
        let head_text = String::from_utf8(ledger.content(Kind::Settings, "config/apex.toml", head.version).unwrap()).unwrap();
        assert!(head_text.contains("postgresql://apex:<redacted>@db.example.com:5432/apex") && head_text.contains("max_daily_loss = 12345.0"), "{head_text}");
        // The ChatGPT entries show the domain and not the person.
        let sign_in = found(&all, "chatgpt.sign_in", true).into_iter().find(|e| e.summary.contains("completed")).unwrap();
        assert!(sign_in.summary.contains("canary-domain.example") && !sign_in.summary.contains(EMAIL_LOCAL), "{}", sign_in.summary);
        assert!(one("chatgpt.sign_out", true).summary.contains("canary-domain.example"));
        // Alerts are objects with history.
        let versions: Vec<(u32, String)> = ledger.history(Kind::Alert, "cp-canary-alert").into_iter().map(|r| (r.version, r.action)).collect();
        assert_eq!(versions, [(1, "create".to_string()), (2, "delete".to_string()), (3, "restore".to_string())]);

        // ── No secret anywhere ──
        let mut everything = String::new();
        files_text(ledger.dir(), &mut everything);
        let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).to_string();
        assert!(!logs.is_empty(), "the log capture must be working for this check to mean anything");
        for canary in ALL {
            assert!(!everything.contains(canary), "{canary} reached a ledger file");
            assert!(!logs.contains(canary), "{canary} reached the log");
        }
        assert!(!everything.contains(EMAIL_LOCAL) && !logs.contains(EMAIL_LOCAL), "the account name reached the trail");
        let entries_text = serde_json::to_string(&all).unwrap();
        for canary in ALL {
            assert!(!entries_text.contains(canary), "{canary} reached an activity entry");
        }
        assert!(ledger.verify().ok, "the chain is still intact");
    }
}
