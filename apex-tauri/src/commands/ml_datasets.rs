//! Dataset builder: turns stored bars into a labelled CSV the trainer can read.
//!
//! Layout under `<data_dir>/datasets/`:
//! * `<name>.csv`: columns `time,symbol,<features...>,fwd_ret,target`
//! * `<name>.dataset.json`: the [`DatasetInfoDto`] describing it
//!
//! Features come from [`super::ml_features`], the same code inference uses. Rows whose features
//! are still warming up, and the last `horizon_bars` rows of each symbol (whose forward return is
//! not known yet), are dropped.

use super::ml_features;
use super::python_runtime::RuntimePaths;
use crate::dto::{BuildDatasetRequestDto, DatasetInfoDto, DatasetPreviewDto, LabelSpecDto};
use crate::state::AppState;
use apex_core::domain::models::OHLCV;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::State;

pub const MAX_ROWS: usize = 200_000;
pub const MAX_SYMBOLS: usize = 30;
pub const MAX_BARS: usize = 20_000;
pub const MAX_HORIZON: usize = 250;
const PREVIEW_ROWS: usize = 12;

/// Dataset names become file names: lowercase letters, digits, `_` and `-`, 1 to 64 characters.
pub fn validate_dataset_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err("dataset name must be 1 to 64 characters: lowercase letters, digits, '_' or '-'".into())
    }
}

pub fn datasets_dir(paths: &RuntimePaths) -> PathBuf {
    paths.data_dir().join("datasets")
}

/// Path of the CSV relative to the work root, the form the trainer resolves.
pub fn relative_csv_path(paths: &RuntimePaths, name: &str) -> String {
    let file = datasets_dir(paths).join(format!("{name}.csv"));
    file.strip_prefix(paths.work_root())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| format!("data/datasets/{name}.csv"))
}

fn csv_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.csv"))
}

fn sidecar_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.dataset.json"))
}

/// Check and normalise a build request (symbols upper-cased and de-duplicated, timeframe
/// canonical, features de-duplicated in order).
pub fn normalize_request(req: &BuildDatasetRequestDto) -> Result<BuildDatasetRequestDto, String> {
    validate_dataset_name(&req.name)?;

    let mut symbols: Vec<String> = Vec::new();
    for s in &req.symbols {
        let s = s.trim().to_uppercase();
        crate::validation::validate_symbol(&s)?;
        if !symbols.contains(&s) {
            symbols.push(s);
        }
    }
    if symbols.is_empty() {
        return Err("choose at least one symbol".into());
    }
    if symbols.len() > MAX_SYMBOLS {
        return Err(format!("too many symbols ({}); the limit is {MAX_SYMBOLS}", symbols.len()));
    }

    let timeframe = ml_features::canonical_timeframe(&req.timeframe)
        .ok_or_else(|| format!("unsupported timeframe `{}` (use 1m, 5m, 15m, 1h, 4h, 1d or 1w)", req.timeframe))?
        .to_string();

    if req.bars == 0 || req.bars > MAX_BARS {
        return Err(format!("bars must be between 1 and {MAX_BARS}"));
    }

    let mut features: Vec<String> = Vec::new();
    for f in &req.features {
        if !ml_features::is_catalog_feature(f) {
            return Err(format!("unknown feature `{f}`; see list_feature_catalog"));
        }
        if !features.contains(f) {
            features.push(f.clone());
        }
    }
    if features.is_empty() {
        return Err("choose at least one feature".into());
    }

    let label = &req.label;
    if label.horizon_bars == 0 || label.horizon_bars > MAX_HORIZON {
        return Err(format!("label horizon_bars must be between 1 and {MAX_HORIZON}"));
    }
    match label.kind.as_str() {
        "direction" => {}
        "return_threshold" => {
            if !label.threshold_pct.is_finite() || label.threshold_pct <= 0.0 || label.threshold_pct > 100.0 {
                return Err("label threshold_pct must be greater than 0 and at most 100 (percent)".into());
            }
        }
        other => return Err(format!("unknown label kind `{other}` (use direction or return_threshold)")),
    }

    Ok(BuildDatasetRequestDto {
        name: req.name.clone(),
        symbols,
        timeframe,
        bars: req.bars,
        features,
        label: LabelSpecDto {
            horizon_bars: label.horizon_bars,
            kind: label.kind.clone(),
            threshold_pct: if label.kind == "direction" { 0.0 } else { label.threshold_pct },
        },
    })
}

/// Class of a row given its forward return (a fraction, 0.01 = 1%).
pub fn label_for(label: &LabelSpecDto, fwd_ret: f64) -> i32 {
    if label.kind == "return_threshold" {
        let thr = label.threshold_pct / 100.0;
        if fwd_ret > thr {
            1
        } else if fwd_ret < -thr {
            -1
        } else {
            0
        }
    } else if fwd_ret > 0.0 {
        1
    } else {
        0
    }
}

/// Build the CSV text and its info from already-loaded bars. Pure: no I/O.
pub fn assemble(
    req: &BuildDatasetRequestDto,
    series: &[(String, Vec<OHLCV>)],
    source: &str,
    path: &str,
    now: DateTime<Utc>,
) -> Result<(String, DatasetInfoDto), String> {
    let h = req.label.horizon_bars;
    let warmup = req
        .features
        .iter()
        .map(|f| ml_features::warmup_of(f).unwrap_or(0))
        .max()
        .unwrap_or(0);
    let need = warmup + h + 1;

    let mut csv = String::from("time,symbol,");
    csv.push_str(&req.features.join(","));
    csv.push_str(",fwd_ret,target\n");

    let mut rows = 0usize;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut first: Option<DateTime<Utc>> = None;
    let mut last: Option<DateTime<Utc>> = None;
    let mut used_symbols: Vec<String> = Vec::new();

    for (symbol, raw) in series {
        let mut bars = raw.clone();
        bars.sort_by_key(|b| b.time);
        bars.dedup_by_key(|b| b.time);
        if bars.len() < need {
            return Err(format!(
                "not enough bars: {symbol} has {} but these features need at least {need} \
                 ({warmup} warmup + {h} horizon + 1); pick fewer or shorter features, a shorter horizon, or fetch more history",
                bars.len()
            ));
        }
        let matrix = ml_features::compute_matrix(&bars, &req.features).map_err(|e| format!("{symbol}: {e}"))?;
        let mut contributed = false;
        for i in 0..bars.len() - h {
            let row = &matrix[i];
            if row.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let fwd = bars[i + h].close / bars[i].close - 1.0;
            if !fwd.is_finite() {
                continue;
            }
            let target = label_for(&req.label, fwd);
            csv.push_str(&bars[i].time.format("%Y-%m-%dT%H:%M:%SZ").to_string());
            csv.push(',');
            csv.push_str(symbol);
            for v in row {
                csv.push(',');
                csv.push_str(&v.to_string());
            }
            csv.push(',');
            csv.push_str(&fwd.to_string());
            csv.push(',');
            csv.push_str(&target.to_string());
            csv.push('\n');
            rows += 1;
            if rows > MAX_ROWS {
                return Err(format!(
                    "dataset would exceed {MAX_ROWS} rows; reduce bars or the number of symbols"
                ));
            }
            *counts.entry(target.to_string()).or_insert(0) += 1;
            first = Some(first.map_or(bars[i].time, |t| t.min(bars[i].time)));
            last = Some(last.map_or(bars[i].time, |t| t.max(bars[i].time)));
            contributed = true;
        }
        if contributed {
            used_symbols.push(symbol.clone());
        }
    }

    if rows == 0 {
        return Err(format!(
            "not enough bars: no usable rows. These features need at least {need} bars per symbol \
             ({warmup} warmup + {h} horizon + 1); fetch more history or choose a shorter horizon"
        ));
    }

    let info = DatasetInfoDto {
        name: req.name.clone(),
        path: path.to_string(),
        rows,
        symbols: used_symbols,
        timeframe: req.timeframe.clone(),
        features: req.features.clone(),
        label: req.label.clone(),
        class_counts: counts,
        created_at: now.to_rfc3339(),
        first_time: first.map(|t| t.to_rfc3339()),
        last_time: last.map(|t| t.to_rfc3339()),
        source: source.to_string(),
    };
    Ok((csv, info))
}

/// Write the CSV and sidecar (temp file then rename, so a reader never sees half a file).
pub fn write_dataset(dir: &Path, csv: &str, info: &DatasetInfoDto) -> Result<(), String> {
    validate_dataset_name(&info.name)?;
    fs::create_dir_all(dir).map_err(|e| format!("could not create datasets directory: {e}"))?;
    let csv_final = csv_path(dir, &info.name);
    let side_final = sidecar_path(dir, &info.name);
    let csv_tmp = dir.join(format!("{}.csv.tmp", info.name));
    let side_tmp = dir.join(format!("{}.dataset.json.tmp", info.name));
    fs::write(&csv_tmp, csv).map_err(|e| format!("could not write dataset: {e}"))?;
    let json = serde_json::to_string_pretty(info).map_err(|e| format!("could not encode dataset info: {e}"))?;
    fs::write(&side_tmp, json).map_err(|e| format!("could not write dataset info: {e}"))?;
    fs::rename(&csv_tmp, &csv_final).map_err(|e| format!("could not finalise dataset: {e}"))?;
    fs::rename(&side_tmp, &side_final).map_err(|e| format!("could not finalise dataset info: {e}"))?;
    Ok(())
}

pub fn read_info(dir: &Path, name: &str) -> Result<DatasetInfoDto, String> {
    validate_dataset_name(name)?;
    let raw = fs::read_to_string(sidecar_path(dir, name)).map_err(|_| format!("unknown dataset `{name}`"))?;
    serde_json::from_str(&raw).map_err(|e| format!("dataset `{name}` has unreadable info: {e}"))
}

pub fn list_datasets_in(dir: &Path) -> Vec<DatasetInfoDto> {
    let mut out: Vec<DatasetInfoDto> = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().to_string();
        let Some(name) = file.strip_suffix(".dataset.json") else {
            continue;
        };
        if validate_dataset_name(name).is_err() {
            continue;
        }
        match read_info(dir, name) {
            Ok(info) => out.push(info),
            Err(error) => tracing::warn!(%error, "skipping unreadable dataset"),
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.name.cmp(&b.name)));
    out
}

fn cell(text: &str) -> serde_json::Value {
    if let Ok(i) = text.parse::<i64>() {
        return serde_json::json!(i);
    }
    if let Ok(f) = text.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(text.to_string())
}

pub fn preview_in(dir: &Path, name: &str) -> Result<DatasetPreviewDto, String> {
    let info = read_info(dir, name)?;
    let text = fs::read_to_string(csv_path(dir, name)).map_err(|e| format!("could not read dataset file: {e}"))?;
    let mut lines = text.lines();
    let columns: Vec<String> = lines
        .next()
        .map(|h| h.split(',').map(String::from).collect())
        .unwrap_or_default();
    let sample = lines
        .take(PREVIEW_ROWS)
        .map(|l| l.split(',').map(cell).collect())
        .collect();
    Ok(DatasetPreviewDto { info, columns, sample })
}

pub fn delete_in(dir: &Path, name: &str) -> Result<bool, String> {
    validate_dataset_name(name)?;
    let mut removed = false;
    for path in [csv_path(dir, name), sidecar_path(dir, name)] {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("could not delete {}: {e}", path.display()))?;
            removed = true;
        }
    }
    Ok(removed)
}

// ── Orchestration with activity logging ──────────────────────────────

/// Load bars, build, write and log. `actor` is "user" or "copilot".
pub(crate) async fn build_dataset_inner(
    state: &AppState,
    paths: &RuntimePaths,
    request: BuildDatasetRequestDto,
    actor: &str,
) -> Result<DatasetInfoDto, String> {
    let name = request.name.clone();
    let result = build_dataset_core(state, paths, request, actor).await;
    match &result {
        Ok(info) => crate::activity::record(
            state,
            actor,
            "dataset.build",
            "dataset",
            &info.name,
            &format!("Built dataset {} ({} rows, {} symbols)", info.name, info.rows, info.symbols.len()),
            true,
            None,
        ),
        Err(error) => crate::activity::record(
            state,
            actor,
            "dataset.build",
            "dataset",
            &name,
            &format!("Dataset build failed: {name}"),
            false,
            Some(error.clone()),
        ),
    }
    result
}

async fn build_dataset_core(
    state: &AppState,
    paths: &RuntimePaths,
    request: BuildDatasetRequestDto,
    actor: &str,
) -> Result<DatasetInfoDto, String> {
    let req = normalize_request(&request)?;
    let mut series: Vec<(String, Vec<OHLCV>)> = Vec::new();
    for symbol in &req.symbols {
        let bars = super::data::load_bars(state, symbol, Some(&req.timeframe), req.bars).await?;
        series.push((symbol.clone(), bars));
    }
    let rel = relative_csv_path(paths, &req.name);
    let (csv, info) = assemble(&req, &series, actor, &rel, Utc::now())?;
    write_dataset(&datasets_dir(paths), &csv, &info)?;
    Ok(info)
}

pub(crate) fn delete_dataset_inner(
    state: &AppState,
    paths: &RuntimePaths,
    name: &str,
    actor: &str,
) -> Result<bool, String> {
    let result = delete_in(&datasets_dir(paths), name);
    match &result {
        Ok(true) => crate::activity::record(
            state, actor, "dataset.delete", "dataset", name, &format!("Deleted dataset {name}"), true, None,
        ),
        Ok(false) => {}
        Err(error) => crate::activity::record(
            state,
            actor,
            "dataset.delete",
            "dataset",
            name,
            &format!("Dataset delete failed: {name}"),
            false,
            Some(error.clone()),
        ),
    }
    result
}

#[tauri::command]
pub async fn build_dataset(
    request: BuildDatasetRequestDto,
    app_state: State<'_, AppState>,
    paths: State<'_, RuntimePaths>,
) -> Result<DatasetInfoDto, String> {
    build_dataset_inner(&app_state, &paths, request, "user").await
}

#[tauri::command]
pub async fn list_datasets(paths: State<'_, RuntimePaths>) -> Result<Vec<DatasetInfoDto>, String> {
    Ok(list_datasets_in(&datasets_dir(&paths)))
}

#[tauri::command]
pub async fn preview_dataset(name: String, paths: State<'_, RuntimePaths>) -> Result<DatasetPreviewDto, String> {
    preview_in(&datasets_dir(&paths), &name)
}

#[tauri::command]
pub async fn delete_dataset(
    name: String,
    app_state: State<'_, AppState>,
    paths: State<'_, RuntimePaths>,
) -> Result<bool, String> {
    delete_dataset_inner(&app_state, &paths, &name, "user")
}

#[cfg(test)]
mod tests {
    use super::super::ml_features::tests::synthetic_bars;
    use super::*;

    fn req(name: &str) -> BuildDatasetRequestDto {
        BuildDatasetRequestDto {
            name: name.into(),
            symbols: vec!["aapl".into()],
            timeframe: "1d".into(),
            bars: 300,
            features: vec!["ret_1".into(), "rsi_14".into(), "vol_10".into()],
            label: LabelSpecDto { horizon_bars: 5, kind: "direction".into(), threshold_pct: 0.0 },
        }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apex-datasets-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn name_rules() {
        for ok in ["a", "my-data_1", &"x".repeat(64)] {
            assert!(validate_dataset_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Upper", "has space", "a.b", "../evil", "a/b", "a\\b", "..", &"x".repeat(65), "naïve"] {
            assert!(validate_dataset_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_that_escape_the_directory_are_refused_everywhere() {
        let dir = temp("escape");
        fs::create_dir_all(&dir).unwrap();
        let outside = dir.join("outside.csv");
        fs::write(&outside, "x").unwrap();
        let inner = dir.join("datasets");
        fs::create_dir_all(&inner).unwrap();
        assert!(delete_in(&inner, "../outside").is_err());
        assert!(preview_in(&inner, "../outside").is_err());
        assert!(read_info(&inner, "..").is_err());
        assert!(outside.exists(), "file outside the datasets dir must be untouched");
        let mut info = assemble(&normalize_request(&req("ok")).unwrap(), &[("AAPL".into(), synthetic_bars(120))], "user", "p", Utc::now())
            .unwrap()
            .1;
        info.name = "../escape".into();
        assert!(write_dataset(&inner, "x", &info).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_validation() {
        let mut r = req("ok");
        r.features.clear();
        assert!(normalize_request(&r).unwrap_err().contains("feature"));
        let mut r = req("ok");
        r.symbols.clear();
        assert!(normalize_request(&r).unwrap_err().contains("symbol"));
        let mut r = req("ok");
        r.symbols = (0..31).map(|i| format!("S{i}")).collect();
        assert!(normalize_request(&r).unwrap_err().contains("too many symbols"));
        let mut r = req("ok");
        r.features = vec!["nope".into()];
        assert!(normalize_request(&r).unwrap_err().contains("`nope`"));
        let mut r = req("ok");
        r.timeframe = "3d".into();
        assert!(normalize_request(&r).is_err());
        let mut r = req("ok");
        r.label.kind = "return_threshold".into();
        r.label.threshold_pct = 0.0;
        assert!(normalize_request(&r).is_err());
        let mut r = req("ok");
        r.label.horizon_bars = 0;
        assert!(normalize_request(&r).is_err());
        let mut r = req("ok");
        r.symbols = vec!["aapl".into(), "AAPL".into()];
        r.features = vec!["ret_1".into(), "ret_1".into()];
        let n = normalize_request(&r).unwrap();
        assert_eq!(n.symbols, vec!["AAPL"]);
        assert_eq!(n.features, vec!["ret_1"]);
    }

    #[test]
    fn label_rules() {
        let dir = LabelSpecDto { horizon_bars: 1, kind: "direction".into(), threshold_pct: 0.0 };
        assert_eq!(label_for(&dir, 0.0001), 1);
        assert_eq!(label_for(&dir, 0.0), 0);
        assert_eq!(label_for(&dir, -0.02), 0);
        let thr = LabelSpecDto { horizon_bars: 1, kind: "return_threshold".into(), threshold_pct: 1.0 };
        assert_eq!(label_for(&thr, 0.0101), 1);
        assert_eq!(label_for(&thr, 0.01), 0, "exactly at the threshold is not above it");
        assert_eq!(label_for(&thr, 0.005), 0);
        assert_eq!(label_for(&thr, -0.005), 0);
        assert_eq!(label_for(&thr, -0.0101), -1);
    }

    #[test]
    fn assemble_drops_warmup_and_the_tail_and_matches_the_labels() {
        let r = normalize_request(&req("ok")).unwrap();
        let bars = synthetic_bars(100);
        let (csv, info) = assemble(&r, &[("AAPL".into(), bars.clone())], "user", "data/datasets/ok.csv", Utc::now()).unwrap();
        // rsi_14 needs 14 leading bars; the last 5 bars have no 5-bar forward return.
        assert_eq!(info.rows, 100 - 14 - 5);
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "time,symbol,ret_1,rsi_14,vol_10,fwd_ret,target");
        let first: Vec<&str> = lines.next().unwrap().split(',').collect();
        assert!(first[0].starts_with("2024-01-15"), "first kept row is bar 14, got {}", first[0]);
        let fwd: f64 = first[5].parse().unwrap();
        assert!((fwd - (bars[19].close / bars[14].close - 1.0)).abs() < 1e-12);
        assert_eq!(first[6], if fwd > 0.0 { "1" } else { "0" });
        let total: usize = info.class_counts.values().sum();
        assert_eq!(total, info.rows);
        assert_eq!(info.symbols, vec!["AAPL"]);
        assert!(info.first_time.unwrap() < info.last_time.unwrap());
    }

    #[test]
    fn threshold_label_has_three_classes() {
        let mut r = req("ok");
        r.label = LabelSpecDto { horizon_bars: 3, kind: "return_threshold".into(), threshold_pct: 0.5 };
        let r = normalize_request(&r).unwrap();
        let (_, info) = assemble(&r, &[("AAPL".into(), synthetic_bars(300))], "user", "p", Utc::now()).unwrap();
        assert!(info.class_counts.contains_key("-1") && info.class_counts.contains_key("1"), "{:?}", info.class_counts);
    }

    #[test]
    fn too_few_bars_gives_an_actionable_error() {
        let r = normalize_request(&req("ok")).unwrap();
        let err = assemble(&r, &[("AAPL".into(), synthetic_bars(12))], "user", "p", Utc::now()).unwrap_err();
        assert!(err.contains("not enough bars") && err.contains("at least 20") && err.contains("AAPL"), "{err}");
    }

    #[test]
    fn write_list_preview_delete_round_trip() {
        let dir = temp("roundtrip");
        let r = normalize_request(&req("my_set")).unwrap();
        let (csv, info) = assemble(&r, &[("AAPL".into(), synthetic_bars(100))], "copilot", "data/datasets/my_set.csv", Utc::now()).unwrap();
        write_dataset(&dir, &csv, &info).unwrap();
        let listed = list_datasets_in(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source, "copilot");
        let p = preview_in(&dir, "my_set").unwrap();
        assert_eq!(p.columns, vec!["time", "symbol", "ret_1", "rsi_14", "vol_10", "fwd_ret", "target"]);
        assert_eq!(p.sample.len(), PREVIEW_ROWS);
        assert!(p.sample[0][2].is_number() && p.sample[0][0].is_string());
        assert!(delete_in(&dir, "my_set").unwrap());
        assert!(!delete_in(&dir, "my_set").unwrap());
        assert!(list_datasets_in(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn csv_path_is_relative_to_the_work_root() {
        let root = temp("relpath");
        let paths = RuntimePaths::for_test(&root);
        assert_eq!(relative_csv_path(&paths, "abc"), "data/datasets/abc.csv");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parity_dataset_features_equal_inference_features() {
        // Dataset path: features of the last usable row of a long series.
        // Inference path: latest_features on the same bars.
        let names: Vec<String> = ml_features::catalog().into_iter().map(|f| f.id).collect();
        let mut r = req("parity");
        r.features = names.clone();
        r.label.horizon_bars = 1;
        let r = normalize_request(&r).unwrap();
        let bars = synthetic_bars(260);
        let (csv, _) = assemble(&r, &[("TEST".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        let last_row: Vec<f64> = csv.lines().last().unwrap().split(',').skip(2).take(names.len()).map(|v| v.parse().unwrap()).collect();
        // The dataset's last row is bar 258 (bar 259 has no forward return): score the same prefix.
        let inference = ml_features::latest_features(&bars[..259], &names, "TEST").unwrap();
        for (i, n) in names.iter().enumerate() {
            assert_eq!(inference[n], last_row[i], "{n}");
        }
        // And on only the most recent 150 bars, as inference sees them: recursive indicators
        // converge, so the values agree closely.
        let short = ml_features::latest_features(&bars[259 - 149..259], &names, "TEST").unwrap();
        for n in &names {
            let (a, b) = (inference[n], short[n]);
            assert!((a - b).abs() <= 1e-3 * a.abs().max(1.0), "{n}: {a} vs {b}");
        }
    }
}
