//! Algorithm recipes: an algorithm assembled from library parts (preprocessing steps, then one
//! model or a voting / stacking ensemble), saved under a name and trained like any other
//! algorithm.
//!
//! A recipe is declarative JSON that only ever names components of the library, so training with
//! one needs no approval (unlike a custom Python estimator, which is arbitrary code).
//!
//! * Files live in `<work root>/algorithms/<name>.json` (atomic write, at most [`MAX_RECIPES`],
//!   [`MAX_RECIPE_BYTES`] each).
//! * Checks come in two layers. [`structural_issues`] is pure Rust: names, counts, lengths and
//!   the cheap shape of every parameter. The deep check runs `python -m ml.recipes validate`,
//!   which applies the catalog's own parameter validator to every member and knows the step
//!   kinds. `ml/recipes.py` and the trainer stay the authority and repeat their checks.
//! * A model trained from a recipe stores a full copy of it in its metadata, so it keeps working
//!   and stays explainable when the saved recipe is edited or deleted.

use super::python_runtime::{self, RuntimePaths};
use super::ml_estimators::{self, run_python_module};
use crate::dto::{RecipeCheckDto, RecipeDto, RecipeIssueDto, RecipeMemberDto, RecipeModelDto, RecipeParams};
use super::versioned_files;
use crate::ledger::{Kind, Ledger, VersionRecord};
use crate::state::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::State;

pub const RECIPES_DIR: &str = "algorithms";
pub const MAX_RECIPE_BYTES: usize = 64 * 1024;
pub const MAX_RECIPES: usize = 100;
pub const MAX_STEPS: usize = 8;
pub const MIN_MEMBERS: usize = 2;
pub const MAX_MEMBERS: usize = 6;
pub const MIN_BLOCKS: usize = 3;
pub const MAX_BLOCKS: usize = 8;
const MAX_LABEL: usize = 80;
const MAX_DESCRIPTION: usize = 1000;
const MAX_TEXT: usize = 200;
const MAX_PARAM_KEYS: usize = 60;
const MAX_JSON_DEPTH: usize = 8;
const MAX_WEIGHT: f64 = 100.0;
const CALIBRATIONS: &[&str] = &["none", "sigmoid", "isotonic"];
const VOTING_MODES: &[&str] = &["soft", "hard"];
/// Algorithms a member may use: the built-in catalog ids, never `custom` or `recipe`.
pub const MEMBER_ALGORITHMS: &[&str] = &[
    "random_forest",
    "extra_trees",
    "gradient_boosting",
    "hist_gradient_boosting",
    "logistic_regression",
    "xgboost",
];
/// Time limit for the short Python calls (validate, building blocks), like the algorithm catalog.
const SHORT_PYTHON_LIMIT: Duration = Duration::from_secs(60);

// ── Names and paths ──────────────────────────────────────────────────

/// `[a-z][a-z0-9_]{0,40}`
pub fn validate_recipe_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.len() <= 41
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err("Algorithm names look like my_ensemble: start with a lowercase letter, then lowercase letters, digits or `_` (41 characters at most)".into())
    }
}

pub fn recipes_dir(work_root: &Path) -> PathBuf {
    work_root.join(RECIPES_DIR)
}

/// Serializes writes and deletes so two saves of one recipe (or the count check and the write)
/// cannot interleave.
static RECIPE_FILES: Mutex<()> = Mutex::new(());

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

// ── Structural checks (no Python) ────────────────────────────────────

fn issue(path: impl Into<String>, message: impl Into<String>) -> RecipeIssueDto {
    RecipeIssueDto { path: path.into(), message: message.into() }
}

fn json_depth(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(o) => 1 + o.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn param_key_ok(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && key.len() <= 61
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn text_issue(value: &str, path: &str, max: usize, out: &mut Vec<RecipeIssueDto>) {
    if value.chars().count() > max {
        out.push(issue(path, format!("at most {max} characters")));
    }
    if value.contains('\0') {
        out.push(issue(path, "contains a NUL character"));
    }
}

/// Parameters are flat: numbers, text, true/false or null. Keys are identifiers.
fn params_issues(params: &RecipeParams, path: &str, out: &mut Vec<RecipeIssueDto>) {
    if params.len() > MAX_PARAM_KEYS {
        out.push(issue(path, format!("too many parameters ({}; at most {MAX_PARAM_KEYS})", params.len())));
        return;
    }
    for (key, value) in params {
        let here = format!("{path}.{key}");
        if !param_key_ok(key) {
            out.push(issue(&here, "invalid parameter name: use letters, digits and `_`"));
            continue;
        }
        match value {
            Value::Null | Value::Bool(_) => {}
            Value::Number(n) if n.as_f64().map(f64::is_finite).unwrap_or(false) => {}
            Value::String(s) => text_issue(s, &here, MAX_TEXT, out),
            _ => out.push(issue(&here, "expected a number, text, true/false or null")),
        }
    }
}

/// The library's own cost caps and key rules for a member (the same validator a normal training
/// request gets), reported at the member's path.
fn member_issues(m: &RecipeMemberDto, path: &str, voting: bool, out: &mut Vec<RecipeIssueDto>) {
    algorithm_and_params_issues(&m.algorithm, &m.params, path, out);
    // A weight only means something to voting; elsewhere it is ignored.
    if let (true, Some(w)) = (voting, m.weight) {
        if !w.is_finite() || w <= 0.0 || w > MAX_WEIGHT {
            out.push(issue(format!("{path}.weight"), format!("use a weight above 0 and at most {MAX_WEIGHT}")));
        }
    }
}

fn algorithm_and_params_issues(algorithm: &str, params: &RecipeParams, path: &str, out: &mut Vec<RecipeIssueDto>) {
    if !MEMBER_ALGORITHMS.contains(&algorithm) {
        out.push(issue(
            format!("{path}.algorithm"),
            format!("unknown algorithm `{}`; use one of {}", clip(algorithm), MEMBER_ALGORITHMS.join(", ")),
        ));
        return;
    }
    let before = out.len();
    params_issues(params, &format!("{path}.params"), out);
    if out.len() == before {
        let map: HashMap<String, Value> = params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Err(message) = ml_estimators::validate_hyperparams(algorithm, &map) {
            out.push(issue(format!("{path}.params"), message));
        }
    }
}

fn clip(text: &str) -> String {
    let mut s: String = text.chars().take(40).collect();
    if text.chars().count() > 40 {
        s.push('…');
    }
    s
}

/// Number of fitted models: members plus the final estimator for stacking.
pub fn model_count(model: &RecipeModelDto) -> usize {
    match model {
        RecipeModelDto::Single { .. } => 1,
        RecipeModelDto::Voting { members, .. } => members.len(),
        RecipeModelDto::Stacking { members, .. } => members.len() + 1,
    }
}

/// Everything wrong with a recipe that can be found without Python. Empty means structurally fine.
pub fn structural_issues(recipe: &RecipeDto) -> Vec<RecipeIssueDto> {
    let mut out = Vec::new();
    if let Err(message) = validate_recipe_name(&recipe.name) {
        out.push(issue("name", message));
    }
    if recipe.label.trim().is_empty() {
        out.push(issue("label", "give the algorithm a name"));
    }
    text_issue(&recipe.label, "label", MAX_LABEL, &mut out);
    text_issue(&recipe.description, "description", MAX_DESCRIPTION, &mut out);
    if !CALIBRATIONS.contains(&recipe.calibrate.as_str()) {
        out.push(issue("calibrate", format!("use one of {}", CALIBRATIONS.join(", "))));
    }
    if recipe.steps.len() > MAX_STEPS {
        out.push(issue("steps", format!("at most {MAX_STEPS} steps")));
    }
    for (i, step) in recipe.steps.iter().enumerate().take(MAX_STEPS * 4) {
        let path = format!("steps[{i}]");
        let kind_ok = !step.kind.is_empty()
            && step.kind.len() <= 40
            && step.kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !kind_ok {
            out.push(issue(format!("{path}.kind"), format!("unknown step `{}`", clip(&step.kind))));
        }
        params_issues(&step.params, &format!("{path}.params"), &mut out);
    }
    match &recipe.model {
        RecipeModelDto::Single { algorithm, params } => {
            algorithm_and_params_issues(algorithm, params, "model", &mut out);
        }
        RecipeModelDto::Voting { voting, members } => {
            if !VOTING_MODES.contains(&voting.as_str()) {
                out.push(issue("model.voting", format!("use one of {}", VOTING_MODES.join(", "))));
            }
            members_issues(members, true, &mut out);
        }
        RecipeModelDto::Stacking { members, final_estimator, blocks } => {
            members_issues(members, false, &mut out);
            member_issues(final_estimator, "model.final", false, &mut out);
            if !(MIN_BLOCKS..=MAX_BLOCKS).contains(blocks) {
                out.push(issue("model.blocks", format!("use {MIN_BLOCKS} to {MAX_BLOCKS} blocks")));
            }
        }
    }
    if let Ok(v) = serde_json::to_value(recipe) {
        if json_depth(&v) > MAX_JSON_DEPTH {
            out.push(issue("", "the recipe is nested too deeply"));
        }
    }
    match serde_json::to_vec(recipe) {
        Ok(bytes) if bytes.len() > MAX_RECIPE_BYTES => out.push(issue(
            "",
            format!("the recipe is too large ({} KB; the limit is {} KB)", bytes.len().div_ceil(1024), MAX_RECIPE_BYTES / 1024),
        )),
        Err(e) => out.push(issue("", format!("the recipe cannot be encoded: {e}"))),
        _ => {}
    }
    out
}

fn members_issues(members: &[RecipeMemberDto], voting: bool, out: &mut Vec<RecipeIssueDto>) {
    if !(MIN_MEMBERS..=MAX_MEMBERS).contains(&members.len()) {
        out.push(issue("model.members", format!("use {MIN_MEMBERS} to {MAX_MEMBERS} models")));
    }
    for (i, m) in members.iter().enumerate().take(MAX_MEMBERS * 4) {
        member_issues(m, &format!("model.members[{i}]"), voting, out);
    }
}

/// `path: message; path: message`, the form the UI shows in an error toast.
pub fn join_issues(issues: &[RecipeIssueDto]) -> String {
    issues
        .iter()
        .map(|e| if e.path.is_empty() { e.message.clone() } else { format!("{}: {}", e.path, e.message) })
        .collect::<Vec<_>>()
        .join("; ")
}

fn check_from_issues(recipe: &RecipeDto, errors: Vec<RecipeIssueDto>) -> RecipeCheckDto {
    RecipeCheckDto {
        ok: errors.is_empty(),
        errors,
        warnings: Vec::new(),
        steps: recipe.steps.len(),
        models: model_count(&recipe.model),
        est_model_mb: None,
    }
}

// ── Deep check and building blocks (Python) ──────────────────────────

/// The recipe as the one argv element passed to Python (never through a shell). Created and
/// updated timestamps are bookkeeping, not part of what Python checks.
fn recipe_argument(recipe: &RecipeDto) -> Result<String, String> {
    let mut bare = recipe.clone();
    bare.created_at = None;
    bare.updated_at = None;
    let text = serde_json::to_string(&bare).map_err(|e| format!("Failed to encode the recipe: {e}"))?;
    if text.len() > MAX_RECIPE_BYTES {
        return Err(format!("The recipe is too large ({} KB; the limit is {} KB)", text.len().div_ceil(1024), MAX_RECIPE_BYTES / 1024));
    }
    Ok(text)
}

/// Parse the last non-empty stdout line of `ml.recipes validate`.
fn parse_check(stdout: &str) -> Result<RecipeCheckDto, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "the recipe check returned nothing".to_string())?;
    serde_json::from_str(line).map_err(|e| format!("Failed to parse the recipe check: {e}"))
}

/// Run `python -m ml.recipes validate`: every parameter through the catalog's validator, the step
/// parameters, member counts and size warnings. A recipe that fails the check is still `Ok` here
/// (`ok == false`); `Err` means the check itself could not run.
pub(crate) async fn deep_check(paths: &RuntimePaths, recipe: &RecipeDto) -> Result<RecipeCheckDto, String> {
    let json = recipe_argument(recipe)?;
    let (_, stdout) = run_python_module(paths, "ml.recipes", &["validate", "--json", &json], SHORT_PYTHON_LIMIT)
        .await
        .map_err(|e| format!("Could not check the algorithm: {e}"))?;
    parse_check(&stdout)
}

/// Structural checks first (no Python needed to say a name is bad), then the deep check.
/// Never writes anything.
pub(crate) async fn check_recipe(paths: &RuntimePaths, recipe: &RecipeDto) -> Result<RecipeCheckDto, String> {
    let structural = structural_issues(recipe);
    if !structural.is_empty() {
        return Ok(check_from_issues(recipe, structural));
    }
    deep_check(paths, recipe).await
}

/// [`check_recipe`] that turns a failing check into one error naming every problem.
pub(crate) async fn require_valid(paths: &RuntimePaths, recipe: &RecipeDto) -> Result<RecipeCheckDto, String> {
    let check = check_recipe(paths, recipe).await?;
    if check.ok {
        Ok(check)
    } else if check.errors.is_empty() {
        Err("The algorithm is not valid".into())
    } else {
        Err(join_issues(&check.errors))
    }
}

/// The step kinds, member algorithms and limits Python offers. Asked afresh every time: which
/// algorithms are importable (xgboost) can change while the app runs, and the builder refreshes
/// on every open.
pub(crate) async fn fetch_building_blocks(paths: &RuntimePaths) -> Result<Value, String> {
    let (_, stdout) = run_python_module(paths, "ml.recipes", &["blocks"], SHORT_PYTHON_LIMIT)
        .await
        .map_err(|e| format!("Could not load the algorithm building blocks: {e}"))?;
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "the building blocks list was empty".to_string())?;
    let blocks: Value =
        serde_json::from_str(line).map_err(|e| format!("Failed to parse the building blocks: {e}"))?;
    let usable = blocks.get("steps").map(Value::is_array).unwrap_or(false)
        && blocks.get("members").map(Value::is_array).unwrap_or(false)
        && blocks.get("limits").map(Value::is_object).unwrap_or(false);
    if !usable {
        return Err("The building blocks list is missing steps, members or limits".into());
    }
    Ok(blocks)
}

// ── Storage ──────────────────────────────────────────────────────────

fn recipe_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// Read and parse one recipe file; the file must be small, not a link, and named after its content.
fn read_recipe_file(path: &Path, name: &str) -> Result<RecipeDto, String> {
    if is_symlink(path) {
        return Err(format!("{name} is a symbolic link; refusing to read it"));
    }
    let meta = fs::metadata(path).map_err(|e| format!("Failed to read {name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a file"));
    }
    if meta.len() > MAX_RECIPE_BYTES as u64 {
        return Err(format!("{name} is larger than {} KB", MAX_RECIPE_BYTES / 1024));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("Failed to read {name}: {e}"))?;
    let recipe: RecipeDto = serde_json::from_str(&text).map_err(|e| format!("{name} is not a valid recipe: {e}"))?;
    if recipe.name != name {
        return Err(format!("{name}.json holds a recipe named `{}`", clip(&recipe.name)));
    }
    Ok(recipe)
}

/// All readable recipes, by name. Malformed, oversized and misnamed files and symlinks are
/// skipped (and logged) so one bad file never hides the rest.
pub(crate) fn list_recipe_files(work_root: &Path) -> Vec<RecipeDto> {
    let dir = recipes_dir(work_root);
    if is_symlink(&dir) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<RecipeDto> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_string_lossy().to_string();
            let stem = file.strip_suffix(".json")?;
            if validate_recipe_name(stem).is_err() {
                return None; // temp files, other tools' files
            }
            match read_recipe_file(&entry.path(), stem) {
                Ok(r) => Some(r),
                Err(error) => {
                    tracing::warn!(file = %file, "skipping algorithm recipe: {error}");
                    None
                }
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Load one saved recipe by name, for training.
pub(crate) fn load_recipe(work_root: &Path, name: &str) -> Result<RecipeDto, String> {
    validate_recipe_name(name)?;
    let dir = recipes_dir(work_root);
    if is_symlink(&dir) {
        return Err("The algorithms folder is a symbolic link; refusing to read through it".into());
    }
    let path = recipe_path(&dir, name);
    if !path.exists() && !is_symlink(&path) {
        return Err(format!("Algorithm `{name}` not found; save it in the algorithm builder first"));
    }
    read_recipe_file(&path, name)
}

/// Whether a recipe file with this (valid) name is already saved.
pub(crate) fn recipe_exists(work_root: &Path, name: &str) -> bool {
    validate_recipe_name(name).is_ok() && fs::symlink_metadata(recipe_path(&recipes_dir(work_root), name)).is_ok()
}

/// A recipe name made safe for the activity log's `target_id`, which is not clipped by the log:
/// control characters are dropped and at most 41 characters (one more than a valid name) are kept.
pub(crate) fn activity_target(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(41).collect()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// A recipe with its timestamps, ready to be written: checked, `created_at` kept from the existing
/// file (a client cannot rewrite history) and `updated_at` set.
struct Prepared {
    #[allow(dead_code)] // only the test-only raw writer uses it
    dir: PathBuf,
    target: PathBuf,
    saved: RecipeDto,
    text: String,
}

fn prepare_recipe(work_root: &Path, recipe: &RecipeDto) -> Result<Prepared, String> {
    validate_recipe_name(&recipe.name)?;
    let dir = recipes_dir(work_root);
    if is_symlink(&dir) {
        return Err("The algorithms folder is a symbolic link; refusing to write through it".into());
    }
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create the algorithms folder: {e}"))?;
    let target = recipe_path(&dir, &recipe.name);
    if is_symlink(&target) {
        return Err(format!("{}.json is a symbolic link; refusing to write through it", recipe.name));
    }
    let existing = if target.exists() { read_recipe_file(&target, &recipe.name).ok() } else { None };
    if target.exists() {
        // Overwriting is always allowed; the count only limits new names.
    } else {
        let count = fs::read_dir(&dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .strip_suffix(".json")
                            .map(|stem| validate_recipe_name(stem).is_ok())
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        if count >= MAX_RECIPES {
            return Err(format!("There are already {MAX_RECIPES} saved algorithms; delete one first"));
        }
    }
    let stamp = now();
    let mut saved = recipe.clone();
    saved.created_at = Some(
        existing
            .as_ref()
            .and_then(|e| e.created_at.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| stamp.clone()),
    );
    saved.updated_at = Some(stamp);
    // Saving what is already saved changes nothing, so it keeps the old timestamp and adds no version.
    if let Some(old) = existing {
        let mut same = saved.clone();
        same.updated_at = old.updated_at.clone();
        if same == old {
            saved = old;
        }
    }
    let text = serde_json::to_string_pretty(&saved).map_err(|e| format!("Failed to encode the recipe: {e}"))?;
    if text.len() > MAX_RECIPE_BYTES {
        return Err(format!(
            "The recipe is too large ({} KB; the limit is {} KB)",
            text.len().div_ceil(1024),
            MAX_RECIPE_BYTES / 1024
        ));
    }
    Ok(Prepared { dir, target, saved, text })
}

/// Save a recipe as the next version of its name in the audit ledger and mirror it to the
/// algorithms folder (temp file, ledger, atomic rename). Keeps `created_at` of an existing recipe and
/// sets `updated_at`; saving an unchanged recipe adds no version. The recipe must already have
/// passed the checks. If the ledger write fails the file keeps its previous content.
pub(crate) fn write_recipe_audited(ledger: &Ledger, work_root: &Path, recipe: &RecipeDto, actor: &str) -> Result<(RecipeDto, VersionRecord), String> {
    validate_recipe_name(&recipe.name)?;
    let _guard = RECIPE_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let p = prepare_recipe(work_root, recipe)?;
    let record = versioned_files::save(ledger, Kind::Recipe, &recipe.name, &p.target, p.text.as_bytes(), actor)?;
    Ok((p.saved, record))
}

/// Raw atomic write with no ledger. Tests only: the app has no unaudited write path.
#[cfg(test)]
pub(crate) fn write_recipe(work_root: &Path, recipe: &RecipeDto) -> Result<RecipeDto, String> {
    validate_recipe_name(&recipe.name)?;
    let _guard = RECIPE_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let p = prepare_recipe(work_root, recipe)?;
    let tmp = p.dir.join(format!(".{}.json.tmp", recipe.name));
    fs::write(&tmp, &p.text).map_err(|e| format!("Failed to write {}: {e}", recipe.name))?;
    if let Err(e) = fs::rename(&tmp, &p.target) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Failed to save {}: {e}", recipe.name));
    }
    Ok(p.saved)
}

/// Delete a recipe as a tombstone in the audit ledger (its history stays, and saving the same name
/// again continues the same object). Returns whether a file was removed. Models trained from it keep
/// their own copy.
pub(crate) fn remove_recipe_audited(ledger: &Ledger, work_root: &Path, name: &str, actor: &str) -> Result<bool, String> {
    validate_recipe_name(name)?;
    let _guard = RECIPE_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let dir = recipes_dir(work_root);
    if is_symlink(&dir) {
        return Err("The algorithms folder is a symbolic link; refusing to delete through it".into());
    }
    let target = recipe_path(&dir, name);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_dir() => Err(format!("{name} is not a file")),
        Ok(meta) if meta.file_type().is_symlink() => Err(format!("{name} is a symbolic link; refusing to delete it")),
        // A missing file still goes through the ledger so a removal made outside the app is recorded.
        _ => versioned_files::delete(ledger, Kind::Recipe, name, &target, actor),
    }
}

/// Raw delete with no ledger. Tests only.
#[cfg(test)]
pub(crate) fn remove_recipe(work_root: &Path, name: &str) -> Result<bool, String> {
    validate_recipe_name(name)?;
    let _guard = RECIPE_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let dir = recipes_dir(work_root);
    if is_symlink(&dir) {
        return Err("The algorithms folder is a symbolic link; refusing to delete through it".into());
    }
    let target = recipe_path(&dir, name);
    match fs::symlink_metadata(&target) {
        Err(_) => Ok(false),
        Ok(meta) if meta.is_dir() => Err(format!("{name} is not a file")),
        Ok(_) => fs::remove_file(&target).map(|_| true).map_err(|e| format!("Failed to delete {name}: {e}")),
    }
}

/// Restore an older version of a recipe as a new version and rewrite the file.
#[allow(dead_code)]
pub(crate) fn restore_recipe(ledger: &Ledger, work_root: &Path, name: &str, version: u32, actor: &str) -> Result<VersionRecord, String> {
    validate_recipe_name(name)?;
    let _guard = RECIPE_FILES.lock().unwrap_or_else(|p| p.into_inner());
    let dir = recipes_dir(work_root);
    if is_symlink(&dir) {
        return Err("The algorithms folder is a symbolic link; refusing to write through it".into());
    }
    versioned_files::restore(ledger, Kind::Recipe, name, &recipe_path(&dir, name), version, actor)
}

// ── Commands ─────────────────────────────────────────────────────────

/// Check, write and log a recipe. `actor` is "user" or "copilot".
pub(crate) async fn save_inner(
    state: &AppState,
    paths: &RuntimePaths,
    recipe: &RecipeDto,
    actor: &str,
) -> Result<RecipeDto, String> {
    let replacing = recipe_exists(paths.work_root(), &recipe.name);
    let result = match require_valid(paths, recipe).await {
        Ok(_) => versioned_files::ledger()
            .and_then(|ledger| write_recipe_audited(ledger, paths.work_root(), recipe, actor))
            .map(|(saved, record)| (saved, Some(record))),
        Err(e) => Err(e),
    };
    let shown = if recipe.label.trim().is_empty() { recipe.name.as_str() } else { recipe.label.as_str() };
    let target = activity_target(&recipe.name);
    match &result {
        Ok((_, record)) => crate::activity::record(
            state, actor, "recipe.save", "system", &target,
            &format!("{} algorithm {}", if replacing { "Replaced" } else { "Saved" }, clip(shown)), true,
            record.as_ref().map(|r| format!("recipe_version={} recipe_hash={}", r.version, r.content_hash.as_deref().unwrap_or(""))),
        ),
        Err(error) => crate::activity::record(
            state, actor, "recipe.save", "system", &target,
            &format!("Saving algorithm {} failed", clip(shown)), false, Some(error.clone()),
        ),
    }
    result.map(|(saved, _)| saved)
}

pub(crate) fn delete_inner(state: &AppState, work_root: &Path, name: &str, actor: &str) -> Result<bool, String> {
    let result = versioned_files::ledger().and_then(|ledger| remove_recipe_audited(ledger, work_root, name, actor));
    let target = activity_target(name);
    match &result {
        Ok(true) => crate::activity::record(
            state, actor, "recipe.delete", "system", &target, &format!("Deleted algorithm {}", clip(name)), true, None,
        ),
        Ok(false) => {}
        Err(error) => crate::activity::record(
            state, actor, "recipe.delete", "system", &target,
            &format!("Deleting algorithm {} failed", clip(name)), false, Some(error.clone()),
        ),
    }
    result
}

/// Every saved recipe, sorted by name.
#[tauri::command]
pub async fn list_recipes(runtime_paths: State<'_, RuntimePaths>) -> Result<Vec<RecipeDto>, String> {
    // An edit made outside the app becomes a version the next time the user looks.
    versioned_files::reconcile_for_listing(runtime_paths.inner(), Kind::Recipe);
    Ok(list_recipe_files(runtime_paths.work_root()))
}

/// Validate and save a recipe (creating or replacing the one with the same name).
#[tauri::command]
pub async fn save_recipe(
    recipe: RecipeDto,
    runtime_paths: State<'_, RuntimePaths>,
    app_state: State<'_, AppState>,
) -> Result<RecipeDto, String> {
    save_inner(&app_state, runtime_paths.inner(), &recipe, "user").await
}

#[tauri::command]
pub async fn delete_recipe(
    name: String,
    runtime_paths: State<'_, RuntimePaths>,
    app_state: State<'_, AppState>,
) -> Result<bool, String> {
    delete_inner(&app_state, runtime_paths.work_root(), &name, "user")
}

/// Check a recipe without saving it.
#[tauri::command]
pub async fn validate_recipe(
    recipe: RecipeDto,
    runtime_paths: State<'_, RuntimePaths>,
) -> Result<RecipeCheckDto, String> {
    check_recipe(runtime_paths.inner(), &recipe).await
}

/// The step kinds, member algorithms and limits the builder offers.
#[tauri::command]
pub async fn recipe_building_blocks(runtime_paths: State<'_, RuntimePaths>) -> Result<Value, String> {
    fetch_building_blocks(runtime_paths.inner()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apex-rec-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn recipe(name: &str) -> RecipeDto {
        serde_json::from_value(json!({
            "name": name, "label": "My algorithm", "description": "",
            "steps": [{"kind": "standard_scaler", "params": {}}],
            "model": {"kind": "single", "algorithm": "random_forest", "params": {"n_estimators": 20}},
            "calibrate": "none"
        }))
        .unwrap()
    }

    fn voting(n: usize) -> RecipeDto {
        let members: Vec<Value> = (0..n).map(|_| json!({"algorithm": "random_forest", "params": {}, "weight": 1})).collect();
        let mut r = recipe("v");
        r.model = serde_json::from_value(json!({"kind": "voting", "voting": "soft", "members": members})).unwrap();
        r
    }

    fn paths_of(issues: &[RecipeIssueDto]) -> Vec<&str> {
        issues.iter().map(|i| i.path.as_str()).collect()
    }

    #[test]
    fn json_null_reads_as_the_default_for_params_text_steps_and_calibrate() {
        let r: RecipeDto = serde_json::from_value(json!({
            "name": "nulls", "label": null, "description": null, "steps": null, "calibrate": null,
            "model": {"kind": "voting", "voting": "soft", "members": [
                {"algorithm": "random_forest", "params": null}, {"algorithm": "extra_trees", "params": null, "weight": null}]}
        }))
        .unwrap();
        assert_eq!((r.label.as_str(), r.description.as_str(), r.calibrate.as_str()), ("", "", "none"));
        assert!(r.steps.is_empty());
        let s: RecipeDto = serde_json::from_value(json!({
            "name": "nulls", "steps": [{"kind": "standard_scaler", "params": null}],
            "model": {"kind": "single", "algorithm": "random_forest", "params": null}
        }))
        .unwrap();
        assert!(s.steps[0].params.is_empty());
        assert!(matches!(&s.model, RecipeModelDto::Single { params, .. } if params.is_empty()));
        // A hand-edited file with a null still lists.
        let root = temp("nulls");
        let dir = recipes_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("nulls.json"), r#"{"name":"nulls","description":null,"model":{"kind":"single","algorithm":"random_forest"}}"#).unwrap();
        assert_eq!(list_recipe_files(&root).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_activity_target_is_short_and_free_of_control_characters() {
        assert_eq!(activity_target("calm_vote"), "calm_vote");
        let hostile = format!("a\n\u{1b}[31m{}", "x".repeat(1_000_000));
        let target = activity_target(&hostile);
        assert_eq!(target.chars().count(), 41);
        assert!(!target.chars().any(char::is_control));
    }

    #[test]
    fn recipe_exists_sees_saved_names_only() {
        let root = temp("exists");
        assert!(!recipe_exists(&root, "mine"));
        write_recipe(&root, &recipe("mine")).unwrap();
        assert!(recipe_exists(&root, "mine"));
        assert!(!recipe_exists(&root, "other") && !recipe_exists(&root, "../mine") && !recipe_exists(&root, ""));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn names_are_strict_and_cannot_escape_the_folder() {
        for ok in ["a", "my_ensemble", "m2", &format!("a{}", "b".repeat(40))] {
            assert!(validate_recipe_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "A", "1abc", "_a", "a-b", "a b", "a.json", "../evil", "a/b", "a\\b", "..", ".hidden", "é", "a\0b",
            &format!("a{}", "b".repeat(41)),
        ] {
            assert!(validate_recipe_name(bad).is_err(), "{bad:?}");
        }
        let root = temp("names");
        for bad in ["../evil", "a/b", ".."] {
            let mut r = recipe("x");
            r.name = bad.into();
            assert!(write_recipe(&root, &r).is_err());
            assert!(load_recipe(&root, bad).is_err());
            assert!(remove_recipe(&root, bad).is_err());
        }
        assert!(!root.parent().unwrap().join("evil.json").exists());
        assert!(!recipes_dir(&root).exists(), "a rejected name creates nothing");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_is_atomic_keeps_created_at_and_lists_in_name_order() {
        let root = temp("save");
        let first = write_recipe(&root, &recipe("zeta")).unwrap();
        write_recipe(&root, &recipe("alpha")).unwrap();
        write_recipe(&root, &recipe("mid")).unwrap();
        let names: Vec<String> = list_recipe_files(&root).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["alpha", "mid", "zeta"]);
        assert!(first.created_at.is_some() && first.created_at == first.updated_at);

        std::thread::sleep(Duration::from_millis(5));
        let mut edited = recipe("zeta");
        edited.label = "Renamed".into();
        edited.created_at = Some("1999-01-01T00:00:00Z".into()); // a client cannot rewrite history
        let second = write_recipe(&root, &edited).unwrap();
        assert_eq!(second.created_at, first.created_at);
        assert_ne!(second.updated_at, first.updated_at);
        assert_eq!(load_recipe(&root, "zeta").unwrap().label, "Renamed");

        let leftovers: Vec<String> = fs::read_dir(recipes_dir(&root))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !n.ends_with(".json") || n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");

        assert!(remove_recipe(&root, "zeta").unwrap());
        assert!(!remove_recipe(&root, "zeta").unwrap(), "second delete reports it was gone");
        assert!(load_recipe(&root, "zeta").unwrap_err().contains("not found"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_fields_are_ignored_and_never_persisted() {
        let root = temp("unknown");
        let r: RecipeDto = serde_json::from_value(json!({
            "name": "extra", "label": "L", "description": "", "steps": [], "calibrate": "none",
            "model": {"kind": "single", "algorithm": "extra_trees", "params": {}, "evil": 1},
            "surprise": {"a": 1}
        }))
        .unwrap();
        write_recipe(&root, &r).unwrap();
        let text = fs::read_to_string(recipes_dir(&root).join("extra.json")).unwrap();
        assert!(!text.contains("surprise") && !text.contains("evil"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_file_is_skipped_not_fatal() {
        let root = temp("malformed");
        write_recipe(&root, &recipe("good")).unwrap();
        let dir = recipes_dir(&root);
        fs::write(dir.join("broken.json"), "{ not json").unwrap();
        fs::write(dir.join("wrongshape.json"), r#"{"name": "wrongshape"}"#).unwrap();
        fs::write(dir.join("misnamed.json"), serde_json::to_string(&recipe("other")).unwrap()).unwrap();
        fs::write(dir.join("Bad Name.json"), "{}").unwrap();
        fs::write(dir.join("notes.txt"), "hi").unwrap();
        fs::write(dir.join("huge.json"), vec![b' '; MAX_RECIPE_BYTES + 1]).unwrap();
        fs::create_dir(dir.join("adir.json")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("good.json"), dir.join("linked.json")).unwrap();
        let names: Vec<String> = list_recipe_files(&root).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["good"]);
        assert!(load_recipe(&root, "broken").is_err());
        #[cfg(unix)]
        {
            assert!(load_recipe(&root, "linked").unwrap_err().contains("symbolic"));
            let mut r = recipe("linked");
            r.label = "x".into();
            assert!(write_recipe(&root, &r).unwrap_err().contains("symbolic"));
        }
        // No folder at all is an empty list, not an error.
        assert!(list_recipe_files(&temp("none")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn at_most_a_hundred_recipes_but_existing_ones_stay_editable() {
        let root = temp("count");
        for i in 0..MAX_RECIPES {
            write_recipe(&root, &recipe(&format!("r{i}"))).unwrap();
        }
        let err = write_recipe(&root, &recipe("one_more")).unwrap_err();
        assert!(err.contains("100"), "{err}");
        let mut edit = recipe("r7");
        edit.label = "still editable".into();
        assert_eq!(write_recipe(&root, &edit).unwrap().label, "still editable");
        assert!(remove_recipe(&root, "r0").unwrap());
        assert!(write_recipe(&root, &recipe("one_more")).is_ok());
        assert_eq!(list_recipe_files(&root).len(), MAX_RECIPES);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn structural_limits() {
        assert!(structural_issues(&recipe("fine")).is_empty());

        let mut r = recipe("fine");
        r.label = "  ".into();
        assert_eq!(paths_of(&structural_issues(&r)), ["label"]);
        r.label = "x".repeat(MAX_LABEL + 1);
        assert_eq!(paths_of(&structural_issues(&r)), ["label"]);
        r.label = "ok".into();
        r.description = "d".repeat(MAX_DESCRIPTION + 1);
        assert_eq!(paths_of(&structural_issues(&r)), ["description"]);
        r.description.clear();
        r.calibrate = "platt".into();
        assert_eq!(paths_of(&structural_issues(&r)), ["calibrate"]);
        r.calibrate = "isotonic".into();
        assert!(structural_issues(&r).is_empty());

        // 9 steps is one too many; 8 is fine.
        let step = |kind: &str| json!({"kind": kind, "params": {}});
        let mut r = recipe("fine");
        r.steps = serde_json::from_value(json!((0..8).map(|_| step("robust_scaler")).collect::<Vec<_>>())).unwrap();
        assert!(structural_issues(&r).is_empty());
        r.steps = serde_json::from_value(json!((0..9).map(|_| step("robust_scaler")).collect::<Vec<_>>())).unwrap();
        assert_eq!(paths_of(&structural_issues(&r)), ["steps"]);
        r.steps = serde_json::from_value(json!([step("Bad Kind!")])).unwrap();
        assert_eq!(paths_of(&structural_issues(&r)), ["steps[0].kind"]);
        r.steps = serde_json::from_value(json!([{"kind": "pca", "params": {"n_components": [1, 2]}}])).unwrap();
        assert_eq!(paths_of(&structural_issues(&r)), ["steps[0].params.n_components"]);
        r.steps = serde_json::from_value(json!([{"kind": "pca", "params": {"bad key": 1}}])).unwrap();
        assert_eq!(paths_of(&structural_issues(&r)), ["steps[0].params.bad key"]);
        r.steps = serde_json::from_value(json!([{"kind": "pca", "params": {"s": "x".repeat(MAX_TEXT + 1)}}])).unwrap();
        assert_eq!(paths_of(&structural_issues(&r)), ["steps[0].params.s"]);

        // Member counts: 2 to 6.
        assert_eq!(paths_of(&structural_issues(&voting(1))), ["model.members"]);
        assert!(structural_issues(&voting(2)).is_empty());
        assert!(structural_issues(&voting(6)).is_empty());
        assert_eq!(paths_of(&structural_issues(&voting(7))), ["model.members"]);
    }

    #[test]
    fn structural_member_and_stacking_rules() {
        let mut r = recipe("fine");
        for bad in ["custom", "recipe", "svm", ""] {
            r.model = serde_json::from_value(json!({"kind": "single", "algorithm": bad, "params": {}})).unwrap();
            assert_eq!(paths_of(&structural_issues(&r)), ["model.algorithm"], "{bad}");
        }
        // The normal training validator runs per member: caps and key rules.
        r.model = serde_json::from_value(json!({"kind": "single", "algorithm": "random_forest", "params": {"n_estimators": 999999}})).unwrap();
        let issues = structural_issues(&r);
        assert_eq!(paths_of(&issues), ["model.params"]);
        assert!(issues[0].message.contains("n_estimators"), "{}", issues[0].message);

        let stack = |blocks: usize, final_alg: &str| -> RecipeDto {
            let mut r = recipe("fine");
            r.model = serde_json::from_value(json!({
                "kind": "stacking", "blocks": blocks,
                "members": [{"algorithm": "random_forest", "params": {}}, {"algorithm": "logistic_regression", "params": {}}],
                "final": {"algorithm": final_alg, "params": {}}
            }))
            .unwrap();
            r
        };
        assert!(structural_issues(&stack(3, "logistic_regression")).is_empty());
        assert!(structural_issues(&stack(8, "logistic_regression")).is_empty());
        assert_eq!(paths_of(&structural_issues(&stack(2, "logistic_regression"))), ["model.blocks"]);
        assert_eq!(paths_of(&structural_issues(&stack(9, "logistic_regression"))), ["model.blocks"]);
        assert_eq!(paths_of(&structural_issues(&stack(5, "custom"))), ["model.final.algorithm"]);

        // Weights are a voting thing (ignored elsewhere); voting must be soft or hard.
        let mut weighted = stack(5, "logistic_regression");
        if let RecipeModelDto::Stacking { members, .. } = &mut weighted.model {
            members[0].weight = Some(-2.0);
        }
        assert!(structural_issues(&weighted).is_empty());
        let mut v = voting(2);
        if let RecipeModelDto::Voting { members, voting } = &mut v.model {
            members[1].weight = Some(0.0);
            *voting = "plurality".into();
        }
        let mut got = paths_of(&structural_issues(&v)).into_iter().map(String::from).collect::<Vec<_>>();
        got.sort();
        assert_eq!(got, ["model.members[1].weight", "model.voting"]);

        // Every problem is reported at once, and joined readably.
        let mut many = voting(1);
        many.label.clear();
        many.name = "Bad".into();
        let joined = join_issues(&structural_issues(&many));
        assert!(joined.contains("name: ") && joined.contains("label: ") && joined.contains("model.members: "), "{joined}");
        assert!(joined.contains("; "));
    }

    #[test]
    fn the_size_cap_applies() {
        let mut r = recipe("fine");
        // 6 members x 60 keys of 200-character text is well past 64 KB.
        let big: serde_json::Map<String, Value> =
            (0..MAX_PARAM_KEYS).map(|i| (format!("k{i}"), json!("x".repeat(MAX_TEXT)))).collect();
        let members: Vec<Value> = (0..6).map(|_| json!({"algorithm": "random_forest", "params": big})).collect();
        r.model = serde_json::from_value(json!({"kind": "voting", "voting": "soft", "members": members})).unwrap();
        r.steps = serde_json::from_value(json!((0..8).map(|_| json!({"kind": "pca", "params": big})).collect::<Vec<_>>())).unwrap();
        let issues = structural_issues(&r);
        assert!(issues.iter().any(|i| i.path.is_empty() && i.message.contains("too large")), "{issues:?}");
        assert!(recipe_argument(&r).is_err());
        let root = temp("big");
        assert!(write_recipe(&root, &r).unwrap_err().contains("too large"));
        assert!(list_recipe_files(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn every_model_kind_round_trips_through_json() {
        let docs = [
            json!({"name": "s", "label": "S", "description": "d", "steps": [{"kind": "pca", "params": {"n_components": 0.9, "whiten": true}}],
                   "model": {"kind": "single", "algorithm": "gradient_boosting", "params": {"learning_rate": 0.05}}, "calibrate": "sigmoid"}),
            json!({"name": "v", "label": "V", "description": "", "steps": [],
                   "model": {"kind": "voting", "voting": "hard", "members": [
                       {"algorithm": "random_forest", "params": {}, "weight": 2.5}, {"algorithm": "extra_trees", "params": {"max_depth": null}}]},
                   "calibrate": "none"}),
            json!({"name": "t", "label": "T", "description": "", "steps": [{"kind": "select_k_best", "params": {"k": 5, "score": "f_classif"}}],
                   "model": {"kind": "stacking", "blocks": 4,
                             "members": [{"algorithm": "random_forest", "params": {}}, {"algorithm": "logistic_regression", "params": {"C": 0.5}}],
                             "final": {"algorithm": "logistic_regression", "params": {}}},
                   "calibrate": "isotonic", "created_at": "2026-01-01T00:00:00+00:00", "updated_at": "2026-01-02T00:00:00+00:00"}),
        ];
        for doc in docs {
            let dto: RecipeDto = serde_json::from_value(doc.clone()).unwrap();
            let back = serde_json::to_value(&dto).unwrap();
            assert_eq!(back, doc, "serialised shape must equal the TypeScript contract");
            let again: RecipeDto = serde_json::from_value(back).unwrap();
            assert_eq!(again, dto);
            assert!(structural_issues(&dto).is_empty(), "{:?}", structural_issues(&dto));
        }
        // The tag spells the contract: kind, snake_case, `final` for the stacking estimator.
        let t: RecipeDto = serde_json::from_value(docs_stacking()).unwrap();
        assert_eq!(serde_json::to_value(&t.model).unwrap()["final"]["algorithm"], "logistic_regression");
        assert_eq!(model_count(&t.model), 3);
        assert!(serde_json::from_value::<RecipeDto>(json!({"name": "x", "model": {"kind": "bagging"}})).is_err());
    }

    fn docs_stacking() -> Value {
        json!({"name": "t", "label": "T", "model": {"kind": "stacking", "blocks": 4,
            "members": [{"algorithm": "random_forest"}, {"algorithm": "extra_trees"}],
            "final": {"algorithm": "logistic_regression"}}})
    }

    #[test]
    fn minimal_documents_get_defaults() {
        let r: RecipeDto = serde_json::from_value(docs_stacking()).unwrap();
        assert_eq!((r.calibrate.as_str(), r.steps.len(), r.description.as_str()), ("none", 0, ""));
        let back = serde_json::to_value(&r).unwrap();
        assert!(back.get("created_at").is_none(), "unset timestamps are not serialised");
    }

    #[test]
    fn check_output_parses_and_tolerates_missing_fields() {
        let c = parse_check(r#"{"ok":false,"errors":[{"path":"model.members","message":"use 2 to 6 models"}],"warnings":["w"],"steps":1,"models":3,"est_model_mb":12.5}"#).unwrap();
        assert!(!c.ok && c.errors[0].path == "model.members" && c.models == 3 && c.est_model_mb == Some(12.5));
        let crash = parse_check("noise\n{\"ok\":false,\"errors\":[{\"path\":\"\",\"message\":\"boom\"}]}\n\n").unwrap();
        assert_eq!((crash.ok, crash.steps, crash.est_model_mb), (false, 0, None));
        assert_eq!(join_issues(&crash.errors), "boom");
        assert!(parse_check("").is_err());
        assert!(parse_check("not json").is_err());
    }

    // ── With Python ──────────────────────────────────────────────────

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    /// A temp work root with the project's Python sources linked in. None (skip) when there is no
    /// virtualenv or the recipe CLI does not exist yet.
    fn python_root(name: &str) -> Option<(PathBuf, RuntimePaths)> {
        let repo = python_runtime::repo_root().unwrap();
        let src = repo.join("apex-python");
        if !src.join(".venv").join("bin").join("python").exists() || !src.join("ml").join("recipes.py").exists() {
            eprintln!("skipping: apex-python virtualenv or ml/recipes.py not found");
            return None;
        }
        #[cfg(not(unix))]
        return None;
        let root = temp(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, root.join("apex-python")).unwrap();
        Some((root.clone(), RuntimePaths::for_test(&root)))
    }

    #[test]
    fn the_python_cli_supplies_blocks_and_a_deep_check() {
        let Some((root, paths)) = python_root("cli") else { return };
        let blocks = block_on(fetch_building_blocks(&paths)).unwrap();
        assert!(blocks["steps"].as_array().unwrap().iter().any(|s| s["kind"] == "standard_scaler"));
        assert_eq!(blocks["limits"]["max_steps"], MAX_STEPS);
        assert_eq!(blocks["limits"]["max_members"], MAX_MEMBERS);
        assert_eq!(blocks["limits"]["min_members"], MIN_MEMBERS);

        let good = block_on(check_recipe(&paths, &recipe("good"))).unwrap();
        assert!(good.ok, "{good:?}");
        assert_eq!(good.models, 1);

        // Structurally fine, but Python knows the step kinds and the real parameter ranges.
        let mut bad = recipe("bad");
        bad.steps = serde_json::from_value(json!([{"kind": "nope", "params": {}}])).unwrap();
        let check = block_on(check_recipe(&paths, &bad)).unwrap();
        assert!(!check.ok && check.errors.iter().any(|e| e.path.starts_with("steps[0]")), "{check:?}");
        let err = block_on(require_valid(&paths, &bad)).unwrap_err();
        assert!(err.contains("steps[0]"), "{err}");

        let mut typo = recipe("typo");
        typo.model = serde_json::from_value(json!({"kind": "single", "algorithm": "random_forest", "params": {"n_estimatorz": 5}})).unwrap();
        let check = block_on(check_recipe(&paths, &typo)).unwrap();
        assert!(!check.ok, "{check:?}");
        assert!(join_issues(&check.errors).contains("n_estimatorz"), "{check:?}");

        // Validation never writes.
        assert!(!recipes_dir(&root).exists());
        let _ = fs::remove_dir_all(&root);
    }

    mod audited {
        use super::*;
        use crate::commands::versioned_files::testutil::{open, tmp};

        #[test]
        fn saves_are_versions_an_unchanged_save_is_a_noop_and_deletes_are_tombstones() {
            let root = tmp("rec");
            let l = open(&root);
            let work = root.0.as_path();
            let (first, v1) = write_recipe_audited(&l, work, &recipe("ens"), "user").unwrap();
            assert_eq!((v1.version, v1.action.as_str()), (1, "create"));
            std::thread::sleep(Duration::from_millis(5));
            let (again, same) = write_recipe_audited(&l, work, &recipe("ens"), "user").unwrap();
            assert_eq!(same, v1, "saving what is already saved adds no version");
            assert_eq!(again.updated_at, first.updated_at);

            let mut edited = recipe("ens");
            edited.label = "Renamed".into();
            edited.created_at = Some("1999-01-01T00:00:00Z".into());
            let (second, v2) = write_recipe_audited(&l, work, &edited, "copilot").unwrap();
            assert_eq!((v2.version, v2.actor.as_str()), (2, "copilot"));
            assert_eq!(second.created_at, first.created_at, "history cannot be rewritten by the client");
            assert_eq!(load_recipe(work, "ens").unwrap().label, "Renamed");
            let old: RecipeDto = serde_json::from_slice(&l.content(Kind::Recipe, "ens", 1).unwrap()).unwrap();
            assert_eq!(old.label, "My algorithm", "version 1 still holds the original");

            assert!(remove_recipe_audited(&l, work, "ens", "user").unwrap());
            assert!(!remove_recipe_audited(&l, work, "ens", "user").unwrap());
            assert!(load_recipe(work, "ens").is_err());
            assert!(l.head(Kind::Recipe, "ens").unwrap().is_tombstone());
            let (_, v4) = write_recipe_audited(&l, work, &recipe("ens"), "user").unwrap();
            assert_eq!(v4.version, 4);
            let restored = restore_recipe(&l, work, "ens", 2, "user").unwrap();
            assert_eq!((restored.version, restored.restored_from), (5, Some(2)));
            assert_eq!(load_recipe(work, "ens").unwrap().label, "Renamed");
        }

        #[test]
        fn bad_names_and_size_limits_are_rejected_before_the_ledger_is_touched() {
            let root = tmp("rec-bad");
            let l = open(&root);
            for bad in ["../x", "a/b", "Bad", "", "1x"] {
                let mut r = recipe("ok");
                r.name = bad.into();
                assert!(write_recipe_audited(&l, &root.0, &r, "user").is_err(), "{bad:?}");
                assert!(remove_recipe_audited(&l, &root.0, bad, "user").is_err(), "{bad:?}");
                assert!(restore_recipe(&l, &root.0, bad, 1, "user").is_err(), "{bad:?}");
            }
            let mut big = recipe("big");
            big.description = "x".repeat(MAX_RECIPE_BYTES);
            assert!(write_recipe_audited(&l, &root.0, &big, "user").unwrap_err().contains("too large"));
            assert!(l.objects(Kind::Recipe, true).is_empty());
        }
    }
}
