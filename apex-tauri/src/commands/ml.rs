use super::{ml_datasets, ml_estimators, ml_expr, ml_features, ml_recipes, python_runtime};
use crate::dto::{
    CustomFeatureDto, HoldoutDto,
    CvFoldDto, FeatureImportanceDto, LabelSpecDto, MLModelDto, MLTrainingRequestDto,
    MLTrainingResultDto, ModelDetailDto, ModelSignalHistoryDto, ModelSignalPointDto,
    ModelUsageDto, RecipeDto, TradingSummaryDto,
};
use crate::ledger::{self, Kind, Ledger, VersionRecord};
use crate::state::AppState;
use crate::validation;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::State;
use tokio::process::Command;

/// Filesystem-backed ML model registry rooted at `<repo>/models`.
pub struct ModelRegistry {
    pub models_dir: PathBuf,
}

impl ModelRegistry {
    pub fn new(runtime_paths: &python_runtime::RuntimePaths) -> Self {
        let models_dir = runtime_paths.models_dir().to_path_buf();
        let _ = fs::create_dir_all(&models_dir);
        remember_data_dir(&models_dir, runtime_paths.data_dir());
        Self { models_dir }
    }
}

// ── The audit ledger for models ──────────────────────────────────────
//
// A model is a versioned object (kind `model`, object id = model id). Its content is the metadata JSON:
// display name, notes and tags change over time, and its existence changes (deleted = tombstone,
// restorable). The artifact `.joblib` is written once by the trainer and never modified; deleting a model
// MOVES its files to `models/archive/<id>@v<n>/` (n = the ledger version of the delete), never removes them.
// The metadata file in `models/` is a convenience mirror of the ledger head.

/// Which data directory (and so which ledger, `<data_dir>/ledger`) goes with a models directory. Filled by
/// `ModelRegistry::new` and by training; without an entry the sibling directory `data` is used, which is
/// the layout of both the app and the tests.
static DATA_DIRS: std::sync::Mutex<Option<HashMap<PathBuf, PathBuf>>> = std::sync::Mutex::new(None);

fn remember_data_dir(models_dir: &Path, data_dir: &Path) {
    let mut guard = DATA_DIRS.lock().unwrap_or_else(|p| p.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(models_dir.to_path_buf(), data_dir.to_path_buf());
}

fn data_dir_for(models_dir: &Path) -> Option<PathBuf> {
    let known = DATA_DIRS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|m| m.get(models_dir).cloned());
    known.or_else(|| models_dir.parent().map(|p| p.join("data")))
}

/// The ledger for changes. Errors when it cannot be opened: an edit that cannot be recorded is refused.
fn model_ledger(models_dir: &Path) -> Result<Ledger, String> {
    let data_dir = data_dir_for(models_dir).ok_or_else(|| "the models directory has no parent directory".to_string())?;
    crate::activity::ledger_for(&data_dir).map_err(|e| format!("the audit ledger is not available: {e}"))
}

/// The ledger for reading, or None when this models directory has no data directory next to it yet.
fn model_ledger_if_known(models_dir: &Path) -> Option<Ledger> {
    let data_dir = data_dir_for(models_dir)?;
    if !data_dir.is_dir() {
        return None;
    }
    match crate::activity::ledger_for(&data_dir) {
        Ok(l) => Some(l),
        Err(error) => {
            tracing::error!(%error, "the audit ledger could not be opened; model history is not checked");
            None
        }
    }
}

fn archive_root(models_dir: &Path) -> PathBuf {
    models_dir.join("archive")
}

/// Where the files of a model deleted at ledger version `version` are kept.
fn archive_dir(models_dir: &Path, model_id: &str, version: u32) -> PathBuf {
    archive_root(models_dir).join(format!("{model_id}@v{version}"))
}

fn deleted_error(model_id: &str) -> String {
    format!(
        "Model `{model_id}` was deleted (archived). Its history is in the audit ledger and it can be restored; it cannot be used until then"
    )
}

/// The fields every metadata file has had since the first trainer. Everything added later is read
/// leniently from the raw JSON (see [`build_detail`]) so an older or hand-edited file still loads.
#[derive(Debug, Deserialize)]
struct ModelMetadataFile {
    model_id: Option<String>,
    algorithm: String,
    feature_names: Vec<String>,
    target_column: String,
    data_path: Option<String>,
    metrics: HashMap<String, Option<f64>>,
    model_file: String,
    created_utc: String,
    /// Timeframe the model was trained on (datasets only); inference loads bars of this size.
    #[serde(default)]
    timeframe: Option<String>,
}

impl ModelMetadataFile {
    fn clean_metrics(&self) -> HashMap<String, f64> {
        self.metrics
            .iter()
            .filter_map(|(k, v)| v.filter(|x| x.is_finite()).map(|x| (k.clone(), x)))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct TrainerCliResult {
    model_id: String,
    algorithm: String,
    metrics: HashMap<String, f64>,
    feature_names: Vec<String>,
    created_at: String,
    model_path: String,
    metadata_path: String,
    data_path: String,
    target_column: String,
    #[serde(default)]
    duration_secs: Option<f64>,
}

fn sample_dataset_path(runtime_paths: &python_runtime::RuntimePaths) -> PathBuf {
    runtime_paths.data_dir().join("sample.csv")
}

fn ensure_training_dataset(
    relative_path: &str,
    runtime_paths: &python_runtime::RuntimePaths,
) -> Result<(), String> {
    let full_path = runtime_paths.resolve_user_relative_path(relative_path);
    if full_path.exists() {
        return Ok(());
    }

    if full_path != sample_dataset_path(runtime_paths) {
        return Err(format!(
            "Training data file not found: {}",
            full_path.display()
        ));
    }

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create sample data directory {:?}: {e}", parent))?;
    }

    let mut csv = String::from(
        "sma_20,sma_50,ema_12,ema_26,rsi_14,macd_signal,bb_upper,bb_lower,atr_14,volume_lag_1,signal\n",
    );
    for index in 0..240 {
        let i = index as f64;
        let base = 100.0 + i * 0.35;
        let wave = (i / 7.0).sin() * 3.0;
        let sma_20 = base + wave;
        let sma_50 = base - 1.8 + wave * 0.35;
        let ema_12 = sma_20 + 0.6 + (i / 5.0).cos() * 0.25;
        let ema_26 = sma_50 + 0.2;
        let rsi_14 = 48.0 + (i / 6.0).sin() * 18.0;
        let macd_signal = ema_12 - ema_26;
        let bb_upper = sma_20 + 2.4;
        let bb_lower = sma_20 - 2.4;
        let atr_14 = 1.1 + (i / 9.0).cos().abs();
        let volume_lag_1 = 450_000.0 + i * 3_500.0 + ((index % 9) as f64 * 1_250.0);
        let signal = if ema_12 > ema_26 && rsi_14 > 50.0 {
            1
        } else {
            0
        };

        csv.push_str(&format!(
            "{sma_20:.4},{sma_50:.4},{ema_12:.4},{ema_26:.4},{rsi_14:.4},{macd_signal:.4},{bb_upper:.4},{bb_lower:.4},{atr_14:.4},{volume_lag_1:.2},{signal}\n"
        ));
    }

    fs::write(&full_path, csv)
        .map_err(|e| format!("Failed to create sample dataset {:?}: {e}", full_path))?;
    Ok(())
}

// ── Model ids, metadata files ────────────────────────────────────────

/// A model id names files inside models_dir: letters, digits, `_`, `-` and `.` only.
/// (validate_symbol also allows `/` and `:`, which would let an id walk out of the directory.)
pub(crate) fn validate_model_id(model_id: &str) -> Result<(), String> {
    if model_id.is_empty()
        || model_id.len() > 128
        || !model_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        || model_id.contains("..")
    {
        return Err("model_id may only contain letters, digits, '_', '-' and '.'".into());
    }
    Ok(())
}

fn metadata_path(models_dir: &Path, model_id: &str) -> PathBuf {
    models_dir.join(format!("{model_id}.json"))
}

fn log_path(models_dir: &Path, model_id: &str) -> PathBuf {
    models_dir.join(format!("{model_id}.log"))
}

/// The metadata names the artifact: require a bare filename so a tampered JSON cannot redirect
/// the loader outside models_dir.
fn validate_model_file(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err("model metadata contains an invalid model_file".into());
    }
    Ok(())
}

fn read_metadata_raw(models_dir: &Path, model_id: &str) -> Result<(ModelMetadataFile, Value), String> {
    validate_model_id(model_id)?;
    if let Some(head) = model_ledger_if_known(models_dir).and_then(|l| l.head(Kind::Model, model_id)) {
        if head.is_tombstone() {
            return Err(deleted_error(model_id));
        }
    }
    let path = metadata_path(models_dir, model_id);
    if !path.exists() {
        return Err(format!("Unknown model `{model_id}` (no metadata file)"));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("Failed to read model metadata: {e}"))?;
    let raw: Value = serde_json::from_str(&text).map_err(|e| format!("Failed to parse model metadata: {e}"))?;
    let meta: ModelMetadataFile =
        serde_json::from_value(raw.clone()).map_err(|e| format!("Failed to parse model metadata: {e}"))?;
    Ok((meta, raw))
}

/// Everything that makes a model's files and the ledger agree. The caller holds the model-file lock.
/// Returns true when the model is deleted (the ledger head is a tombstone).
///
/// * no history: the model predates the ledger and is registered as version 1 (actor "system");
/// * a pending `<id>.json.apexnew` equal to the head is moved into place (an interrupted save/restore);
/// * a metadata file that differs from the head is an external edit and becomes its own version;
/// * a tombstoned model whose files are still in `models/` (a delete interrupted after the tombstone)
///   gets its files moved to the archive.
fn sync_model_locked(ledger: &Ledger, models_dir: &Path, model_id: &str) -> Result<bool, String> {
    let path = metadata_path(models_dir, model_id);
    let pending = ledger::pending_path(&path)?;
    let present = path.is_file();
    match ledger.head(Kind::Model, model_id) {
        None => {
            if present {
                let bytes = fs::read(&path).map_err(|e| format!("Failed to read model metadata: {e}"))?;
                ledger.put(
                    Kind::Model,
                    model_id,
                    &bytes,
                    "system",
                    "Registered an existing model (trained before the audit ledger) as version 1",
                )?;
            }
            Ok(false)
        }
        Some(head) if head.is_tombstone() => {
            if !present {
                return Ok(true);
            }
            let bytes = fs::read(&path).map_err(|e| format!("Failed to read model metadata: {e}"))?;
            let hash = Ledger::sha256_hex(&bytes);
            let last_live = ledger.history(Kind::Model, model_id).into_iter().rev().find(|r| !r.is_tombstone());
            if last_live.and_then(|r| r.content_hash).as_deref() == Some(hash.as_str()) {
                let files = model_file_set(models_dir, model_id)?;
                move_files(&files, &archive_dir(models_dir, model_id, head.version))?;
                Ok(true)
            } else {
                // Not the deleted model: someone put a new file under this id. Record it.
                ledger::reconcile_file(ledger, Kind::Model, model_id, &path)?;
                Ok(false)
            }
        }
        Some(_) => {
            if present || pending.is_file() {
                ledger::reconcile_file(ledger, Kind::Model, model_id, &path)?;
            }
            Ok(false)
        }
    }
}

fn sync_all_models(ledger: &Ledger, models_dir: &Path, hidden: &mut std::collections::HashSet<String>) {
    let Ok(entries) = fs::read_dir(models_dir) else {
        return;
    };
    let mut ids = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".json").or_else(|| name.strip_suffix(".json.apexnew")) else {
            continue;
        };
        if validate_model_id(id).is_ok() {
            ids.insert(id.to_string());
        }
    }
    for id in ids {
        match sync_model_locked(ledger, models_dir, &id) {
            Ok(true) => {
                hidden.insert(id);
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, model_id = %id, "could not bring a model in step with the audit ledger"),
        }
    }
}

/// The files that belong to a model: metadata, artifact, training log and the copy of a custom
/// estimator saved next to it.
fn model_file_set(models_dir: &Path, model_id: &str) -> Result<Vec<PathBuf>, String> {
    let metadata = metadata_path(models_dir, model_id);
    let mut artifact = models_dir.join(format!("{model_id}.joblib"));
    if let Ok(raw) = fs::read_to_string(&metadata) {
        // A damaged metadata file must not stop the model from being deleted.
        if let Ok(meta) = serde_json::from_str::<ModelMetadataFile>(&raw) {
            validate_model_file(&meta.model_file)?;
            artifact = models_dir.join(meta.model_file);
        }
    }
    Ok(vec![metadata, artifact, log_path(models_dir, model_id), models_dir.join(format!("{model_id}_estimator.py"))])
}

/// Move the files that exist into `dest` (created on demand). Never overwrites and never deletes.
fn move_files(files: &[PathBuf], dest: &Path) -> Result<usize, String> {
    let mut moved = 0;
    for file in files {
        if !file.exists() {
            continue;
        }
        let Some(name) = file.file_name() else { continue };
        fs::create_dir_all(dest).map_err(|e| format!("Failed to create {}: {e}", dest.display()))?;
        let target = dest.join(name);
        if target.exists() {
            return Err(format!("{} already exists; refusing to overwrite it", target.display()));
        }
        fs::rename(file, &target).map_err(|e| format!("Failed to move {}: {e}", file.display()))?;
        moved += 1;
    }
    Ok(moved)
}

pub(crate) fn load_registered_models(models_dir: &Path) -> Result<Vec<MLModelDto>, String> {
    if !models_dir.exists() {
        return Ok(Vec::new());
    }

    // Models from before the ledger are registered here as version 1; an interrupted save, delete or
    // restore is finished; a hand edit of a metadata file becomes its own version.
    let mut hidden: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(ledger) = model_ledger_if_known(models_dir) {
        let _guard = lock_model_files();
        sync_all_models(&ledger, models_dir, &mut hidden);
    }
    let mut models = Vec::new();
    let entries = fs::read_dir(models_dir)
        .map_err(|e| format!("Failed to read models directory {:?}: {e}", models_dir))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to inspect model entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if path.file_stem().and_then(|s| s.to_str()).is_some_and(|stem| hidden.contains(stem)) {
            continue;
        }

        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read model metadata {:?}: {e}", path))?;
        let metadata: ModelMetadataFile = match serde_json::from_str(&raw) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(?error, path = ?path, "Skipping malformed model metadata file");
                continue;
            }
        };

        let metrics = metadata.clean_metrics();
        let model_id = metadata.model_id.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown_model")
                .to_string()
        });

        let model_path = models_dir.join(&metadata.model_file);
        let status = if model_path.exists() {
            "completed"
        } else {
            "missing_artifact"
        };

        models.push(MLModelDto {
            model_id,
            algorithm: metadata.algorithm,
            status: status.to_string(),
            metrics,
            feature_names: metadata.feature_names,
            created_at: metadata.created_utc,
            data_path: metadata.data_path.unwrap_or_else(|| "unknown".into()),
            target_column: metadata.target_column,
        });
    }

    models.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(models)
}

/// List all trained ML models.
#[tauri::command]
pub async fn list_ml_models(state: State<'_, ModelRegistry>) -> Result<Vec<MLModelDto>, String> {
    load_registered_models(&state.models_dir)
}

// ── Model detail and edits ───────────────────────────────────────────

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_string()
    } else {
        text.chars().skip(count - max).collect()
    }
}

fn field<T: serde::de::DeserializeOwned>(raw: &Value, key: &str) -> Option<T> {
    raw.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Everything about one model. Every field added after the first trainer is optional in the file;
/// a model trained before it existed gets empty values (no folds, no importances, no trading).
pub(crate) fn build_detail(
    models_dir: &Path,
    model_id: &str,
    used_by: Vec<ModelUsageDto>,
) -> Result<ModelDetailDto, String> {
    let (meta, raw) = read_metadata_raw(models_dir, model_id)?;
    validate_model_file(&meta.model_file)?;
    let artifact = models_dir.join(&meta.model_file);
    let artifact_bytes = fs::metadata(&artifact).ok().map(|m| m.len());
    let log_tail = fs::read_to_string(log_path(models_dir, model_id))
        .map(|t| tail_chars(&t, 4000))
        .unwrap_or_default();
    let source = match raw.get("source").and_then(|v| v.as_str()) {
        Some(s @ ("user" | "copilot")) => s.to_string(),
        _ => "unknown".to_string(),
    };
    let library_versions: HashMap<String, String> = raw
        .get("library_versions")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    Ok(ModelDetailDto {
        model_id: model_id.to_string(),
        algorithm: meta.algorithm.clone(),
        status: if artifact.exists() { "completed" } else { "missing_artifact" }.into(),
        metrics: meta.clean_metrics(),
        feature_names: meta.feature_names.clone(),
        created_at: meta.created_utc.clone(),
        data_path: meta.data_path.clone().unwrap_or_else(|| "unknown".into()),
        target_column: meta.target_column.clone(),
        display_name: field(&raw, "display_name").unwrap_or_default(),
        notes: field(&raw, "notes").unwrap_or_default(),
        tags: field(&raw, "tags").unwrap_or_default(),
        source,
        hyperparams: field(&raw, "hyperparams").unwrap_or_default(),
        n_splits: field(&raw, "n_splits").unwrap_or(0),
        lag_periods: field(&raw, "lag_periods").unwrap_or_default(),
        dataset: field(&raw, "dataset"),
        symbols: field(&raw, "symbols").unwrap_or_default(),
        timeframe: field(&raw, "timeframe"),
        label: field::<LabelSpecDto>(&raw, "label"),
        rows: field(&raw, "rows"),
        duration_secs: field(&raw, "duration_secs"),
        artifact_bytes,
        library_versions,
        cv_folds: field::<Vec<CvFoldDto>>(&raw, "cv_folds").unwrap_or_default(),
        feature_importance: field::<Vec<FeatureImportanceDto>>(&raw, "feature_importance").unwrap_or_default(),
        trading: field::<TradingSummaryDto>(&raw, "trading"),
        log_tail,
        used_by,
        trade_mode: match raw.get("trade_mode").and_then(|v| v.as_str()) {
            Some(mode @ ("long_flat" | "long_short")) => mode.to_string(),
            _ => "long_flat".to_string(),
        },
        holdout: field::<HoldoutDto>(&raw, "holdout"),
        custom_features: field::<Vec<CustomFeatureDto>>(&raw, "custom_features").unwrap_or_default(),
        context_symbols: field(&raw, "context_symbols").unwrap_or_default(),
        fee_pct: field(&raw, "fee_pct").unwrap_or(0.0),
        estimator_file: field(&raw, "estimator_file"),
        recipe: field::<RecipeDto>(&raw, "recipe"),
        dataset_version: field(&raw, "dataset_version"),
        dataset_csv_sha256: field(&raw, "dataset_csv_sha256"),
        estimator_version: field(&raw, "estimator_file_version"),
        recipe_version: field(&raw, "recipe_version"),
        ledger_version: model_ledger_if_known(models_dir)
            .and_then(|l| l.head(Kind::Model, model_id))
            .filter(|h| !h.is_tombstone())
            .map(|h| h.version),
    })
}

pub(crate) fn usages_of(state: &AppState, model_id: &str) -> Vec<ModelUsageDto> {
    state
        .automations
        .list()
        .into_iter()
        .filter(|rule| rule.model_id == model_id)
        .map(|rule| ModelUsageDto {
            kind: "automation".into(),
            id: rule.id,
            name: rule.name,
            enabled: rule.enabled,
            symbol: rule.symbol,
        })
        .collect()
}

/// Full detail of one model, including the automations that use it.
#[tauri::command]
pub async fn get_ml_model(
    model_id: String,
    state: State<'_, ModelRegistry>,
    app_state: State<'_, AppState>,
) -> Result<ModelDetailDto, String> {
    let used_by = usages_of(&app_state, &model_id);
    build_detail(&state.models_dir, &model_id, used_by)
}

/// Serializes edits and deletes of model metadata files. Without it two edits read the same
/// document and the later rename erases the earlier one, and a delete between an edit's read and
/// rename gets undone.
static MODEL_FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_model_files() -> std::sync::MutexGuard<'static, ()> {
    MODEL_FILES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_NOTES_CHARS: usize = 4000;
pub const MAX_TAGS: usize = 12;
pub const MAX_TAG_CHARS: usize = 30;

/// Edit only `display_name`, `notes` and `tags` of a model's metadata. Every other field,
/// including ones this build does not know about, is written back unchanged. The edit is a NEW VERSION
/// of the model in the audit ledger (the earlier one is kept); the metadata file is its mirror, replaced
/// atomically after the ledger has the version. Saving an edit that changes nothing adds no version.
pub(crate) fn update_model_metadata(
    models_dir: &Path,
    model_id: &str,
    display_name: Option<&str>,
    notes: Option<&str>,
    tags: Option<&[String]>,
    actor: &str,
) -> Result<VersionRecord, String> {
    validate_model_id(model_id)?;
    let _guard = lock_model_files();
    let ledger = model_ledger(models_dir)?;
    if sync_model_locked(&ledger, models_dir, model_id)? {
        return Err(deleted_error(model_id));
    }
    let path = metadata_path(models_dir, model_id);
    let text = fs::read_to_string(&path).map_err(|_| format!("Unknown model `{model_id}` (no metadata file)"))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| format!("Failed to parse model metadata: {e}"))?;
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| "model metadata is not a JSON object".to_string())?;
    let mut changed: Vec<&str> = Vec::new();

    if let Some(name) = display_name {
        let name = name.trim();
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(format!("display name is too long (max {MAX_NAME_CHARS} characters)"));
        }
        obj.insert("display_name".into(), Value::String(name.to_string()));
        changed.push("display name");
    }
    if let Some(notes) = notes {
        if notes.chars().count() > MAX_NOTES_CHARS {
            return Err(format!("notes are too long (max {MAX_NOTES_CHARS} characters)"));
        }
        obj.insert("notes".into(), Value::String(notes.to_string()));
        changed.push("notes");
    }
    if let Some(tags) = tags {
        let mut clean: Vec<String> = Vec::new();
        for tag in tags {
            let tag = tag.trim();
            if tag.is_empty() || clean.iter().any(|t| t == tag) {
                continue;
            }
            if tag.chars().count() > MAX_TAG_CHARS {
                return Err(format!("tag `{tag}` is too long (max {MAX_TAG_CHARS} characters)"));
            }
            clean.push(tag.to_string());
        }
        if clean.len() > MAX_TAGS {
            return Err(format!("too many tags ({}; max {MAX_TAGS})", clean.len()));
        }
        obj.insert("tags".into(), serde_json::json!(clean));
        changed.push("tags");
    }

    let body = serde_json::to_string_pretty(&doc).map_err(|e| format!("Failed to encode metadata: {e}"))?;
    let summary = if changed.is_empty() { "Saved without changes".to_string() } else { format!("Edited {}", changed.join(", ")) };
    ledger::save_working_copy(&ledger, Kind::Model, model_id, &path, body.as_bytes(), actor, &summary)
}

/// Rename a model and edit its notes and tags. Omitted fields are left alone.
#[tauri::command]
pub async fn update_ml_model(
    model_id: String,
    display_name: Option<String>,
    notes: Option<String>,
    tags: Option<Vec<String>>,
    state: State<'_, ModelRegistry>,
    app_state: State<'_, AppState>,
) -> Result<ModelDetailDto, String> {
    update_ml_model_inner(
        &app_state, &state.models_dir, &model_id, display_name.as_deref(), notes.as_deref(), tags.as_deref(), "user",
    )
}

pub(crate) fn update_ml_model_inner(
    state: &AppState,
    models_dir: &Path,
    model_id: &str,
    display_name: Option<&str>,
    notes: Option<&str>,
    tags: Option<&[String]>,
    actor: &str,
) -> Result<ModelDetailDto, String> {
    let result = update_model_metadata(models_dir, model_id, display_name, notes, tags, actor)
        .and_then(|_| build_detail(models_dir, model_id, usages_of(state, model_id)));
    match &result {
        Ok(detail) => {
            let label = if detail.display_name.is_empty() { model_id } else { &detail.display_name };
            crate::activity::record(
                state, actor, "model.update", "model", model_id, &format!("Edited details of {label}"), true, None,
            );
        }
        Err(error) => crate::activity::record(
            state, actor, "model.update", "model", model_id, &format!("Editing {model_id} failed"), false,
            Some(error.clone()),
        ),
    }
    result
}

// ── Training ─────────────────────────────────────────────────────────

pub(crate) use ml_estimators::validate_hyperparams;

pub(crate) const MAX_HOLDOUT_PCT: f64 = 40.0;
pub(crate) const TRADE_MODES: &[&str] = &["long_flat", "long_short"];
/// How long a training run may take before it is stopped. Override with `APEX_ML_TRAIN_TIMEOUT_SECS`.
pub(crate) const DEFAULT_TRAIN_TIMEOUT_SECS: u64 = 20 * 60;

pub(crate) fn training_timeout() -> std::time::Duration {
    timeout_from(std::env::var("APEX_ML_TRAIN_TIMEOUT_SECS").ok().as_deref())
}

fn timeout_from(setting: Option<&str>) -> std::time::Duration {
    let secs = setting
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.clamp(1, 7200))
        .unwrap_or(DEFAULT_TRAIN_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Request after validation, with dataset defaults applied, plus the JSON sent to the trainer.
#[derive(Debug)]
pub(crate) struct PreparedTraining {
    pub payload: Value,
    pub data_path: String,
}

/// Validate a request and build the trainer payload. No I/O except reading the dataset sidecar and
/// checking that the training file exists (or creating the bundled sample).
pub(crate) fn prepare_training(
    request: &MLTrainingRequestDto,
    models_dir: &Path,
    runtime_paths: &python_runtime::RuntimePaths,
    actor: &str,
) -> Result<PreparedTraining, String> {
    ml_estimators::validate_algorithm_id(&request.algorithm)?;
    validate_hyperparams(&request.algorithm, &request.hyperparams)?;
    if request.n_splits < 2 || request.n_splits > 20 {
        return Err("n_splits must be between 2 and 20".into());
    }
    let trade_mode = request.trade_mode.as_deref().unwrap_or("long_flat");
    if !TRADE_MODES.contains(&trade_mode) {
        return Err(format!("trade_mode must be one of {}", TRADE_MODES.join(", ")));
    }
    let holdout_pct = request.holdout_pct.unwrap_or(0.0);
    if !holdout_pct.is_finite() || !(0.0..=MAX_HOLDOUT_PCT).contains(&holdout_pct) {
        return Err(format!("holdout_pct must be between 0 and {MAX_HOLDOUT_PCT}"));
    }
    if let Some(seed) = request.random_state {
        if !(0..=2_147_483_647).contains(&seed) {
            return Err("random_state must be an integer from 0 to 2147483647".into());
        }
    }
    for lag in &request.lag_periods {
        if *lag < 1 || *lag > 250 {
            return Err("lag_periods values must be between 1 and 250".into());
        }
    }
    if let Some(col) = &request.return_column {
        validation::validate_string_length(col, "return_column")?;
    }

    let mut payload = serde_json::json!({
        "algorithm": request.algorithm,
        "hyperparams": request.hyperparams,
        "n_splits": request.n_splits,
        "lag_periods": request.lag_periods,
        "output_dir": models_dir.to_string_lossy().to_string(),
        "source": if actor == "copilot" { "copilot" } else { "user" },
        "return_column": request.return_column,
        "trade_mode": trade_mode,
        "holdout_pct": holdout_pct,
        "random_state": request.random_state,
    });
    // What the model is built from, as ledger versions, so a model traces to the exact code and data.
    // Stored in the model's metadata by the trainer.
    let mut provenance = serde_json::Map::new();
    if request.dataset_version.is_some() && request.dataset.is_none() {
        return Err("dataset_version needs a dataset".into());
    }
    if request.algorithm == "custom" {
        let name = request
            .estimator_file
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| "algorithm `custom` needs an estimator_file (save one in the Train tab first)".to_string())?;
        let path = ml_estimators::resolve_for_training(runtime_paths.work_root(), name)?;
        payload["estimator_file"] = serde_json::json!(name);
        payload["estimator_path"] = serde_json::json!(path.to_string_lossy());
        let version = fs::read(&path)
            .ok()
            .and_then(|bytes| estimator_ledger_version(runtime_paths, name, &Ledger::sha256_hex(&bytes)));
        provenance.insert("estimator_file_version".into(), serde_json::json!(version));
    }

    if request.algorithm == ml_estimators::RECIPE_ALGORITHM {
        let name = request
            .recipe
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| "algorithm `recipe` needs a recipe name (save one in the algorithm builder first)".to_string())?;
        let recipe = ml_recipes::load_recipe(runtime_paths.work_root(), name)?;
        // The saved file may have been edited by hand; the structural rules apply again. The deep
        // check needs Python and runs in `train_core_with`.
        let issues = ml_recipes::structural_issues(&recipe);
        if !issues.is_empty() {
            return Err(format!("Algorithm `{name}` is not valid: {}", ml_recipes::join_issues(&issues)));
        }
        // Parameters live in the recipe, so nothing the request carried applies.
        payload["hyperparams"] = serde_json::json!({});
        payload["recipe"] = serde_json::to_value(&recipe).map_err(|e| format!("Failed to encode the recipe: {e}"))?;
        payload["recipe_name"] = serde_json::json!(name);
        provenance.insert("recipe_version".into(), serde_json::json!(recipe_ledger_version(runtime_paths, name)));
    }

    let data_path;
    if let Some(name) = &request.dataset {
        let dir = ml_datasets::datasets_dir(runtime_paths);
        // The latest version unless the request names one. The data file of a version is write-once, so
        // the version and its hash say exactly which rows the model saw.
        let info = ml_datasets::read_info_version(&dir, name, request.dataset_version)?;
        provenance.insert("dataset_version".into(), serde_json::json!(info.version));
        if !info.csv_sha256.is_empty() {
            provenance.insert("dataset_csv_sha256".into(), serde_json::json!(info.csv_sha256));
        }
        data_path = info.path.clone();
        validation::validate_path(&data_path)?;
        if !runtime_paths.resolve_user_relative_path(&data_path).exists() {
            return Err(format!("dataset `{name}` has no data file at {data_path}; rebuild it"));
        }
        // Catalog features AND the dataset's custom formulas are columns of the CSV.
        let available = ml_datasets::all_feature_columns(&info);
        let features = if request.feature_columns.is_empty() {
            available.clone()
        } else {
            for f in &request.feature_columns {
                if !available.contains(f) {
                    return Err(format!(
                        "feature `{f}` is not in dataset `{name}` (it has: {})",
                        available.join(", ")
                    ));
                }
            }
            request.feature_columns.clone()
        };
        payload["data_path"] = serde_json::json!(data_path);
        payload["target_column"] = serde_json::json!("target");
        payload["feature_columns"] = serde_json::json!(features);
        if request.return_column.is_none() {
            payload["return_column"] = serde_json::json!("fwd_ret");
        }
        payload["dataset"] = serde_json::json!(name);
        payload["horizon_bars"] = serde_json::json!(info.label.horizon_bars);
        // The recipe a model needs to rebuild its features at inference time is written into the
        // model's metadata by the trainer, from here. Read from the serialized sidecar so a dataset
        // built before these fields existed simply yields the defaults.
        let side = serde_json::to_value(&info).unwrap_or(Value::Null);
        payload["dataset_meta"] = serde_json::json!({
            "name": info.name,
            "symbols": info.symbols,
            "timeframe": info.timeframe,
            "label": info.label,
            "custom_features": side.get("custom_features").cloned().unwrap_or_else(|| serde_json::json!([])),
            "context_symbols": side.get("context_symbols").cloned().unwrap_or_else(|| serde_json::json!([])),
            "fee_pct": side.get("fee_pct").and_then(|v| v.as_f64()).unwrap_or(0.0),
        });
    } else {
        validation::validate_path(&request.data_path)?;
        validation::validate_string_length(&request.target_column, "target_column")?;
        for col in &request.feature_columns {
            validation::validate_string_length(col, "feature_column")?;
        }
        if request.data_path.trim().is_empty() {
            return Err("data_path is required when no dataset is given".into());
        }
        if request.target_column.trim().is_empty() {
            return Err("target_column is required when no dataset is given".into());
        }
        if request.feature_columns.is_empty() {
            return Err("feature_columns must not be empty".into());
        }
        ensure_training_dataset(&request.data_path, runtime_paths)?;
        data_path = request.data_path.clone();
        payload["data_path"] = serde_json::json!(request.data_path);
        payload["target_column"] = serde_json::json!(request.target_column);
        payload["feature_columns"] = serde_json::json!(request.feature_columns);
    }

    payload["provenance"] = Value::Object(provenance);
    Ok(PreparedTraining { payload, data_path })
}

/// The ledger version of a custom estimator file whose content is `sha256`: the newest version of the
/// file's object with that content. None when the ledger has no such version.
fn estimator_ledger_version(runtime_paths: &python_runtime::RuntimePaths, name: &str, sha256: &str) -> Option<u32> {
    let ledger = crate::activity::ledger_for(runtime_paths.data_dir()).ok()?;
    let stem = name.strip_suffix(".py").unwrap_or(name);
    [name.to_string(), format!("estimators/{name}"), stem.to_string()].iter().find_map(|id| {
        ledger
            .history(Kind::Estimator, id)
            .into_iter()
            .rev()
            .find(|r| !r.is_tombstone() && r.content_hash.as_deref() == Some(sha256))
            .map(|r| r.version)
    })
}

/// The ledger version of a saved recipe: its current head.
fn recipe_ledger_version(runtime_paths: &python_runtime::RuntimePaths, name: &str) -> Option<u32> {
    let ledger = crate::activity::ledger_for(runtime_paths.data_dir()).ok()?;
    [name.to_string(), format!("algorithms/{name}.json"), format!("{name}.json")]
        .iter()
        .find_map(|id| ledger.head(Kind::Recipe, id).filter(|h| !h.is_tombstone()).map(|h| h.version))
}

/// Train a new ML model by invoking the Python trainer.
#[tauri::command]
pub async fn train_ml_model(
    request: MLTrainingRequestDto,
    state: State<'_, ModelRegistry>,
    runtime_paths: State<'_, python_runtime::RuntimePaths>,
    app_state: State<'_, AppState>,
) -> Result<MLTrainingResultDto, String> {
    train_ml_model_inner(request, &state.models_dir, runtime_paths.inner(), &app_state, "user").await
}

/// Shared training implementation, also used by the copilot agent loop. `actor` is "user" or
/// "copilot". Records a `model.train` activity entry for success and for failure.
pub(crate) async fn train_ml_model_inner(
    request: MLTrainingRequestDto,
    models_dir: &Path,
    runtime_paths: &python_runtime::RuntimePaths,
    state: &AppState,
    actor: &str,
) -> Result<MLTrainingResultDto, String> {
    let algorithm = match (request.algorithm.as_str(), request.recipe.as_deref()) {
        (ml_estimators::RECIPE_ALGORITHM, Some(name)) => {
            let label = ml_recipes::load_recipe(runtime_paths.work_root(), name)
                .ok()
                .map(|r| r.label)
                .filter(|l| !l.trim().is_empty())
                .unwrap_or_else(|| name.to_string());
            format!("algorithm {label}")
        }
        _ => request.algorithm.clone(),
    };
    let source_label = request
        .dataset
        .clone()
        .unwrap_or_else(|| request.data_path.clone());
    let result = train_core(&request, models_dir, runtime_paths, actor).await;
    match &result {
        Ok(r) => {
            let acc = r
                .metrics
                .get("accuracy")
                .map(|a| format!(", accuracy {:.1}%", a * 100.0))
                .unwrap_or_default();
            // Which version of the dataset (and which file, by hash) the model was trained on.
            let data = read_metadata_raw(models_dir, &r.model_id)
                .ok()
                .and_then(|(_, raw)| {
                    let version = raw.get("dataset_version").and_then(|v| v.as_u64())?;
                    let sha = raw.get("dataset_csv_sha256").and_then(|v| v.as_str()).unwrap_or("");
                    Some(format!(" version {version}{}", if sha.len() >= 12 { format!(" (csv sha256 {})", &sha[..12]) } else { String::new() }))
                })
                .unwrap_or_default();
            crate::activity::record(
                state, actor, "model.train", "model", &r.model_id,
                &format!("Trained {algorithm} on {source_label}{data}{acc}"), true, None,
            );
        }
        Err(error) => crate::activity::record(
            state, actor, "model.train", "model", "",
            &format!("Training {algorithm} on {source_label} failed"), false, Some(error.clone()),
        ),
    }
    result
}

/// Version 1 (action "create") of a freshly trained model: the metadata the trainer wrote.
fn register_trained_model(models_dir: &Path, model_id: &str, actor: &str) -> Result<VersionRecord, String> {
    validate_model_id(model_id)?;
    let _guard = lock_model_files();
    let bytes = fs::read(metadata_path(models_dir, model_id))
        .map_err(|e| format!("Failed to read the new model's metadata: {e}"))?;
    let doc: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut summary = format!("Trained {}", doc.get("algorithm").and_then(|v| v.as_str()).unwrap_or("a model"));
    if let Some(name) = doc.get("dataset").and_then(|v| v.as_str()) {
        summary.push_str(&format!(" on dataset {name}"));
        if let Some(v) = doc.get("dataset_version").and_then(|v| v.as_u64()) {
            summary.push_str(&format!(" version {v}"));
        }
    }
    model_ledger(models_dir)?.put(Kind::Model, model_id, &bytes, actor, &summary)
}

async fn train_core(
    request: &MLTrainingRequestDto,
    models_dir: &Path,
    runtime_paths: &python_runtime::RuntimePaths,
    actor: &str,
) -> Result<MLTrainingResultDto, String> {
    train_core_with(request, models_dir, runtime_paths, actor, training_timeout()).await
}

/// [`train_core`] with an explicit time limit. When it runs out the trainer process is killed and
/// the caller records a failed activity entry.
pub(crate) async fn train_core_with(
    request: &MLTrainingRequestDto,
    models_dir: &Path,
    runtime_paths: &python_runtime::RuntimePaths,
    actor: &str,
    limit: std::time::Duration,
) -> Result<MLTrainingResultDto, String> {
    let prepared = prepare_training(request, models_dir, runtime_paths, actor)?;

    // Keys and values are checked against the installed estimator before a trainer is started.
    if request.algorithm == ml_estimators::RECIPE_ALGORITHM {
        // Every member's parameters go through the catalog validator inside the recipe check.
        let recipe: RecipeDto = serde_json::from_value(prepared.payload["recipe"].clone())
            .map_err(|e| format!("Failed to read the recipe: {e}"))?;
        ml_recipes::require_valid(runtime_paths, &recipe).await?;
    } else {
        let catalog = ml_estimators::fetch_algorithms(runtime_paths).await?;
        ml_estimators::validate_with_catalog(&catalog, &request.algorithm, &request.hyperparams)?;
    }

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
        .arg("ml.trainer")
        .arg("--config-json")
        .arg(
            serde_json::to_string(&prepared.payload)
                .map_err(|e| format!("Failed to encode training payload: {e}"))?,
        );
    // Numeric libraries default to every core; a training run should not take the whole machine.
    for var in ["OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS"] {
        if std::env::var_os(var).is_none() {
            command.env(var, "4");
        }
    }
    let output = ml_estimators::output_with_limit(command, limit)
        .await
        .map_err(|_| {
            format!(
                "Training took longer than {} and was stopped. Use fewer rounds, a smaller dataset or a faster algorithm.",
                ml_estimators::describe_duration(limit)
            )
        })?
        .map_err(|e| format!("Failed to start Python trainer with {}: {e}", python.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        let message = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .or_else(|| stdout.lines().rev().find(|line| !line.trim().is_empty()))
            .unwrap_or("ML training failed without an error message");
        return Err(python_runtime::enrich_python_error(
            message,
            &["APEX_ML_PYTHON_PATH", "APEX_STRATEGY_PYTHON_PATH"],
        ));
    }

    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| "Python trainer did not return a JSON result".to_string())?;
    let result: TrainerCliResult = serde_json::from_str(json_line)
        .map_err(|e| format!("Failed to parse trainer output: {e}"))?;

    // Version 1 of the model in the audit ledger. A failure is logged loudly and not fatal: the model
    // exists on disk, and it is registered the first time it is listed or edited.
    remember_data_dir(models_dir, runtime_paths.data_dir());
    if let Err(error) = register_trained_model(models_dir, &result.model_id, actor) {
        tracing::error!(%error, model_id = %result.model_id, "could not record the new model in the audit ledger");
    }

    tracing::info!(
        model_id = %result.model_id,
        algorithm = %result.algorithm,
        model_path = %result.model_path,
        metadata_path = %result.metadata_path,
        created_at = %result.created_at,
        data_path = %result.data_path,
        target_column = %result.target_column,
        "ML training completed"
    );

    Ok(MLTrainingResultDto {
        model_id: result.model_id,
        metrics: result.metrics,
        feature_names: result.feature_names,
        status: "completed".into(),
        duration_secs: result.duration_secs,
    })
}

// ── Inference ─────────────────────────────────────────────────────────

/// One model's signal on a symbol's latest bar.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelSignalDto {
    pub model_id: String,
    pub symbol: String,
    pub signal: i64,
    pub probability: Option<f64>,
    pub features_used: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PredictorCliResult {
    signal: i64,
    probability: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    model_id: String,
    #[serde(default)]
    features_used: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PredictorBatchResult {
    signals: Vec<i64>,
    probabilities: Vec<Option<f64>>,
}

struct ScoringModel {
    meta: ModelMetadataFile,
    model_path: PathBuf,
    /// Formulas the model's features include, read from the metadata (empty for older models).
    custom_features: Vec<crate::dto::CustomFeatureDto>,
    /// Symbols those formulas read with `other(...)`, plus any the metadata lists.
    context_symbols: Vec<String>,
}

fn load_scoring_model(models_dir: &Path, model_id: &str) -> Result<ScoringModel, String> {
    let (meta, raw) = read_metadata_raw(models_dir, model_id)?;
    validate_model_file(&meta.model_file)?;
    let model_path = models_dir.join(&meta.model_file);
    if !model_path.exists() {
        return Err(format!("Model artifact missing: {}", model_path.display()));
    }
    let (custom_features, context_symbols) = ml_features::custom_spec_from_metadata(&raw)?;
    // Only what this model's features actually use: a model trained on a subset of the dataset's
    // columns must not be refused because an unused formula's context symbol has no bars.
    let (custom_features, context_symbols) =
        ml_features::narrow_to_used(&custom_features, &context_symbols, &meta.feature_names)?;
    Ok(ScoringModel { meta, model_path, custom_features, context_symbols })
}

fn scoring_timeframe(meta: &ModelMetadataFile) -> &'static str {
    meta.timeframe
        .as_deref()
        .and_then(ml_features::canonical_timeframe)
        .unwrap_or("1d")
}

/// How old the newest stored bar may be before a model scored on it is considered stale.
/// Intraday allows a long weekend; daily and weekly allow a few missed sessions.
pub(crate) fn max_bar_age(timeframe: &str) -> chrono::Duration {
    match timeframe {
        "1w" => chrono::Duration::days(14),
        "1d" => chrono::Duration::days(6),
        _ => chrono::Duration::days(4),
    }
}

/// Timeframe (canonical, e.g. "1h") a model scores on.
pub(crate) fn model_timeframe(models_dir: &Path, model_id: &str) -> Result<&'static str, String> {
    Ok(scoring_timeframe(&load_scoring_model(models_dir, model_id)?.meta))
}

/// Refresh the bars `model_signal_inner` will score on (the model's own timeframe, not daily)
/// and refuse when the newest one is too old to trade or report on.
pub(crate) async fn ensure_fresh_model_bars(
    state: &AppState,
    models_dir: &Path,
    model_id: &str,
    symbol: &str,
) -> Result<(), String> {
    let timeframe = model_timeframe(models_dir, model_id)?;
    let bars = super::data::load_bars(state, symbol, Some(timeframe), 300).await?;
    match bars.last() {
        None => return Err(format!("no {timeframe} bars for {symbol}")),
        Some(last) if chrono::Utc::now() - last.time > max_bar_age(timeframe) => {
            return Err(format!(
                "newest {timeframe} bar for {symbol} is from {} (stale); refusing to score on old data",
                last.time.format("%Y-%m-%d %H:%M")
            ))
        }
        Some(_) => {}
    }
    // Formulas that read other symbols (other("SPY", ...)) are scored from stored bars, so those
    // symbols need the same refresh, or the context goes stale and every run fails.
    for context in scoring_context_symbols(models_dir, model_id)? {
        if context.eq_ignore_ascii_case(symbol) {
            continue;
        }
        super::data::load_bars(state, &context, Some(timeframe), 300)
            .await
            .map_err(|e| format!("could not refresh context symbol {context}: {e}"))?;
    }
    Ok(())
}

/// The other symbols this model's formulas read when it scores (empty for most models).
pub(crate) fn scoring_context_symbols(models_dir: &Path, model_id: &str) -> Result<Vec<String>, String> {
    Ok(load_scoring_model(models_dir, model_id)?.context_symbols)
}

/// How long one scoring call may run before it is killed.
const PREDICT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Run `python -m ml.predict <args>` and return the last JSON line of its output.
async fn run_predictor(
    runtime_paths: &python_runtime::RuntimePaths,
    args: Vec<OsString>,
    context: &str,
) -> Result<String, String> {
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
        .arg("-m")
        .arg("ml.predict")
        .args(args);
    // Scoring loads the model, and a model can be a user's own estimator, so a stuck predictor
    // must not hang an automation or the Copilot forever. The child is killed on timeout.
    let output = ml_estimators::output_with_limit(command, PREDICT_TIMEOUT)
        .await
        .map_err(|why| format!("{context}: the predictor {why}; the model or its estimator may be stuck"))?
        .map_err(|e| format!("Failed to start Python predictor with {}: {e}", python.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let message = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .or_else(|| stdout.lines().rev().find(|l| !l.trim().is_empty()))
            .unwrap_or("prediction failed without an error message");
        return Err(format!("Predictor error for {context}: {message}"));
    }
    stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| "Predictor returned no JSON".to_string())
}

/// Compute exactly the model's features for the newest stored bar of `symbol` (through
/// [`ml_features`], the pipeline the dataset builder uses) and score them via the Python predictor.
pub(crate) async fn model_signal_inner(
    model_id: &str,
    symbol: &str,
    models_dir: &Path,
    runtime_paths: &python_runtime::RuntimePaths,
    storage: &std::sync::Arc<dyn apex_core::ports::storage::StoragePort>,
) -> Result<ModelSignalDto, String> {
    use apex_core::domain::models::{OHLCVQuery, Symbol};

    validation::validate_symbol(symbol)?;
    let model = load_scoring_model(models_dir, model_id)?;
    let names = &model.meta.feature_names;
    let need = ml_features::required_bars_with(names, &model.custom_features)?;
    let timeframe = ml_features::parse_timeframe(scoring_timeframe(&model.meta))
        .unwrap_or(apex_core::domain::models::Timeframe::D1);

    let limit = (need + 50).clamp(300, 2000);
    let bars = storage
        .query_ohlcv(OHLCVQuery {
            symbol: Symbol(symbol.to_string()),
            timeframe: timeframe.clone(),
            from: chrono::DateTime::UNIX_EPOCH,
            to: chrono::Utc::now(),
            limit: Some(limit),
        })
        .await
        .map_err(|e| format!("Failed to load bars for {symbol}: {e}"))?;
    if bars.len() < need {
        return Err(format!(
            "Not enough stored bars for {symbol} ({} loaded; this model's features need {need}); fetch history first",
            bars.len()
        ));
    }

    let context_symbols = ml_features::context_symbols_needed(&model.custom_features, &model.context_symbols)?;
    let context = ml_features::load_context_from_storage(storage, &context_symbols, timeframe.clone(), limit * 2).await?;
    if let Some(last) = bars.last() {
        ml_features::check_context_fresh(&context, last.time, max_bar_age(scoring_timeframe(&model.meta)))?;
    }
    let features = ml_features::latest_features_with(&bars, names, &model.custom_features, &context, symbol)?;
    let stdout = run_predictor(
        runtime_paths,
        vec![
            "--model-path".into(),
            model.model_path.clone().into_os_string(),
            "--features-json".into(),
            serde_json::to_string(&features)
                .map_err(|e| format!("Failed to encode features: {e}"))?
                .into(),
        ],
        &format!("{model_id} on {symbol}"),
    )
    .await?;
    let res: PredictorCliResult = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse predictor output `{stdout}`: {e}"))?;

    Ok(ModelSignalDto {
        model_id: model_id.to_string(),
        symbol: symbol.to_string(),
        signal: res.signal,
        probability: res.probability,
        features_used: res.features_used,
    })
}

/// Score the latest bars of `symbol` with a trained model — returns
/// {signal, probability}. Shared by the UI, copilot, and automation engine.
#[tauri::command]
pub async fn predict_model_signal(
    model_id: String,
    symbol: String,
    models: tauri::State<'_, ModelRegistry>,
    runtime_paths: tauri::State<'_, python_runtime::RuntimePaths>,
    app_state: tauri::State<'_, crate::state::AppState>,
) -> Result<ModelSignalDto, String> {
    model_signal_inner(
        &model_id,
        &symbol,
        &models.models_dir,
        &runtime_paths,
        &app_state.storage,
    )
    .await
}

pub const MAX_HISTORY_BARS: usize = 300;

/// Rows of the model's feature matrix for the last `n` bars that can be scored (every feature
/// defined). Returns the bar indexes and the rows, computed once for the whole series.
pub(crate) fn scorable_rows(
    bars: &[apex_core::domain::models::OHLCV],
    names: &[String],
    n: usize,
) -> Result<(Vec<usize>, Vec<Vec<f64>>), String> {
    scorable_rows_with(bars, names, &[], &ml_expr::Context::new(), n)
}

/// [`scorable_rows`] for a model with custom features and context symbols.
pub(crate) fn scorable_rows_with(
    bars: &[apex_core::domain::models::OHLCV],
    names: &[String],
    custom: &[crate::dto::CustomFeatureDto],
    context: &ml_expr::Context,
    n: usize,
) -> Result<(Vec<usize>, Vec<Vec<f64>>), String> {
    let matrix = ml_features::model_matrix_with(bars, names, custom, context)?;
    let first = bars.len().saturating_sub(n);
    let mut idx = Vec::new();
    let mut rows = Vec::new();
    for i in first..bars.len() {
        if matrix[i].iter().all(|v| v.is_finite()) {
            idx.push(i);
            rows.push(matrix[i].clone());
        }
    }
    Ok((idx, rows))
}

/// Signals a model would have given on each of the last `bars` bars of `symbol` (capped at 300).
/// The feature matrix is computed once and all rows are scored in a single predictor call.
#[tauri::command]
pub async fn model_signal_history(
    model_id: String,
    symbol: String,
    bars: Option<usize>,
    models: tauri::State<'_, ModelRegistry>,
    runtime_paths: tauri::State<'_, python_runtime::RuntimePaths>,
    app_state: tauri::State<'_, AppState>,
) -> Result<ModelSignalHistoryDto, String> {
    validation::validate_symbol(&symbol)?;
    let n = bars.unwrap_or(60);
    if n == 0 {
        return Err("bars must be at least 1".into());
    }
    let n = n.min(MAX_HISTORY_BARS);
    let model = load_scoring_model(&models.models_dir, &model_id)?;
    let names = &model.meta.feature_names;
    let need = ml_features::required_bars_with(names, &model.custom_features)?;

    let timeframe_name = scoring_timeframe(&model.meta);
    let loaded = super::data::load_bars(&app_state, &symbol, Some(timeframe_name), n + need + 5).await?;
    let mut context = ml_expr::Context::new();
    for sym in ml_features::context_symbols_needed(&model.custom_features, &model.context_symbols)? {
        let ctx_bars = super::data::load_bars(&app_state, &sym, Some(timeframe_name), (n + need + 5) * 2).await?;
        if ctx_bars.is_empty() {
            return Err(format!("no stored bars for context symbol {sym}, which this model's custom features read; fetch its history first"));
        }
        context.insert(sym, ctx_bars);
    }
    let (idx, rows) = scorable_rows_with(&loaded, names, &model.custom_features, &context, n)?;
    if rows.is_empty() {
        return Err(format!(
            "Not enough stored bars for {symbol} ({} loaded; this model's features need {need}); fetch history first",
            loaded.len()
        ));
    }

    let stdout = run_predictor(
        &runtime_paths,
        vec![
            "--model-path".into(),
            model.model_path.clone().into_os_string(),
            "--features-matrix-json".into(),
            serde_json::to_string(&rows)
                .map_err(|e| format!("Failed to encode features: {e}"))?
                .into(),
        ],
        &format!("{model_id} on {symbol}"),
    )
    .await?;
    let batch: PredictorBatchResult = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse predictor output `{stdout}`: {e}"))?;
    if batch.signals.len() != idx.len() || batch.probabilities.len() != idx.len() {
        return Err("Predictor returned a different number of rows than were sent".into());
    }

    let points = idx
        .iter()
        .enumerate()
        .map(|(k, &i)| ModelSignalPointDto {
            time: loaded[i].time.to_rfc3339(),
            close: loaded[i].close,
            signal: batch.signals[k],
            probability: batch.probabilities[k],
        })
        .collect();
    Ok(ModelSignalHistoryDto { model_id, symbol, points })
}

// ── Delete ───────────────────────────────────────────────────────────

/// Delete a model the audit way: append a tombstone version to the ledger, then MOVE its metadata,
/// artifact, training log and estimator copy to `models/archive/<id>@v<n>/`. Nothing is removed. The
/// model disappears from the list and cannot be scored; `restore_model` brings it back. Returns whether
/// there was a live model.
pub(crate) fn delete_model_files(models_dir: &Path, model_id: &str, actor: &str) -> Result<bool, String> {
    validate_model_id(model_id)?;
    let _guard = lock_model_files();
    let ledger = model_ledger(models_dir)?;
    // Registers a model from before the ledger, so its last state is in the history before the delete.
    sync_model_locked(&ledger, models_dir, model_id)?;
    if !ledger.head(Kind::Model, model_id).is_some_and(|h| !h.is_tombstone()) {
        return Ok(false);
    }
    let files = model_file_set(models_dir, model_id)?;
    let record = ledger.tombstone(
        Kind::Model,
        model_id,
        actor,
        "Deleted: files moved to models/archive, every version stays in the ledger",
    )?;
    move_files(&files, &archive_dir(models_dir, model_id, record.version)).map_err(|e| {
        format!("{model_id} is marked deleted in the audit ledger but its files could not be archived ({e}); this is retried the next time models are listed")
    })?;
    Ok(true)
}

/// Bring a model back: the files return from `models/archive/<id>@v<n>/` and a new version (action
/// "restore") is appended whose metadata equals version `version` (default: the last one before the
/// delete). For a model that is not deleted, `version` is required and only the metadata is rolled back,
/// again as a new version. Returns the new ledger record.
pub(crate) fn restore_model_files(
    models_dir: &Path,
    model_id: &str,
    version: Option<u32>,
    actor: &str,
) -> Result<VersionRecord, String> {
    validate_model_id(model_id)?;
    let _guard = lock_model_files();
    let ledger = model_ledger(models_dir)?;
    sync_model_locked(&ledger, models_dir, model_id)?;
    let head = ledger
        .head(Kind::Model, model_id)
        .ok_or_else(|| format!("There is no model `{model_id}` in the audit ledger"))?;
    let target = match version {
        Some(v) => v,
        None if head.is_tombstone() => ledger
            .history(Kind::Model, model_id)
            .into_iter()
            .rev()
            .find(|r| !r.is_tombstone())
            .map(|r| r.version)
            .ok_or_else(|| format!("Model `{model_id}` has no earlier version to restore"))?,
        None => return Err("Say which version to restore".into()),
    };
    let bytes = ledger.content(Kind::Model, model_id, target)?;
    let meta: ModelMetadataFile = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Version {target} of `{model_id}` does not hold readable model metadata: {e}"))?;
    validate_model_file(&meta.model_file)?;

    let path = metadata_path(models_dir, model_id);
    let mut to_move: Vec<PathBuf> = Vec::new();
    let archive = archive_dir(models_dir, model_id, head.version);
    if head.is_tombstone() {
        if !archive.is_dir() {
            return Err(format!("The archived files of `{model_id}` were not found at {}", archive.display()));
        }
        for name in [meta.model_file.clone(), format!("{model_id}.log"), format!("{model_id}_estimator.py")] {
            let from = archive.join(&name);
            if from.exists() {
                if models_dir.join(&name).exists() {
                    return Err(format!("{name} already exists in the models directory; refusing to overwrite it"));
                }
                to_move.push(from);
            }
        }
        if !to_move.iter().any(|f| f.file_name().and_then(|n| n.to_str()) == Some(meta.model_file.as_str()))
            && !models_dir.join(&meta.model_file).exists()
        {
            return Err(format!("The model file {} of `{model_id}` is not in the archive", meta.model_file));
        }
        if path.exists() {
            return Err(format!("{} already exists; refusing to overwrite it", path.display()));
        }
    }

    // Same order as a save: the new metadata beside its place first, then the ledger, then the rename.
    let pending = ledger::pending_path(&path)?;
    if pending.exists() {
        let aside = pending.with_extension(format!("apexnew.orphan-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        fs::rename(&pending, &aside).map_err(|e| format!("Failed to set aside a leftover pending file: {e}"))?;
    }
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)
            .map_err(|e| format!("Failed to write {}: {e}", pending.display()))?;
        std::io::Write::write_all(&mut f, &bytes).and_then(|_| f.sync_all()).map_err(|e| {
            let _ = fs::remove_file(&pending);
            format!("Failed to write {}: {e}", pending.display())
        })?;
    }
    let undo = |e: String| {
        let _ = fs::remove_file(&pending);
        e
    };
    for from in &to_move {
        let Some(name) = from.file_name() else { continue };
        fs::rename(from, models_dir.join(name)).map_err(|e| undo(format!("Failed to move {} back: {e}", from.display())))?;
    }
    let (record, _) = ledger.restore(Kind::Model, model_id, target, actor).map_err(undo)?;
    fs::rename(&pending, &path).map_err(|e| {
        format!("Restored as version {} in the audit ledger, but {} could not be written ({e}); it is completed the next time models are listed", record.version, path.display())
    })?;
    Ok(record)
}

/// Restore a deleted model (or roll a live one's metadata back to `version`) and record it. The ledger
/// keeps every earlier version. Used by the ledger restore command.
#[allow(dead_code)]
pub(crate) fn restore_model(
    state: &AppState,
    models_dir: &Path,
    model_id: &str,
    version: Option<u32>,
    actor: &str,
) -> Result<ModelDetailDto, String> {
    let result = restore_model_files(models_dir, model_id, version, actor)
        .and_then(|record| build_detail(models_dir, model_id, usages_of(state, model_id)).map(|d| (record, d)));
    match &result {
        Ok((record, _)) => crate::activity::record(
            state,
            actor,
            "model.restore",
            "model",
            model_id,
            &format!("Restored model {model_id} as version {} (from version {})", record.version, record.restored_from.unwrap_or(0)),
            true,
            None,
        ),
        Err(error) => crate::activity::record(
            state, actor, "model.restore", "model", model_id, &format!("Restoring {model_id} failed"), false, Some(error.clone()),
        ),
    }
    result.map(|(_, detail)| detail)
}

pub(crate) fn delete_ml_model_inner(
    state: &AppState,
    models_dir: &Path,
    model_id: &str,
    actor: &str,
) -> Result<bool, String> {
    let result = delete_model_files(models_dir, model_id, actor);
    match &result {
        Ok(true) => crate::activity::record(
            state,
            actor,
            "model.delete",
            "model",
            model_id,
            &format!("Archived model {model_id} (files moved to models/archive; every version stays in the audit ledger)"),
            true,
            None,
        ),
        Ok(false) => {}
        Err(error) => crate::activity::record(
            state, actor, "model.delete", "model", model_id, &format!("Deleting {model_id} failed"), false,
            Some(error.clone()),
        ),
    }
    result
}

/// Delete a trained ML model by ID.
#[tauri::command]
pub async fn delete_ml_model(
    model_id: String,
    state: State<'_, ModelRegistry>,
    app_state: State<'_, AppState>,
) -> Result<bool, String> {
    delete_ml_model_inner(&app_state, &state.models_dir, &model_id, "user")
}

#[cfg(test)]
mod tests {
    use super::super::ml_features::tests::synthetic_bars;
    use super::*;
    use serde_json::json;

    /// An empty models directory with its `data` sibling (where the ledger lives), as in the app.
    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("apex-ml-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data")).unwrap();
        let d = root.join("models");
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Remove the whole temp root of a [`temp`] directory (the models directory and its `data` sibling).
    fn cleanup(models_dir: &Path) {
        let root = models_dir.parent().expect("a temp models directory has a parent");
        // Never remove anything but a root made by `temp`.
        assert!(root.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("apex-ml-")), "{}", root.display());
        let _ = fs::remove_dir_all(root);
    }

    fn hp(v: Value) -> HashMap<String, Value> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn hyperparameters_get_a_structural_check_before_anything_starts() {
        // Everything the old allow-list accepted still passes.
        assert!(validate_hyperparams("random_forest", &hp(json!({"n_estimators": 200, "max_depth": null, "min_samples_leaf": 3, "max_features": "sqrt", "class_weight": "balanced", "random_state": 7}))).is_ok());
        assert!(validate_hyperparams("random_forest", &hp(json!({"max_features": 0.5}))).is_ok());
        assert!(validate_hyperparams("gradient_boosting", &hp(json!({"learning_rate": 0.05, "subsample": 0.8, "max_depth": 3}))).is_ok());
        assert!(validate_hyperparams("logistic_regression", &hp(json!({"C": 0.5, "max_iter": 500, "class_weight": "balanced"}))).is_ok());
        assert!(validate_hyperparams("xgboost", &hp(json!({"colsample_bytree": 0.7, "learning_rate": 0.1}))).is_ok());
        assert!(validate_hyperparams("random_forest", &HashMap::new()).is_ok());
        // Parameters the old list did not know are fine, and so are the new algorithms.
        assert!(validate_hyperparams("random_forest", &hp(json!({"min_samples_split": 8, "bootstrap": false, "criterion": "entropy"}))).is_ok());
        assert!(validate_hyperparams("hist_gradient_boosting", &hp(json!({"max_iter": 50, "l2_regularization": 1.5}))).is_ok());
        assert!(validate_hyperparams("extra_trees", &hp(json!({"n_estimators": 50}))).is_ok());
        assert!(validate_hyperparams("svm", &HashMap::new()).is_err(), "unknown algorithm");

        // Cost parameters have hard caps; the caps themselves are allowed.
        for (algo, key, cap) in [
            ("random_forest", "n_estimators", 5000),
            ("hist_gradient_boosting", "max_iter", 100_000),
            ("logistic_regression", "max_iter", 100_000),
            ("gradient_boosting", "max_depth", 200),
            ("xgboost", "max_depth", 200),
        ] {
            assert!(validate_hyperparams(algo, &hp(json!({ key: cap }))).is_ok(), "{algo} {key}");
            let err = validate_hyperparams(algo, &hp(json!({ key: cap + 1 }))).unwrap_err();
            assert!(err.contains(key) && err.contains(&cap.to_string()), "{err}");
        }
        // Objects, arrays, bad names and too many keys never reach Python.
        for bad in [json!({"init": {"a": 1}}), json!({"n_estimators": [1, 2]}), json!({"bad key": 1}), json!({"1x": 1}), json!({"": 1})] {
            assert!(validate_hyperparams("random_forest", &hp(bad.clone())).is_err(), "{bad}");
        }
        let many: HashMap<String, Value> = (0..61).map(|i| (format!("k{i}"), json!(1))).collect();
        assert!(validate_hyperparams("custom", &many).unwrap_err().contains("too many"));
        // Custom estimators take lists of scalars.
        assert!(validate_hyperparams("custom", &hp(json!({"weights": [1, 2, 3], "k": 4}))).is_ok());
        assert!(validate_hyperparams("custom", &hp(json!({"weights": [[1]]}))).is_err());
    }

    fn param(key: &str, kind: &str, min: Option<f64>, max: Option<f64>, choices: Option<Vec<&str>>) -> crate::dto::ParamDefDto {
        crate::dto::ParamDefDto {
            key: key.into(), label: key.into(), kind: kind.into(), default: Value::Null, min, max,
            choices: choices.map(|c| c.into_iter().map(String::from).collect()), help: String::new(),
        }
    }

    fn mini_catalog() -> Vec<crate::dto::AlgorithmDto> {
        vec![
            crate::dto::AlgorithmDto {
                id: "random_forest".into(), label: "RF".into(), description: String::new(), accepts_extra_params: true,
                params: vec![
                    param("n_estimators", "int", Some(1.0), Some(5000.0), None),
                    param("max_features", "choice", None, None, Some(vec!["sqrt", "log2"])),
                    param("bootstrap", "bool", None, None, None),
                    param("min_weight_fraction_leaf", "float", Some(0.0), Some(0.5), None),
                ],
            },
            crate::dto::AlgorithmDto { id: "custom".into(), label: "C".into(), description: String::new(), accepts_extra_params: true, params: vec![] },
        ]
    }

    #[test]
    fn the_catalog_check_lists_valid_keys_and_checks_types_and_ranges() {
        let cat = mini_catalog();
        let check = |v: Value| ml_estimators::validate_with_catalog(&cat, "random_forest", &hp(v));
        assert!(check(json!({"n_estimators": 10, "max_features": "log2", "bootstrap": false, "min_weight_fraction_leaf": 0.1})).is_ok());
        assert!(check(json!({"max_features": 3})).is_ok(), "a count is a valid max_features");
        assert!(check(json!({"n_jobs": 8})).is_ok(), "n_jobs is clamped by the trainer, not rejected");
        assert!(check(json!({"max_depth": null})).is_err(), "not in this mini catalog");
        let err = check(json!({"definitely_not_real": 1})).unwrap_err();
        assert!(err.contains("unknown hyperparameter `definitely_not_real`"), "{err}");
        assert!(err.contains("valid: n_estimators, max_features, bootstrap, min_weight_fraction_leaf"), "{err}");
        for (bad, key) in [
            (json!({"n_estimators": 0}), "n_estimators"),
            (json!({"n_estimators": 5001}), "n_estimators"),
            (json!({"n_estimators": 10.5}), "n_estimators"),
            (json!({"n_estimators": "10"}), "n_estimators"),
            (json!({"max_features": "auto"}), "max_features"),
            (json!({"bootstrap": 1}), "bootstrap"),
            (json!({"min_weight_fraction_leaf": 0.9}), "min_weight_fraction_leaf"),
            (json!({"n_jobs": 0}), "n_jobs"),
            (json!({"n_jobs": 2.5}), "n_jobs"),
        ] {
            let err = check(bad.clone()).unwrap_err();
            assert!(err.contains(key), "{bad}: {err}");
        }
        // An algorithm that is not installed (xgboost missing) is reported as such.
        let err = ml_estimators::validate_with_catalog(&cat, "xgboost", &HashMap::new()).unwrap_err();
        assert!(err.contains("not available") && err.contains("random_forest"), "{err}");
        // Custom estimators take any parameter names.
        assert!(ml_estimators::validate_with_catalog(&cat, "custom", &hp(json!({"anything": 1}))).is_ok());
    }

    #[test]
    fn model_ids_cannot_escape_the_directory() {
        for ok in ["random_forest_20261106T044919Z", "a-b.c_d"] {
            assert!(validate_model_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", "../x", "a/b", "a\\b", "..", "a..b", "a b", "a:b"] {
            assert!(validate_model_id(bad).is_err(), "{bad}");
        }
        let dir = temp("ids");
        assert!(update_model_metadata(&dir, "../x", Some("n"), None, None, "user").is_err());
        assert!(delete_model_files(&dir, "../x", "user").is_err());
        assert!(build_detail(&dir, "a/b", vec![]).is_err());
        cleanup(&dir);
    }

    const OLD_METADATA: &str = r#"{
      "model_id": "random_forest_20261106T044919Z",
      "algorithm": "random_forest",
      "hyperparams": {},
      "feature_names": ["ret_1", "ret_5", "volume_lag1"],
      "target_column": "target",
      "data_path": "data/exports/GC_F_d1_labeled.csv",
      "n_splits": 5,
      "metrics": {"accuracy": 0.52, "f1": 0.51, "precision": 0.53, "recall": 0.52},
      "model_file": "random_forest_20261106T044919Z.joblib",
      "created_utc": "20261106T044919Z"
    }"#;

    fn write_old_model(dir: &Path) -> String {
        let id = "random_forest_20261106T044919Z";
        fs::write(dir.join(format!("{id}.json")), OLD_METADATA).unwrap();
        fs::write(dir.join(format!("{id}.joblib")), vec![0u8; 321]).unwrap();
        id.to_string()
    }

    #[test]
    fn an_old_format_metadata_file_still_loads() {
        let dir = temp("old");
        let id = write_old_model(&dir);
        let list = load_registered_models(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].model_id, id);
        assert_eq!(list[0].status, "completed");
        assert!((list[0].metrics["accuracy"] - 0.52).abs() < 1e-12);

        let d = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!(d.source, "unknown");
        assert_eq!(d.display_name, "");
        assert!(d.cv_folds.is_empty() && d.feature_importance.is_empty() && d.trading.is_none());
        assert!(d.dataset.is_none() && d.label.is_none() && d.rows.is_none());
        assert_eq!(d.n_splits, 5);
        assert_eq!(d.artifact_bytes, Some(321));
        assert_eq!(d.log_tail, "");
        cleanup(&dir);
    }

    #[test]
    fn a_null_metric_does_not_hide_the_model() {
        let dir = temp("nullmetric");
        let text = OLD_METADATA.replace("\"f1\": 0.51", "\"f1\": null");
        fs::write(dir.join("m.json"), text).unwrap();
        let list = load_registered_models(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].metrics.contains_key("f1"));
        cleanup(&dir);
    }

    #[test]
    fn new_fields_are_read_and_log_tail_is_the_last_4000_chars() {
        let dir = temp("new");
        let id = write_old_model(&dir);
        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["source"] = json!("copilot");
        doc["display_name"] = json!("My model");
        doc["tags"] = json!(["a", "b"]);
        doc["dataset"] = json!("ds");
        doc["symbols"] = json!(["AAPL"]);
        doc["timeframe"] = json!("1d");
        doc["label"] = json!({"horizon_bars": 5, "kind": "direction", "threshold_pct": 0.0});
        doc["rows"] = json!(480);
        doc["duration_secs"] = json!(1.5);
        doc["library_versions"] = json!({"python": "3.12", "scikit_learn": "1.9.1"});
        doc["cv_folds"] = json!([{"fold": 1, "train_rows": 100, "test_rows": 50, "accuracy": 0.6, "f1": 0.5, "precision": 0.5, "recall": 0.5, "baseline_accuracy": 0.5, "trade_return_pct": null, "hit_rate": null, "trades": null}]);
        doc["feature_importance"] = json!([{"feature": "ret_1", "importance": 0.7}]);
        doc["trading"] = json!({"strategy_return_pct": 4.0, "buy_hold_return_pct": 2.0, "hit_rate": 0.55, "trades": 30, "max_drawdown_pct": -3.0, "sharpe": 1.2});
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
        let log: String = (0..5000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        fs::write(dir.join(format!("{id}.log")), &log).unwrap();

        let d = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!(d.source, "copilot");
        assert_eq!(d.display_name, "My model");
        assert_eq!(d.tags, vec!["a", "b"]);
        assert_eq!(d.cv_folds.len(), 1);
        assert_eq!(d.cv_folds[0].trades, None);
        assert_eq!(d.trading.as_ref().unwrap().trades, 30);
        assert_eq!(d.label.as_ref().unwrap().horizon_bars, 5);
        assert_eq!(d.library_versions["scikit_learn"], "1.9.1");
        assert_eq!(d.log_tail.chars().count(), 4000);
        assert!(log.ends_with(&d.log_tail));
        cleanup(&dir);
    }

    #[test]
    fn update_changes_only_the_three_fields_and_keeps_unknown_ones() {
        let dir = temp("update");
        let id = write_old_model(&dir);
        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["future_field"] = json!({"nested": [1, 2, 3]});
        doc["hyperparams"] = json!({"n_estimators": 50});
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();

        update_model_metadata(&dir, &id, Some("  Nice name "), Some("notes here"), Some(&["x".into(), " y ".into(), "x".into(), "".into()]), "user").unwrap();
        let after: Value = serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!(after["display_name"], "Nice name");
        assert_eq!(after["notes"], "notes here");
        assert_eq!(after["tags"], json!(["x", "y"]));
        assert_eq!(after["future_field"], json!({"nested": [1, 2, 3]}));
        for key in ["model_id", "algorithm", "feature_names", "metrics", "model_file", "created_utc", "data_path", "hyperparams", "n_splits", "target_column"] {
            assert_eq!(after[key], doc[key], "{key} changed");
        }
        assert!(!dir.join(format!("{id}.json.tmp")).exists(), "temp file left behind");

        // Omitted fields stay as they are.
        update_model_metadata(&dir, &id, None, Some("only notes"), None, "user").unwrap();
        let again: Value = serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!(again["display_name"], "Nice name");
        assert_eq!(again["notes"], "only notes");
        assert_eq!(again["tags"], json!(["x", "y"]));
        cleanup(&dir);
    }

    #[test]
    fn update_enforces_limits_and_leaves_the_file_alone_on_error() {
        let dir = temp("limits");
        let id = write_old_model(&dir);
        let before = fs::read_to_string(dir.join(format!("{id}.json"))).unwrap();
        assert!(update_model_metadata(&dir, &id, Some(&"n".repeat(81)), None, None, "user").is_err());
        assert!(update_model_metadata(&dir, &id, Some(&"n".repeat(80)), None, None, "user").is_ok());
        assert!(update_model_metadata(&dir, &id, None, Some(&"n".repeat(4001)), None, "user").is_err());
        let many: Vec<String> = (0..13).map(|i| format!("t{i}")).collect();
        assert!(update_model_metadata(&dir, &id, None, None, Some(&many), "user").is_err());
        assert!(update_model_metadata(&dir, &id, None, None, Some(&many[..12]), "user").is_ok());
        assert!(update_model_metadata(&dir, &id, None, None, Some(&["t".repeat(31)]), "user").is_err());
        assert!(update_model_metadata(&dir, "nope", Some("x"), None, None, "user").is_err());
        assert_ne!(before, fs::read_to_string(dir.join(format!("{id}.json"))).unwrap());
        cleanup(&dir);
    }

    #[test]
    fn concurrent_edits_of_different_fields_are_not_lost() {
        let dir = temp("concurrent-edit");
        let id = write_old_model(&dir);
        for i in 0..150 {
            let (a, b) = (dir.clone(), dir.clone());
            let (ida, idb) = (id.clone(), id.clone());
            let name = format!("name-{i}");
            let notes = format!("notes-{i}");
            let (n2, o2) = (name.clone(), notes.clone());
            let t1 = std::thread::spawn(move || update_model_metadata(&a, &ida, Some(&n2), None, None, "user"));
            let t2 = std::thread::spawn(move || update_model_metadata(&b, &idb, None, Some(&o2), None, "user"));
            t1.join().unwrap().expect("name edit");
            t2.join().unwrap().expect("notes edit");
            let doc: Value =
                serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.json"))).unwrap()).unwrap();
            assert_eq!(doc["display_name"], name, "name lost on round {i}");
            assert_eq!(doc["notes"], notes, "notes lost on round {i}");
        }
        cleanup(&dir);
    }

    #[test]
    fn delete_removes_metadata_artifact_and_log() {
        let dir = temp("delete");
        let id = write_old_model(&dir);
        fs::write(dir.join(format!("{id}.log")), "log").unwrap();
        fs::write(dir.join("keep.txt"), "x").unwrap();
        assert!(delete_model_files(&dir, &id, "user").unwrap());
        for ext in ["json", "joblib", "log"] {
            assert!(!dir.join(format!("{id}.{ext}")).exists(), "{ext}");
        }
        assert!(dir.join("keep.txt").exists());
        assert!(!delete_model_files(&dir, &id, "user").unwrap());
        cleanup(&dir);
    }

    #[test]
    fn staleness_limits_follow_the_models_timeframe() {
        let days = |tf| max_bar_age(tf).num_days();
        assert_eq!(days("1d"), 6);
        assert_eq!(days("1w"), 14);
        for tf in ["1m", "5m", "15m", "1h", "4h"] {
            assert_eq!(days(tf), 4, "{tf}");
        }
        // A 5-day-old hourly bar is stale even though the same age passes for daily bars.
        assert!(chrono::Duration::days(5) > max_bar_age("1h"));
        assert!(chrono::Duration::days(5) <= max_bar_age("1d"));
    }

    #[test]
    fn scoring_context_symbols_are_those_the_used_formulas_read() {
        let dir = temp("ctx");
        let id = "ctx_model_20261106T000000Z";
        let meta = r#"{
          "model_id": "ctx_model_20261106T000000Z", "algorithm": "random_forest",
          "feature_names": ["ret_1", "rel_qqq"], "target_column": "target",
          "metrics": {"accuracy": 0.5}, "model_file": "ctx_model_20261106T000000Z.joblib",
          "created_utc": "20261106T000000Z",
          "custom_features": [
            {"name": "rel_qqq", "expr": "close / other(\"QQQ\", \"close\")"},
            {"name": "unused", "expr": "close / other(\"SPY\", \"close\")"}
          ],
          "context_symbols": ["QQQ", "SPY"]
        }"#;
        fs::write(dir.join(format!("{id}.json")), meta).unwrap();
        fs::write(dir.join(format!("{id}.joblib")), b"x").unwrap();
        // SPY is recorded and has a formula, but the model's features do not use it.
        assert_eq!(scoring_context_symbols(&dir, id).unwrap(), vec!["QQQ".to_string()]);
        let old = write_old_model(&dir);
        assert!(scoring_context_symbols(&dir, &old).unwrap().is_empty());
    }

    #[test]
    fn model_timeframe_reads_the_trained_timeframe_and_defaults_to_daily() {
        let dir = temp("tf");
        let id = write_old_model(&dir);
        assert_eq!(model_timeframe(&dir, &id).unwrap(), "1d");
        let path = dir.join(format!("{id}.json"));
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        doc["timeframe"] = Value::String("1h".into());
        fs::write(&path, doc.to_string()).unwrap();
        assert_eq!(model_timeframe(&dir, &id).unwrap(), "1h");
        cleanup(&dir);
    }

    #[test]
    fn scorable_rows_cover_the_last_n_bars_and_skip_warmup() {
        let bars = synthetic_bars(80);
        let names: Vec<String> = vec!["ret_1".into(), "sma_ratio_50".into(), "ret_1_lag2".into()];
        let (idx, rows) = scorable_rows(&bars, &names, 60).unwrap();
        // The last 60 bars are 20..80; sma_ratio_50 first exists at bar 49.
        assert_eq!(idx.first(), Some(&49));
        assert_eq!(idx.last(), Some(&79));
        assert_eq!(rows.len(), idx.len());
        assert!(rows.iter().flatten().all(|v| v.is_finite()));
        assert!(scorable_rows(&bars, &["nope".to_string()], 10).is_err());
    }

    #[test]
    fn parity_inference_path_equals_the_matrix_row_used_for_training() {
        let bars = synthetic_bars(200);
        let names: Vec<String> = vec!["ret_5".into(), "rsi_14".into(), "vol_20".into(), "rsi_14_lag3".into(), "ret_1_lag1".into()];
        let latest = ml_features::latest_features(&bars, &names, "TEST").unwrap();
        let matrix = ml_features::model_matrix(&bars, &names).unwrap();
        for (j, n) in names.iter().enumerate() {
            assert_eq!(latest[n], matrix[199][j], "{n}");
        }
    }

    #[test]
    fn training_request_defaults_keep_old_json_working() {
        let old = r#"{"algorithm":"random_forest","data_path":"data/sample.csv","target_column":"signal","feature_columns":["a"],"hyperparams":{},"n_splits":5,"lag_periods":[1,5]}"#;
        let r: MLTrainingRequestDto = serde_json::from_str(old).unwrap();
        assert!(r.dataset.is_none() && r.return_column.is_none());
        let minimal: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"xgboost","dataset":"d"}"#).unwrap();
        assert_eq!(minimal.n_splits, 5);
        assert!(minimal.lag_periods.is_empty() && minimal.feature_columns.is_empty());
        assert_eq!(minimal.dataset.as_deref(), Some("d"));
    }

    #[test]
    fn custom_feature_columns_can_be_selected_and_are_included_by_default() {
        use crate::dto::{BuildDatasetRequestDto, CustomFeatureDto, LabelSpecDto};
        let root = temp("prepare-custom");
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        let req = BuildDatasetRequestDto {
            name: "ds2".into(),
            symbols: vec!["AAPL".into()],
            timeframe: "1d".into(),
            bars: 200,
            features: vec!["ret_1".into()],
            custom_features: vec![CustomFeatureDto { name: "mom".into(), expr: "ret(close, 5)".into() }],
            label: LabelSpecDto { horizon_bars: 3, kind: "direction".into(), threshold_pct: 0.0, ..Default::default() },
            ..Default::default()
        };
        let req = ml_datasets::normalize_request(&req).unwrap();
        let rel = ml_datasets::relative_csv_path(&paths, "ds2");
        let (csv, info) = ml_datasets::assemble_with(
            &req,
            &[("AAPL".into(), synthetic_bars(120))],
            &ml_expr::Context::new(),
            "user",
            &rel,
            chrono::Utc::now(),
        )
        .unwrap();
        ml_datasets::write_dataset(&ml_datasets::datasets_dir(&paths), &csv, &info).unwrap();

        // Default: every column of the dataset, custom ones included.
        let request: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"ds2"}"#).unwrap();
        let p = prepare_training(&request, &models, &paths, "user").unwrap();
        assert_eq!(p.payload["feature_columns"], json!(["ret_1", "mom"]));

        // Explicit selection may name the custom column; unknown names are still rejected.
        let request: MLTrainingRequestDto =
            serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"ds2","feature_columns":["mom"]}"#).unwrap();
        let p = prepare_training(&request, &models, &paths, "user").unwrap();
        assert_eq!(p.payload["feature_columns"], json!(["mom"]));
        let request: MLTrainingRequestDto =
            serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"ds2","feature_columns":["nope"]}"#).unwrap();
        let err = prepare_training(&request, &models, &paths, "user").unwrap_err();
        assert!(err.contains("nope") && err.contains("mom"), "{err}");
    }

    #[test]
    fn prepare_training_applies_dataset_defaults() {
        use crate::dto::{BuildDatasetRequestDto, LabelSpecDto};
        let root = temp("prepare");
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        let req = BuildDatasetRequestDto {
            name: "ds1".into(),
            symbols: vec!["AAPL".into()],
            timeframe: "1d".into(),
            bars: 200,
            features: vec!["ret_1".into(), "rsi_14".into()],
            label: LabelSpecDto { horizon_bars: 3, kind: "direction".into(), threshold_pct: 0.0, ..Default::default() },
            ..Default::default()
        };
        let req = ml_datasets::normalize_request(&req).unwrap();
        let rel = ml_datasets::relative_csv_path(&paths, "ds1");
        let (csv, info) = ml_datasets::assemble(&req, &[("AAPL".into(), synthetic_bars(120))], "user", &rel, chrono::Utc::now()).unwrap();
        ml_datasets::write_dataset(&ml_datasets::datasets_dir(&paths), &csv, &info).unwrap();

        let mut request: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"ds1"}"#).unwrap();
        let p = prepare_training(&request, &models, &paths, "copilot").unwrap();
        assert_eq!(p.data_path, "data/datasets/ds1/v1.csv");
        assert_eq!(p.payload["provenance"]["dataset_version"], 1);
        assert_eq!(p.payload["provenance"]["dataset_csv_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(p.payload["target_column"], "target");
        assert_eq!(p.payload["return_column"], "fwd_ret");
        assert_eq!(p.payload["feature_columns"], json!(["ret_1", "rsi_14"]));
        assert_eq!(p.payload["lag_periods"], json!([]));
        assert_eq!(p.payload["horizon_bars"], 3);
        assert_eq!(p.payload["source"], "copilot");
        assert_eq!(p.payload["dataset_meta"]["symbols"], json!(["AAPL"]));

        request.feature_columns = vec!["vol_10".into()];
        assert!(prepare_training(&request, &models, &paths, "user").unwrap_err().contains("vol_10"));
        request.feature_columns.clear();
        request.dataset = Some("../x".into());
        assert!(prepare_training(&request, &models, &paths, "user").is_err());
        request.dataset = Some("missing".into());
        assert!(prepare_training(&request, &models, &paths, "user").unwrap_err().contains("unknown dataset"));
        request.dataset = None;
        assert!(prepare_training(&request, &models, &paths, "user").is_err(), "no data_path");
        request.dataset = Some("ds1".into());
        request.hyperparams = hp(json!({"bad key": 1}));
        assert!(prepare_training(&request, &models, &paths, "user").unwrap_err().contains("bad key"));
        request.hyperparams.clear();
        request.n_splits = 1;
        assert!(prepare_training(&request, &models, &paths, "user").is_err());
        cleanup(&root);
    }

    /// Real Python round trip: a dataset built by the Rust pipeline is trained by `ml.trainer`,
    /// the metadata is read back through the Rust loader, and the batch predictor agrees with the
    /// single-row predictor on features computed by the shared pipeline. Skipped when the project
    /// virtualenv is not present.
    #[test]
    fn rust_dataset_to_python_trainer_to_predictor_end_to_end() {
        use crate::dto::{BuildDatasetRequestDto, LabelSpecDto};
        let repo = python_runtime::repo_root().unwrap();
        let python_root = repo.join("apex-python");
        if !python_root.join(".venv").join("bin").join("python").exists() {
            eprintln!("skipping: apex-python virtualenv not found");
            return;
        }
        let root = temp("e2e");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&python_root, root.join("apex-python")).unwrap();
        #[cfg(not(unix))]
        return;
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        fs::create_dir_all(&models).unwrap();

        let req = ml_datasets::normalize_request(&BuildDatasetRequestDto {
            name: "e2e".into(),
            symbols: vec!["AAA".into(), "BBB".into()],
            timeframe: "1d".into(),
            bars: 400,
            features: vec!["ret_1".into(), "ret_5".into(), "rsi_14".into(), "vol_10".into(), "macd_hist".into()],
            label: LabelSpecDto { horizon_bars: 5, kind: "direction".into(), threshold_pct: 0.0, ..Default::default() },
            ..Default::default()
        })
        .unwrap();
        let series = vec![("AAA".to_string(), synthetic_bars(400)), ("BBB".to_string(), synthetic_bars(380))];
        let rel = ml_datasets::relative_csv_path(&paths, "e2e");
        let (csv, info) = ml_datasets::assemble(&req, &series, "user", &rel, chrono::Utc::now()).unwrap();
        ml_datasets::write_dataset(&ml_datasets::datasets_dir(&paths), &csv, &info).unwrap();

        let request: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "random_forest", "dataset": "e2e", "n_splits": 3,
            "hyperparams": {"n_estimators": 15, "max_depth": 4}
        }))
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(train_core(&request, &models, &paths, "copilot")).unwrap();
        assert_eq!(result.status, "completed");
        assert!(result.duration_secs.is_some());

        let detail = build_detail(&models, &result.model_id, vec![]).unwrap();
        assert_eq!(detail.source, "copilot");
        assert_eq!(detail.dataset.as_deref(), Some("e2e"));
        assert_eq!(detail.symbols, vec!["AAA", "BBB"]);
        assert_eq!(detail.timeframe.as_deref(), Some("1d"));
        assert_eq!(detail.label.as_ref().unwrap().horizon_bars, 5);
        assert_eq!(detail.cv_folds.len(), 3);
        assert!(detail.trading.is_some());
        let total: f64 = detail.feature_importance.iter().map(|f| f.importance).sum();
        assert!((total - 1.0).abs() < 1e-6, "importances sum to {total}");
        assert!(detail.log_tail.contains("Fold 1"), "{}", detail.log_tail);
        assert_eq!(detail.feature_names, vec!["ret_1", "ret_5", "rsi_14", "vol_10", "macd_hist"]);
        assert!(load_registered_models(&models).unwrap().iter().any(|m| m.model_id == result.model_id));

        // Inference on features from the shared pipeline: batch == single for the last bar.
        let model = load_scoring_model(&models, &result.model_id).unwrap();
        let bars = synthetic_bars(300);
        let names = &model.meta.feature_names;
        let single_features = ml_features::latest_features(&bars, names, "AAA").unwrap();
        let (idx, rows) = scorable_rows(&bars, names, 30).unwrap();
        assert_eq!(*idx.last().unwrap(), 299);
        let rt2 = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let single: PredictorCliResult = serde_json::from_str(
            &rt2.block_on(run_predictor(
                &paths,
                vec!["--model-path".into(), model.model_path.clone().into_os_string(), "--features-json".into(), serde_json::to_string(&single_features).unwrap().into()],
                "e2e",
            ))
            .unwrap(),
        )
        .unwrap();
        let batch: PredictorBatchResult = serde_json::from_str(
            &rt2.block_on(run_predictor(
                &paths,
                vec!["--model-path".into(), model.model_path.clone().into_os_string(), "--features-matrix-json".into(), serde_json::to_string(&rows).unwrap().into()],
                "e2e",
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(batch.signals.len(), rows.len());
        assert_eq!(*batch.signals.last().unwrap(), single.signal);
        assert_eq!(single.features_used, *names);

        // Lag features the model never saw are not requested; an unknown one is named.
        assert!(ml_features::latest_features(&bars, &["not_a_feature".to_string()], "AAA").unwrap_err().contains("not_a_feature"));
        cleanup(&root);
    }


    // ── Trade mode, hold-out, seeds, custom estimators ───────────────

    fn build_dataset_files(paths: &python_runtime::RuntimePaths, name: &str, symbols: &[&str], bars: usize) {
        build_dataset_files_with(paths, name, symbols, bars, json!({}));
    }

    /// Like [`build_dataset_files`], with extra request fields (custom_features, fee_pct, ...), built
    /// by the real builder so the CSV and the sidecar agree.
    fn build_dataset_files_with(paths: &python_runtime::RuntimePaths, name: &str, symbols: &[&str], bars: usize, extra: Value) {
        use crate::dto::BuildDatasetRequestDto;
        let mut doc = json!({
            "name": name, "symbols": symbols, "timeframe": "1d", "bars": bars,
            "features": ["ret_1", "ret_5", "rsi_14", "vol_10", "macd_hist"],
            "label": {"horizon_bars": 5, "kind": "direction", "threshold_pct": 0.0}
        });
        doc.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let req: BuildDatasetRequestDto = serde_json::from_value(doc).unwrap();
        let req = ml_datasets::normalize_request(&req).unwrap();
        let series: Vec<(String, Vec<apex_core::domain::models::OHLCV>)> =
            symbols.iter().enumerate().map(|(i, s)| (s.to_string(), synthetic_bars(bars - i * 15))).collect();
        let rel = ml_datasets::relative_csv_path(paths, name);
        let (csv, info) =
            ml_datasets::assemble_with(&req, &series, &ml_expr::Context::new(), "user", &rel, chrono::Utc::now()).unwrap();
        ml_datasets::write_dataset(&ml_datasets::datasets_dir(paths), &csv, &info).unwrap();
    }

    /// Change fields of a dataset's info the way a custom-feature build would have written them.
    fn edit_sidecar(paths: &python_runtime::RuntimePaths, name: &str, edit: impl FnOnce(&mut Value)) {
        // Datasets are write-once, so "editing" one is a new version with the same rows.
        let dir = ml_datasets::datasets_dir(paths);
        let info = ml_datasets::read_info(&dir, name).unwrap();
        let csv = fs::read_to_string(dir.join(name).join(format!("v{}.csv", info.version))).unwrap();
        let mut doc = serde_json::to_value(&info).unwrap();
        edit(&mut doc);
        let mut edited: crate::dto::DatasetInfoDto = serde_json::from_value(doc).unwrap();
        edited.path = ml_datasets::relative_csv_path(paths, name);
        ml_datasets::write_dataset(&dir, &csv, &edited).unwrap();
    }

    /// A temp work root with the project's Python sources linked in, or None when there is no
    /// virtualenv (the test is then skipped).
    fn python_root(name: &str) -> Option<(PathBuf, python_runtime::RuntimePaths, PathBuf)> {
        let repo = python_runtime::repo_root().unwrap();
        let src = repo.join("apex-python");
        if !src.join(".venv").join("bin").join("python").exists() {
            eprintln!("skipping: apex-python virtualenv not found");
            return None;
        }
        let root = temp(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, root.join("apex-python")).unwrap();
        #[cfg(not(unix))]
        return None;
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        fs::create_dir_all(&models).unwrap();
        Some((root, paths, models))
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn new_request_fields_are_optional_and_round_trip() {
        let r: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "custom", "dataset": "d", "estimator_file": "mine.py", "trade_mode": "long_short",
            "holdout_pct": 15.0, "random_state": 7, "hyperparams": {"k": 3}
        }))
        .unwrap();
        assert_eq!((r.trade_mode.as_deref(), r.holdout_pct, r.random_state), (Some("long_short"), Some(15.0), Some(7)));
        assert_eq!(r.estimator_file.as_deref(), Some("mine.py"));
        let plain: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"d"}"#).unwrap();
        assert!(plain.trade_mode.is_none() && plain.holdout_pct.is_none() && plain.estimator_file.is_none() && plain.random_state.is_none());
        let back = serde_json::to_value(&plain).unwrap();
        assert!(back.get("trade_mode").is_none(), "unset options are not serialized");
        assert_eq!(MLTrainingRequestDto::default().n_splits, 5);
    }

    #[test]
    fn prepare_training_validates_and_forwards_the_new_options() {
        let root = temp("prepare-new");
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        build_dataset_files(&paths, "dsx", &["AAPL"], 120);
        edit_sidecar(&paths, "dsx", |d| {
            d["custom_features"] = json!([{"name": "mom", "expr": "close / shift(close, 5) - 1"}]);
            d["context_symbols"] = json!(["SPY"]);
            d["fee_pct"] = json!(0.1);
        });

        let mut req: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"dsx"}"#).unwrap();
        let p = prepare_training(&req, &models, &paths, "user").unwrap();
        assert_eq!(p.payload["trade_mode"], "long_flat");
        assert_eq!(p.payload["holdout_pct"], 0.0);
        assert!(p.payload["random_state"].is_null());
        assert_eq!(p.payload["dataset_meta"]["custom_features"], json!([{"name": "mom", "expr": "close / shift(close, 5) - 1"}]));
        assert_eq!(p.payload["dataset_meta"]["context_symbols"], json!(["SPY"]));
        assert_eq!(p.payload["dataset_meta"]["fee_pct"], 0.1);
        assert!(p.payload.get("estimator_path").is_none());

        req.trade_mode = Some("long_short".into());
        req.holdout_pct = Some(25.0);
        req.random_state = Some(9);
        let p = prepare_training(&req, &models, &paths, "user").unwrap();
        assert_eq!((p.payload["trade_mode"].as_str(), p.payload["holdout_pct"].as_f64(), p.payload["random_state"].as_i64()), (Some("long_short"), Some(25.0), Some(9)));

        for (mutate, fragment) in [
            (Box::new(|r: &mut MLTrainingRequestDto| r.trade_mode = Some("short_only".into())) as Box<dyn Fn(&mut MLTrainingRequestDto)>, "trade_mode"),
            (Box::new(|r| r.holdout_pct = Some(41.0)), "holdout_pct"),
            (Box::new(|r| r.holdout_pct = Some(-1.0)), "holdout_pct"),
            (Box::new(|r| r.holdout_pct = Some(f64::NAN)), "holdout_pct"),
            (Box::new(|r| r.random_state = Some(-1)), "random_state"),
            (Box::new(|r| r.random_state = Some(2_147_483_648)), "random_state"),
            (Box::new(|r| r.algorithm = "svm".into()), "Unknown algorithm"),
        ] {
            let mut bad = req.clone();
            mutate(&mut bad);
            assert!(prepare_training(&bad, &models, &paths, "user").unwrap_err().contains(fragment), "{fragment}");
        }
        req.holdout_pct = Some(40.0);
        assert!(prepare_training(&req, &models, &paths, "user").is_ok(), "40 is allowed");

        // Custom estimators need a saved file, found by exact name inside <work root>/estimators.
        req.algorithm = "custom".into();
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("estimator_file"));
        req.estimator_file = Some("../evil.py".into());
        assert!(prepare_training(&req, &models, &paths, "user").is_err());
        req.estimator_file = Some("mine.py".into());
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("not found"));
        ml_estimators::write_file(&root, "mine.py", ml_estimators::ESTIMATOR_TEMPLATE).unwrap();
        let p = prepare_training(&req, &models, &paths, "user").unwrap();
        assert_eq!(p.payload["estimator_file"], "mine.py");
        assert_eq!(p.payload["estimator_path"].as_str().unwrap(), root.join("estimators").join("mine.py").to_string_lossy());
        cleanup(&root);
    }

    #[test]
    fn model_detail_reads_the_new_metadata_and_old_files_get_defaults() {
        let dir = temp("detail-new");
        let id = write_old_model(&dir);
        let old = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!(old.trade_mode, "long_flat");
        assert!(old.holdout.is_none() && old.custom_features.is_empty() && old.context_symbols.is_empty());
        assert_eq!(old.fee_pct, 0.0);
        assert!(old.estimator_file.is_none());

        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["trade_mode"] = json!("long_short");
        doc["holdout"] = json!({"rows": 40, "accuracy": 0.55, "baseline_accuracy": 0.5, "pct": 20.0, "steps": 20, "start_step": 80, "embargo_steps": 5, "total_steps": 100,
            "trading": {"strategy_return_pct": 3.0, "buy_hold_return_pct": 1.0, "hit_rate": 0.6, "trades": 12, "max_drawdown_pct": -2.0, "sharpe": 0.9}});
        doc["custom_features"] = json!([{"name": "mom", "expr": "ret(close, 5)"}]);
        doc["context_symbols"] = json!(["SPY"]);
        doc["fee_pct"] = json!(0.05);
        doc["estimator_file"] = json!("mine.py");
        doc["random_state"] = json!(42);
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
        let d = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!(d.trade_mode, "long_short");
        let h = d.holdout.unwrap();
        assert_eq!((h.rows, h.accuracy, h.baseline_accuracy), (40, 0.55, Some(0.5)));
        assert_eq!(h.trading.unwrap().trades, 12);
        assert_eq!(d.custom_features.len(), 1);
        assert_eq!(d.custom_features[0].name, "mom");
        assert_eq!(d.context_symbols, vec!["SPY"]);
        assert_eq!(d.fee_pct, 0.05);
        assert_eq!(d.estimator_file.as_deref(), Some("mine.py"));

        // A garbage trade_mode or a malformed hold-out never hides the model.
        doc["trade_mode"] = json!("sideways");
        doc["holdout"] = json!({"rows": "many"});
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
        let d = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!(d.trade_mode, "long_flat");
        assert!(d.holdout.is_none());
        // And the serialized detail has the field names the UI reads.
        let wire = serde_json::to_value(&d).unwrap();
        for key in ["trade_mode", "holdout", "custom_features", "context_symbols", "fee_pct", "estimator_file"] {
            assert!(wire.get(key).is_some(), "{key}");
        }
        cleanup(&dir);
    }

    #[test]
    fn deleting_a_model_also_removes_its_estimator_copy() {
        let dir = temp("delete-copy");
        let id = write_old_model(&dir);
        fs::write(dir.join(format!("{id}_estimator.py")), "def make_estimator(p): pass\n").unwrap();
        fs::write(dir.join("other_estimator.py"), "keep").unwrap();
        assert!(delete_model_files(&dir, &id, "user").unwrap());
        assert!(!dir.join(format!("{id}_estimator.py")).exists());
        assert!(dir.join("other_estimator.py").exists());
        cleanup(&dir);
    }

    #[test]
    fn trainer_timeout_defaults_to_twenty_minutes_and_is_bounded() {
        let secs = |v| timeout_from(v).as_secs();
        assert_eq!(secs(None), 1200);
        assert_eq!(secs(Some("not a number")), 1200);
        assert_eq!(secs(Some(" 90 ")), 90);
        assert_eq!(secs(Some("0")), 1, "never zero");
        assert_eq!(secs(Some("999999")), 7200, "never more than two hours");
    }

    #[test]
    fn a_slow_training_run_is_stopped_with_a_clear_message() {
        let Some((root, paths, models)) = python_root("timeout") else { return };
        build_dataset_files(&paths, "slow", &["AAA"], 300);
        let request: MLTrainingRequestDto = serde_json::from_value(json!({"algorithm": "random_forest", "dataset": "slow", "n_splits": 3})).unwrap();
        let err = block_on(train_core_with(&request, &models, &paths, "user", std::time::Duration::from_millis(50))).unwrap_err();
        assert!(err.contains("longer than 50 ms") && err.contains("was stopped"), "{err}");
        assert!(fs::read_dir(&models).unwrap().next().is_none(), "a stopped run leaves no model behind");
        cleanup(&root);
    }

    #[test]
    fn unknown_hyperparameters_fail_before_a_trainer_is_started() {
        let Some((root, paths, models)) = python_root("badkey") else { return };
        build_dataset_files(&paths, "bk", &["AAA"], 200);
        let request: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "random_forest", "dataset": "bk", "hyperparams": {"learning_rate": 0.1}
        }))
        .unwrap();
        // A zero-length limit would fail with "stopped" if a trainer were started.
        let err = block_on(train_core_with(&request, &models, &paths, "user", std::time::Duration::from_millis(1))).unwrap_err();
        assert!(err.contains("unknown hyperparameter `learning_rate`") && err.contains("n_estimators"), "{err}");
        // A parameter the old list never allowed passes the catalog check and trains.
        let ok: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "extra_trees", "dataset": "bk", "n_splits": 3, "random_state": 11,
            "hyperparams": {"n_estimators": 12, "max_depth": 3, "criterion": "entropy", "min_samples_split": 6, "n_jobs": 32}
        }))
        .unwrap();
        let r = block_on(train_core(&ok, &models, &paths, "user")).unwrap();
        let d = build_detail(&models, &r.model_id, vec![]).unwrap();
        assert_eq!(d.algorithm, "extra_trees");
        assert_eq!(d.hyperparams["criterion"], "entropy");
        assert_eq!(d.hyperparams["n_jobs"], 4, "n_jobs is clamped, never passed through");
        assert_eq!(d.hyperparams["random_state"], 11);
        cleanup(&root);
    }

    #[test]
    fn trade_mode_holdout_and_the_dataset_recipe_reach_the_model_detail() {
        let Some((root, paths, models)) = python_root("modes") else { return };
        // A real custom feature, so its column is in the data file as well as the sidecar.
        build_dataset_files_with(
            &paths,
            "md",
            &["AAA", "BBB"],
            400,
            json!({"custom_features": [{"name": "mom", "expr": "close / shift(close, 5) - 1"}], "fee_pct": 0.1}),
        );
        // The context symbol is recorded on the dataset without any formula reading it.
        edit_sidecar(&paths, "md", |d| {
            d["context_symbols"] = json!(["SPY"]);
        });
        let train = |extra: Value| {
            let mut req = json!({"algorithm": "random_forest", "dataset": "md", "n_splits": 3, "hyperparams": {"n_estimators": 12, "max_depth": 4}});
            req.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let request: MLTrainingRequestDto = serde_json::from_value(req).unwrap();
            let r = block_on(train_core(&request, &models, &paths, "user")).unwrap();
            build_detail(&models, &r.model_id, vec![]).unwrap()
        };
        let plain = train(json!({}));
        assert_eq!(plain.trade_mode, "long_flat");
        assert!(plain.holdout.is_none());
        assert_eq!(plain.fee_pct, 0.1);
        assert_eq!(plain.context_symbols, vec!["SPY"]);
        assert_eq!(plain.custom_features[0].name, "mom");
        assert_eq!(plain.hyperparams["random_state"], 42);

        let both = train(json!({"trade_mode": "long_short", "holdout_pct": 20.0}));
        assert_eq!(both.trade_mode, "long_short");
        let h = both.holdout.expect("hold-out result");
        assert!(h.rows > 0 && (0.0..=1.0).contains(&h.accuracy));
        assert!(h.baseline_accuracy.is_some());
        let t = both.trading.unwrap();
        assert!(t.trades > plain.trading.unwrap().trades, "class 0 now shorts, so there are more trades");
        assert!(both.log_tail.contains("Hold-out"), "{}", both.log_tail);
        cleanup(&root);
    }

    #[test]
    fn a_custom_estimator_trains_and_is_scored_from_its_copy_after_the_original_is_deleted() {
        let Some((root, paths, models)) = python_root("custom") else { return };
        build_dataset_files(&paths, "cu", &["AAA", "BBB"], 300);
        // The template the UI offers is itself a working estimator.
        ml_estimators::write_file(&root, "centroid.py", ml_estimators::ESTIMATOR_TEMPLATE).unwrap();
        let request: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "custom", "dataset": "cu", "n_splits": 3, "estimator_file": "centroid.py",
            "hyperparams": {"shrink": 0.1}, "holdout_pct": 15.0
        }))
        .unwrap();
        let r = block_on(train_core(&request, &models, &paths, "user")).unwrap();
        let d = build_detail(&models, &r.model_id, vec![]).unwrap();
        assert_eq!(d.algorithm, "custom");
        assert_eq!(d.estimator_file.as_deref(), Some("centroid.py"));
        assert!(d.holdout.is_some());
        let copy = models.join(format!("{}_estimator.py", r.model_id));
        assert_eq!(fs::read_to_string(&copy).unwrap(), ml_estimators::ESTIMATOR_TEMPLATE);
        let raw: Value = serde_json::from_str(&fs::read_to_string(models.join(format!("{}.json", r.model_id))).unwrap()).unwrap();
        assert_eq!(raw["estimator_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(raw["estimator_copy"], format!("{}_estimator.py", r.model_id));

        // Edit, then delete the original; the model must still score.
        ml_estimators::write_file(&root, "centroid.py", "def make_estimator(params):\n    raise RuntimeError('edited')\n").unwrap();
        ml_estimators::remove_file(&root, "centroid.py").unwrap();
        let model = load_scoring_model(&models, &r.model_id).unwrap();
        let features = ml_features::latest_features(&synthetic_bars(300), &model.meta.feature_names, "AAA").unwrap();
        let out = block_on(run_predictor(
            &paths,
            vec!["--model-path".into(), model.model_path.clone().into_os_string(), "--features-json".into(), serde_json::to_string(&features).unwrap().into()],
            "custom",
        ))
        .unwrap();
        let signal: PredictorCliResult = serde_json::from_str(&out).unwrap();
        assert!(signal.signal == 0 || signal.signal == 1);
        assert!(signal.probability.is_some());

        // Deleting the model deletes the copy too.
        assert!(delete_model_files(&models, &r.model_id, "user").unwrap());
        assert!(!copy.exists());
        cleanup(&root);
    }

    #[test]
    fn a_broken_custom_estimator_fails_with_the_python_error() {
        let Some((root, paths, models)) = python_root("custom-bad") else { return };
        build_dataset_files(&paths, "cb", &["AAA"], 200);
        ml_estimators::write_file(&root, "broken.py", "def make_estimator(params):\n    return object()\n").unwrap();
        let request: MLTrainingRequestDto = serde_json::from_value(json!({"algorithm": "custom", "dataset": "cb", "estimator_file": "broken.py"})).unwrap();
        let err = block_on(train_core(&request, &models, &paths, "user")).unwrap_err();
        assert!(err.contains("fit()"), "{err}");
        assert!(fs::read_dir(&models).unwrap().next().is_none());
        cleanup(&root);
    }

    // ── Algorithm recipes ────────────────────────────────────────────

    fn sample_recipe(name: &str) -> RecipeDto {
        serde_json::from_value(json!({
            "name": name, "label": "Scaled forest", "description": "",
            "steps": [{"kind": "standard_scaler", "params": {}}],
            "model": {"kind": "single", "algorithm": "random_forest", "params": {"n_estimators": 15, "max_depth": 4}},
            "calibrate": "none"
        }))
        .unwrap()
    }

    #[test]
    fn old_requests_and_metadata_load_without_recipe_fields() {
        let r: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"d"}"#).unwrap();
        assert!(r.recipe.is_none());
        assert!(serde_json::to_value(&r).unwrap().get("recipe").is_none(), "unset recipe is not serialized");
        let r: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"recipe","dataset":"d","recipe":"mine"}"#).unwrap();
        assert_eq!(r.recipe.as_deref(), Some("mine"));

        let dir = temp("detail-recipe");
        let id = write_old_model(&dir);
        let old = build_detail(&dir, &id, vec![]).unwrap();
        assert!(old.recipe.is_none());
        assert!(serde_json::to_value(&old).unwrap()["recipe"].is_null(), "the UI reads recipe: null");

        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["algorithm"] = json!("recipe");
        doc["recipe_name"] = json!("mine");
        doc["recipe"] = serde_json::to_value(sample_recipe("mine")).unwrap();
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
        let d = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!(d.algorithm, "recipe");
        assert_eq!(d.recipe.as_ref().unwrap().label, "Scaled forest");
        assert!(load_registered_models(&dir).unwrap().iter().any(|m| m.model_id == id && m.algorithm == "recipe"));

        // A recipe that does not parse never hides the model.
        doc["recipe"] = json!({"name": "mine", "model": {"kind": "bagging"}});
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
        assert!(build_detail(&dir, &id, vec![]).unwrap().recipe.is_none());
        cleanup(&dir);
    }

    #[test]
    fn prepare_training_builds_the_payload_for_a_saved_recipe_and_refuses_a_missing_one() {
        let root = temp("prepare-recipe");
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        build_dataset_files(&paths, "dsr", &["AAPL"], 120);

        let mut req: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "recipe", "dataset": "dsr", "recipe": "mine", "trade_mode": "long_short",
            "holdout_pct": 20.0, "random_state": 5, "n_splits": 4,
            // Parameters live in the recipe; whatever the request carries is dropped.
            "hyperparams": {"n_estimators": 999}
        }))
        .unwrap();
        let err = prepare_training(&req, &models, &paths, "user").unwrap_err();
        assert!(err.contains("`mine` not found"), "{err}");

        ml_recipes::write_recipe(&root, &sample_recipe("mine")).unwrap();
        let p = prepare_training(&req, &models, &paths, "user").unwrap();
        assert_eq!(p.payload["algorithm"], "recipe");
        assert_eq!(p.payload["recipe_name"], "mine");
        assert_eq!(p.payload["recipe"]["label"], "Scaled forest");
        assert_eq!(p.payload["recipe"]["model"]["kind"], "single");
        assert_eq!(p.payload["recipe"]["steps"][0]["kind"], "standard_scaler");
        assert_eq!(p.payload["hyperparams"], json!({}), "hyperparams are forced empty");
        assert_eq!(p.payload["trade_mode"], "long_short");
        assert_eq!(p.payload["holdout_pct"], 20.0);
        assert_eq!(p.payload["random_state"], 5);
        assert_eq!(p.payload["n_splits"], 4);
        assert_eq!(p.payload["dataset"], "dsr");
        assert_eq!(p.payload["horizon_bars"], 5);
        assert!(p.payload.get("estimator_path").is_none());

        // The recipe name is required, and must be a name (no path tricks).
        req.recipe = None;
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("recipe name"));
        req.recipe = Some("  ".into());
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("recipe name"));
        for bad in ["../mine", "mine.json", "Mine", "a/b"] {
            req.recipe = Some(bad.into());
            assert!(prepare_training(&req, &models, &paths, "user").is_err(), "{bad}");
        }

        // A recipe name on another algorithm is ignored; the other algorithms are untouched.
        let mut plain: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"dsr","recipe":"mine","hyperparams":{"n_estimators":7}}"#).unwrap();
        let p = prepare_training(&plain, &models, &paths, "user").unwrap();
        assert!(p.payload.get("recipe").is_none() && p.payload.get("recipe_name").is_none());
        assert_eq!(p.payload["hyperparams"], json!({"n_estimators": 7}));
        plain.algorithm = "custom".into();
        assert!(prepare_training(&plain, &models, &paths, "user").unwrap_err().contains("estimator_file"));

        // A hand-edited recipe file that breaks the structural rules is refused with its problems.
        let file = ml_recipes::recipes_dir(&root).join("mine.json");
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        doc["model"] = json!({"kind": "voting", "voting": "soft", "members": [{"algorithm": "custom", "params": {}}]});
        fs::write(&file, doc.to_string()).unwrap();
        req.recipe = Some("mine".into());
        let err = prepare_training(&req, &models, &paths, "user").unwrap_err();
        assert!(err.contains("model.members") && err.contains("model.members[0].algorithm"), "{err}");
        cleanup(&root);
    }

    /// A work root whose apex-python holds only the given stub modules (and the real virtualenv),
    /// to test what Rust sends and how it reads the answers without the real Python code.
    fn stub_python_root(name: &str, files: &[(&str, &str)]) -> Option<(PathBuf, python_runtime::RuntimePaths, PathBuf)> {
        let repo = python_runtime::repo_root().unwrap();
        let venv = repo.join("apex-python").join(".venv");
        if !venv.join("bin").join("python").exists() {
            eprintln!("skipping: apex-python virtualenv not found");
            return None;
        }
        #[cfg(not(unix))]
        return None;
        let root = temp(name);
        let ml = root.join("apex-python").join("ml");
        fs::create_dir_all(&ml).unwrap();
        fs::write(ml.join("__init__.py"), "").unwrap();
        for (file, text) in files {
            fs::write(ml.join(file), text).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&venv, root.join("apex-python").join(".venv")).unwrap();
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        fs::create_dir_all(&models).unwrap();
        Some((root, paths, models))
    }

    const STUB_RECIPES: &str = r#"
import json, sys
if sys.argv[1] == "blocks":
    print(json.dumps({"steps": [], "members": [], "limits": {}}))
else:
    assert sys.argv[2] == "--json" and len(sys.argv) == 4, sys.argv
    recipe = json.loads(sys.argv[3])
    errors = []
    if recipe["label"] == "reject me":
        errors = [{"path": "steps[0].params.k", "message": "k too big"}, {"path": "model.members[1]", "message": "twice"}]
    print("noise before")
    print(json.dumps({"ok": not errors, "errors": errors, "warnings": [], "steps": len(recipe["steps"]), "models": 1, "est_model_mb": 1.5}))
"#;

    /// Records the payload it gets and answers like the real trainer. There is no ml.catalog here:
    /// asking the algorithm catalog about "recipe" would fail the run.
    const STUB_TRAINER: &str = r#"
import json, sys
payload = json.loads(sys.argv[sys.argv.index("--config-json") + 1])
open(payload["output_dir"] + "/payload.json", "w").write(json.dumps(payload))
print(json.dumps({"model_id": "m_stub", "algorithm": "recipe", "metrics": {"accuracy": 0.6}, "feature_names": ["a"],
    "created_at": "2026-01-01T00:00:00+00:00", "model_path": "x", "metadata_path": "y", "data_path": payload["data_path"], "target_column": "target"}))
"#;

    #[test]
    fn a_recipe_is_deep_checked_then_the_trainer_gets_the_recipe_and_its_name() {
        let Some((root, paths, models)) = stub_python_root("stub-recipe", &[("recipes.py", STUB_RECIPES), ("trainer.py", STUB_TRAINER)]) else { return };
        build_dataset_files(&paths, "dst", &["AAA"], 150);
        ml_recipes::write_recipe(&root, &sample_recipe("mine")).unwrap();
        let request: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "recipe", "dataset": "dst", "recipe": "mine", "hyperparams": {"n_estimators": 3}
        }))
        .unwrap();
        let r = block_on(train_core(&request, &models, &paths, "user")).unwrap();
        assert_eq!(r.model_id, "m_stub");
        let seen: Value = serde_json::from_str(&fs::read_to_string(models.join("payload.json")).unwrap()).unwrap();
        assert_eq!(seen["algorithm"], "recipe");
        assert_eq!(seen["recipe_name"], "mine");
        assert_eq!(seen["recipe"]["model"]["algorithm"], "random_forest");
        assert_eq!(seen["hyperparams"], json!({}));
        assert_eq!(seen["horizon_bars"], 5);

        // The deep check can refuse: nothing is trained and every problem is named.
        let mut rejected = sample_recipe("rejected");
        rejected.label = "reject me".into();
        ml_recipes::write_recipe(&root, &rejected).unwrap();
        fs::remove_file(models.join("payload.json")).unwrap();
        let bad: MLTrainingRequestDto = serde_json::from_value(json!({"algorithm": "recipe", "dataset": "dst", "recipe": "rejected"})).unwrap();
        let err = block_on(train_core(&bad, &models, &paths, "user")).unwrap_err();
        assert_eq!(err, "steps[0].params.k: k too big; model.members[1]: twice");
        assert!(!models.join("payload.json").exists(), "the trainer never started");

        // Saving goes through the same check and writes nothing when it fails.
        let err = block_on(ml_recipes::require_valid(&paths, &rejected)).unwrap_err();
        assert!(err.contains("k too big"), "{err}");
        let check = block_on(ml_recipes::check_recipe(&paths, &sample_recipe("fine"))).unwrap();
        assert!(check.ok && check.steps == 1 && check.est_model_mb == Some(1.5));
        let blocks = block_on(ml_recipes::fetch_building_blocks(&paths)).unwrap();
        assert!(blocks["steps"].is_array());

        // Saving goes through the check and writes the file; a failed check writes nothing.
        let state = block_on(crate::state::AppState::init(paths.clone())).expect("test AppState");
        let saved = block_on(ml_recipes::save_inner(&state, &paths, &sample_recipe("kept"), "user")).unwrap();
        assert!(saved.updated_at.is_some());
        let err = block_on(ml_recipes::save_inner(&state, &paths, &rejected, "user")).unwrap_err();
        assert!(err.contains("k too big"));
        assert!(ml_recipes::load_recipe(&root, "kept").is_ok());
        assert!(ml_recipes::delete_inner(&state, &root, "kept", "user").unwrap());
        assert!(!ml_recipes::delete_inner(&state, &root, "kept", "user").unwrap());
        // (The activity entries are only written when a Tauri app handle exists, so they are not asserted here.)
        cleanup(&root);
    }

    #[test]
    fn training_with_a_recipe_works_end_to_end_and_the_detail_keeps_a_copy() {
        let Some((root, paths, models)) = python_root("recipe-e2e") else { return };
        if !python_runtime::repo_root().unwrap().join("apex-python").join("ml").join("recipes.py").exists() {
            eprintln!("skipping: apex-python/ml/recipes.py not found");
            return;
        }
        build_dataset_files(&paths, "rc", &["AAA", "BBB"], 900);
        let recipe: RecipeDto = serde_json::from_value(json!({
            "name": "ens", "label": "Forest and linear", "description": "e2e",
            "steps": [{"kind": "standard_scaler", "params": {}}, {"kind": "select_k_best", "params": {"k": 4, "score": "f_classif"}}],
            "model": {"kind": "stacking", "blocks": 3,
                "members": [{"algorithm": "random_forest", "params": {"n_estimators": 10, "max_depth": 3}},
                            {"algorithm": "logistic_regression", "params": {}}],
                "final": {"algorithm": "logistic_regression", "params": {}}},
            "calibrate": "none"
        }))
        .unwrap();
        let state = block_on(crate::state::AppState::init(paths.clone())).expect("test AppState");
        let saved = block_on(ml_recipes::save_inner(&state, &paths, &recipe, "user")).unwrap();
        assert!(saved.created_at.is_some());
        assert_eq!(ml_recipes::list_recipe_files(&root).len(), 1);

        let request: MLTrainingRequestDto = serde_json::from_value(json!({
            "algorithm": "recipe", "dataset": "rc", "recipe": "ens", "n_splits": 3, "holdout_pct": 15.0
        }))
        .unwrap();
        let r = block_on(train_core(&request, &models, &paths, "user")).unwrap();
        assert_eq!(r.status, "completed");
        let d = build_detail(&models, &r.model_id, vec![]).unwrap();
        assert_eq!(d.algorithm, "recipe");
        assert_eq!(d.recipe.as_ref().map(|r| r.name.as_str()), Some("ens"));
        assert_eq!(d.cv_folds.len(), 3);

        // Deleting the saved recipe leaves the model and its copy alone.
        assert!(ml_recipes::remove_recipe(&root, "ens").unwrap());
        let d = build_detail(&models, &r.model_id, vec![]).unwrap();
        assert_eq!(d.recipe.unwrap().label, "Forest and linear");
        cleanup(&root);
    }

    // ── Models in the audit ledger ───────────────────────────────────

    fn model_ledger_of(models_dir: &Path) -> Ledger {
        model_ledger(models_dir).unwrap()
    }

    fn actions(history: &[VersionRecord]) -> Vec<(u32, &str)> {
        history.iter().map(|r| (r.version, r.action.as_str())).collect()
    }

    #[test]
    fn an_old_model_is_registered_as_version_1_when_listed_and_not_again() {
        let dir = temp("lazy-model");
        let id = write_old_model(&dir);
        let ledger = model_ledger_of(&dir);
        assert!(ledger.history(Kind::Model, &id).is_empty());
        assert_eq!(load_registered_models(&dir).unwrap().len(), 1);
        let history = ledger.history(Kind::Model, &id);
        assert_eq!(history.len(), 1);
        assert_eq!((history[0].action.as_str(), history[0].actor.as_str()), ("create", "system"));
        assert_eq!(ledger.content(Kind::Model, &id, 1).unwrap(), OLD_METADATA.as_bytes(), "the original file, byte for byte");
        load_registered_models(&dir).unwrap();
        assert_eq!(ledger.history(Kind::Model, &id).len(), 1);
        assert_eq!(build_detail(&dir, &id, vec![]).unwrap().ledger_version, Some(1));
        cleanup(&dir);
    }

    #[test]
    fn each_edit_is_a_new_version_with_the_new_content_and_nothing_is_overwritten() {
        let dir = temp("model-versions");
        let id = write_old_model(&dir);
        let rec = update_model_metadata(&dir, &id, Some("First name"), None, None, "user").unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.actor.as_str()), (2, "update", "user"));
        update_model_metadata(&dir, &id, None, Some("some notes"), Some(&["a".to_string()]), "copilot").unwrap();
        // The same edit again changes nothing and adds no version.
        let noop = update_model_metadata(&dir, &id, None, Some("some notes"), None, "user").unwrap();
        assert_eq!(noop.version, 3);

        let ledger = model_ledger_of(&dir);
        let history = ledger.history(Kind::Model, &id);
        assert_eq!(actions(&history), vec![(1, "create"), (2, "update"), (3, "update")]);
        assert_eq!(history[2].actor, "copilot");
        // Version 1 is still the file as it was; 2 and 3 hold the metadata at their time.
        assert_eq!(ledger.content(Kind::Model, &id, 1).unwrap(), OLD_METADATA.as_bytes());
        let v2: Value = serde_json::from_slice(&ledger.content(Kind::Model, &id, 2).unwrap()).unwrap();
        assert_eq!(v2["display_name"], "First name");
        assert!(v2["notes"].is_null(), "the old file had no notes field");
        let v3: Value = serde_json::from_slice(&ledger.content(Kind::Model, &id, 3).unwrap()).unwrap();
        assert_eq!((v3["display_name"].as_str(), v3["notes"].as_str(), v3["tags"].clone()), (Some("First name"), Some("some notes"), json!(["a"])));
        // The working file is the mirror of the head.
        let on_disk = fs::read(dir.join(format!("{id}.json"))).unwrap();
        assert_eq!(on_disk, ledger.content(Kind::Model, &id, 3).unwrap());
        assert_eq!(build_detail(&dir, &id, vec![]).unwrap().ledger_version, Some(3));
        assert!(ledger.verify().ok);
        cleanup(&dir);
    }

    #[test]
    fn a_hand_edit_of_the_metadata_file_becomes_its_own_version_before_the_next_edit() {
        let dir = temp("model-external");
        let id = write_old_model(&dir);
        update_model_metadata(&dir, &id, Some("mine"), None, None, "user").unwrap();
        let path = dir.join(format!("{id}.json"));
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        doc["notes"] = json!("edited in a text editor");
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        update_model_metadata(&dir, &id, None, None, Some(&["t".to_string()]), "user").unwrap();
        let history = model_ledger_of(&dir).history(Kind::Model, &id);
        assert_eq!(actions(&history), vec![(1, "create"), (2, "update"), (3, "external"), (4, "update")]);
        assert_eq!(history[2].actor, "external");
        let v3: Value = serde_json::from_slice(&model_ledger_of(&dir).content(Kind::Model, &id, 3).unwrap()).unwrap();
        assert_eq!(v3["notes"], "edited in a text editor", "the hand edit is kept in the history");
        let v4: Value = serde_json::from_slice(&model_ledger_of(&dir).content(Kind::Model, &id, 4).unwrap()).unwrap();
        assert_eq!(v4["notes"], "edited in a text editor", "and carried into the next version");
        cleanup(&dir);
    }

    #[test]
    fn an_interrupted_save_is_finished_when_models_are_listed() {
        let dir = temp("model-pending");
        let id = write_old_model(&dir);
        load_registered_models(&dir).unwrap();
        // A crash after the ledger append and before the rename: the new content is in the ledger and
        // in `<id>.json.apexnew`, the working file still has the old content.
        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["display_name"] = json!("after the crash");
        let bytes = serde_json::to_vec_pretty(&doc).unwrap();
        let ledger = model_ledger_of(&dir);
        ledger.put(Kind::Model, &id, &bytes, "user", "Edited display name").unwrap();
        fs::write(dir.join(format!("{id}.json.apexnew")), &bytes).unwrap();
        let listed = load_registered_models(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(fs::read(dir.join(format!("{id}.json"))).unwrap(), bytes);
        assert!(!dir.join(format!("{id}.json.apexnew")).exists());
        assert_eq!(ledger.history(Kind::Model, &id).len(), 2, "no extra version for finishing the rename");
        cleanup(&dir);
    }

    #[test]
    fn delete_is_a_tombstone_and_the_files_are_archived_not_removed() {
        let dir = temp("model-delete");
        let id = write_old_model(&dir);
        fs::write(dir.join(format!("{id}.log")), "the log").unwrap();
        fs::write(dir.join(format!("{id}_estimator.py")), "def make_estimator(p): pass\n").unwrap();
        fs::write(dir.join("keep.txt"), "x").unwrap();
        update_model_metadata(&dir, &id, Some("named"), None, None, "user").unwrap();

        assert!(delete_model_files(&dir, &id, "user").unwrap());
        assert!(!delete_model_files(&dir, &id, "user").unwrap(), "already deleted");
        // Gone from the models directory and from every reader...
        for name in [format!("{id}.json"), format!("{id}.joblib"), format!("{id}.log"), format!("{id}_estimator.py")] {
            assert!(!dir.join(&name).exists(), "{name}");
        }
        assert!(dir.join("keep.txt").exists());
        assert!(load_registered_models(&dir).unwrap().is_empty());
        let err = build_detail(&dir, &id, vec![]).unwrap_err();
        assert!(err.contains("deleted"), "{err}");
        assert!(load_scoring_model(&dir, &id).err().unwrap().contains("deleted"));
        assert!(model_timeframe(&dir, &id).is_err());
        // ...but every file is in the archive, named after the delete's ledger version.
        let archive = dir.join("archive").join(format!("{id}@v3"));
        for name in [format!("{id}.json"), format!("{id}.joblib"), format!("{id}.log"), format!("{id}_estimator.py")] {
            assert!(archive.join(&name).is_file(), "{name} is archived");
        }
        assert_eq!(fs::read(archive.join(format!("{id}.joblib"))).unwrap().len(), 321, "the artifact is untouched");
        // The ledger has the whole story and lists the model only when deleted ones are asked for.
        let ledger = model_ledger_of(&dir);
        assert_eq!(actions(&ledger.history(Kind::Model, &id)), vec![(1, "create"), (2, "update"), (3, "delete")]);
        assert!(ledger.objects(Kind::Model, false).is_empty());
        let all = ledger.objects(Kind::Model, true);
        assert_eq!((all.len(), all[0].deleted, all[0].head_version), (1, true, 3));
        assert!(update_model_metadata(&dir, &id, Some("x"), None, None, "user").unwrap_err().contains("deleted"));
        cleanup(&dir);
    }

    #[test]
    fn restore_brings_the_files_back_as_a_new_version() {
        let dir = temp("model-restore");
        let id = write_old_model(&dir);
        fs::write(dir.join(format!("{id}.log")), "the log").unwrap();
        update_model_metadata(&dir, &id, Some("before delete"), None, None, "user").unwrap();
        delete_model_files(&dir, &id, "user").unwrap();

        let rec = restore_model_files(&dir, &id, None, "user").unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.restored_from), (4, "restore", Some(2)));
        for ext in ["json", "joblib", "log"] {
            assert!(dir.join(format!("{id}.{ext}")).is_file(), "{ext}");
        }
        let detail = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!((detail.display_name.as_str(), detail.ledger_version), ("before delete", Some(4)));
        assert_eq!(load_registered_models(&dir).unwrap().len(), 1);
        let ledger = model_ledger_of(&dir);
        assert_eq!(actions(&ledger.history(Kind::Model, &id)), vec![(1, "create"), (2, "update"), (3, "delete"), (4, "restore")]);
        assert_eq!(fs::read(dir.join(format!("{id}.json"))).unwrap(), ledger.content(Kind::Model, &id, 4).unwrap());
        // Restoring a live model without a version is refused; a named older version is a rollback that
        // appends yet another version.
        assert!(restore_model_files(&dir, &id, None, "user").is_err());
        let back = restore_model_files(&dir, &id, Some(1), "user").unwrap();
        assert_eq!((back.version, back.restored_from), (5, Some(1)));
        assert_eq!(build_detail(&dir, &id, vec![]).unwrap().display_name, "");
        assert!(ledger.content(Kind::Model, &id, 2).is_ok(), "the edit that was rolled back is still in the history");
        // A second delete after a restore archives under its own version number: nothing collides.
        delete_model_files(&dir, &id, "user").unwrap();
        assert!(dir.join("archive").join(format!("{id}@v3")).is_dir() && dir.join("archive").join(format!("{id}@v6")).is_dir());
        assert!(ledger.verify().ok);
        cleanup(&dir);
    }

    #[test]
    fn restore_refuses_when_the_archive_is_missing_or_a_file_is_in_the_way() {
        let dir = temp("model-restore-refuse");
        let id = write_old_model(&dir);
        delete_model_files(&dir, &id, "user").unwrap();
        // A new file under the same name appeared: restore must not overwrite it.
        fs::write(dir.join(format!("{id}.joblib")), "someone else's").unwrap();
        assert!(restore_model_files(&dir, &id, None, "user").unwrap_err().contains("refusing to overwrite"));
        assert_eq!(fs::read_to_string(dir.join(format!("{id}.joblib"))).unwrap(), "someone else's");
        assert_eq!(model_ledger_of(&dir).history(Kind::Model, &id).len(), 2, "nothing was recorded");
        assert!(restore_model_files(&dir, "no_such_model", None, "user").is_err());
        cleanup(&dir);
    }

    #[test]
    fn a_delete_interrupted_after_the_tombstone_is_finished_when_models_are_listed() {
        let dir = temp("model-half-delete");
        let id = write_old_model(&dir);
        load_registered_models(&dir).unwrap();
        model_ledger_of(&dir).tombstone(Kind::Model, &id, "user", "half done").unwrap();
        assert!(dir.join(format!("{id}.json")).exists());
        assert!(load_registered_models(&dir).unwrap().is_empty());
        assert!(!dir.join(format!("{id}.json")).exists());
        assert!(dir.join("archive").join(format!("{id}@v2")).join(format!("{id}.joblib")).is_file());
        assert_eq!(model_ledger_of(&dir).history(Kind::Model, &id).len(), 2, "no spurious external version");
        cleanup(&dir);
    }

    #[test]
    fn model_provenance_and_ledger_version_reach_the_detail_and_old_files_get_none() {
        let dir = temp("model-prov");
        let id = write_old_model(&dir);
        let old = build_detail(&dir, &id, vec![]).unwrap();
        assert!(old.dataset_version.is_none() && old.dataset_csv_sha256.is_none() && old.estimator_version.is_none() && old.recipe_version.is_none());
        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["dataset_version"] = json!(4);
        doc["dataset_csv_sha256"] = json!("ab".repeat(32));
        doc["estimator_file_version"] = json!(2);
        doc["recipe_version"] = json!(7);
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
        let d = build_detail(&dir, &id, vec![]).unwrap();
        assert_eq!((d.dataset_version, d.estimator_version, d.recipe_version), (Some(4), Some(2), Some(7)));
        assert_eq!(d.dataset_csv_sha256.as_deref(), Some("ab".repeat(32).as_str()));
        cleanup(&dir);
    }

    #[test]
    fn training_can_name_a_dataset_version_and_defaults_to_the_latest() {
        let root = temp("prepare-version");
        let paths = python_runtime::RuntimePaths::for_test(&root);
        let models = root.join("models");
        build_dataset_files(&paths, "dsv", &["AAPL"], 120);
        build_dataset_files(&paths, "dsv", &["AAPL"], 150);
        let dir = ml_datasets::datasets_dir(&paths);
        let (v1, v2) = (ml_datasets::read_info_version(&dir, "dsv", Some(1)).unwrap(), ml_datasets::read_info_version(&dir, "dsv", Some(2)).unwrap());
        assert_ne!(v1.csv_sha256, v2.csv_sha256);

        let mut req: MLTrainingRequestDto = serde_json::from_str(r#"{"algorithm":"random_forest","dataset":"dsv"}"#).unwrap();
        let latest = prepare_training(&req, &models, &paths, "user").unwrap();
        assert_eq!(latest.payload["provenance"]["dataset_version"], 2);
        assert_eq!(latest.payload["provenance"]["dataset_csv_sha256"], json!(v2.csv_sha256));
        assert_eq!(latest.data_path, "data/datasets/dsv/v2.csv");
        req.dataset_version = Some(1);
        let first = prepare_training(&req, &models, &paths, "user").unwrap();
        assert_eq!(first.payload["provenance"]["dataset_version"], 1);
        assert_eq!(first.payload["provenance"]["dataset_csv_sha256"], json!(v1.csv_sha256));
        assert_eq!(first.data_path, "data/datasets/dsv/v1.csv");
        req.dataset_version = Some(9);
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("version 9"));
        req.dataset = None;
        req.dataset_version = Some(1);
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("needs a dataset"));
        // A deleted dataset cannot be trained on, by name or by version.
        req.dataset = Some("dsv".into());
        ml_datasets::delete_in(&dir, "dsv").unwrap();
        assert!(prepare_training(&req, &models, &paths, "user").unwrap_err().contains("unknown dataset"));
        cleanup(&root);
    }

    #[test]
    fn a_model_records_the_dataset_version_it_saw_and_a_later_rebuild_does_not_change_it() {
        let Some((root, paths, models)) = python_root("prov-e2e") else { return };
        build_dataset_files(&paths, "pv", &["AAA", "BBB"], 300);
        build_dataset_files(&paths, "pv", &["AAA", "BBB"], 360);
        let dir = ml_datasets::datasets_dir(&paths);
        let v2 = ml_datasets::read_info(&dir, "pv").unwrap();
        assert_eq!(v2.version, 2);
        let request = |extra: Value| -> MLTrainingRequestDto {
            let mut req = json!({"algorithm": "random_forest", "dataset": "pv", "n_splits": 3, "hyperparams": {"n_estimators": 8, "max_depth": 3}});
            req.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value(req).unwrap()
        };
        let latest = block_on(train_core(&request(json!({})), &models, &paths, "user")).unwrap();
        let detail = build_detail(&models, &latest.model_id, vec![]).unwrap();
        assert_eq!((detail.dataset_version, detail.dataset_csv_sha256.as_deref()), (Some(2), Some(v2.csv_sha256.as_str())));
        assert!(detail.log_tail.contains("Dataset pv version 2") && detail.log_tail.contains(&v2.csv_sha256), "{}", detail.log_tail);
        // Version 1 of the model object exists, created by the user, with the provenance inside.
        let ledger = model_ledger_of(&models);
        let h = ledger.history(Kind::Model, &latest.model_id);
        assert_eq!((h.len(), h[0].action.as_str(), h[0].actor.as_str()), (1, "create", "user"));
        let stored: Value = serde_json::from_slice(&ledger.content(Kind::Model, &latest.model_id, 1).unwrap()).unwrap();
        assert_eq!(stored["dataset_version"], 2);
        assert!(stored["artifact_sha256"].as_str().unwrap().len() == 64);

        // Rebuild the dataset (version 3): the old model still names version 2 and its hash.
        build_dataset_files(&paths, "pv", &["AAA", "BBB"], 330);
        assert_eq!(ml_datasets::read_info(&dir, "pv").unwrap().version, 3);
        let again = build_detail(&models, &latest.model_id, vec![]).unwrap();
        assert_eq!((again.dataset_version, again.dataset_csv_sha256.as_deref()), (Some(2), Some(v2.csv_sha256.as_str())));
        assert_eq!(ml_datasets::hash_file(&dir.join("pv").join("v2.csv")).unwrap().0, v2.csv_sha256);

        // A model trained on an explicit older version says so.
        let first = block_on(train_core(&request(json!({"dataset_version": 1})), &models, &paths, "user")).unwrap();
        assert_eq!(build_detail(&models, &first.model_id, vec![]).unwrap().dataset_version, Some(1));

        // The trainer refuses a file that no longer matches the recorded hash.
        let v1_path = dir.join("pv").join("v1.csv");
        let mut perms = fs::metadata(&v1_path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&v1_path, perms).unwrap();
        let mut tampered = fs::read_to_string(&v1_path).unwrap();
        tampered.push_str("2030-01-01T00:00:00Z,AAA,0.1,50,0.1,0.0,0\n");
        fs::write(&v1_path, tampered).unwrap();
        let err = block_on(train_core(&request(json!({"dataset_version": 1})), &models, &paths, "user")).unwrap_err();
        assert!(err.contains("does not match the audit ledger"), "{err}");

        // Deleting the model archives it and it can no longer be scored.
        assert!(delete_model_files(&models, &latest.model_id, "user").unwrap());
        assert!(load_scoring_model(&models, &latest.model_id).err().unwrap().contains("deleted"));
        restore_model_files(&models, &latest.model_id, None, "user").unwrap();
        assert!(load_scoring_model(&models, &latest.model_id).is_ok());
        cleanup(&root);
    }
}
