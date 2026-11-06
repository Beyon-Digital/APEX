use super::{ml_datasets, ml_estimators, ml_expr, ml_features, python_runtime};
use crate::dto::{
    CustomFeatureDto, HoldoutDto,
    CvFoldDto, FeatureImportanceDto, LabelSpecDto, MLModelDto, MLTrainingRequestDto,
    MLTrainingResultDto, ModelDetailDto, ModelSignalHistoryDto, ModelSignalPointDto,
    ModelUsageDto, TradingSummaryDto,
};
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
        Self { models_dir }
    }
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

pub(crate) fn load_registered_models(models_dir: &Path) -> Result<Vec<MLModelDto>, String> {
    if !models_dir.exists() {
        return Ok(Vec::new());
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
/// including ones this build does not know about, is written back unchanged. The file is replaced
/// atomically (temp file in the same directory, then rename).
pub(crate) fn update_model_metadata(
    models_dir: &Path,
    model_id: &str,
    display_name: Option<&str>,
    notes: Option<&str>,
    tags: Option<&[String]>,
) -> Result<(), String> {
    validate_model_id(model_id)?;
    let _guard = lock_model_files();
    let path = metadata_path(models_dir, model_id);
    let text = fs::read_to_string(&path).map_err(|_| format!("Unknown model `{model_id}` (no metadata file)"))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| format!("Failed to parse model metadata: {e}"))?;
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| "model metadata is not a JSON object".to_string())?;

    if let Some(name) = display_name {
        let name = name.trim();
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(format!("display name is too long (max {MAX_NAME_CHARS} characters)"));
        }
        obj.insert("display_name".into(), Value::String(name.to_string()));
    }
    if let Some(notes) = notes {
        if notes.chars().count() > MAX_NOTES_CHARS {
            return Err(format!("notes are too long (max {MAX_NOTES_CHARS} characters)"));
        }
        obj.insert("notes".into(), Value::String(notes.to_string()));
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
    }

    let tmp = models_dir.join(format!("{model_id}.json.tmp"));
    let body = serde_json::to_string_pretty(&doc).map_err(|e| format!("Failed to encode metadata: {e}"))?;
    fs::write(&tmp, body).map_err(|e| format!("Failed to write metadata: {e}"))?;
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Failed to replace metadata: {e}"));
    }
    Ok(())
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
    let result = update_model_metadata(models_dir, model_id, display_name, notes, tags)
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
    if request.algorithm == "custom" {
        let name = request
            .estimator_file
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| "algorithm `custom` needs an estimator_file (save one in the Train tab first)".to_string())?;
        let path = ml_estimators::resolve_for_training(runtime_paths.work_root(), name)?;
        payload["estimator_file"] = serde_json::json!(name);
        payload["estimator_path"] = serde_json::json!(path.to_string_lossy());
    }

    let data_path;
    if let Some(name) = &request.dataset {
        let dir = ml_datasets::datasets_dir(runtime_paths);
        let info = ml_datasets::read_info(&dir, name)?;
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

    Ok(PreparedTraining { payload, data_path })
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
    let algorithm = request.algorithm.clone();
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
            crate::activity::record(
                state, actor, "model.train", "model", &r.model_id,
                &format!("Trained {algorithm} on {source_label}{acc}"), true, None,
            );
        }
        Err(error) => crate::activity::record(
            state, actor, "model.train", "model", "",
            &format!("Training {algorithm} on {source_label} failed"), false, Some(error.clone()),
        ),
    }
    result
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
    let catalog = ml_estimators::fetch_algorithms(runtime_paths).await?;
    ml_estimators::validate_with_catalog(&catalog, &request.algorithm, &request.hyperparams)?;

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

/// Remove a model's metadata, artifact and training log. Returns whether anything was removed.
pub(crate) fn delete_model_files(models_dir: &Path, model_id: &str) -> Result<bool, String> {
    validate_model_id(model_id)?;
    let _guard = lock_model_files();
    let metadata = metadata_path(models_dir, model_id);
    let mut artifact = models_dir.join(format!("{model_id}.joblib"));
    if let Ok(raw) = fs::read_to_string(&metadata) {
        // A damaged metadata file must not stop the model from being deleted.
        if let Ok(meta) = serde_json::from_str::<ModelMetadataFile>(&raw) {
            validate_model_file(&meta.model_file)?;
            artifact = models_dir.join(meta.model_file);
        }
    }
    let mut removed_any = false;
    // A custom estimator's code is saved next to the model under a name derived from its id.
    let estimator_copy = models_dir.join(format!("{model_id}_estimator.py"));
    for path in [metadata, artifact, log_path(models_dir, model_id), estimator_copy] {
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to delete {:?}: {e}", path))?;
            removed_any = true;
        }
    }
    Ok(removed_any)
}

pub(crate) fn delete_ml_model_inner(
    state: &AppState,
    models_dir: &Path,
    model_id: &str,
    actor: &str,
) -> Result<bool, String> {
    let result = delete_model_files(models_dir, model_id);
    match &result {
        Ok(true) => crate::activity::record(
            state, actor, "model.delete", "model", model_id, &format!("Deleted model {model_id}"), true, None,
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

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apex-ml-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
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
        assert!(update_model_metadata(&dir, "../x", Some("n"), None, None).is_err());
        assert!(delete_model_files(&dir, "../x").is_err());
        assert!(build_detail(&dir, "a/b", vec![]).is_err());
        let _ = fs::remove_dir_all(&dir);
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_null_metric_does_not_hide_the_model() {
        let dir = temp("nullmetric");
        let text = OLD_METADATA.replace("\"f1\": 0.51", "\"f1\": null");
        fs::write(dir.join("m.json"), text).unwrap();
        let list = load_registered_models(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].metrics.contains_key("f1"));
        let _ = fs::remove_dir_all(&dir);
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_changes_only_the_three_fields_and_keeps_unknown_ones() {
        let dir = temp("update");
        let id = write_old_model(&dir);
        let mut doc: Value = serde_json::from_str(OLD_METADATA).unwrap();
        doc["future_field"] = json!({"nested": [1, 2, 3]});
        doc["hyperparams"] = json!({"n_estimators": 50});
        fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();

        update_model_metadata(&dir, &id, Some("  Nice name "), Some("notes here"), Some(&["x".into(), " y ".into(), "x".into(), "".into()])).unwrap();
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
        update_model_metadata(&dir, &id, None, Some("only notes"), None).unwrap();
        let again: Value = serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!(again["display_name"], "Nice name");
        assert_eq!(again["notes"], "only notes");
        assert_eq!(again["tags"], json!(["x", "y"]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_enforces_limits_and_leaves_the_file_alone_on_error() {
        let dir = temp("limits");
        let id = write_old_model(&dir);
        let before = fs::read_to_string(dir.join(format!("{id}.json"))).unwrap();
        assert!(update_model_metadata(&dir, &id, Some(&"n".repeat(81)), None, None).is_err());
        assert!(update_model_metadata(&dir, &id, Some(&"n".repeat(80)), None, None).is_ok());
        assert!(update_model_metadata(&dir, &id, None, Some(&"n".repeat(4001)), None).is_err());
        let many: Vec<String> = (0..13).map(|i| format!("t{i}")).collect();
        assert!(update_model_metadata(&dir, &id, None, None, Some(&many)).is_err());
        assert!(update_model_metadata(&dir, &id, None, None, Some(&many[..12])).is_ok());
        assert!(update_model_metadata(&dir, &id, None, None, Some(&["t".repeat(31)])).is_err());
        assert!(update_model_metadata(&dir, "nope", Some("x"), None, None).is_err());
        assert_ne!(before, fs::read_to_string(dir.join(format!("{id}.json"))).unwrap());
        let _ = fs::remove_dir_all(&dir);
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
            let t1 = std::thread::spawn(move || update_model_metadata(&a, &ida, Some(&n2), None, None));
            let t2 = std::thread::spawn(move || update_model_metadata(&b, &idb, None, Some(&o2), None));
            t1.join().unwrap().expect("name edit");
            t2.join().unwrap().expect("notes edit");
            let doc: Value =
                serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.json"))).unwrap()).unwrap();
            assert_eq!(doc["display_name"], name, "name lost on round {i}");
            assert_eq!(doc["notes"], notes, "notes lost on round {i}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_metadata_artifact_and_log() {
        let dir = temp("delete");
        let id = write_old_model(&dir);
        fs::write(dir.join(format!("{id}.log")), "log").unwrap();
        fs::write(dir.join("keep.txt"), "x").unwrap();
        assert!(delete_model_files(&dir, &id).unwrap());
        for ext in ["json", "joblib", "log"] {
            assert!(!dir.join(format!("{id}.{ext}")).exists(), "{ext}");
        }
        assert!(dir.join("keep.txt").exists());
        assert!(!delete_model_files(&dir, &id).unwrap());
        let _ = fs::remove_dir_all(&dir);
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
        let _ = fs::remove_dir_all(&dir);
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
        assert_eq!(p.data_path, "data/datasets/ds1.csv");
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
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(&root);
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

    /// Rewrite fields of a dataset's sidecar the way a custom-feature build would have written them.
    fn edit_sidecar(paths: &python_runtime::RuntimePaths, name: &str, edit: impl FnOnce(&mut Value)) {
        let path = ml_datasets::datasets_dir(paths).join(format!("{name}.dataset.json"));
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        edit(&mut doc);
        fs::write(&path, doc.to_string()).unwrap();
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
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_model_also_removes_its_estimator_copy() {
        let dir = temp("delete-copy");
        let id = write_old_model(&dir);
        fs::write(dir.join(format!("{id}_estimator.py")), "def make_estimator(p): pass\n").unwrap();
        fs::write(dir.join("other_estimator.py"), "keep").unwrap();
        assert!(delete_model_files(&dir, &id).unwrap());
        assert!(!dir.join(format!("{id}_estimator.py")).exists());
        assert!(dir.join("other_estimator.py").exists());
        let _ = fs::remove_dir_all(&dir);
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
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(&root);
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
        assert!(delete_model_files(&models, &r.model_id).unwrap());
        assert!(!copy.exists());
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(&root);
    }
}
