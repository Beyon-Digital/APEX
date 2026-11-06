//! Activity log: a durable record of what changed, who did it, and when.
//!
//! Every strategy, model and automation that is created, changed or deleted writes one line to
//! `<data_dir>/audit/activity.jsonl`, whether the user did it in the UI or the Copilot did it
//! through a tool call. The same call announces the change to the UI (`artifacts-changed`), so
//! open panels refresh instead of showing a list that was loaded once at startup.
//!
//! Details never contain secrets: callers pass a short summary and, for failures, the error text.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};

const MAX_FILE_BYTES: u64 = 5_000_000;
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

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("audit").join("activity.jsonl")
}

/// Held while appending so two writers cannot interleave inside a line or both rotate the file
/// (the second rename would replace the previous history with the fresh, nearly empty log).
static APPEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Append one entry (rotating the file when it grows large).
pub fn append(data_dir: &Path, entry: &ActivityDto) -> Result<(), String> {
    let _guard = APPEND_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = log_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create audit directory: {e}"))?;
    }
    if std::fs::metadata(&path).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_file_name("activity.1.jsonl"));
    }
    let mut entry = entry.clone();
    entry.summary = clip(&entry.summary, MAX_SUMMARY);
    entry.details = entry.details.map(|d| clip(&d, MAX_DETAILS));
    let mut line = serde_json::to_string(&entry).map_err(|e| format!("could not encode activity entry: {e}"))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("could not open activity log: {e}"))?;
    file.write_all(line.as_bytes()).map_err(|e| format!("could not write activity log: {e}"))
}

/// Newest entries first, optionally filtered by kind and/or target.
pub fn read(data_dir: &Path, limit: usize, kind: Option<&str>, target_id: Option<&str>) -> Vec<ActivityDto> {
    let mut entries: Vec<ActivityDto> = Vec::new();
    // Older rotated file first so the combined order stays chronological.
    for name in ["activity.1.jsonl", "activity.jsonl"] {
        let path = data_dir.join("audit").join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            entries.extend(text.lines().filter_map(|l| serde_json::from_str::<ActivityDto>(l).ok()));
        }
    }
    entries.reverse();
    entries
        .into_iter()
        .filter(|e| kind.map(|k| e.kind == k).unwrap_or(true))
        .filter(|e| target_id.map(|t| e.target_id == t).unwrap_or(true))
        .take(limit.clamp(1, 1000))
        .collect()
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
    if let Some(dir) = data_dir(state) {
        if let Err(error) = append(&dir, &entry) {
            tracing::warn!(%error, action, "could not write activity log");
        }
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
    Ok(read(
        runtime_paths.data_dir(),
        limit.unwrap_or(200),
        kind.as_deref(),
        target_id.as_deref(),
    ))
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

    #[test]
    fn long_text_is_clipped_and_bad_lines_are_skipped() {
        let dir = temp("clip");
        let mut e = entry(1, "model", "m");
        e.summary = "s".repeat(1000);
        e.details = Some("d".repeat(5000));
        append(&dir, &e).unwrap();
        std::fs::OpenOptions::new().append(true).open(log_path(&dir)).unwrap().write_all(b"not json\n").unwrap();
        let got = read(&dir, 10, None, None);
        assert_eq!(got.len(), 1);
        assert!(got[0].summary.chars().count() <= MAX_SUMMARY + 1);
        assert!(got[0].details.as_ref().unwrap().chars().count() <= MAX_DETAILS + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_log_rotates_and_both_files_are_read() {
        let dir = temp("rotate");
        append(&dir, &entry(1, "model", "old")).unwrap();
        let big = std::fs::OpenOptions::new().append(true).open(log_path(&dir)).unwrap();
        big.set_len(MAX_FILE_BYTES + 10).unwrap();
        append(&dir, &entry(2, "model", "new")).unwrap();
        assert!(dir.join("audit").join("activity.1.jsonl").exists());
        let ids: Vec<String> = read(&dir, 10, None, None).into_iter().map(|e| e.target_id).collect();
        assert_eq!(ids.first().map(String::as_str), Some("new"));
        assert!(ids.contains(&"old".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_never_interleave_inside_a_line() {
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
        let text = std::fs::read_to_string(log_path(&dir)).unwrap();
        assert_eq!(text.lines().count(), 8 * 100);
        assert_eq!(read(&dir, 1000, None, None).len(), 8 * 100, "every line parses");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
