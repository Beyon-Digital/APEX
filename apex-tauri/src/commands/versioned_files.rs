//! Shared pattern for the file-backed audited objects: strategies, estimators, algorithm recipes
//! and notebooks.
//!
//! The ledger is the source of truth; the file under strategies/, estimators/, algorithms/ or
//! notebooks/ is a mirror of the head version that the runner and trainer read. Every write goes
//! through `ledger::save_working_copy` (write `<file>.apexnew`, ledger put, atomic rename), a delete
//! is a tombstone followed by removing the file, and a restore appends a new version.
//!
//! Anything that changes a file behind the app's back (an editor, `git checkout`) is picked up by
//! [`reconcile_kind`] / [`reconcile_all`] and recorded as a version by actor "external". The limit of
//! that: it sees the file as it is when looked at, so two external edits between two looks are one
//! version, and an edit that keeps both size and mtime (to the nanosecond) is not noticed by the
//! cheap lazy check (the startup pass always hashes).

use super::ml_estimators;
use super::ml_recipes;
use super::python_runtime::RuntimePaths;
use crate::ledger::{self, Kind, Ledger, VersionRecord};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Serializes every save, delete, restore and reconcile so two of them cannot interleave on one file.
static FILE_OPS: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FILE_OPS.lock().unwrap_or_else(|p| p.into_inner())
}

// ── The ledger handle ────────────────────────────────────────────────

#[cfg_attr(test, allow(dead_code))]
const NO_LEDGER: &str = "The audit ledger is not available, so this change was not saved. Restart APEX; if this keeps happening check that the data folder is writable";

/// The process-wide ledger for a write. A write never goes ahead without one: an unrecorded change
/// to a strategy or model would be an audit gap. (Test builds fall back to a private temporary
/// ledger so tests that never start the app still exercise the real code path.)
pub(crate) fn ledger() -> Result<&'static Ledger, String> {
    if let Some(l) = ledger::global() {
        return Ok(l);
    }
    #[cfg(test)]
    {
        return Ok(test_ledger());
    }
    #[cfg(not(test))]
    Err(NO_LEDGER.to_string())
}

/// The process-wide ledger for a read-only pass (listing). `None` when it was not opened.
pub(crate) fn try_ledger() -> Option<&'static Ledger> {
    ledger::global()
}

#[cfg(test)]
fn test_ledger() -> &'static Ledger {
    static L: std::sync::OnceLock<Ledger> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("apex-versioned-files-shared-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Ledger::open(&dir).expect("open the shared test ledger")
    })
}

// ── Summaries ────────────────────────────────────────────────────────

/// Lines added and removed going from `old` to `new`, counted as a multiset of lines (cheap and
/// order-blind: a moved line counts as neither added nor removed).
pub(crate) fn line_delta(old: &str, new: &str) -> (usize, usize) {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for line in old.lines() {
        *counts.entry(line).or_insert(0) += 1;
    }
    for line in new.lines() {
        *counts.entry(line).or_insert(0) -= 1;
    }
    let removed = counts.values().filter(|n| **n > 0).map(|n| *n as usize).sum();
    let added = counts.values().filter(|n| **n < 0).map(|n| n.unsigned_abs() as usize).sum();
    (added, removed)
}

/// "Saved mom.py (+12 -3 lines)" / "Created mom.py (+20 lines)".
pub(crate) fn describe_change(name: &str, old: Option<&[u8]>, new: &[u8]) -> String {
    let new_text = String::from_utf8_lossy(new);
    let Some(old) = old else {
        return format!("Created {name} (+{} lines)", new_text.lines().count());
    };
    let (added, removed) = line_delta(&String::from_utf8_lossy(old), &new_text);
    let delta = match (added, removed) {
        (0, 0) => "same lines, different text".to_string(),
        (a, 0) => format!("+{a} lines"),
        (0, r) => format!("-{r} lines"),
        (a, r) => format!("+{a} -{r} lines"),
    };
    format!("Saved {name} ({delta})")
}

fn display_name(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

// ── Save, delete, restore ────────────────────────────────────────────

/// Write `bytes` as the next version of `id` and mirror it to `path`. Identical content is a no-op.
/// On a ledger failure the file keeps its previous content.
pub(crate) fn save(ledger: &Ledger, kind: Kind, id: &str, path: &Path, bytes: &[u8], actor: &str) -> Result<VersionRecord, String> {
    let _guard = lock();
    let old = fs::read(path).ok();
    let existed = old.is_some() && ledger.head(kind, id).map(|h| !h.is_tombstone()).unwrap_or(true);
    let summary = describe_change(display_name(id), if existed { old.as_deref() } else { None }, bytes);
    let record = ledger::save_working_copy(ledger, kind, id, path, bytes, actor, &summary)?;
    note_synced(path, &record);
    Ok(record)
}

/// Record a delete as a tombstone, then remove the working file. Returns whether a file was removed.
/// An edit made outside the app is recorded as a version first, so it stays in the history.
pub(crate) fn delete(ledger: &Ledger, kind: Kind, id: &str, path: &Path, actor: &str) -> Result<bool, String> {
    let _guard = lock();
    ledger::reconcile_file(ledger, kind, id, path)?;
    if fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    ledger.tombstone(kind, id, actor, &format!("Deleted {}", display_name(id)))?;
    forget(path);
    fs::remove_file(path).map_err(|e| {
        format!(
            "Recorded the delete of {} in the audit ledger, but the file could not be removed ({e}); it is recorded as re-created the next time APEX looks at it",
            display_name(id)
        )
    })?;
    Ok(true)
}

/// Make an older version the new head and rewrite the working copy to match. The restore record is
/// the new version; the working copy write adds no second one. Same order as a save: temp file,
/// ledger, rename.
pub(crate) fn restore(ledger: &Ledger, kind: Kind, id: &str, path: &Path, version: u32, actor: &str) -> Result<VersionRecord, String> {
    let _guard = lock();
    ledger::reconcile_file(ledger, kind, id, path)?;
    // Read (and hash-check) the bytes first so a version that cannot be restored fails before anything is written.
    let bytes = ledger.content(kind, id, version)?;
    let pending = ledger::pending_path(path)?;
    if pending.exists() {
        let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let aside = pending.with_extension(format!("apexnew.orphan-{stamp}"));
        fs::rename(&pending, &aside).map_err(|e| format!("could not set aside a leftover pending file: {e}"))?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let write = || -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&pending)?;
        f.write_all(&bytes)?;
        f.sync_all()
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&pending);
        format!("could not write {}: {e}", pending.display())
    })?;
    let (record, _) = match ledger.restore(kind, id, version, actor) {
        Ok(r) => r,
        Err(e) => {
            let _ = fs::remove_file(&pending);
            return Err(e);
        }
    };
    fs::rename(&pending, path).map_err(|e| {
        format!(
            "restored as version {} in the audit ledger, but the file {} could not be updated ({e}); it will be completed at next start",
            record.version,
            path.display()
        )
    })?;
    note_synced(path, &record);
    Ok(record)
}

// ── Where each kind lives ────────────────────────────────────────────

pub(crate) const RESTORABLE: [Kind; 4] = [Kind::Strategy, Kind::Estimator, Kind::Recipe, Kind::Notebook];

/// Relative path made of plain names only (no `..`, `.`, roots or prefixes).
fn plain_relative(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(p.to_path_buf())
}

/// The working file for a ledger object id, or None when the id could not have been produced by
/// the app (so it can never point outside the managed folders).
pub(crate) fn path_for_id(paths: &RuntimePaths, kind: Kind, id: &str) -> Option<PathBuf> {
    match kind {
        Kind::Strategy => {
            let rel = plain_relative(id.strip_prefix("strategies/")?)?;
            (rel.extension()? == "py").then(|| paths.strategies_dir().join(rel))
        }
        Kind::Estimator => {
            ml_estimators::validate_estimator_name(id).ok()?;
            Some(ml_estimators::estimators_dir(paths.work_root()).join(id))
        }
        Kind::Recipe => {
            ml_recipes::validate_recipe_name(id).ok()?;
            Some(ml_recipes::recipes_dir(paths.work_root()).join(format!("{id}.json")))
        }
        Kind::Notebook => {
            let rel = plain_relative(id.strip_prefix("notebooks/")?)?;
            rel.file_name()?.to_str()?.ends_with(".apexnb.json").then(|| paths.work_root().join("notebooks").join(rel))
        }
        _ => None,
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

fn walk_recursive(dir: &Path, rel: &str, keep: &dyn Fn(&str) -> bool, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        let id = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if meta.is_dir() {
            walk_recursive(&path, &id, keep, out);
        } else if meta.is_file() && keep(&name) {
            out.push((id, path));
        }
    }
}

/// (object id, file) for every file that is a live candidate of this kind. Symbolic links are skipped.
pub(crate) fn walk(paths: &RuntimePaths, kind: Kind) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    match kind {
        Kind::Strategy => {
            walk_recursive(paths.strategies_dir(), "strategies", &|n| n.ends_with(".py"), &mut out);
        }
        Kind::Notebook => {
            walk_recursive(&paths.work_root().join("notebooks"), "notebooks", &|n| n.ends_with(".apexnb.json"), &mut out);
        }
        Kind::Estimator => {
            let dir = ml_estimators::estimators_dir(paths.work_root());
            if !is_symlink(&dir) {
                walk_flat(&dir, &mut out, |name| ml_estimators::validate_estimator_name(name).ok().map(|_| name.to_string()));
            }
        }
        Kind::Recipe => {
            let dir = ml_recipes::recipes_dir(paths.work_root());
            if !is_symlink(&dir) {
                walk_flat(&dir, &mut out, |name| {
                    let stem = name.strip_suffix(".json")?;
                    ml_recipes::validate_recipe_name(stem).ok().map(|_| stem.to_string())
                });
            }
        }
        _ => {}
    }
    out.sort();
    out
}

fn walk_flat(dir: &Path, out: &mut Vec<(String, PathBuf)>, id_of: impl Fn(&str) -> Option<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(id) = id_of(&name) else { continue };
        let path = entry.path();
        if fs::symlink_metadata(&path).map(|m| m.is_file()).unwrap_or(false) {
            out.push((id, path));
        }
    }
}

// ── Reconcile ────────────────────────────────────────────────────────

/// What a reconcile pass did.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct ReconcileSummary {
    /// Files and ledger objects looked at (files skipped by the cache are not counted).
    pub checked: usize,
    /// Files the ledger had never seen, now recorded as version 1 by actor "external".
    pub adopted: usize,
    /// Files whose content changed outside the app, now a new "external" version.
    pub external_edits: usize,
    /// Files that disappeared outside the app, now recorded as deleted by actor "external".
    pub removed: usize,
    /// Saves interrupted after the ledger write whose rename was finished.
    pub completed_pending: usize,
    pub errors: Vec<String>,
}

impl ReconcileSummary {
    pub fn merge(&mut self, other: ReconcileSummary) {
        self.checked += other.checked;
        self.adopted += other.adopted;
        self.external_edits += other.external_edits;
        self.removed += other.removed;
        self.completed_pending += other.completed_pending;
        self.errors.extend(other.errors);
    }

    /// Whether the ledger gained a version.
    pub fn changed(&self) -> bool {
        self.adopted + self.external_edits + self.removed > 0
    }
}

/// (mtime in ns, size, head content hash) at the last time a file was seen in step with the ledger.
type Seen = (u128, u64, String);
static SEEN: Mutex<Option<HashMap<PathBuf, Seen>>> = Mutex::new(None);

fn stat_key(path: &Path) -> Option<(u128, u64)> {
    let meta = fs::metadata(path).ok()?;
    let nanos = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((nanos, meta.len()))
}

fn with_seen<R>(f: impl FnOnce(&mut HashMap<PathBuf, Seen>) -> R) -> R {
    let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

fn note_synced(path: &Path, head: &VersionRecord) {
    match (stat_key(path), head.content_hash.as_ref()) {
        (Some((mtime, size)), Some(hash)) if !head.is_tombstone() => {
            with_seen(|m| m.insert(path.to_path_buf(), (mtime, size, hash.clone())));
        }
        _ => forget(path),
    }
}

fn forget(path: &Path) {
    with_seen(|m| m.remove(path));
}

/// Whether size, mtime and the head hash are all what they were when the file last matched.
fn unchanged_since_seen(ledger: &Ledger, kind: Kind, id: &str, path: &Path) -> bool {
    let Some((mtime, size)) = stat_key(path) else { return false };
    let Some(seen) = with_seen(|m| m.get(path).cloned()) else { return false };
    if seen.0 != mtime || seen.1 != size {
        return false;
    }
    if ledger::pending_path(path).map(|p| p.exists()).unwrap_or(true) {
        return false;
    }
    ledger.head(kind, id).and_then(|h| h.content_hash).as_deref() == Some(seen.2.as_str())
}

fn reconcile_one(ledger: &Ledger, kind: Kind, id: &str, path: &Path, out: &mut ReconcileSummary) {
    out.checked += 1;
    let had_head = ledger.head(kind, id).is_some();
    match ledger::reconcile_file_detailed(ledger, kind, id, path) {
        Ok(outcome) => {
            if outcome.completed_pending {
                out.completed_pending += 1;
            }
            if let Some(why) = outcome.pending_left {
                tracing::warn!(id, "{why}");
            }
            if let Some(rec) = outcome.record {
                if !had_head {
                    out.adopted += 1;
                } else if rec.is_tombstone() {
                    out.removed += 1;
                } else {
                    out.external_edits += 1;
                }
            }
        }
        Err(error) => out.errors.push(format!("{} {id}: {error}", kind.as_str())),
    }
    if let Some(head) = ledger.head(kind, id) {
        note_synced(path, &head);
    }
}

/// Bring one kind in step with the ledger. `use_cache` skips files whose size, mtime and head hash
/// match the last look (the cheap path for listings); the startup pass hashes everything.
pub(crate) fn reconcile_kind(ledger: &Ledger, paths: &RuntimePaths, kind: Kind, use_cache: bool) -> ReconcileSummary {
    let _guard = lock();
    let mut out = ReconcileSummary::default();
    let files = walk(paths, kind);
    let mut seen_ids: HashSet<&str> = HashSet::new();
    for (id, path) in &files {
        seen_ids.insert(id.as_str());
        if use_cache && unchanged_since_seen(ledger, kind, id, path) {
            continue;
        }
        reconcile_one(ledger, kind, id, path, &mut out);
    }
    // Objects the ledger holds as live whose file is gone (or only exists as a pending save).
    for object in ledger.objects(kind, false) {
        if seen_ids.contains(object.object_id.as_str()) {
            continue;
        }
        let Some(path) = path_for_id(paths, kind, &object.object_id) else { continue };
        if fs::symlink_metadata(&path).is_ok() {
            continue; // present but skipped by the walk (a symbolic link)
        }
        // The lazy pass only reports a removal for a file this process saw in step with the ledger.
        // (Only in tests do several data folders share one ledger; without this, listing one folder
        // would mark another folder's strategies as removed. The startup pass checks everything.)
        if use_cache && !with_seen(|m| m.contains_key(&path)) {
            continue;
        }
        reconcile_one(ledger, kind, &object.object_id, &path, &mut out);
    }
    out
}

/// Startup entry point: walk strategies/, estimators/, algorithms/ and notebooks/, finish
/// interrupted saves, and record external edits, additions and removals as versions.
/// Call once at startup after the ledger is installed:
/// `commands::versioned_files::reconcile_all(&ledger, &runtime_paths)`.
#[allow(dead_code)]
pub fn reconcile_all(ledger: &Ledger, paths: &RuntimePaths) -> ReconcileSummary {
    let mut total = ReconcileSummary::default();
    for kind in RESTORABLE {
        total.merge(reconcile_kind(ledger, paths, kind, false));
    }
    total
}

/// The lazy pass the list commands run (cheap when nothing changed). Never fails the listing:
/// problems are logged. Does nothing when the ledger is not open.
pub(crate) fn reconcile_for_listing(paths: &RuntimePaths, kind: Kind) {
    let Some(ledger) = try_ledger() else { return };
    let summary = reconcile_kind(ledger, paths, kind, true);
    for error in &summary.errors {
        tracing::warn!("audit reconcile: {error}");
    }
    if summary.changed() {
        tracing::info!(kind = kind.as_str(), adopted = summary.adopted, edited = summary.external_edits, removed = summary.removed, "recorded changes made outside APEX");
    }
}

// ── Provenance ───────────────────────────────────────────────────────

/// The exact version of a strategy (or estimator) that ran.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provenance {
    pub version: u32,
    pub hash: String,
}

impl Provenance {
    /// `strategy_version=3 strategy_hash=<sha256>`, the text recorded in activity entries.
    pub fn details(&self, label: &str) -> String {
        format!("{label}_version={} {label}_hash={}", self.version, self.hash)
    }
}

/// The live head of an object after recording any unsaved external edit. `None` for an object the
/// ledger does not hold (or holds as deleted).
pub(crate) fn resolve_head(ledger: &Ledger, kind: Kind, id: &str, path: &Path) -> Result<Option<Provenance>, String> {
    let _guard = lock();
    ledger::reconcile_file(ledger, kind, id, path)?;
    Ok(ledger.head(kind, id).filter(|h| !h.is_tombstone()).and_then(|h| h.content_hash.map(|hash| Provenance { version: h.version, hash })))
}

/// Dispatch for the generic restore command: which working file a restore of `kind`/`id` rewrites.
#[allow(dead_code)]
pub fn restore_by_kind(ledger: &Ledger, paths: &RuntimePaths, kind: Kind, id: &str, version: u32, actor: &str) -> Result<VersionRecord, String> {
    let path = path_for_id(paths, kind, id).ok_or_else(|| format!("'{id}' is not a {} the app manages", kind.as_str()))?;
    restore(ledger, kind, id, &path, version, actor)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    /// A temp directory removed at the end of the test.
    pub(crate) struct Tmp(pub PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn tmp(name: &str) -> Tmp {
        let dir = std::env::temp_dir().join(format!("apex-vf-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }

    /// A private ledger for one test, next to (not inside) the work root.
    pub(crate) fn open(dir: &Tmp) -> Ledger {
        Ledger::open(&dir.0.join("ledger-store")).unwrap()
    }

    pub(crate) fn versions(l: &Ledger, kind: Kind, id: &str) -> Vec<(u32, String, String)> {
        l.history(kind, id).into_iter().map(|r| (r.version, r.action, r.actor)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sha(bytes: &[u8]) -> String {
        Ledger::sha256_hex(bytes)
    }

    #[test]
    fn line_delta_counts_added_and_removed_lines() {
        assert_eq!(line_delta("a\nb\nc\n", "a\nb\nc\n"), (0, 0));
        assert_eq!(line_delta("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(line_delta("", "x\ny\n"), (2, 0));
        assert_eq!(line_delta("x\nx\ny\n", "x\n"), (0, 2));
        assert_eq!(describe_change("mom.py", None, b"a\nb\n"), "Created mom.py (+2 lines)");
        assert_eq!(describe_change("mom.py", Some(b"a\nb\nc\n"), b"a\nB\nc\nd\n"), "Saved mom.py (+2 -1 lines)");
        assert_eq!(describe_change("mom.py", Some(b"a\n"), b"a\n\n"), "Saved mom.py (+1 lines)");
        assert_eq!(describe_change("mom.py", Some(b"a\nb\n"), b"b\na\n"), "Saved mom.py (same lines, different text)");
    }

    #[test]
    fn ids_map_to_files_inside_the_managed_folders_only() {
        let root = tmp("ids");
        let paths = RuntimePaths::for_test(&root.0);
        assert_eq!(path_for_id(&paths, Kind::Strategy, "strategies/a/b.py"), Some(root.0.join("strategies/a/b.py")));
        assert_eq!(path_for_id(&paths, Kind::Estimator, "mine.py"), Some(root.0.join("estimators/mine.py")));
        assert_eq!(path_for_id(&paths, Kind::Recipe, "ens"), Some(root.0.join("algorithms/ens.json")));
        assert_eq!(path_for_id(&paths, Kind::Notebook, "notebooks/x.apexnb.json"), Some(root.0.join("notebooks/x.apexnb.json")));
        for (kind, bad) in [
            (Kind::Strategy, "strategies/../../etc/x.py"),
            (Kind::Strategy, "strategies//abs.py"),
            (Kind::Strategy, "strategies/x.txt"),
            (Kind::Strategy, "x.py"),
            (Kind::Estimator, "../x.py"),
            (Kind::Estimator, "Bad Name.py"),
            (Kind::Recipe, "../x"),
            (Kind::Notebook, "notebooks/../x.apexnb.json"),
            (Kind::Notebook, "other/x.apexnb.json"),
            (Kind::Dataset, "anything"),
        ] {
            assert!(path_for_id(&paths, kind, bad).is_none(), "{kind:?} {bad:?}");
        }
    }

    #[test]
    fn each_save_is_a_new_version_the_file_matches_and_an_identical_save_is_a_noop() {
        let root = tmp("save");
        let l = open(&root);
        let file = root.0.join("strategies").join("mom.py");
        let id = "strategies/mom.py";
        let v1 = save(&l, Kind::Strategy, id, &file, b"a\nb\n", "user").unwrap();
        assert_eq!((v1.version, v1.action.as_str(), v1.actor.as_str()), (1, "create", "user"));
        assert_eq!(v1.summary, "Created mom.py (+2 lines)");
        assert_eq!(fs::read(&file).unwrap(), b"a\nb\n");
        let v2 = save(&l, Kind::Strategy, id, &file, b"a\nB\nc\n", "copilot").unwrap();
        assert_eq!((v2.version, v2.action.as_str(), v2.actor.as_str()), (2, "update", "copilot"));
        assert_eq!(v2.summary, "Saved mom.py (+2 -1 lines)");
        assert_eq!(fs::read(&file).unwrap(), b"a\nB\nc\n");
        let again = save(&l, Kind::Strategy, id, &file, b"a\nB\nc\n", "user").unwrap();
        assert_eq!(again, v2, "an identical save adds no version");
        assert_eq!(l.history(Kind::Strategy, id).len(), 2);
        // Older content is still readable: nothing was overwritten.
        assert_eq!(l.content(Kind::Strategy, id, 1).unwrap(), b"a\nb\n");
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no temp file left: {leftovers:?}");
    }

    #[test]
    fn delete_is_a_tombstone_and_recreating_continues_the_same_object() {
        let root = tmp("delete");
        let l = open(&root);
        let file = root.0.join("estimators").join("mine.py");
        save(&l, Kind::Estimator, "mine.py", &file, b"one", "user").unwrap();
        save(&l, Kind::Estimator, "mine.py", &file, b"two", "user").unwrap();
        assert!(delete(&l, Kind::Estimator, "mine.py", &file, "user").unwrap());
        assert!(!file.exists());
        assert!(!delete(&l, Kind::Estimator, "mine.py", &file, "user").unwrap(), "deleting twice removes nothing");
        assert_eq!(l.history(Kind::Estimator, "mine.py").len(), 3, "the second delete added nothing");
        assert!(l.head(Kind::Estimator, "mine.py").unwrap().is_tombstone());
        assert_eq!(l.head(Kind::Estimator, "mine.py").unwrap().actor, "user", "recorded by the app, not found later as an external delete");
        assert_eq!(l.content(Kind::Estimator, "mine.py", 1).unwrap(), b"one", "the history of a deleted object stays readable");
        let v4 = save(&l, Kind::Estimator, "mine.py", &file, b"three", "user").unwrap();
        assert_eq!((v4.version, v4.action.as_str(), v4.parent_version), (4, "create", Some(3)));
        assert_eq!(versions(&l, Kind::Estimator, "mine.py").iter().map(|v| v.0).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(fs::read(&file).unwrap(), b"three");
        // Deleting something the ledger never saw is a plain "nothing to delete".
        assert!(!delete(&l, Kind::Estimator, "ghost.py", &root.0.join("estimators/ghost.py"), "user").unwrap());
        assert!(l.head(Kind::Estimator, "ghost.py").is_none());
    }

    #[test]
    fn an_external_edit_becomes_an_external_version_on_the_next_look_and_before_a_delete() {
        let root = tmp("external");
        let paths = RuntimePaths::for_test(&root.0);
        let l = open(&root);
        let file = root.0.join("strategies").join("mom.py");
        save(&l, Kind::Strategy, "strategies/mom.py", &file, b"print(1)\n", "user").unwrap();
        // Nothing changed: a lazy pass looks at nothing.
        assert_eq!(reconcile_kind(&l, &paths, Kind::Strategy, true).checked, 0);

        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(&file, "print(2)\n").unwrap();
        let s = reconcile_kind(&l, &paths, Kind::Strategy, true);
        assert_eq!((s.checked, s.external_edits, s.adopted, s.removed), (1, 1, 0, 0), "{s:?}");
        let head = l.head(Kind::Strategy, "strategies/mom.py").unwrap();
        assert_eq!((head.version, head.action.as_str(), head.actor.as_str()), (2, "external", "external"));
        assert_eq!(head.content_hash.as_deref(), Some(sha(b"print(2)\n").as_str()));
        assert_eq!(reconcile_kind(&l, &paths, Kind::Strategy, true).checked, 0, "the cache remembers it");

        // A file the ledger has never seen is adopted; a removed one is recorded as deleted by external.
        fs::write(root.0.join("strategies").join("new.py"), "x = 1\n").unwrap();
        let s = reconcile_kind(&l, &paths, Kind::Strategy, true);
        assert_eq!((s.adopted, s.external_edits), (1, 0), "{s:?}");
        assert_eq!(versions(&l, Kind::Strategy, "strategies/new.py"), [(1, "external".into(), "external".into())]);
        fs::remove_file(root.0.join("strategies").join("new.py")).unwrap();
        let s = reconcile_kind(&l, &paths, Kind::Strategy, true);
        assert_eq!((s.removed, s.checked), (1, 1), "{s:?}");
        let gone = l.head(Kind::Strategy, "strategies/new.py").unwrap();
        assert!((gone.is_tombstone(), gone.actor.as_str()) == (true, "external"));

        // An external edit that is still unseen when the user deletes the file stays in the history.
        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(&file, "print(3)\n").unwrap();
        assert!(delete(&l, Kind::Strategy, "strategies/mom.py", &file, "user").unwrap());
        let h = versions(&l, Kind::Strategy, "strategies/mom.py");
        assert_eq!(h.iter().map(|v| (v.0, v.1.as_str())).collect::<Vec<_>>(), [(1, "create"), (2, "external"), (3, "external"), (4, "delete")]);
        assert_eq!(l.content(Kind::Strategy, "strategies/mom.py", 3).unwrap(), b"print(3)\n");
    }

    #[test]
    fn the_startup_pass_hashes_everything_and_walks_all_four_folders() {
        let root = tmp("startup");
        let paths = RuntimePaths::for_test(&root.0);
        let l = open(&root);
        for (rel, body) in [
            ("strategies/sub/a.py", "a"),
            ("estimators/e_one.py", "e"),
            ("algorithms/ens.json", "{}"),
            ("notebooks/n.apexnb.json", "{}"),
            ("estimators/Bad Name.py", "ignored"),
            ("strategies/readme.txt", "ignored"),
        ] {
            let p = root.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        let s = reconcile_all(&l, &paths);
        assert_eq!((s.adopted, s.external_edits, s.removed, s.errors.len()), (4, 0, 0, 0), "{s:?}");
        for (kind, id) in [
            (Kind::Strategy, "strategies/sub/a.py"),
            (Kind::Estimator, "e_one.py"),
            (Kind::Recipe, "ens"),
            (Kind::Notebook, "notebooks/n.apexnb.json"),
        ] {
            assert_eq!(l.head(kind, id).map(|h| h.version), Some(1), "{id}");
        }
        // A second pass finds nothing to do, even with the cache bypassed.
        let again = reconcile_all(&l, &paths);
        assert!(!again.changed() && again.errors.is_empty(), "{again:?}");
        // A live object whose file vanished is recorded as removed at startup.
        fs::remove_file(root.0.join("estimators/e_one.py")).unwrap();
        let s = reconcile_all(&l, &paths);
        assert_eq!(s.removed, 1, "{s:?}");
        assert!(l.head(Kind::Estimator, "e_one.py").unwrap().is_tombstone());
    }

    #[test]
    fn a_save_interrupted_after_the_ledger_write_is_completed_by_reconcile() {
        let root = tmp("crash");
        let paths = RuntimePaths::for_test(&root.0);
        let l = open(&root);
        let file = root.0.join("strategies").join("mom.py");
        let id = "strategies/mom.py";
        save(&l, Kind::Strategy, id, &file, b"old", "user").unwrap();
        // The app died after the ledger write and before the rename: the new bytes are only in the pending file.
        l.put(Kind::Strategy, id, b"new", "user", "interrupted save").unwrap();
        fs::write(ledger::pending_path(&file).unwrap(), b"new").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"old");

        let s = reconcile_all(&l, &paths);
        assert_eq!((s.completed_pending, s.external_edits, s.adopted), (1, 0, 0), "{s:?}");
        assert_eq!(fs::read(&file).unwrap(), b"new");
        assert!(!ledger::pending_path(&file).unwrap().exists());
        assert_eq!(l.history(Kind::Strategy, id).len(), 2, "completing the save adds no version");

        // Same when the file never existed (first save of a new strategy).
        let fresh = root.0.join("strategies").join("fresh.py");
        l.put(Kind::Strategy, "strategies/fresh.py", b"first", "user", "interrupted").unwrap();
        fs::write(ledger::pending_path(&fresh).unwrap(), b"first").unwrap();
        let s = reconcile_all(&l, &paths);
        assert_eq!(s.completed_pending, 1, "{s:?}");
        assert_eq!(fs::read(&fresh).unwrap(), b"first");
        assert_eq!(l.history(Kind::Strategy, "strategies/fresh.py").len(), 1);
    }

    #[test]
    fn restore_brings_old_content_back_as_exactly_one_new_version() {
        let root = tmp("restore");
        let l = open(&root);
        let file = root.0.join("algorithms").join("ens.json");
        save(&l, Kind::Recipe, "ens", &file, b"{\"v\":1}", "user").unwrap();
        save(&l, Kind::Recipe, "ens", &file, b"{\"v\":2}", "user").unwrap();
        let r = restore(&l, Kind::Recipe, "ens", &file, 1, "user").unwrap();
        assert_eq!((r.version, r.action.as_str(), r.restored_from), (3, "restore", Some(1)));
        assert_eq!(fs::read(&file).unwrap(), b"{\"v\":1}");
        assert_eq!(l.history(Kind::Recipe, "ens").len(), 3, "the working copy write added no second version");
        assert_eq!(l.content(Kind::Recipe, "ens", 2).unwrap(), b"{\"v\":2}", "the version that was replaced is still there");
        assert!(!ledger::pending_path(&file).unwrap().exists());

        // A restore after a delete brings the object back; a delete marker cannot be restored.
        delete(&l, Kind::Recipe, "ens", &file, "user").unwrap();
        assert!(!file.exists());
        assert!(restore(&l, Kind::Recipe, "ens", &file, 4, "user").unwrap_err().contains("delete marker"));
        assert!(!file.exists(), "a failed restore writes nothing");
        assert!(!ledger::pending_path(&file).unwrap().exists());
        let back = restore(&l, Kind::Recipe, "ens", &file, 2, "user").unwrap();
        assert_eq!((back.version, back.restored_from), (5, Some(2)));
        assert_eq!(fs::read(&file).unwrap(), b"{\"v\":2}");
        assert!(restore(&l, Kind::Recipe, "ens", &file, 99, "user").is_err());
    }

    #[test]
    fn restore_by_kind_only_touches_managed_files() {
        let root = tmp("restore-kind");
        let paths = RuntimePaths::for_test(&root.0);
        let l = open(&root);
        let file = root.0.join("notebooks").join("n.apexnb.json");
        save(&l, Kind::Notebook, "notebooks/n.apexnb.json", &file, b"one", "user").unwrap();
        save(&l, Kind::Notebook, "notebooks/n.apexnb.json", &file, b"two", "user").unwrap();
        restore_by_kind(&l, &paths, Kind::Notebook, "notebooks/n.apexnb.json", 1, "user").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"one");
        assert!(restore_by_kind(&l, &paths, Kind::Notebook, "notebooks/../../x.apexnb.json", 1, "user").is_err());
        assert!(restore_by_kind(&l, &paths, Kind::Model, "m", 1, "user").is_err());
    }

    #[test]
    fn a_ledger_failure_fails_the_save_and_leaves_the_old_file_untouched() {
        let root = tmp("failure");
        let l = open(&root);
        let file = root.0.join("strategies").join("mom.py");
        let id = "strategies/mom.py";
        save(&l, Kind::Strategy, id, &file, b"old", "user").unwrap();

        // Make new content impossible to store: no more entries can be added to the blob store.
        let blobs = root.0.join("ledger-store").join("blobs");
        let mut locked = vec![blobs.clone()];
        locked.extend(fs::read_dir(&blobs).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        for dir in &locked {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let unlock = |locked: &[PathBuf]| {
            for dir in locked {
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
            }
        };
        if fs::write(blobs.join("probe"), b"x").is_ok() {
            unlock(&locked);
            eprintln!("skipped: running with rights that ignore directory permissions");
            return;
        }

        let result = save(&l, Kind::Strategy, id, &file, b"brand new content", "user");
        let after = (fs::read(&file).unwrap(), ledger::pending_path(&file).unwrap().exists(), l.history(Kind::Strategy, id).len());
        unlock(&locked);

        assert!(result.is_err(), "the save must fail when the ledger cannot record it");
        assert_eq!(after.0, b"old", "the file keeps its previous content");
        assert!(!after.1, "no pending file is left");
        assert_eq!(after.2, 1, "no version was added");
    }

    #[test]
    fn provenance_pins_the_head_after_recording_an_unsaved_external_edit() {
        let root = tmp("prov");
        let l = open(&root);
        let file = root.0.join("strategies").join("mom.py");
        let id = "strategies/mom.py";
        assert_eq!(resolve_head(&l, Kind::Strategy, id, &file).unwrap(), None, "unknown file");
        save(&l, Kind::Strategy, id, &file, b"v1", "user").unwrap();
        let p = resolve_head(&l, Kind::Strategy, id, &file).unwrap().unwrap();
        assert_eq!((p.version, p.hash.as_str()), (1, sha(b"v1").as_str()));
        assert_eq!(p.details("strategy"), format!("strategy_version=1 strategy_hash={}", sha(b"v1")));

        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(&file, "edited in an editor").unwrap();
        let p = resolve_head(&l, Kind::Strategy, id, &file).unwrap().unwrap();
        assert_eq!((p.version, p.hash.as_str()), (2, sha(b"edited in an editor").as_str()));
        assert_eq!(l.head(Kind::Strategy, id).unwrap().actor, "external");

        delete(&l, Kind::Strategy, id, &file, "user").unwrap();
        assert_eq!(resolve_head(&l, Kind::Strategy, id, &file).unwrap(), None, "a deleted strategy has no live version");
    }
}
