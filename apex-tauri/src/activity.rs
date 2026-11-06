//! Activity log: a durable record of what changed, who did it, and when.
//!
//! Every strategy, model and automation that is created, changed or deleted writes one entry to the
//! hash-chained "activity" stream of the audit ledger (`<data_dir>/ledger/streams/activity*.jsonl`),
//! whether the user did it in the UI or the Copilot did it through a tool call. Editing or deleting an
//! entry in the middle of the log breaks the chain and is reported by the ledger verify. Older
//! installs kept `<data_dir>/audit/activity.jsonl`; it is imported on first open and renamed to
//! `activity.jsonl.imported` (never deleted). The same call announces the change to the UI (`artifacts-changed`), so
//! open panels refresh instead of showing a list that was loaded once at startup.
//!
//! Details never contain secrets: callers pass a short summary and, for failures, the error text.

use crate::ledger::Ledger;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

const STREAM: &str = "activity";
const MAX_SUMMARY: usize = 300;
const MAX_DETAILS: usize = 2_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityDto {
    pub id: String,
    pub ts: String,
    /// "user" | "copilot" | "automation" | "system"
    pub actor: String,
    /// e.g. "strategy.save", "model.train", "automation.create"
    pub action: String,
    /// "strategy" | "model" | "automation" | "dataset" | "order" | "system"
    pub kind: String,
    pub target_id: String,
    pub summary: String,
    pub ok: bool,
    pub details: Option<String>,
}

/// Payload of the `artifacts-changed` event.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactEventDto {
    pub kind: String,
    pub id: String,
    pub action: String,
    pub actor: String,
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// The pre-ledger log. Only read as a legacy source and imported.
#[allow(dead_code)]
pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("audit").join("activity.jsonl")
}

const LEGACY_FILES: [&str; 2] = ["activity.1.jsonl", "activity.jsonl"];

static LEDGERS: Mutex<Option<HashMap<PathBuf, Ledger>>> = Mutex::new(None);

/// The ledger under `<data_dir>/ledger`, opened once per process (the first open imports the legacy
/// activity log). The app installs the same instance as the process-wide ledger.
pub fn ledger_for(data_dir: &Path) -> Result<Ledger, String> {
    let dir = data_dir.join("ledger");
    let mut guard = LEDGERS.lock().unwrap_or_else(|p| p.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(l) = cache.get(&dir) {
        if dir.is_dir() {
            return Ok(l.clone());
        }
    }
    let ledger = Ledger::open(&dir)?;
    match import_legacy(&ledger, data_dir) {
        Ok(0) => {}
        Ok(n) => tracing::info!(entries = n, "imported the legacy activity log into the audit ledger"),
        Err(error) => tracing::warn!(%error, "could not import the legacy activity log; it stays where it is and is still read"),
    }
    cache.insert(dir, ledger.clone());
    Ok(ledger)
}

fn legacy_paths(data_dir: &Path) -> Vec<PathBuf> {
    LEGACY_FILES.iter().map(|n| data_dir.join("audit").join(n)).filter(|p| p.is_file()).collect()
}

fn legacy_entries(paths: &[PathBuf]) -> Vec<ActivityDto> {
    let mut out = Vec::new();
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            let text = String::from_utf8_lossy(&bytes);
            out.extend(text.lines().filter_map(|l| serde_json::from_str::<ActivityDto>(l).ok()));
        }
    }
    out
}

/// Copy the entries of the old activity files into the stream, in order, then rename the files to
/// `*.imported`. Nothing is deleted. Entries already in the stream (an earlier import that was cut
/// short) are skipped by id, so running this twice never duplicates. Returns the entries added.
pub fn import_legacy(ledger: &Ledger, data_dir: &Path) -> Result<usize, String> {
    let paths = legacy_paths(data_dir);
    if paths.is_empty() {
        return Ok(0);
    }
    let mut seen: HashSet<String> = ledger
        .read_events(STREAM, &|_| true, usize::MAX)
        .into_iter()
        .filter_map(|e| e.value.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();
    let mut added = 0;
    for entry in legacy_entries(&paths) {
        if !seen.insert(entry.id.clone()) {
            continue;
        }
        let value = serde_json::to_value(&entry).map_err(|e| e.to_string())?;
        ledger.append_event(STREAM, value)?;
        added += 1;
    }
    ledger.flush_stream(STREAM)?;
    for path in &paths {
        let mut target = PathBuf::from(format!("{}.imported", path.display()));
        let mut n = 1;
        while target.exists() {
            target = PathBuf::from(format!("{}.imported.{n}", path.display()));
            n += 1;
        }
        std::fs::rename(path, &target).map_err(|e| format!("imported, but could not rename {}: {e}", path.display()))?;
    }
    Ok(added)
}

fn prepare(entry: &ActivityDto) -> ActivityDto {
    let mut entry = entry.clone();
    entry.summary = clip(&entry.summary, MAX_SUMMARY);
    entry.details = entry.details.map(|d| clip(&d, MAX_DETAILS));
    entry
}

/// Append one entry to the activity stream of a ledger.
pub fn append_to(ledger: &Ledger, entry: &ActivityDto) -> Result<(), String> {
    let value = serde_json::to_value(prepare(entry)).map_err(|e| format!("could not encode activity entry: {e}"))?;
    ledger.append_event(STREAM, value).map(|_| ())
}

/// Append one entry to the ledger under `data_dir`.
pub fn append(data_dir: &Path, entry: &ActivityDto) -> Result<(), String> {
    append_to(&ledger_for(data_dir)?, entry)
}

fn dto_of(event: crate::ledger::EventRecord) -> Option<ActivityDto> {
    let mut value = event.value;
    value.as_object_mut()?.insert("ts".into(), serde_json::Value::String(event.ts));
    serde_json::from_value(value).ok()
}

/// Newest entries first, optionally filtered by kind and/or target, from a ledger plus any legacy
/// files that are still waiting to be imported.
pub fn read_from(ledger: Option<&Ledger>, legacy_dir: &Path, limit: usize, kind: Option<&str>, target_id: Option<&str>) -> Vec<ActivityDto> {
    let limit = limit.clamp(1, 1000);
    let matches = |kind_: &str, target: &str| kind.map(|k| kind_ == k).unwrap_or(true) && target_id.map(|t| target == t).unwrap_or(true);
    let filter = |v: &serde_json::Value| {
        matches(v.get("kind").and_then(|x| x.as_str()).unwrap_or(""), v.get("target_id").and_then(|x| x.as_str()).unwrap_or(""))
    };
    let from_stream: Vec<ActivityDto> = ledger.map(|l| l.read_events(STREAM, &filter, limit)).unwrap_or_default().into_iter().filter_map(dto_of).collect();
    let legacy = legacy_entries(&legacy_paths(legacy_dir));
    if legacy.is_empty() {
        return from_stream;
    }
    // A failed or interrupted import: merge the old entries in by time, once each (stable for equal times).
    let in_stream: HashSet<&str> = from_stream.iter().map(|e| e.id.as_str()).collect();
    let mut all: Vec<ActivityDto> = legacy.into_iter().filter(|e| !in_stream.contains(e.id.as_str())).collect();
    all.extend(from_stream.iter().rev().cloned());
    let when = |e: &ActivityDto| chrono::DateTime::parse_from_rfc3339(&e.ts).map(|t| t.timestamp_millis()).unwrap_or(i64::MIN);
    all.sort_by_key(when);
    all.reverse();
    all.into_iter().filter(|e| matches(&e.kind, &e.target_id)).take(limit).collect()
}

/// Newest entries first, optionally filtered by kind and/or target.
pub fn read(data_dir: &Path, limit: usize, kind: Option<&str>, target_id: Option<&str>) -> Vec<ActivityDto> {
    let ledger = ledger_for(data_dir).ok();
    read_from(ledger.as_ref(), data_dir, limit, kind, target_id)
}

fn data_dir(state: &AppState) -> Option<PathBuf> {
    state
        .app
        .get()
        .and_then(|app| app.try_state::<crate::commands::python_runtime::RuntimePaths>())
        .map(|paths| paths.data_dir().to_path_buf())
}

/// Whether an entry should fire `artifacts-changed` so open panels reload. Only a successful
/// change to a strategy, model, automation or dataset does; failures and read-only runs do not.
pub fn announces_change(kind: &str, ok: bool, informational: bool) -> bool {
    ok && !informational && matches!(kind, "strategy" | "model" | "automation" | "dataset")
}

/// Record an activity and tell the UI. Never fails the caller: an audit write problem is logged
/// but must not undo or block the action that was just performed.
#[allow(clippy::too_many_arguments)]
pub fn record(
    state: &AppState,
    actor: &str,
    action: &str,
    kind: &str,
    target_id: &str,
    summary: &str,
    ok: bool,
    details: Option<String>,
) {
    record_with(state, actor, action, kind, target_id, summary, ok, details, false);
}

/// Like [`record`] for entries that describe something that happened without changing the
/// artifact (a backtest run, a session ending). It is logged and shown in the activity feed
/// (`activity-added`) but does not fire `artifacts-changed`, so lists are not reloaded for nothing.
#[allow(clippy::too_many_arguments)]
pub fn record_info(
    state: &AppState,
    actor: &str,
    action: &str,
    kind: &str,
    target_id: &str,
    summary: &str,
    ok: bool,
    details: Option<String>,
) {
    record_with(state, actor, action, kind, target_id, summary, ok, details, true);
}

#[allow(clippy::too_many_arguments)]
fn record_with(
    state: &AppState,
    actor: &str,
    action: &str,
    kind: &str,
    target_id: &str,
    summary: &str,
    ok: bool,
    details: Option<String>,
    informational: bool,
) {
    let entry = ActivityDto {
        id: uuid::Uuid::new_v4().to_string(),
        ts: chrono::Utc::now().to_rfc3339(),
        actor: actor.to_string(),
        action: action.to_string(),
        kind: kind.to_string(),
        target_id: target_id.to_string(),
        summary: summary.to_string(),
        ok,
        details,
    };
    let written = match crate::ledger::global() {
        Some(ledger) => Some(append_to(ledger, &entry)),
        None => data_dir(state).map(|dir| append(&dir, &entry)),
    };
    if let Some(Err(error)) = written {
        tracing::warn!(%error, action, "could not write activity log");
    }
    if let Some(app) = state.app.get() {
        let _ = app.emit("activity-added", &entry);
        if announces_change(kind, ok, informational) {
            let _ = app.emit(
                "artifacts-changed",
                ArtifactEventDto {
                    kind: kind.to_string(),
                    id: target_id.to_string(),
                    action: action.to_string(),
                    actor: actor.to_string(),
                },
            );
        }
    }
}

/// Recent activity, newest first. Filter by `kind` and/or `target_id` to audit one artifact.
#[tauri::command]
pub async fn list_activity(
    limit: Option<usize>,
    kind: Option<String>,
    target_id: Option<String>,
    runtime_paths: tauri::State<'_, crate::commands::python_runtime::RuntimePaths>,
) -> Result<Vec<ActivityDto>, String> {
    let (limit, kind, target_id) = (limit.unwrap_or(200), kind.as_deref(), target_id.as_deref());
    Ok(match crate::ledger::global() {
        Some(ledger) => read_from(Some(ledger), runtime_paths.data_dir(), limit, kind, target_id),
        None => read(runtime_paths.data_dir(), limit, kind, target_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: usize, kind: &str, target: &str) -> ActivityDto {
        ActivityDto {
            id: format!("id{n}"),
            ts: format!("2026-01-01T00:00:{n:02}Z"),
            actor: "copilot".into(),
            action: "strategy.save".into(),
            kind: kind.into(),
            target_id: target.into(),
            summary: format!("entry {n}"),
            ok: true,
            details: None,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("apex-activity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn entries_come_back_newest_first_and_filter_by_kind_and_target() {
        let dir = temp("order");
        append(&dir, &entry(1, "strategy", "a.py")).unwrap();
        append(&dir, &entry(2, "model", "m1")).unwrap();
        append(&dir, &entry(3, "strategy", "b.py")).unwrap();
        let all = read(&dir, 10, None, None);
        assert_eq!(all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["id3", "id2", "id1"]);
        assert_eq!(read(&dir, 10, Some("strategy"), None).len(), 2);
        assert_eq!(read(&dir, 10, None, Some("m1"))[0].id, "id2");
        assert_eq!(read(&dir, 1, None, None).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_successful_changes_to_artifacts_announce_themselves() {
        assert!(announces_change("strategy", true, false));
        assert!(announces_change("automation", true, false));
        assert!(announces_change("model", true, false));
        assert!(announces_change("dataset", true, false));
        assert!(!announces_change("strategy", false, false), "a failure changed nothing");
        assert!(!announces_change("strategy", true, true), "a backtest run is informational");
        assert!(!announces_change("order", true, false));
        assert!(!announces_change("system", true, false));
    }

    fn write_legacy(dir: &Path, name: &str, entries: &[ActivityDto], junk: bool) {
        std::fs::create_dir_all(dir.join("audit")).unwrap();
        let mut text: String = entries.iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect();
        if junk {
            text.push_str("not json\n");
        }
        std::fs::write(dir.join("audit").join(name), text).unwrap();
    }

    #[test]
    fn long_text_is_clipped() {
        let dir = temp("clip");
        let mut e = entry(1, "model", "m");
        e.summary = "s".repeat(1000);
        e.details = Some("d".repeat(5000));
        append(&dir, &e).unwrap();
        let got = read(&dir, 10, None, None);
        assert_eq!(got.len(), 1);
        assert!(got[0].summary.chars().count() <= MAX_SUMMARY + 1);
        assert!(got[0].details.as_ref().unwrap().chars().count() <= MAX_DETAILS + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_are_hash_chained_and_editing_one_is_detected() {
        let dir = temp("chain");
        for n in 1..=4 {
            append(&dir, &entry(n, "model", &format!("m{n}"))).unwrap();
        }
        let ledger = ledger_for(&dir).unwrap();
        assert!(ledger.verify().ok);
        let path = dir.join("ledger").join("streams").join("activity.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 4);
        std::fs::write(&path, text.replace("entry 2", "entry TWO")).unwrap();
        let report = ledger.verify();
        assert!(!report.ok);
        assert_eq!(report.problems[0].kind, "stream_hash");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_old_log_is_imported_in_order_without_losing_the_file() {
        let dir = temp("import");
        write_legacy(&dir, "activity.1.jsonl", &[entry(1, "model", "a")], false);
        write_legacy(&dir, "activity.jsonl", &[entry(2, "model", "b"), entry(3, "strategy", "c")], true);
        // Opening imports. New entries go after the imported ones.
        append(&dir, &entry(4, "model", "d")).unwrap();
        let ids: Vec<String> = read(&dir, 10, None, None).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["id4", "id3", "id2", "id1"]);
        let got = read(&dir, 10, Some("strategy"), None);
        assert_eq!((got.len(), got[0].ts.as_str()), (1, "2026-01-01T00:00:03Z"), "original timestamps are kept");
        // The old files are renamed, not deleted, and keep their content (including the line that was not JSON).
        assert!(!log_path(&dir).exists());
        let kept = std::fs::read_to_string(dir.join("audit").join("activity.jsonl.imported")).unwrap();
        assert!(kept.contains("entry 2") && kept.contains("not json"));
        assert!(dir.join("audit").join("activity.1.jsonl.imported").exists());
        assert!(ledger_for(&dir).unwrap().verify().ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interrupted_import_is_finished_without_duplicates() {
        let dir = temp("import-twice");
        write_legacy(&dir, "activity.jsonl", &[entry(1, "model", "a"), entry(2, "model", "b")], false);
        // Simulate a crash after the first entry was imported but before the rename.
        let ledger = Ledger::open(&dir.join("ledger")).unwrap();
        append_to(&ledger, &entry(1, "model", "a")).unwrap();
        // The unrenamed file is still shown, once.
        let ids: Vec<String> = read_from(Some(&ledger), &dir, 10, None, None).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["id2", "id1"]);
        assert_eq!(import_legacy(&ledger, &dir).unwrap(), 1);
        let ids: Vec<String> = read_from(Some(&ledger), &dir, 10, None, None).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["id2", "id1"]);
        assert_eq!(import_legacy(&ledger, &dir).unwrap(), 0);
        assert!(ledger.verify().ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_keep_one_valid_chain() {
        let dir = temp("concurrent");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for n in 0..100 {
                        let mut e = entry(n % 60, "model", &format!("t{t}-{n}"));
                        e.details = Some("x".repeat(1500));
                        append(&dir, &e).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(read(&dir, 1000, None, None).len(), 8 * 100, "every entry reads back");
        let report = ledger_for(&dir).unwrap().verify();
        assert!(report.ok, "{:?}", report.problems);
        assert_eq!(report.checked_events, 800);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
