//! Algorithm catalog, hyperparameter checks and custom estimator files for the ML workbench.
//!
//! * `list_algorithms` asks `python -m ml.catalog` which estimators are installed and what every
//!   constructor parameter is (the answer is cached for the life of the process).
//! * [`validate_hyperparams`] is the quick structural check run before anything is started;
//!   [`validate_with_catalog`] checks keys and values against the catalog. `ml/catalog.py` stays
//!   the authority and repeats every check inside the trainer.
//! * Custom estimators are Python files in `<work root>/estimators/`. See [`ESTIMATOR_TEMPLATE`]
//!   for the contract. A file is arbitrary local code, exactly like a strategy file.

use super::python_runtime::{self, RuntimePaths};
use crate::dto::{AlgorithmDto, EstimatorFileDto, ParamDefDto};
use crate::state::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::State;
use tokio::process::Command;

// ── Algorithm ids and hyperparameter checks ──────────────────────────

/// Every algorithm id a training request may name. Whether `xgboost` is actually installed is
/// decided by the catalog (and by the trainer).
pub const ALGORITHMS: &[&str] = &[
    "random_forest",
    "extra_trees",
    "gradient_boosting",
    "hist_gradient_boosting",
    "logistic_regression",
    "xgboost",
    "custom",
];

pub fn validate_algorithm_id(algorithm: &str) -> Result<(), String> {
    if ALGORITHMS.contains(&algorithm) {
        Ok(())
    } else {
        Err(format!("Unknown algorithm: {algorithm}. Valid: {}", ALGORITHMS.join(", ")))
    }
}

pub const MAX_PARAM_KEYS: usize = 60;
const MAX_TEXT: usize = 200;
const MAX_LIST: usize = 100;
/// `n_jobs` is clamped by the trainer to this many workers.
pub const MAX_JOBS: i64 = 4;
const GENERIC_INT_LIMIT: f64 = 1e9;

/// Ceilings on the parameters that decide how much work a fit does (mirrors `ml/catalog.py`).
pub const CAPS: &[(&str, f64)] = &[
    ("n_estimators", 5000.0),
    ("max_iter", 100_000.0),
    ("max_depth", 200.0),
    ("max_leaf_nodes", 100_000.0),
    ("max_leaves", 100_000.0),
    ("max_bins", 255.0),
    ("max_bin", 4096.0),
    ("n_iter_no_change", 1000.0),
    ("num_parallel_tree", 100.0),
];

fn valid_param_name(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    key.len() <= 61 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn scalar_ok(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(_) => true,
        Value::Number(n) => n.as_f64().map(f64::is_finite).unwrap_or(false),
        Value::String(s) => s.chars().count() <= MAX_TEXT,
        _ => false,
    }
}

/// Structural check that needs no catalog: a known algorithm, sane keys, JSON scalar values,
/// and the caps on cost parameters. Unknown keys are caught by [`validate_with_catalog`] (and by
/// the trainer).
pub(crate) fn validate_hyperparams(algorithm: &str, hyperparams: &HashMap<String, Value>) -> Result<(), String> {
    validate_algorithm_id(algorithm)?;
    if hyperparams.len() > MAX_PARAM_KEYS {
        return Err(format!("too many hyperparameters ({}; max {MAX_PARAM_KEYS})", hyperparams.len()));
    }
    let mut keys: Vec<&String> = hyperparams.keys().collect();
    keys.sort();
    for key in keys {
        let value = &hyperparams[key];
        if !valid_param_name(key) {
            return Err(format!("invalid hyperparameter name `{key}`: use letters, digits and `_`"));
        }
        let ok = scalar_ok(value)
            || (algorithm == "custom"
                && matches!(value, Value::Array(a) if a.len() <= MAX_LIST && a.iter().all(scalar_ok)));
        if !ok {
            return Err(format!(
                "invalid value {value} for hyperparameter `{key}` of {algorithm}: expected a number, text, true/false or null"
            ));
        }
        if key == "n_jobs" {
            continue; // clamped by the trainer
        }
        if let (Some(&(_, cap)), Some(n)) = (CAPS.iter().find(|(k, _)| k == key), value.as_f64()) {
            if n > cap {
                return Err(format!(
                    "invalid value {value} for hyperparameter `{key}` of {algorithm}: at most {} is allowed",
                    cap as i64
                ));
            }
        }
    }
    Ok(())
}

fn fmt_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Check one value against its catalog entry. Null always passes (the trainer knows which
/// parameters may be null); exact open/closed bounds are left to the trainer as well.
fn check_value(algorithm: &str, def: &ParamDefDto, v: &Value) -> Result<(), String> {
    if v.is_null() {
        return Ok(());
    }
    let bad = |expected: String| Err(format!("invalid value {v} for hyperparameter `{}` of {algorithm}: expected {expected}", def.key));
    match def.kind.as_str() {
        "bool" => {
            if v.is_boolean() {
                Ok(())
            } else {
                bad("true or false".into())
            }
        }
        "int" | "float" => {
            let is_int = def.kind == "int";
            let Some(n) = v.as_f64().filter(|n| n.is_finite()) else {
                return bad(if is_int { "an integer".into() } else { "a number".into() });
            };
            if is_int && n.fract() != 0.0 {
                return bad("an integer".into());
            }
            let cap = CAPS.iter().find(|(k, _)| *k == def.key).map(|(_, c)| *c);
            let hi = match (def.max, cap) {
                (Some(m), Some(c)) => Some(m.min(c)),
                (m, c) => m.or(c).or(if is_int { Some(GENERIC_INT_LIMIT) } else { None }),
            };
            if def.min.map(|m| n < m).unwrap_or(false) || hi.map(|m| n > m).unwrap_or(false) {
                let range = match (def.min, hi) {
                    (Some(lo), Some(hi)) => format!("from {} to {}", fmt_num(lo), fmt_num(hi)),
                    (Some(lo), None) => format!("of at least {}", fmt_num(lo)),
                    (None, Some(hi)) => format!("of at most {}", fmt_num(hi)),
                    (None, None) => String::new(),
                };
                return bad(format!("{} {range}", if is_int { "an integer" } else { "a number" }));
            }
            Ok(())
        }
        "choice" => {
            let choices = def.choices.clone().unwrap_or_default();
            // A number is allowed for parameters like max_features ('sqrt', 'log2' or a count/share);
            // the trainer checks its range.
            if v.as_str().map(|s| choices.iter().any(|c| c == s)).unwrap_or(false) || v.is_number() {
                Ok(())
            } else {
                bad(format!("one of {}", choices.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")))
            }
        }
        _ => {
            if v.as_str().map(|s| s.chars().count() <= MAX_TEXT).unwrap_or(false) {
                Ok(())
            } else {
                bad(format!("text of at most {MAX_TEXT} characters"))
            }
        }
    }
}

/// Reject keys the algorithm does not have (listing the valid ones) and values that do not fit
/// the declared type or range. `n_jobs` is accepted (the trainer clamps it).
pub(crate) fn validate_with_catalog(
    catalog: &[AlgorithmDto],
    algorithm: &str,
    hyperparams: &HashMap<String, Value>,
) -> Result<(), String> {
    validate_hyperparams(algorithm, hyperparams)?;
    let algo = catalog
        .iter()
        .find(|a| a.id == algorithm)
        .ok_or_else(|| format!("algorithm `{algorithm}` is not available in this installation (installed: {})", catalog.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ")))?;
    if algorithm == "custom" {
        return Ok(());
    }
    let mut keys: Vec<&String> = hyperparams.keys().collect();
    keys.sort();
    for key in keys {
        if key == "n_jobs" {
            let n = hyperparams[key].as_f64().unwrap_or(f64::NAN);
            if !(n.fract() == 0.0 && (n == -1.0 || n >= 1.0)) {
                return Err(format!(
                    "invalid value {} for hyperparameter `n_jobs` of {algorithm}: expected an integer from 1 to {MAX_JOBS} (or -1)",
                    hyperparams[key]
                ));
            }
            continue;
        }
        let Some(def) = algo.params.iter().find(|p| &p.key == key) else {
            return Err(format!(
                "unknown hyperparameter `{key}` for {algorithm}; valid: {}",
                algo.params.iter().map(|p| p.key.as_str()).collect::<Vec<_>>().join(", ")
            ));
        };
        check_value(algorithm, def, &hyperparams[key])?;
    }
    Ok(())
}

// ── Catalog from Python ──────────────────────────────────────────────

static CATALOG_CACHE: Mutex<Option<HashMap<PathBuf, Vec<AlgorithmDto>>>> = Mutex::new(None);

/// Run `python -m <module> <args>` from the work root and return stdout, with a time limit.
pub(crate) async fn run_python_module(
    runtime_paths: &RuntimePaths,
    module: &str,
    args: &[&str],
    limit: Duration,
) -> Result<(PathBuf, String), String> {
    let python = python_runtime::resolve_python_executable(
        runtime_paths,
        "APEX_ML_PYTHON_PATH",
        &["APEX_STRATEGY_PYTHON_PATH"],
    )?;
    let mut command = Command::new(&python);
    command
        .current_dir(runtime_paths.work_root())
        .env("PYTHONPATH", python_runtime::build_python_path(runtime_paths)?)
        .env("PYTHONIOENCODING", "utf-8")
        .kill_on_drop(true)
        .arg("-m")
        .arg(module)
        .args(args);
    let output = output_with_limit(command, limit)
        .await?
        .map_err(|e| format!("Failed to start Python with {}: {e}", python.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no error message");
        return Err(python_runtime::enrich_python_error(
            &format!("python -m {module} failed: {message}"),
            &["APEX_ML_PYTHON_PATH", "APEX_STRATEGY_PYTHON_PATH"],
        ));
    }
    Ok((python, stdout))
}

/// Wait for a command's output for at most `limit`. On timeout the future is dropped, which kills
/// the child (the command must have `kill_on_drop(true)`). The outer error is the timeout.
pub(crate) async fn output_with_limit(
    mut command: Command,
    limit: Duration,
) -> Result<std::io::Result<std::process::Output>, String> {
    command.kill_on_drop(true);
    match tokio::time::timeout(limit, command.output()).await {
        Ok(result) => Ok(result),
        Err(_) => Err(format!("timed out after {}", describe_duration(limit))),
    }
}

pub(crate) fn describe_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        format!("{} ms", d.as_millis())
    } else if secs >= 60 && secs % 60 == 0 {
        let m = secs / 60;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else {
        format!("{secs} second{}", if secs == 1 { "" } else { "s" })
    }
}

/// The installed algorithms and their parameters (cached per Python interpreter).
pub(crate) async fn fetch_algorithms(runtime_paths: &RuntimePaths) -> Result<Vec<AlgorithmDto>, String> {
    let python = python_runtime::resolve_python_executable(
        runtime_paths,
        "APEX_ML_PYTHON_PATH",
        &["APEX_STRATEGY_PYTHON_PATH"],
    )?;
    if let Some(hit) = CATALOG_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|m| m.get(&python).cloned())
    {
        return Ok(hit);
    }
    let (_, stdout) = run_python_module(runtime_paths, "ml.catalog", &[], Duration::from_secs(60)).await?;
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "the algorithm catalog was empty".to_string())?;
    let catalog: Vec<AlgorithmDto> =
        serde_json::from_str(line).map_err(|e| format!("Failed to parse the algorithm catalog: {e}"))?;
    CATALOG_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(python, catalog.clone());
    Ok(catalog)
}

/// Every algorithm that can be trained here, with all its tunable parameters and their defaults.
#[tauri::command]
pub async fn list_algorithms(runtime_paths: State<'_, RuntimePaths>) -> Result<Vec<AlgorithmDto>, String> {
    fetch_algorithms(runtime_paths.inner()).await
}

// ── Custom estimator files ───────────────────────────────────────────

pub const MAX_ESTIMATOR_BYTES: usize = 200 * 1024;
pub const ESTIMATORS_DIR: &str = "estimators";

/// The file a new custom estimator starts from. It documents the contract.
pub const ESTIMATOR_TEMPLATE: &str = r#""""A custom estimator for APEX model training.

Contract
--------
Define make_estimator(params) at the top level of this file. APEX calls it once per
cross-validation fold and once for the final model, passing the parameters you enter in the
Train tab (a dict; random_state is always included). It must return an object with

    fit(X, y)       X is a 2-D float array, y the class labels (for example 0/1 or -1/0/1)
    predict(X)      one class label per row, from the labels seen in fit

and may also provide

    predict_proba(X)         class probabilities, shown as the signal's confidence
    feature_importances_     one number per feature, shown in the model view

Only numpy and scikit-learn are guaranteed to be installed. The file runs on your machine with
your permissions, like a strategy file: only run code you trust. A copy is saved next to every
model trained with it, so the model keeps working if you later edit or delete this file.
"""

import numpy as np


class NearestCentroid:
    """Predicts the class whose average training row is closest (after scaling each feature)."""

    def __init__(self, shrink=0.0):
        self.shrink = shrink

    def fit(self, X, y):
        X = np.asarray(X, dtype=float)
        y = np.asarray(y)
        self.classes_ = np.unique(y)
        self.scale_ = X.std(axis=0) + 1e-9
        self.centers_ = np.array([X[y == c].mean(axis=0) for c in self.classes_]) * (1.0 - self.shrink)
        return self

    def _distances(self, X):
        X = np.asarray(X, dtype=float)
        return np.stack([(((X - c) / self.scale_) ** 2).sum(axis=1) for c in self.centers_], axis=1)

    def predict(self, X):
        return self.classes_[self._distances(X).argmin(axis=1)]

    def predict_proba(self, X):
        weights = np.exp(-self._distances(X))
        return weights / weights.sum(axis=1, keepdims=True)


def make_estimator(params):
    return NearestCentroid(shrink=float(params.get("shrink", 0.0)))
"#;

/// Return the starting template for a new estimator file.
#[tauri::command]
pub fn estimator_template() -> String {
    ESTIMATOR_TEMPLATE.to_string()
}

/// `[a-z][a-z0-9_]{0,40}.py`
pub fn validate_estimator_name(name: &str) -> Result<(), String> {
    let ok = name
        .strip_suffix(".py")
        .map(|stem| {
            let mut chars = stem.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
                && stem.len() <= 41
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err("Estimator names look like my_model.py: start with a lowercase letter, then lowercase letters, digits or `_` (41 characters at most before .py)".into())
    }
}

/// Whether the source defines `make_estimator` at the top level (a `def`, an assignment or an import).
pub(crate) fn defines_make_estimator(content: &str) -> bool {
    content.lines().any(|line| {
        if let Some(rest) = line.strip_prefix("def make_estimator") {
            return rest.starts_with(|c: char| c == '(' || c == ' ' || c == '\t');
        }
        if let Some(rest) = line.strip_prefix("make_estimator") {
            let rest = rest.trim_start();
            return rest.starts_with('=') && !rest.starts_with("==") || rest.starts_with(':');
        }
        line.starts_with("from ")
            && line.contains(" import ")
            && line.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "make_estimator")
    })
}

pub fn estimators_dir(work_root: &Path) -> PathBuf {
    work_root.join(ESTIMATORS_DIR)
}

/// Serializes writes so two saves of one file cannot interleave.
static ESTIMATOR_FILES: Mutex<()> = Mutex::new(());

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

fn describe(path: &Path, name: &str, content: String) -> EstimatorFileDto {
    let updated_at = fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default();
    EstimatorFileDto { name: name.to_string(), path: format!("{ESTIMATORS_DIR}/{name}"), content, updated_at }
}

/// All valid estimator files, by name. Files with other names, symlinks and unreadable files are skipped.
pub(crate) fn list_files(work_root: &Path) -> Vec<EstimatorFileDto> {
    let dir = estimators_dir(work_root);
    if is_symlink(&dir) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<EstimatorFileDto> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            validate_estimator_name(&name).ok()?;
            let path = entry.path();
            if is_symlink(&path) || !path.is_file() {
                return None;
            }
            let content = fs::read_to_string(&path).ok()?;
            Some(describe(&path, &name, content))
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write an estimator file atomically (temp file in the same directory, then rename).
pub(crate) fn write_file(work_root: &Path, name: &str, content: &str) -> Result<EstimatorFileDto, String> {
    validate_estimator_name(name)?;
    if content.len() > MAX_ESTIMATOR_BYTES {
        return Err(format!(
            "The estimator file is too large ({} KB; the limit is {} KB)",
            content.len().div_ceil(1024),
            MAX_ESTIMATOR_BYTES / 1024
        ));
    }
    if content.contains('\0') {
        return Err("The estimator file contains a NUL character".into());
    }
    if !defines_make_estimator(content) {
        return Err("The file must define make_estimator(params) at the top level, returning a scikit-learn style classifier with fit and predict".into());
    }
    let _guard = ESTIMATOR_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let dir = estimators_dir(work_root);
    if is_symlink(&dir) {
        return Err("The estimators folder is a symbolic link; refusing to write through it".into());
    }
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create the estimators folder: {e}"))?;
    let target = dir.join(name);
    if is_symlink(&target) {
        return Err(format!("{name} is a symbolic link; refusing to write through it"));
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    fs::write(&tmp, content).map_err(|e| format!("Failed to write {name}: {e}"))?;
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Failed to save {name}: {e}"));
    }
    Ok(describe(&target, name, content.to_string()))
}

/// Delete an estimator file. Returns whether it existed. Models trained with it keep their own copy.
pub(crate) fn remove_file(work_root: &Path, name: &str) -> Result<bool, String> {
    validate_estimator_name(name)?;
    let _guard = ESTIMATOR_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let dir = estimators_dir(work_root);
    if is_symlink(&dir) {
        return Err("The estimators folder is a symbolic link; refusing to delete through it".into());
    }
    let target = dir.join(name);
    match fs::symlink_metadata(&target) {
        Err(_) => Ok(false),
        Ok(meta) if meta.is_dir() => Err(format!("{name} is not a file")),
        Ok(_) => fs::remove_file(&target).map(|_| true).map_err(|e| format!("Failed to delete {name}: {e}")),
    }
}

/// The absolute path of an existing estimator file, for the trainer.
pub(crate) fn resolve_for_training(work_root: &Path, name: &str) -> Result<PathBuf, String> {
    validate_estimator_name(name)?;
    let dir = estimators_dir(work_root);
    let path = dir.join(name);
    if is_symlink(&dir) || is_symlink(&path) {
        return Err(format!("{name} is reached through a symbolic link; refusing to run it"));
    }
    if !path.is_file() {
        return Err(format!("Estimator file {name} not found; save it in the Train tab first"));
    }
    Ok(path)
}

/// For the Copilot tool layer (`copilot_tools::needs_approval`), which is outside this module.
#[allow(dead_code)]
/// The key under which the Copilot's trusted-files map remembers an approved estimator.
pub fn trust_key(name: &str) -> String {
    format!("{ESTIMATORS_DIR}/{name}")
}

/// Whether the Copilot needs the user's approval before training with this estimator file: true
/// unless the file's current content is exactly what was approved (same mechanism as
/// `run_backtest`: the entry in `copilot_trusted_files` holds the approved content hash).
#[allow(dead_code)]
pub fn needs_approval(state: &AppState, work_root: &Path, name: &str) -> bool {
    let current = resolve_for_training(work_root, name)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|text| super::copilot_tools::content_hash(&text));
    let trusted = state
        .copilot_trusted_files
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&trust_key(name))
        .cloned();
    !(current.is_some() && current == trusted)
}

/// Save an estimator file and log it. `actor` is "user" or "copilot".
pub(crate) fn save_inner(
    state: &AppState,
    work_root: &Path,
    name: &str,
    content: &str,
    actor: &str,
) -> Result<EstimatorFileDto, String> {
    let result = write_file(work_root, name, content);
    match &result {
        Ok(_) => crate::activity::record(state, actor, "estimator.save", "system", name, &format!("Saved estimator {name}"), true, None),
        Err(error) => crate::activity::record(
            state, actor, "estimator.save", "system", name, &format!("Saving estimator {name} failed"), false, Some(error.clone()),
        ),
    }
    result
}

pub(crate) fn delete_inner(state: &AppState, work_root: &Path, name: &str, actor: &str) -> Result<bool, String> {
    let result = remove_file(work_root, name);
    match &result {
        Ok(true) => crate::activity::record(state, actor, "estimator.delete", "system", name, &format!("Deleted estimator {name}"), true, None),
        Ok(false) => {}
        Err(error) => crate::activity::record(
            state, actor, "estimator.delete", "system", name, &format!("Deleting estimator {name} failed"), false, Some(error.clone()),
        ),
    }
    result
}

#[tauri::command]
pub async fn list_estimator_files(runtime_paths: State<'_, RuntimePaths>) -> Result<Vec<EstimatorFileDto>, String> {
    Ok(list_files(runtime_paths.work_root()))
}

#[tauri::command]
pub async fn save_estimator_file(
    name: String,
    content: String,
    runtime_paths: State<'_, RuntimePaths>,
    app_state: State<'_, AppState>,
) -> Result<EstimatorFileDto, String> {
    save_inner(&app_state, runtime_paths.work_root(), &name, &content, "user")
}

#[tauri::command]
pub async fn delete_estimator_file(
    name: String,
    runtime_paths: State<'_, RuntimePaths>,
    app_state: State<'_, AppState>,
) -> Result<bool, String> {
    delete_inner(&app_state, runtime_paths.work_root(), &name, "user")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apex-est-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const GOOD: &str = "def make_estimator(params):\n    return None\n";

    #[test]
    fn estimator_names_are_strict() {
        for ok in ["a.py", "my_model.py", "m2.py", &format!("a{}.py", "b".repeat(40))] {
            assert!(validate_estimator_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", ".py", "A.py", "1a.py", "_a.py", "a-b.py", "a b.py", "a.PY", "a.txt", "a", "a.py.py", "../a.py", "a/b.py", "a\\b.py",
            "..", "/etc/passwd", "a.py/", ".hidden.py", "a\0.py", "é.py", &format!("a{}.py", "b".repeat(41)),
        ] {
            assert!(validate_estimator_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names_that_try_to_leave_the_folder_write_and_delete_nothing() {
        let root = temp("escape");
        fs::write(root.join("victim.py"), "keep").unwrap();
        fs::create_dir_all(root.join("estimators")).unwrap();
        for bad in ["../victim.py", "..\\victim.py", "/tmp/x.py", "a/../../victim.py", "sub/x.py", "x.py\0", ""] {
            assert!(write_file(&root, bad, GOOD).is_err(), "{bad:?}");
            assert!(remove_file(&root, bad).is_err(), "{bad:?}");
            assert!(resolve_for_training(&root, bad).is_err(), "{bad:?}");
        }
        assert_eq!(fs::read_to_string(root.join("victim.py")).unwrap(), "keep");
        assert!(fs::read_dir(root.join("estimators")).unwrap().next().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_never_followed() {
        let root = temp("symlink");
        let outside = temp("symlink-outside");
        fs::write(outside.join("secret.py"), "def make_estimator(p): pass\n").unwrap();
        // A linked file inside the folder.
        fs::create_dir_all(root.join("estimators")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.py"), root.join("estimators").join("linked.py")).unwrap();
        assert!(write_file(&root, "linked.py", GOOD).is_err());
        assert!(resolve_for_training(&root, "linked.py").is_err());
        assert!(list_files(&root).is_empty(), "a linked file is not listed");
        assert_eq!(fs::read_to_string(outside.join("secret.py")).unwrap(), "def make_estimator(p): pass\n");
        // A linked folder.
        let root2 = temp("symlink2");
        std::os::unix::fs::symlink(&outside, root2.join("estimators")).unwrap();
        assert!(write_file(&root2, "new.py", GOOD).is_err());
        assert!(remove_file(&root2, "secret.py").is_err());
        assert!(list_files(&root2).is_empty());
        assert!(outside.join("secret.py").exists() && !outside.join("new.py").exists());
        for d in [root, root2, outside] {
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn save_list_overwrite_and_delete() {
        let root = temp("crud");
        assert!(list_files(&root).is_empty());
        let saved = write_file(&root, "b_model.py", GOOD).unwrap();
        assert_eq!((saved.name.as_str(), saved.path.as_str(), saved.content.as_str()), ("b_model.py", "estimators/b_model.py", GOOD));
        assert!(chrono::DateTime::parse_from_rfc3339(&saved.updated_at).is_ok(), "{}", saved.updated_at);
        write_file(&root, "a_model.py", &format!("# v1\n{GOOD}")).unwrap();
        write_file(&root, "a_model.py", &format!("# v2\n{GOOD}")).unwrap();
        fs::write(root.join("estimators").join("notes.txt"), "not an estimator").unwrap();
        fs::write(root.join("estimators").join("Bad Name.py"), GOOD).unwrap();
        let names: Vec<String> = list_files(&root).into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["a_model.py", "b_model.py"]);
        assert!(list_files(&root)[0].content.starts_with("# v2"));
        assert!(!fs::read_dir(root.join("estimators")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")), "temp file left behind");
        assert_eq!(resolve_for_training(&root, "a_model.py").unwrap(), root.join("estimators").join("a_model.py"));
        assert!(resolve_for_training(&root, "nope.py").unwrap_err().contains("not found"));

        assert!(remove_file(&root, "a_model.py").unwrap());
        assert!(!remove_file(&root, "a_model.py").unwrap(), "deleting twice reports nothing removed");
        assert_eq!(list_files(&root).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn content_rules_size_cap_and_make_estimator() {
        let root = temp("content");
        for (text, why) in [
            ("x = 1\n", "no make_estimator"),
            ("# def make_estimator(params):\n", "only in a comment"),
            ("    def make_estimator(self):\n        pass\n", "indented (a method)"),
            ("def make_estimator_v2(params):\n    pass\n", "different name"),
            ("make_estimator == 1\n", "comparison"),
            ("", "empty"),
        ] {
            assert!(write_file(&root, "x.py", text).unwrap_err().contains("make_estimator"), "{why}");
        }
        for ok in [
            "def make_estimator(params):\n    pass\n",
            "import numpy\n\ndef make_estimator (params):\n    pass\n",
            "make_estimator = lambda params: None\n",
            "from mylib import make_estimator\n",
            "from mylib import a, make_estimator as m\n",
        ] {
            assert!(write_file(&root, "x.py", ok).is_ok(), "{ok}");
        }
        assert!(write_file(&root, "x.py", &format!("{GOOD}\0")).unwrap_err().contains("NUL"));
        let at_cap = format!("{GOOD}#{}", "x".repeat(MAX_ESTIMATOR_BYTES - GOOD.len() - 1));
        assert_eq!(at_cap.len(), MAX_ESTIMATOR_BYTES);
        assert!(write_file(&root, "big.py", &at_cap).is_ok());
        let err = write_file(&root, "big2.py", &format!("{at_cap}x")).unwrap_err();
        assert!(err.contains("too large") && err.contains("200 KB"), "{err}");
        assert!(!root.join("estimators").join("big2.py").exists());
        // A rejected overwrite leaves the previous version in place.
        write_file(&root, "keep.py", &format!("# original\n{GOOD}")).unwrap();
        assert!(write_file(&root, "keep.py", "garbage").is_err());
        assert!(fs::read_to_string(root.join("estimators").join("keep.py")).unwrap().starts_with("# original"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_template_states_the_contract_and_is_accepted() {
        assert!(defines_make_estimator(ESTIMATOR_TEMPLATE));
        assert!(ESTIMATOR_TEMPLATE.len() < MAX_ESTIMATOR_BYTES);
        for needle in ["make_estimator(params)", "fit(X, y)", "predict(X)", "predict_proba", "random_state", "copy is saved next to every"] {
            assert!(ESTIMATOR_TEMPLATE.contains(needle), "{needle}");
        }
        assert_eq!(estimator_template(), ESTIMATOR_TEMPLATE);
        let root = temp("template");
        assert!(write_file(&root, "starter.py", ESTIMATOR_TEMPLATE).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn algorithm_ids_cover_the_catalog() {
        for id in ["random_forest", "extra_trees", "gradient_boosting", "hist_gradient_boosting", "logistic_regression", "xgboost", "custom"] {
            assert!(validate_algorithm_id(id).is_ok(), "{id}");
        }
        assert!(validate_algorithm_id("svm").unwrap_err().contains("hist_gradient_boosting"));
    }

    #[test]
    fn a_command_that_outlives_its_limit_is_killed() {
        let dir = temp("kill");
        let pid_file = dir.join("pid");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("echo $$ > '{}'; exec sleep 30", pid_file.display()));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let started = std::time::Instant::now();
        let outcome = rt.block_on(output_with_limit(cmd, Duration::from_millis(600)));
        assert!(started.elapsed() < Duration::from_secs(10), "returned promptly");
        let err = outcome.unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        let pid = fs::read_to_string(&pid_file).expect("the command started").trim().to_string();
        let alive = || {
            let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid]).output().unwrap();
            let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
            !stat.is_empty() && !stat.starts_with('Z')
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "process {pid} is still running after the limit");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_inside_its_limit_returns_its_output() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo hello");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let out = rt.block_on(output_with_limit(cmd, Duration::from_secs(30))).unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[test]
    fn durations_are_described_for_people() {
        assert_eq!(describe_duration(Duration::from_secs(1200)), "20 minutes");
        assert_eq!(describe_duration(Duration::from_secs(60)), "1 minute");
        assert_eq!(describe_duration(Duration::from_secs(90)), "90 seconds");
        assert_eq!(describe_duration(Duration::from_millis(50)), "50 ms");
    }

    #[test]
    fn catalog_json_from_python_parses_into_the_dto() {
        let line = json!([{
            "id": "random_forest", "label": "Random forest", "description": "d", "accepts_extra_params": true,
            "params": [
                {"key": "n_estimators", "label": "N estimators", "type": "int", "default": 100, "min": 1, "max": 5000, "help": "h"},
                {"key": "max_depth", "label": "Max depth", "type": "int", "default": null, "help": "h"},
                {"key": "criterion", "label": "Criterion", "type": "choice", "default": "gini", "choices": ["gini", "entropy"], "help": "h"}
            ]
        }]);
        let parsed: Vec<AlgorithmDto> = serde_json::from_value(line).unwrap();
        assert_eq!(parsed[0].params[0].kind, "int");
        assert_eq!(parsed[0].params[0].max, Some(5000.0));
        assert!(parsed[0].params[1].default.is_null() && parsed[0].params[1].min.is_none());
        let wire = serde_json::to_value(&parsed).unwrap();
        assert_eq!(wire[0]["params"][0]["type"], "int", "the field is called `type` on the wire");
        assert!(wire[0]["params"][1].get("min").is_none());
    }
}
