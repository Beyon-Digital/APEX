//! Dataset builder: turns stored bars into a labelled CSV the trainer can read.
//!
//! A dataset name is a versioned object in the audit ledger (kind `dataset`, object id = name).
//! Every build is a NEW VERSION under the same name and never overwrites an earlier one; that would
//! be an audit violation. Layout under `<data_dir>/datasets/`:
//! * `<name>/v<n>.csv`: columns `time,symbol,<catalog features...>,<custom features...>,fwd_ret,target`
//! * `<name>/v<n>.dataset.json`: the [`DatasetInfoDto`] describing it (also the ledger content)
//!
//! Both files are created once (`create_new`) and made read-only; `n` is the ledger version the build
//! gets. The ledger record holds the sidecar JSON, which carries `csv_sha256`, so one version covers
//! both files while the (large) CSV itself stays a write-once file. Deleting a dataset appends a
//! tombstone version: the files stay on disk, the name disappears from the list.
//!
//! Datasets built before versioning live at `<name>.csv` + `<name>.dataset.json`. They are registered
//! lazily as version 1 the first time they are listed, read or deleted. Their files stay where they are.
//!
//! Features come from [`super::ml_features`], the same code inference uses. Rows whose features
//! are still warming up, and the last `horizon_bars` rows of each symbol (whose forward return is
//! not known yet), are dropped. `fwd_ret` is net of `fee_pct` and the label is cut from that net
//! value, so labels and any trading evaluation already include costs.

use super::ml_expr::{self, Context};
use super::ml_features;
use super::python_runtime::RuntimePaths;
use crate::dto::{
    BuildDatasetRequestDto, DatasetInfoDto, DatasetPreviewDto, ExpressionCheckDto, ExpressionSampleDto, LabelSpecDto,
    SymbolDataDto,
};
use crate::ledger::{Kind, Ledger};
use crate::state::AppState;
use apex_core::domain::models::OHLCV;
use chrono::{DateTime, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Read, Write};
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

/// Where a dataset built before versioning keeps its CSV, relative to the work root. New builds
/// go to `<name>/v<n>.csv` (see [`write_dataset`], which rewrites `DatasetInfoDto::path`).
pub fn relative_csv_path(paths: &RuntimePaths, name: &str) -> String {
    let file = datasets_dir(paths).join(format!("{name}.csv"));
    file.strip_prefix(paths.work_root())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| format!("data/datasets/{name}.csv"))
}

/// Old layout.
fn legacy_csv_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.csv"))
}

/// Old layout.
fn legacy_sidecar_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.dataset.json"))
}

fn versioned_csv_path(dir: &Path, name: &str, version: u32) -> PathBuf {
    dir.join(name).join(format!("v{version}.csv"))
}

fn versioned_sidecar_path(dir: &Path, name: &str, version: u32) -> PathBuf {
    dir.join(name).join(format!("v{version}.dataset.json"))
}

/// The CSV of one dataset version, for either layout.
pub(crate) fn csv_of(dir: &Path, info: &DatasetInfoDto) -> PathBuf {
    let versioned = versioned_csv_path(dir, &info.name, info.version);
    if info.version > 0 && versioned.exists() {
        versioned
    } else {
        legacy_csv_path(dir, &info.name)
    }
}

/// Relative path (to the work root) of a new version's CSV, built from the path the caller gave for
/// the old layout (`<prefix><name>.csv`).
fn versioned_rel_path(given: &str, name: &str, version: u32) -> String {
    let prefix = given
        .strip_suffix(&format!("{name}.csv"))
        .filter(|p| p.is_empty() || p.ends_with('/'))
        .unwrap_or("data/datasets/");
    format!("{prefix}{name}/v{version}.csv")
}

/// SHA-256 and size of a file, streamed (datasets can be tens of MB).
pub(crate) fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().iter().map(|b| format!("{b:02x}")).collect(), total))
}

/// The ledger of the data directory a datasets directory (`<data_dir>/datasets`) belongs to.
fn ledger_for_dir(dir: &Path) -> Result<Ledger, String> {
    let data_dir = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "the datasets directory has no parent data directory".to_string())?;
    crate::activity::ledger_for(data_dir)
}

/// Serializes version numbering + file creation + the ledger append, so two builds under one name
/// cannot pick the same number.
static DATASET_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn lock_writes() -> std::sync::MutexGuard<'static, ()> {
    DATASET_WRITES.lock().unwrap_or_else(|p| p.into_inner())
}

/// Create a file that must not exist yet, flush it, and make it read-only.
fn write_once(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    file.sync_all().map_err(|e| format!("could not flush {}: {e}", path.display()))?;
    drop(file);
    let mut perms = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms).map_err(|e| format!("could not make {} read-only: {e}", path.display()))
}

pub const MAX_FEE_PCT: f64 = 10.0;

fn parse_date(label: &str, v: &Option<String>) -> Result<Option<NaiveDate>, String> {
    match v.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| format!("`{label}` must be a date like 2024-01-31, got `{text}`")),
    }
}

/// The time window a request selects: `from` at 00:00 UTC (inclusive) and the start of the day
/// after `to` (exclusive), so both dates are inclusive.
pub fn date_bounds(req: &BuildDatasetRequestDto) -> Result<(Option<DateTime<Utc>>, Option<DateTime<Utc>>), String> {
    let from = parse_date("from", &req.from)?.map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc());
    let to = parse_date("to", &req.to)?
        .map(|d| (d + chrono::Duration::days(1)).and_hms_opt(0, 0, 0).unwrap().and_utc());
    Ok((from, to))
}

/// Choose the bars of one symbol: those inside `[from, to]` when dates are given, else the latest
/// `bars`. With only `to` it is the latest `bars` ending at `to`.
fn select_bars(mut bars: Vec<OHLCV>, req: &BuildDatasetRequestDto, bounds: (Option<DateTime<Utc>>, Option<DateTime<Utc>>)) -> Vec<OHLCV> {
    bars.sort_by_key(|b| b.time);
    bars.dedup_by_key(|b| b.time);
    let (from, to_excl) = bounds;
    if let Some(t) = to_excl {
        bars.retain(|b| b.time < t);
    }
    if let Some(t) = from {
        bars.retain(|b| b.time >= t);
    }
    let cap = if from.is_some() { MAX_BARS } else { req.bars };
    let start = bars.len().saturating_sub(cap);
    bars.split_off(start)
}

/// Check and normalise a build request (symbols upper-cased and de-duplicated, timeframe
/// canonical, features de-duplicated in order, custom formulas compiled, dates and thresholds
/// validated).
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

    let from = parse_date("from", &req.from)?;
    let to = parse_date("to", &req.to)?;
    if let (Some(f), Some(t)) = (from, to) {
        if f > t {
            return Err(format!("`from` ({f}) is after `to` ({t})"));
        }
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

    // Custom features: valid unique names, formulas that compile, every context symbol declared.
    let customs = ml_features::Customs::new(&req.custom_features)?;
    for c in &req.custom_features {
        if features.contains(&c.name) {
            return Err(format!("custom feature `{}` has the same name as a selected catalog feature", c.name));
        }
    }
    if features.is_empty() && req.custom_features.is_empty() {
        return Err("choose at least one feature (a catalog feature or a custom formula)".into());
    }
    let context_symbols = ml_features::normalize_symbols(&req.context_symbols)?;
    for sym in customs.referenced_symbols() {
        if !context_symbols.contains(&sym) {
            return Err(format!(
                "a custom feature reads {sym} with other(\"{sym}\", ...) but {sym} is not in the context symbols; add it there"
            ));
        }
    }

    if !req.fee_pct.is_finite() || req.fee_pct < 0.0 || req.fee_pct > MAX_FEE_PCT {
        return Err(format!("fee_pct is the round-trip cost in percent and must be between 0 and {MAX_FEE_PCT}"));
    }

    let label = &req.label;
    if label.horizon_bars == 0 || label.horizon_bars > MAX_HORIZON {
        return Err(format!("label horizon_bars must be between 1 and {MAX_HORIZON}"));
    }
    let valid_pct = |v: f64| v.is_finite() && v > 0.0 && v <= 100.0;
    let norm_label = match label.kind.as_str() {
        "direction" => LabelSpecDto {
            horizon_bars: label.horizon_bars,
            kind: label.kind.clone(),
            threshold_pct: 0.0,
            up_threshold_pct: None,
            down_threshold_pct: None,
        },
        "return_threshold" => {
            let (up, down) = thresholds(label);
            if !valid_pct(up) || !valid_pct(down) {
                return Err(
                    "label thresholds must be greater than 0 and at most 100 (percent); set threshold_pct, or both up_threshold_pct and down_threshold_pct"
                        .into(),
                );
            }
            let asymmetric = up != down;
            LabelSpecDto {
                horizon_bars: label.horizon_bars,
                kind: label.kind.clone(),
                threshold_pct: if valid_pct(label.threshold_pct) { label.threshold_pct } else { up },
                up_threshold_pct: asymmetric.then_some(up),
                down_threshold_pct: asymmetric.then_some(down),
            }
        }
        other => return Err(format!("unknown label kind `{other}` (use direction or return_threshold)")),
    };

    Ok(BuildDatasetRequestDto {
        name: req.name.clone(),
        symbols,
        timeframe,
        bars: req.bars,
        features,
        label: norm_label,
        from: from.map(|d| d.to_string()),
        to: to.map(|d| d.to_string()),
        custom_features: req.custom_features.clone(),
        context_symbols,
        fee_pct: req.fee_pct,
    })
}

/// Effective (up, down) thresholds in percent: the specific one when set, else `threshold_pct`.
pub fn thresholds(label: &LabelSpecDto) -> (f64, f64) {
    (
        label.up_threshold_pct.unwrap_or(label.threshold_pct),
        label.down_threshold_pct.unwrap_or(label.threshold_pct),
    )
}

/// Class of a row. `fwd_ret` is the stored forward return, a fraction (0.01 = 1%), net of the
/// round-trip `fee` (also a fraction) AS IF the trade were long: `gross - fee`.
///
/// A long earns `gross - fee`, so class 1 means the long cleared the up threshold after costs.
/// A short earns `-gross - fee`, which in terms of the stored value is `-fwd_ret - 2*fee`, so
/// class -1 means the short cleared the down threshold after costs: `fwd_ret < -(down + 2*fee)`.
/// Cutting it at `-down` alone would label rows as profitable shorts that lose money once the
/// fee is paid. (A 2-class direction label has no short class; its 0 means "the long did not pay".)
pub fn label_for(label: &LabelSpecDto, fwd_ret: f64, fee: f64) -> i32 {
    if label.kind == "return_threshold" {
        let (up, down) = thresholds(label);
        if fwd_ret > up / 100.0 {
            1
        } else if fwd_ret < -(down / 100.0 + 2.0 * fee) {
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

/// Every feature column of a dataset in CSV order: catalog ids, then custom feature names.
pub fn all_feature_columns(info: &DatasetInfoDto) -> Vec<String> {
    info.features
        .iter()
        .cloned()
        .chain(info.custom_features.iter().map(|c| c.name.clone()))
        .collect()
}

/// Build the CSV text and its info from already-loaded bars. Pure: no I/O. Catalog features
/// only; see [`assemble_with`] for custom features that read context symbols.
#[allow(dead_code)]
pub fn assemble(
    req: &BuildDatasetRequestDto,
    series: &[(String, Vec<OHLCV>)],
    source: &str,
    path: &str,
    now: DateTime<Utc>,
) -> Result<(String, DatasetInfoDto), String> {
    assemble_with(req, series, &Context::new(), source, path, now)
}

/// [`assemble`] with the bars of the context symbols (keys upper case) for custom formulas.
/// `req` must already be normalised. The date window of the request applies to the dataset
/// symbols; context symbols are cut only at `to`, so their values before `from` still fill forward
/// into the first rows.
pub fn assemble_with(
    req: &BuildDatasetRequestDto,
    series: &[(String, Vec<OHLCV>)],
    context: &Context,
    source: &str,
    path: &str,
    now: DateTime<Utc>,
) -> Result<(String, DatasetInfoDto), String> {
    let h = req.label.horizon_bars;
    let customs = ml_features::Customs::new(&req.custom_features)?;
    let warmup = req
        .features
        .iter()
        .map(|f| ml_features::warmup_of(f).unwrap_or(0))
        .chain(req.custom_features.iter().map(|c| customs.warmup(&c.name).unwrap_or(0)))
        .max()
        .unwrap_or(0);
    let need = warmup + h + 1;
    let names: Vec<String> = req
        .features
        .iter()
        .cloned()
        .chain(req.custom_features.iter().map(|c| c.name.clone()))
        .collect();
    let bounds = date_bounds(req)?;
    let fee = req.fee_pct / 100.0;
    let in_range = if req.from.is_some() || req.to.is_some() {
        format!(
            " in the chosen date range ({} to {})",
            req.from.as_deref().unwrap_or("the start"),
            req.to.as_deref().unwrap_or("now")
        )
    } else {
        String::new()
    };

    // Context bars, cut at `to` so nothing after the dataset's end is visible.
    let mut ctx: Context = Context::new();
    for (sym, bars) in context {
        let mut kept: Vec<OHLCV> = bars.clone();
        if let Some(t) = bounds.1 {
            kept.retain(|b| b.time < t);
        }
        ctx.insert(sym.to_uppercase(), kept);
    }

    let mut csv = String::from("time,symbol,");
    csv.push_str(&names.join(","));
    csv.push_str(",fwd_ret,target\n");

    let mut rows = 0usize;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut first: Option<DateTime<Utc>> = None;
    let mut last: Option<DateTime<Utc>> = None;
    let mut used_symbols: Vec<String> = Vec::new();
    let mut unused: Vec<String> = Vec::new();

    for (symbol, raw) in series {
        let bars = select_bars(raw.clone(), req, bounds);
        if bars.len() < need {
            return Err(format!(
                "not enough bars: {symbol} has {} bars{in_range} but these features need at least {need} \
                 ({warmup} warmup + {h} horizon + 1); pick fewer or shorter features, a shorter horizon, a wider date range, \
                 or fetch more history with fetch_symbol_history",
                bars.len()
            ));
        }
        let matrix = ml_features::model_matrix_compiled(&bars, &names, &customs, &ctx).map_err(|e| format!("{symbol}: {e}"))?;
        let mut contributed = false;
        for i in 0..bars.len() - h {
            let row = &matrix[i];
            if row.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let fwd = bars[i + h].close / bars[i].close - 1.0 - fee;
            if !fwd.is_finite() {
                continue;
            }
            let target = label_for(&req.label, fwd, fee);
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
        } else {
            unused.push(format!("{symbol} ({} bars)", bars.len()));
        }
    }

    if rows == 0 {
        let context_note = if ctx.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = ctx
                .iter()
                .map(|(s, b)| {
                    format!(
                        "{s} ({} bars{})",
                        b.len(),
                        b.first().map(|x| format!(", from {}", x.time.format("%Y-%m-%d"))).unwrap_or_default()
                    )
                })
                .collect();
            format!(" Context symbols loaded: {}; they must have bars covering the same dates.", list.join(", "))
        };
        return Err(format!(
            "not enough bars: no usable rows{in_range}. These features need at least {need} bars per symbol \
             ({warmup} warmup + {h} horizon + 1) with every value defined. No rows from: {}.{context_note} \
             Fetch more history with fetch_symbol_history, widen the date range, or choose a shorter horizon",
            unused.join(", ")
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
        custom_features: req.custom_features.clone(),
        context_symbols: req.context_symbols.clone(),
        fee_pct: req.fee_pct,
        // Set by `write_dataset`, which knows the version and hashes the file it writes.
        version: 0,
        csv_sha256: String::new(),
        csv_bytes: 0,
    };
    Ok((csv, info))
}

/// Write a dataset as the NEXT VERSION of `info.name`: `<name>/v<n>.csv` and `<name>/v<n>.dataset.json`
/// (both created once and made read-only), then one ledger record carrying the sidecar. Nothing that
/// exists is ever overwritten. Returns the info as stored (version, csv hash and size, path filled in).
///
/// `info.path` should be the old-layout path (`.../<name>.csv`); it is rewritten to the versioned file.
/// Files left behind by an interrupted earlier build of the same number (no ledger record) are renamed
/// aside, never deleted.
pub fn write_dataset(dir: &Path, csv: &str, info: &DatasetInfoDto) -> Result<DatasetInfoDto, String> {
    validate_dataset_name(&info.name)?;
    let name = info.name.as_str();
    let ledger = ledger_for_dir(dir)?;
    let _guard = lock_writes();
    fs::create_dir_all(dir).map_err(|e| format!("could not create datasets directory: {e}"))?;
    // A dataset from before versioning becomes version 1 first, so this build is version 2.
    migrate_legacy(&ledger, dir, name)?;
    let head = ledger.head(Kind::Dataset, name);
    let version = head.as_ref().map(|h| h.version + 1).unwrap_or(1);

    fs::create_dir_all(dir.join(name)).map_err(|e| format!("could not create the dataset directory: {e}"))?;
    let csv_final = versioned_csv_path(dir, name, version);
    let side_final = versioned_sidecar_path(dir, name, version);
    for leftover in [&csv_final, &side_final] {
        if leftover.exists() {
            let aside = leftover.with_file_name(format!(
                "{}.orphan-{}",
                leftover.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
                Utc::now().timestamp_nanos_opt().unwrap_or(0)
            ));
            fs::rename(leftover, &aside).map_err(|e| format!("could not set aside a leftover {}: {e}", leftover.display()))?;
        }
    }

    let mut stored = info.clone();
    stored.version = version;
    stored.csv_sha256 = Ledger::sha256_hex(csv.as_bytes());
    stored.csv_bytes = csv.len() as u64;
    stored.path = versioned_rel_path(&info.path, name, version);
    let json = serde_json::to_string_pretty(&stored).map_err(|e| format!("could not encode dataset info: {e}"))?;

    write_once(&csv_final, csv.as_bytes())?;
    write_once(&side_final, json.as_bytes())?;
    let actor = if info.source.is_empty() { "user" } else { info.source.as_str() };
    let summary = format!(
        "Built version {version}: {} rows, {} on {}, csv sha256 {}",
        stored.rows,
        stored.symbols.join(", "),
        stored.timeframe,
        &stored.csv_sha256[..12]
    );
    let record = ledger.put(Kind::Dataset, name, json.as_bytes(), actor, &summary)?;
    if record.version != version {
        return Err(format!("the audit ledger numbered this build {} instead of {version}; the files were kept", record.version));
    }
    Ok(stored)
}

/// Register a dataset from before versioning as version 1 (files stay where they are). The ledger
/// content is the old sidecar plus `version`, `csv_sha256` and `csv_bytes`. Caller holds the write lock.
fn migrate_legacy(ledger: &Ledger, dir: &Path, name: &str) -> Result<bool, String> {
    if !ledger.history(Kind::Dataset, name).is_empty() {
        return Ok(false);
    }
    let Ok(raw) = fs::read_to_string(legacy_sidecar_path(dir, name)) else {
        return Ok(false);
    };
    let mut info: DatasetInfoDto =
        serde_json::from_str(&raw).map_err(|e| format!("dataset `{name}` has unreadable info: {e}"))?;
    info.name = name.to_string();
    info.version = 1;
    match hash_file(&legacy_csv_path(dir, name)) {
        Ok((hash, size)) => {
            info.csv_sha256 = hash;
            info.csv_bytes = size;
        }
        Err(error) => tracing::warn!(%error, dataset = name, "dataset file is missing or unreadable while registering it in the audit ledger"),
    }
    let json = serde_json::to_string_pretty(&info).map_err(|e| format!("could not encode dataset info: {e}"))?;
    ledger.put(
        Kind::Dataset,
        name,
        json.as_bytes(),
        "system",
        "Registered a dataset built before versioning as version 1 (files left in place)",
    )?;
    Ok(true)
}

fn parse_info(name: &str, bytes: &[u8]) -> Result<DatasetInfoDto, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("dataset `{name}` has unreadable info: {e}"))
}

/// One version of a dataset, or the latest when `version` is None. A deleted dataset is unknown.
pub fn read_info_version(dir: &Path, name: &str, version: Option<u32>) -> Result<DatasetInfoDto, String> {
    validate_dataset_name(name)?;
    let ledger = ledger_for_dir(dir)?;
    {
        let _guard = lock_writes();
        migrate_legacy(&ledger, dir, name)?;
    }
    let head = ledger
        .head(Kind::Dataset, name)
        .filter(|h| !h.is_tombstone())
        .ok_or_else(|| format!("unknown dataset `{name}`"))?;
    let wanted = version.unwrap_or(head.version);
    let bytes = ledger
        .content(Kind::Dataset, name, wanted)
        .map_err(|e| format!("dataset `{name}` has no usable version {wanted}: {e}"))?;
    parse_info(name, &bytes)
}

/// The latest version of a dataset.
pub fn read_info(dir: &Path, name: &str) -> Result<DatasetInfoDto, String> {
    read_info_version(dir, name, None)
}

/// The latest version of every dataset that has not been deleted. Datasets from before versioning are
/// registered on the way.
pub fn list_datasets_checked(dir: &Path) -> Result<Vec<DatasetInfoDto>, String> {
    let ledger = ledger_for_dir(dir)?;
    if let Ok(entries) = fs::read_dir(dir) {
        let _guard = lock_writes();
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().to_string();
            let Some(name) = file.strip_suffix(".dataset.json") else {
                continue;
            };
            if validate_dataset_name(name).is_err() {
                continue;
            }
            if let Err(error) = migrate_legacy(&ledger, dir, name) {
                tracing::warn!(%error, dataset = name, "skipping unreadable dataset");
            }
        }
    }
    let mut out = Vec::new();
    for object in ledger.objects(Kind::Dataset, false) {
        match ledger
            .content(Kind::Dataset, &object.object_id, object.head_version)
            .and_then(|bytes| parse_info(&object.object_id, &bytes))
        {
            Ok(info) => out.push(info),
            Err(error) => tracing::warn!(%error, "skipping unreadable dataset"),
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.name.cmp(&b.name)));
    Ok(out)
}

pub fn list_datasets_in(dir: &Path) -> Vec<DatasetInfoDto> {
    list_datasets_checked(dir).unwrap_or_else(|error| {
        tracing::error!(%error, "could not list datasets");
        Vec::new()
    })
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

/// Preview of the latest version.
pub fn preview_in(dir: &Path, name: &str) -> Result<DatasetPreviewDto, String> {
    let info = read_info(dir, name)?;
    let file = fs::File::open(csv_of(dir, &info)).map_err(|e| format!("could not read dataset file: {e}"))?;
    let mut lines = std::io::BufReader::new(file).lines();
    let columns: Vec<String> = match lines.next() {
        Some(h) => h.map_err(|e| format!("could not read dataset file: {e}"))?.split(',').map(String::from).collect(),
        None => Vec::new(),
    };
    let mut sample = Vec::new();
    for line in lines.take(PREVIEW_ROWS) {
        let line = line.map_err(|e| format!("could not read dataset file: {e}"))?;
        sample.push(line.split(',').map(cell).collect());
    }
    Ok(DatasetPreviewDto { info, columns, sample })
}

/// Delete = append a tombstone version. Every file and every earlier version stays; the name just
/// disappears from the list. Returns whether there was a live dataset.
pub fn delete_in_as(dir: &Path, name: &str, actor: &str) -> Result<bool, String> {
    validate_dataset_name(name)?;
    let ledger = ledger_for_dir(dir)?;
    let _guard = lock_writes();
    migrate_legacy(&ledger, dir, name)?;
    match ledger.head(Kind::Dataset, name) {
        Some(head) if !head.is_tombstone() => {
            ledger.tombstone(Kind::Dataset, name, actor, &format!("Deleted dataset (version {} was the latest); files kept", head.version))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn delete_in(dir: &Path, name: &str) -> Result<bool, String> {
    delete_in_as(dir, name, "user")
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
            &format!("Built dataset {} version {} ({} rows, {} symbols)", info.name, info.version, info.rows, info.symbols.len()),
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

/// Bars to ask the loader for so a date window is covered: `bars` when no dates are given.
fn load_limit(req: &BuildDatasetRequestDto, now: DateTime<Utc>) -> Result<usize, String> {
    let (from, to_excl) = date_bounds(req)?;
    let bucket = bucket_secs(&req.timeframe);
    let since = |t: DateTime<Utc>| ((now - t).num_seconds().max(0) / bucket) as usize + 10;
    let limit = match (from, to_excl) {
        (Some(f), _) => since(f),
        (None, Some(t)) => req.bars + since(t),
        (None, None) => req.bars,
    };
    Ok(limit.clamp(req.bars, MAX_BARS))
}

fn bucket_secs(timeframe: &str) -> i64 {
    match timeframe {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "1h" => 3600,
        "4h" => 14_400,
        "1w" => 604_800,
        _ => 86_400,
    }
}

async fn build_dataset_core(
    state: &AppState,
    paths: &RuntimePaths,
    request: BuildDatasetRequestDto,
    actor: &str,
) -> Result<DatasetInfoDto, String> {
    let req = normalize_request(&request)?;
    let limit = load_limit(&req, Utc::now())?;
    let mut series: Vec<(String, Vec<OHLCV>)> = Vec::new();
    for symbol in &req.symbols {
        let bars = super::data::load_bars(state, symbol, Some(&req.timeframe), limit).await?;
        if bars.is_empty() {
            return Err(format!(
                "no {} bars stored for {symbol}; fetch its history first (fetch_symbol_history)",
                req.timeframe
            ));
        }
        series.push((symbol.clone(), bars));
    }
    let mut context = Context::new();
    for symbol in &req.context_symbols {
        let bars = super::data::load_bars(state, symbol, Some(&req.timeframe), limit).await?;
        if bars.is_empty() {
            return Err(format!(
                "no {} bars stored for context symbol {symbol}; fetch its history first (fetch_symbol_history)",
                req.timeframe
            ));
        }
        context.insert(symbol.clone(), bars);
    }
    let rel = relative_csv_path(paths, &req.name);
    let (csv, info) = assemble_with(&req, &series, &context, actor, &rel, Utc::now())?;
    write_dataset(&datasets_dir(paths), &csv, &info)
}

pub(crate) fn delete_dataset_inner(
    state: &AppState,
    paths: &RuntimePaths,
    name: &str,
    actor: &str,
) -> Result<bool, String> {
    let result = delete_in_as(&datasets_dir(paths), name, actor);
    match &result {
        Ok(true) => crate::activity::record(
            state, actor, "dataset.delete", "dataset", name,
            &format!("Deleted dataset {name} (all versions and files are kept in the audit ledger)"), true, None,
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
    list_datasets_checked(&datasets_dir(&paths))
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

// ── Formula check and data selection ─────────────────────────────────

/// Most symbols one `list_symbol_data` call accepts.
pub const MAX_LISTED_SYMBOLS: usize = 40;
const SAMPLE_VALUES: usize = 5;

type Storage = std::sync::Arc<dyn apex_core::ports::storage::StoragePort>;

fn timeframe_or_err(timeframe: &str) -> Result<(&'static str, apex_core::domain::models::Timeframe), String> {
    let name = ml_features::canonical_timeframe(timeframe)
        .ok_or_else(|| format!("unsupported timeframe `{timeframe}` (use 1m, 5m, 15m, 1h, 4h, 1d or 1w)"))?;
    let tf = ml_features::parse_timeframe(name).ok_or_else(|| format!("unsupported timeframe `{timeframe}`"))?;
    Ok((name, tf))
}

/// Validate a formula and show its last few values on the stored bars of `symbol`. A formula
/// that parses is `ok` even when `symbol` has no stored bars (the sample is then empty). It is
/// not `ok` when an `other(...)` symbol it reads has no stored bars, because it could not be used.
pub(crate) async fn check_expression_core(
    storage: &Storage,
    expr: &str,
    symbol: &str,
    timeframe: &str,
) -> Result<ExpressionCheckDto, String> {
    let (_, tf) = timeframe_or_err(timeframe)?;
    let compiled = match ml_expr::compile(expr) {
        Ok(c) => c,
        Err(e) => {
            return Ok(ExpressionCheckDto {
                ok: false,
                error: Some(e.message),
                error_pos: e.pos,
                warmup_bars: 0,
                sample: vec![],
            })
        }
    };
    let mut dto = ExpressionCheckDto {
        ok: true,
        error: None,
        error_pos: None,
        warmup_bars: compiled.warmup,
        sample: vec![],
    };
    let symbol = symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return Ok(dto);
    }
    crate::validation::validate_symbol(&symbol)?;
    let limit = (compiled.warmup + 200).clamp(200, 3000);
    let bars = super::data::stored_bars(storage, &symbol, tf.clone(), limit).await?;
    if bars.is_empty() {
        return Ok(dto);
    }
    let mut context = Context::new();
    for sym in &compiled.referenced_symbols {
        let ctx_bars = super::data::stored_bars(storage, sym, tf.clone(), limit * 2).await?;
        if ctx_bars.is_empty() {
            dto.ok = false;
            dto.error = Some(format!(
                "no stored bars for {sym}, which this formula reads with other(\"{sym}\", ...); fetch its history first"
            ));
            return Ok(dto);
        }
        context.insert(sym.clone(), ctx_bars);
    }
    match ml_expr::evaluate(&compiled, &bars, &context) {
        Ok(values) => {
            let first = bars.len().saturating_sub(SAMPLE_VALUES);
            dto.sample = bars[first..]
                .iter()
                .zip(&values[first..])
                .map(|(b, v)| ExpressionSampleDto { time: b.time.to_rfc3339(), value: v.is_finite().then_some(*v) })
                .collect();
        }
        Err(e) => {
            dto.ok = false;
            dto.error = Some(e.message);
        }
    }
    Ok(dto)
}

fn summarize(symbol: &str, timeframe: &str, bars: &[OHLCV]) -> SymbolDataDto {
    SymbolDataDto {
        symbol: symbol.to_string(),
        timeframe: timeframe.to_string(),
        bars: bars.len(),
        first_time: bars.first().map(|b| b.time.to_rfc3339()),
        last_time: bars.last().map(|b| b.time.to_rfc3339()),
    }
}

/// How many bars are stored for each symbol (zero when none), without contacting any provider.
pub(crate) async fn list_symbol_data_core(
    storage: &Storage,
    symbols: &[String],
    timeframe: &str,
) -> Result<Vec<SymbolDataDto>, String> {
    let (name, tf) = timeframe_or_err(timeframe)?;
    let mut wanted: Vec<String> = Vec::new();
    for s in symbols {
        let s = s.trim().to_uppercase();
        crate::validation::validate_symbol(&s)?;
        if !wanted.contains(&s) {
            wanted.push(s);
        }
    }
    if wanted.len() > MAX_LISTED_SYMBOLS {
        return Err(format!("too many symbols ({}); the limit is {MAX_LISTED_SYMBOLS}", wanted.len()));
    }
    let mut out = Vec::with_capacity(wanted.len());
    for s in wanted {
        let bars = super::data::stored_bars(storage, &s, tf.clone(), super::data::MAX_STORED_BARS).await?;
        out.push(summarize(&s, name, &bars));
    }
    Ok(out)
}

/// Download history for one symbol into storage and report what is stored afterwards.
pub(crate) async fn fetch_symbol_history_core(
    storage: &Storage,
    source: &std::sync::Arc<dyn apex_core::ports::market_data::MarketDataPort>,
    symbol: &str,
    timeframe: &str,
    bars: usize,
) -> Result<SymbolDataDto, String> {
    let (name, tf) = timeframe_or_err(timeframe)?;
    let symbol = symbol.trim().to_uppercase();
    crate::validation::validate_symbol(&symbol)?;
    if bars == 0 {
        return Err("bars must be at least 1".into());
    }
    let bars = bars.min(super::data::MAX_FETCH_BARS);
    super::data::fetch_history(storage, source, &symbol, tf.clone(), bars).await?;
    let stored = super::data::stored_bars(storage, &symbol, tf, super::data::MAX_STORED_BARS).await?;
    Ok(summarize(&symbol, name, &stored))
}

#[tauri::command]
pub async fn check_expression(
    expr: String,
    symbol: String,
    timeframe: String,
    app_state: State<'_, AppState>,
) -> Result<ExpressionCheckDto, String> {
    check_expression_core(&app_state.storage, &expr, &symbol, &timeframe).await
}

#[tauri::command]
pub async fn list_symbol_data(
    symbols: Vec<String>,
    timeframe: String,
    app_state: State<'_, AppState>,
) -> Result<Vec<SymbolDataDto>, String> {
    list_symbol_data_core(&app_state.storage, &symbols, &timeframe).await
}

/// Download history for a symbol so it can be used for datasets. Logged as `dataset.fetch`
/// (information only: no list needs to reload).
pub(crate) async fn fetch_symbol_history_inner(
    state: &AppState,
    symbol: &str,
    timeframe: &str,
    bars: usize,
    actor: &str,
) -> Result<SymbolDataDto, String> {
    let result = fetch_symbol_history_core(&state.storage, &state.history_source, symbol, timeframe, bars).await;
    let target = symbol.trim().to_uppercase();
    match &result {
        Ok(d) => crate::activity::record_info(
            state,
            actor,
            "dataset.fetch",
            "dataset",
            &target,
            &format!("Fetched {} history for {} ({} bars stored)", d.timeframe, d.symbol, d.bars),
            true,
            None,
        ),
        Err(error) => crate::activity::record_info(
            state,
            actor,
            "dataset.fetch",
            "dataset",
            &target,
            &format!("Fetching history for {target} failed"),
            false,
            Some(error.clone()),
        ),
    }
    result
}

#[tauri::command]
pub async fn fetch_symbol_history(
    symbol: String,
    timeframe: String,
    bars: usize,
    app_state: State<'_, AppState>,
) -> Result<SymbolDataDto, String> {
    fetch_symbol_history_inner(&app_state, &symbol, &timeframe, bars, "user").await
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
            label: LabelSpecDto { horizon_bars: 5, kind: "direction".into(), threshold_pct: 0.0, ..Default::default() },
            ..Default::default()
        }
    }

    /// `<root>/data/datasets`, the layout of the app: the ledger lives in `<root>/data/ledger`.
    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("apex-datasets-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root.join("data").join("datasets")
    }

    /// Remove the whole temp root of a [`temp`] directory.
    fn cleanup(dir: &Path) {
        let root = dir.parent().and_then(|p| p.parent()).expect("a temp datasets directory has two parents");
        // Never remove anything but a root made by `temp`.
        assert!(root.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("apex-datasets-")), "{}", root.display());
        let _ = fs::remove_dir_all(root);
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
        let inner = temp("escape");
        let dir = inner.clone();
        fs::create_dir_all(&inner).unwrap();
        let outside = inner.parent().unwrap().join("outside.csv");
        fs::write(&outside, "x").unwrap();
        assert!(delete_in(&inner, "../outside").is_err());
        assert!(preview_in(&inner, "../outside").is_err());
        assert!(read_info(&inner, "..").is_err());
        assert!(outside.exists(), "file outside the datasets dir must be untouched");
        let mut info = assemble(&normalize_request(&req("ok")).unwrap(), &[("AAPL".into(), synthetic_bars(120))], "user", "p", Utc::now())
            .unwrap()
            .1;
        info.name = "../escape".into();
        assert!(write_dataset(&inner, "x", &info).is_err());
        cleanup(&dir);
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
        let dir = LabelSpecDto { horizon_bars: 1, kind: "direction".into(), threshold_pct: 0.0, ..Default::default() };
        assert_eq!(label_for(&dir, 0.0001, 0.0), 1);
        assert_eq!(label_for(&dir, 0.0, 0.0), 0);
        assert_eq!(label_for(&dir, -0.02, 0.0), 0);
        let thr = LabelSpecDto { horizon_bars: 1, kind: "return_threshold".into(), threshold_pct: 1.0, ..Default::default() };
        assert_eq!(label_for(&thr, 0.0101, 0.0), 1);
        assert_eq!(label_for(&thr, 0.01, 0.0), 0, "exactly at the threshold is not above it");
        assert_eq!(label_for(&thr, 0.005, 0.0), 0);
        assert_eq!(label_for(&thr, -0.005, 0.0), 0);
        assert_eq!(label_for(&thr, -0.0101, 0.0), -1);
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
        r.label = LabelSpecDto { horizon_bars: 3, kind: "return_threshold".into(), threshold_pct: 0.5, ..Default::default() };
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
        cleanup(&dir);
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

    // ── Custom features, date range, fees, thresholds ───────────────

    use crate::dto::CustomFeatureDto;
    use apex_core::domain::models::Symbol;

    fn cf(items: &[(&str, &str)]) -> Vec<CustomFeatureDto> {
        items.iter().map(|(n, e)| CustomFeatureDto { name: n.to_string(), expr: e.to_string() }).collect()
    }

    fn spy(n: usize) -> Context {
        let bars: Vec<OHLCV> = synthetic_bars(n)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 4 != 3)
            .map(|(_, mut b)| {
                b.symbol = Symbol("SPY".into());
                b.close = b.close * 3.0 - 10.0;
                b
            })
            .collect();
        let mut c = Context::new();
        c.insert("SPY".into(), bars);
        c
    }

    fn csv_rows(csv: &str) -> Vec<Vec<String>> {
        csv.lines().skip(1).map(|l| l.split(',').map(String::from).collect()).collect()
    }

    #[test]
    fn custom_features_become_named_columns_after_the_catalog_ones() {
        let mut r = req("c");
        r.features = vec!["ret_1".into()];
        r.custom_features = cf(&[("mom10", "close / shift(close, 10) - 1"), ("above", "close > sma(close, 15)")]);
        let r = normalize_request(&r).unwrap();
        let bars = synthetic_bars(120);
        let (csv, info) = assemble(&r, &[("AAPL".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        assert_eq!(csv.lines().next().unwrap(), "time,symbol,ret_1,mom10,above,fwd_ret,target");
        assert_eq!(info.features, vec!["ret_1"], "info.features lists catalog ids only");
        assert_eq!(info.custom_features, r.custom_features);
        assert_eq!(all_feature_columns(&info), vec!["ret_1", "mom10", "above"]);
        // Warmup is the longest of the formulas: mom10 needs 10 bars, above needs 15.
        let rows = csv_rows(&csv);
        assert_eq!(rows.len(), 120 - 14 - 5);
        let k = 14;
        assert!(rows[0][0].starts_with("2024-01-15"), "first row is bar {k}: {}", rows[0][0]);
        let want = bars[k].close / bars[k - 10].close - 1.0;
        assert_eq!(rows[0][3].parse::<f64>().unwrap(), want);
        let above = bars[k].close > bars[k - 14..=k].iter().map(|b| b.close).sum::<f64>() / 15.0;
        assert_eq!(rows[0][4], if above { "1" } else { "0" });
    }

    #[test]
    fn a_dataset_may_use_only_custom_features() {
        let mut r = req("c");
        r.features.clear();
        r.custom_features = cf(&[("m", "ret(close, 3)")]);
        assert!(normalize_request(&r).is_ok());
        let r = normalize_request(&r).unwrap();
        let (csv, info) = assemble(&r, &[("AAPL".into(), synthetic_bars(60))], "user", "p", Utc::now()).unwrap();
        assert_eq!(csv.lines().next().unwrap(), "time,symbol,m,fwd_ret,target");
        assert_eq!(info.rows, 60 - 3 - 5);
        let mut none = req("c");
        none.features.clear();
        assert!(normalize_request(&none).unwrap_err().contains("at least one feature"));
    }

    #[test]
    fn custom_feature_requests_are_validated() {
        let mut r = req("c");
        r.custom_features = cf(&[("ret_5", "close")]);
        assert!(normalize_request(&r).unwrap_err().contains("catalog"));
        r.custom_features = cf(&[("x", "sma(close)")]);
        let e = normalize_request(&r).unwrap_err();
        assert!(e.contains("`x`") && e.contains("takes 2 arguments"), "{e}");
        r.custom_features = cf(&[("x", "other('SPY', 'close')")]);
        let e = normalize_request(&r).unwrap_err();
        assert!(e.contains("SPY") && e.contains("context symbols"), "{e}");
        r.context_symbols = vec!["spy".into()];
        let n = normalize_request(&r).unwrap();
        assert_eq!(n.context_symbols, vec!["SPY"]);
        r.fee_pct = -0.1;
        assert!(normalize_request(&r).unwrap_err().contains("fee_pct"));
        r.fee_pct = f64::NAN;
        assert!(normalize_request(&r).is_err());
        r.fee_pct = 10.5;
        assert!(normalize_request(&r).is_err());
        r.fee_pct = 0.2;
        assert_eq!(normalize_request(&r).unwrap().fee_pct, 0.2);
    }

    #[test]
    fn context_symbols_feed_formulas_and_nothing_after_the_end_date_leaks() {
        let mut r = req("c");
        r.features = vec!["ret_1".into()];
        r.custom_features = cf(&[("rel", "close / other('SPY', 'close')")]);
        r.context_symbols = vec!["SPY".into()];
        r.to = Some("2024-03-01".into()); // day 60
        let r = normalize_request(&r).unwrap();
        let bars = synthetic_bars(200);
        let ctx = spy(200);
        let (csv_a, info) = assemble_with(&r, &[("AAA".into(), bars.clone())], &ctx, "user", "p", Utc::now()).unwrap();
        assert_eq!(info.context_symbols, vec!["SPY"]);
        // Wreck the context bars after the end date: nothing may change.
        let mut wrecked = ctx.clone();
        for b in wrecked.get_mut("SPY").unwrap().iter_mut().filter(|b| b.time > bars[60].time) {
            b.close = 1e9;
        }
        let (csv_b, _) = assemble_with(&r, &[("AAA".into(), bars.clone())], &wrecked, "user", "p", Utc::now()).unwrap();
        assert_eq!(csv_a, csv_b);
        // A value from the table: forward fill picks the last known context bar.
        let rows = csv_rows(&csv_a);
        let row = rows.iter().find(|r| r[0].starts_with("2024-02-04")).unwrap(); // day 34: 34 % 4 == 2, present
        let day = 34;
        let spy_close = ctx["SPY"].iter().find(|b| b.time == bars[day].time).unwrap().close;
        assert!((row[3].parse::<f64>().unwrap() - bars[day].close / spy_close).abs() < 1e-12);
        let row = rows.iter().find(|r| r[0].starts_with("2024-02-05")).unwrap(); // day 35: 35 % 4 == 3, missing
        let filled = ctx["SPY"].iter().find(|b| b.time == bars[34].time).unwrap().close;
        assert!((row[3].parse::<f64>().unwrap() - bars[35].close / filled).abs() < 1e-12, "day 35 uses day 34's value");
    }

    #[test]
    fn missing_context_bars_fail_naming_the_symbol() {
        let mut r = req("c");
        r.features = vec!["ret_1".into()];
        r.custom_features = cf(&[("rel", "close / other('SPY', 'close')")]);
        r.context_symbols = vec!["SPY".into()];
        let r = normalize_request(&r).unwrap();
        let e = assemble(&r, &[("AAA".into(), synthetic_bars(100))], "user", "p", Utc::now()).unwrap_err();
        assert!(e.contains("AAA") && e.contains("SPY"), "{e}");
    }

    #[test]
    fn no_usable_rows_names_the_short_symbols_and_suggests_fetching() {
        let mut r = req("c");
        r.features = vec!["ret_1".into()];
        r.custom_features = cf(&[("rel", "close / other('SPY', 'close')")]);
        r.context_symbols = vec!["SPY".into()];
        let r = normalize_request(&r).unwrap();
        // The context symbol's history starts after the last main bar: every row is undefined.
        let main = synthetic_bars(60);
        let late: Vec<OHLCV> = synthetic_bars(160)
            .into_iter()
            .skip(100)
            .map(|mut b| {
                b.symbol = Symbol("SPY".into());
                b
            })
            .collect();
        let mut ctx = Context::new();
        ctx.insert("SPY".into(), late);
        let e = assemble_with(&r, &[("AAA".into(), main)], &ctx, "user", "p", Utc::now()).unwrap_err();
        assert!(e.contains("no usable rows") && e.contains("AAA (60 bars)"), "{e}");
        assert!(e.contains("SPY (60 bars, from 2024-04-10)") && e.contains("fetch_symbol_history"), "{e}");
        // Too few bars for the symbol also points at fetching.
        let e = assemble(&normalize_request(&req("c")).unwrap(), &[("MSFT".into(), synthetic_bars(10))], "user", "p", Utc::now()).unwrap_err();
        assert!(e.contains("MSFT") && e.contains("fetch_symbol_history"), "{e}");
    }

    #[test]
    fn dates_bound_the_bars_inclusively() {
        let bars = synthetic_bars(300);
        let mut r = req("c");
        r.features = vec!["ret_1".into()];
        r.label.horizon_bars = 5;
        r.from = Some("2024-02-01".into()); // day 31
        r.to = Some("2024-03-31".into()); // day 90
        let r = normalize_request(&r).unwrap();
        let (csv, info) = assemble(&r, &[("AAA".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        let rows = csv_rows(&csv);
        // Bars 31..=90; ret_1 is undefined on the first bar of the range, the last 5 have no outcome.
        assert_eq!(rows.len(), 90 - 5 - 32 + 1);
        assert!(rows.first().unwrap()[0].starts_with("2024-02-02"));
        assert!(rows.last().unwrap()[0].starts_with("2024-03-26"));
        assert!(info.first_time.unwrap().starts_with("2024-02-02") && info.last_time.unwrap().starts_with("2024-03-26"));
        // The first row's return uses the in-range previous bar only.
        assert_eq!(rows[0][2].parse::<f64>().unwrap(), bars[32].close / bars[31].close - 1.0);

        // `to` alone: the latest `bars` bars up to that day.
        let mut r2 = req("c");
        r2.features = vec!["ret_1".into()];
        r2.bars = 40;
        r2.to = Some("2024-03-31".into());
        let r2 = normalize_request(&r2).unwrap();
        let (csv2, _) = assemble(&r2, &[("AAA".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        let rows2 = csv_rows(&csv2);
        assert!(rows2.first().unwrap()[0].starts_with("2024-02-22"), "bars 51..=90: first usable is 52 (Feb 22), got {}", rows2[0][0]);
        assert!(rows2.last().unwrap()[0].starts_with("2024-03-26"));

        // `from` alone runs to the newest bar.
        let mut r3 = req("c");
        r3.features = vec!["ret_1".into()];
        r3.from = Some("2024-10-01".into());
        let r3 = normalize_request(&r3).unwrap();
        let (csv3, _) = assemble(&r3, &[("AAA".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        assert!(csv_rows(&csv3).last().unwrap()[0].starts_with("2024-10-21"), "newest usable bar is 294");
    }

    #[test]
    fn date_range_errors_are_clear() {
        let mut r = req("c");
        r.from = Some("2024-13-01".into());
        assert!(normalize_request(&r).unwrap_err().contains("`from`"));
        r.from = Some("2024-03-01".into());
        r.to = Some("2024-02-01".into());
        assert!(normalize_request(&r).unwrap_err().contains("after"));
        r.to = Some("yesterday".into());
        assert!(normalize_request(&r).unwrap_err().contains("`to`"));
        r.from = Some("  ".into());
        r.to = Some("".into());
        let n = normalize_request(&r).unwrap();
        assert!(n.from.is_none() && n.to.is_none(), "blank dates mean no bound");
        // A range with too few bars says so.
        r.from = Some("2024-01-01".into());
        r.to = Some("2024-01-10".into());
        let n = normalize_request(&r).unwrap();
        let e = assemble(&n, &[("AAA".into(), synthetic_bars(100))], "user", "p", Utc::now()).unwrap_err();
        assert!(e.contains("date range") && e.contains("2024-01-01 to 2024-01-10"), "{e}");
    }

    #[test]
    fn fees_come_off_the_forward_return_before_labelling() {
        let bars = synthetic_bars(150);
        let mut r = req("c");
        r.label.horizon_bars = 3;
        let free = normalize_request(&r).unwrap();
        r.fee_pct = 1.0;
        let paid = normalize_request(&r).unwrap();
        let (a, ia) = assemble(&free, &[("AAA".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        let (b, ib) = assemble(&paid, &[("AAA".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        assert_eq!(ib.fee_pct, 1.0);
        assert_eq!(ia.fee_pct, 0.0);
        let (ra, rb) = (csv_rows(&a), csv_rows(&b));
        assert_eq!(ra.len(), rb.len());
        let mut flipped = 0;
        for (x, y) in ra.iter().zip(&rb) {
            let (fa, fb): (f64, f64) = (x[5].parse().unwrap(), y[5].parse().unwrap());
            assert!((fa - fb - 0.01).abs() < 1e-12, "fwd_ret is net of the 1% round trip");
            assert_eq!(y[6], if fb > 0.0 { "1" } else { "0" }, "label is cut from the net return");
            if x[6] != y[6] {
                flipped += 1;
                assert_eq!((x[6].as_str(), y[6].as_str()), ("1", "0"));
            }
        }
        assert!(flipped > 0, "a 1% fee should turn some small winners into 0");
        assert!(ib.class_counts.get("1") < ia.class_counts.get("1"));
    }

    #[test]
    fn a_short_label_means_the_short_cleared_the_threshold_after_paying_the_fee() {
        let thr = LabelSpecDto { horizon_bars: 1, kind: "return_threshold".into(), threshold_pct: 0.5, ..Default::default() };
        let fee = 0.004; // 0.4% round trip
        // The stored value is gross - fee (as if long).
        let stored = |gross: f64| gross - fee;
        // A -0.15% move: the old cut (stored < -0.5%) called this a short, but the short earns
        // 0.15% - 0.4% = -0.25% after the fee.
        assert_eq!(label_for(&thr, stored(-0.0015), fee), 0);
        // A -1.5% move: the short earns 1.5% - 0.4% = 1.1%, above the 0.5% threshold.
        assert_eq!(label_for(&thr, stored(-0.015), fee), -1);
        // The boundary agrees with the short's real profit: it pays more than `down` exactly
        // when the label says -1.
        for gross in [-0.003, -0.008, -0.0089, -0.0091, -0.02] {
            let short_profit = -gross - fee;
            let expected = if short_profit > 0.005 { -1 } else { 0 };
            assert_eq!(label_for(&thr, stored(gross), fee), expected, "gross {gross}");
        }
        // Long side is unchanged: it clears the up threshold on gross - fee.
        assert_eq!(label_for(&thr, stored(0.0095), fee), 1);
        assert_eq!(label_for(&thr, stored(0.0085), fee), 0);
    }

    #[test]
    fn asymmetric_thresholds() {
        let l = LabelSpecDto {
            horizon_bars: 1,
            kind: "return_threshold".into(),
            threshold_pct: 1.0,
            up_threshold_pct: Some(2.0),
            down_threshold_pct: Some(0.5),
        };
        assert_eq!(label_for(&l, 0.015, 0.0), 0, "below the 2% up threshold");
        assert_eq!(label_for(&l, 0.0201, 0.0), 1);
        assert_eq!(label_for(&l, -0.006, 0.0), -1, "beyond the 0.5% down threshold");
        assert_eq!(label_for(&l, -0.004, 0.0), 0);
        // One side only falls back to threshold_pct on the other.
        let up_only = LabelSpecDto { up_threshold_pct: Some(3.0), down_threshold_pct: None, ..l.clone() };
        assert_eq!(thresholds(&up_only), (3.0, 1.0));
        assert_eq!(label_for(&up_only, -0.011, 0.0), -1);
        assert_eq!(label_for(&up_only, 0.025, 0.0), 0);

        let mut r = req("c");
        r.label = l.clone();
        let n = normalize_request(&r).unwrap();
        assert_eq!((n.label.up_threshold_pct, n.label.down_threshold_pct), (Some(2.0), Some(0.5)));
        // Both given and threshold_pct left at 0 is fine.
        r.label.threshold_pct = 0.0;
        let n = normalize_request(&r).unwrap();
        assert_eq!(n.label.threshold_pct, 2.0);
        // Symmetric requests keep the old shape.
        r.label = LabelSpecDto { horizon_bars: 2, kind: "return_threshold".into(), threshold_pct: 1.0, ..Default::default() };
        let n = normalize_request(&r).unwrap();
        assert!(n.label.up_threshold_pct.is_none() && n.label.down_threshold_pct.is_none());
        let v = serde_json::to_value(&n.label).unwrap();
        assert!(v.get("up_threshold_pct").is_none(), "old sidecars stay byte-compatible: {v}");
        // Invalid values.
        for bad in [(Some(0.0), Some(1.0)), (Some(1.0), Some(-1.0)), (Some(101.0), None), (Some(f64::NAN), None)] {
            r.label = LabelSpecDto {
                horizon_bars: 2, kind: "return_threshold".into(), threshold_pct: 1.0,
                up_threshold_pct: bad.0, down_threshold_pct: bad.1,
            };
            assert!(normalize_request(&r).is_err(), "{bad:?}");
        }
        // Direction labels ignore thresholds.
        r.label = LabelSpecDto { horizon_bars: 2, kind: "direction".into(), threshold_pct: 5.0, up_threshold_pct: Some(1.0), down_threshold_pct: Some(1.0) };
        let n = normalize_request(&r).unwrap();
        assert!(n.label.up_threshold_pct.is_none() && n.label.threshold_pct == 0.0);

        // On data: a tight down threshold makes more -1 rows than the symmetric 2%.
        let bars = synthetic_bars(300);
        let sym = LabelSpecDto { horizon_bars: 3, kind: "return_threshold".into(), threshold_pct: 2.0, ..Default::default() };
        let asym = LabelSpecDto { up_threshold_pct: Some(2.0), down_threshold_pct: Some(0.2), ..sym.clone() };
        let mut rs = req("c");
        rs.label = sym;
        let (_, a) = assemble(&normalize_request(&rs).unwrap(), &[("A".into(), bars.clone())], "user", "p", Utc::now()).unwrap();
        rs.label = asym;
        let (_, b) = assemble(&normalize_request(&rs).unwrap(), &[("A".into(), bars)], "user", "p", Utc::now()).unwrap();
        assert!(b.class_counts.get("-1") > a.class_counts.get("-1"), "{:?} vs {:?}", a.class_counts, b.class_counts);
        assert_eq!(a.class_counts.get("1"), b.class_counts.get("1"), "up side unchanged");
    }

    #[test]
    fn parity_custom_features_dataset_path_equals_inference_path() {
        let custom = cf(&[
            ("mom", "ret(close, 4)"),
            ("rel", "ret(close, 3) - ret(other('SPY', 'close'), 3)"),
            ("vol_z", "zscore(volume, 30)"),
            ("trend", "close > ema(close, 12)"),
        ]);
        let mut r = req("p");
        r.features = vec!["rsi_14".into(), "ret_5".into()];
        r.custom_features = custom.clone();
        r.context_symbols = vec!["SPY".into()];
        r.label.horizon_bars = 1;
        let r = normalize_request(&r).unwrap();
        let bars = synthetic_bars(260);
        // The context runs past the last main bar (as it does in storage at scoring time): the
        // extra bars must not matter.
        let ctx = spy(300);
        let (csv, info) = assemble_with(&r, &[("AAA".into(), bars.clone())], &ctx, "user", "p", Utc::now()).unwrap();
        let cols = all_feature_columns(&info);
        assert_eq!(cols, vec!["rsi_14", "ret_5", "mom", "rel", "vol_z", "trend"]);
        let rows = csv_rows(&csv);
        let last = rows.last().unwrap();
        // The dataset's last row is bar 258 (bar 259 has no 1-bar outcome): score the same prefix.
        assert!(last[0].starts_with(&bars[258].time.format("%Y-%m-%d").to_string()));
        let inference = ml_features::latest_features_with(&bars[..259], &cols, &custom, &ctx, "AAA").unwrap();
        for (j, name) in cols.iter().enumerate() {
            let from_dataset: f64 = last[2 + j].parse().unwrap();
            assert_eq!(inference[name], from_dataset, "{name}");
        }
        // Every row, not just the last.
        let matrix = ml_features::model_matrix_with(&bars, &cols, &custom, &ctx).unwrap();
        for row in &rows {
            let day = bars.iter().position(|b| row[0].starts_with(&b.time.format("%Y-%m-%d").to_string())).unwrap();
            for (j, _) in cols.iter().enumerate() {
                assert_eq!(matrix[day][j], row[2 + j].parse::<f64>().unwrap(), "col {j} bar {day}");
            }
        }
    }

    #[test]
    fn dataset_info_round_trips_the_new_fields_and_reads_old_sidecars() {
        let dir = temp("sidecar");
        let mut r = req("rt");
        r.features = vec!["ret_1".into()];
        r.custom_features = cf(&[("m", "ret(close, 2)")]);
        r.fee_pct = 0.1;
        let r = normalize_request(&r).unwrap();
        let (csv, info) = assemble(&r, &[("AAA".into(), synthetic_bars(80))], "user", "data/datasets/rt.csv", Utc::now()).unwrap();
        write_dataset(&dir, &csv, &info).unwrap();
        let back = read_info(&dir, "rt").unwrap();
        assert_eq!(back.custom_features, info.custom_features);
        assert_eq!(back.fee_pct, 0.1);
        let p = preview_in(&dir, "rt").unwrap();
        assert_eq!(p.columns, vec!["time", "symbol", "ret_1", "m", "fwd_ret", "target"]);
        // A sidecar written before this round has none of the new keys.
        let old = serde_json::json!({
            "name": "old", "path": "data/datasets/old.csv", "rows": 3, "symbols": ["A"], "timeframe": "1d",
            "features": ["ret_1"], "label": {"horizon_bars": 1, "kind": "direction", "threshold_pct": 0.0},
            "class_counts": {"0": 1, "1": 2}, "created_at": "2025-01-01T00:00:00Z", "first_time": null,
            "last_time": null, "source": "user"
        });
        fs::write(legacy_sidecar_path(&dir, "old"), old.to_string()).unwrap();
        let o = read_info(&dir, "old").unwrap();
        assert!(o.custom_features.is_empty() && o.context_symbols.is_empty() && o.fee_pct == 0.0);
        assert!(o.label.up_threshold_pct.is_none());
        cleanup(&dir);
    }

    #[test]
    fn requests_from_older_callers_still_deserialize() {
        let json = serde_json::json!({
            "name": "a", "symbols": ["SPY"], "timeframe": "1d", "bars": 100, "features": ["ret_1"],
            "label": {"horizon_bars": 5, "kind": "direction"}
        });
        let r: BuildDatasetRequestDto = serde_json::from_value(json).unwrap();
        assert!(r.custom_features.is_empty() && r.from.is_none() && r.fee_pct == 0.0);
        assert!(normalize_request(&r).is_ok());
        let full = serde_json::json!({
            "name": "a", "symbols": ["SPY"], "timeframe": "1d", "bars": 100, "features": [],
            "label": {"horizon_bars": 5, "kind": "return_threshold", "threshold_pct": 1, "up_threshold_pct": null, "down_threshold_pct": 0.5},
            "from": "2024-01-01", "to": null,
            "custom_features": [{"name": "m", "expr": "ret(close)"}], "context_symbols": [], "fee_pct": 0.05
        });
        let r: BuildDatasetRequestDto = serde_json::from_value(full).unwrap();
        assert_eq!(r.label.down_threshold_pct, Some(0.5));
        assert_eq!(r.custom_features[0].name, "m");
    }

    #[test]
    fn load_window_covers_the_requested_dates() {
        let now = Utc::now();
        let mut r = req("c");
        r.features = vec!["ret_1".into()];
        r.bars = 300;
        assert_eq!(load_limit(&normalize_request(&r).unwrap(), now).unwrap(), 300, "no dates: the latest `bars`");
        r.from = Some((now - chrono::Duration::days(1000)).format("%Y-%m-%d").to_string());
        let n = load_limit(&normalize_request(&r).unwrap(), now).unwrap();
        assert!((1000..=1020).contains(&n), "1000 daily bars back, got {n}");
        r.from = Some("2000-01-01".into());
        r.timeframe = "1m".into();
        assert_eq!(load_limit(&normalize_request(&r).unwrap(), now).unwrap(), MAX_BARS, "clamped");
        let mut only_to = req("c");
        only_to.bars = 100;
        only_to.to = Some((now - chrono::Duration::days(500)).format("%Y-%m-%d").to_string());
        let n = load_limit(&normalize_request(&only_to).unwrap(), now).unwrap();
        assert!((600..=620).contains(&n), "100 bars ending 500 days ago needs ~600 loaded, got {n}");
    }

    // ── Formula check, stored data, history ─────────────────────────

    async fn storage() -> Storage {
        let s = apex_adapters::storage::sqlite_storage::SqliteStorage::new(":memory:").unwrap();
        s.init_schema().await.unwrap();
        std::sync::Arc::new(s)
    }

    async fn store(st: &Storage, symbol: &str, bars: Vec<OHLCV>) {
        let relabelled: Vec<OHLCV> = bars
            .into_iter()
            .map(|mut b| {
                b.symbol = Symbol(symbol.into());
                b
            })
            .collect();
        st.write_ohlcv(&relabelled).await.unwrap();
    }

    #[tokio::test]
    async fn check_expression_reports_syntax_errors_with_positions() {
        let st = storage().await;
        let c = check_expression_core(&st, "sma(close, 0)", "AAA", "1d").await.unwrap();
        assert!(!c.ok && c.error_pos == Some(11) && c.sample.is_empty());
        assert!(c.error.unwrap().contains("between 1 and 500"));
        let c = check_expression_core(&st, "close +", "", "1d").await.unwrap();
        assert!(!c.ok && c.error_pos == Some(7));
        assert!(check_expression_core(&st, "close", "AAA", "7d").await.is_err(), "bad timeframe is a command error");
    }

    #[tokio::test]
    async fn check_expression_without_stored_bars_still_validates() {
        let st = storage().await;
        let c = check_expression_core(&st, "sma(close, 20) / close - 1", "NOBARS", "1d").await.unwrap();
        assert!(c.ok && c.error.is_none() && c.error_pos.is_none());
        assert_eq!(c.warmup_bars, 19);
        assert!(c.sample.is_empty());
        let c = check_expression_core(&st, "close", "", "1d").await.unwrap();
        assert!(c.ok && c.sample.is_empty());
    }

    #[tokio::test]
    async fn check_expression_samples_the_last_five_values() {
        let st = storage().await;
        let bars = synthetic_bars(40);
        store(&st, "AAA", bars.clone()).await;
        let c = check_expression_core(&st, "sma(close, 3)", " aaa ", "D1").await.unwrap();
        assert!(c.ok);
        assert_eq!(c.sample.len(), 5);
        for (k, s) in c.sample.iter().enumerate() {
            let i = 35 + k;
            let want = (bars[i].close + bars[i - 1].close + bars[i - 2].close) / 3.0;
            assert!((s.value.unwrap() - want).abs() < 1e-9);
            assert_eq!(s.time, bars[i].time.to_rfc3339());
        }
        // A formula that is still warming up on those bars gives nulls.
        let c = check_expression_core(&st, "sma(close, 100)", "AAA", "1d").await.unwrap();
        assert!(c.ok && c.warmup_bars == 99);
        assert!(c.sample.len() == 5 && c.sample.iter().all(|s| s.value.is_none()));
        // Division by zero is null, not an error.
        let c = check_expression_core(&st, "close / (close - close)", "AAA", "1d").await.unwrap();
        assert!(c.ok && c.sample.iter().all(|s| s.value.is_none()));
    }

    #[tokio::test]
    async fn check_expression_with_context_symbols() {
        let st = storage().await;
        store(&st, "AAA", synthetic_bars(50)).await;
        let f = "close / other(\"SPY\", \"close\")";
        let c = check_expression_core(&st, f, "AAA", "1d").await.unwrap();
        assert!(!c.ok && c.error.as_deref().unwrap().contains("SPY") && c.error.unwrap().contains("fetch"));
        store(&st, "SPY", synthetic_bars(50)).await;
        let c = check_expression_core(&st, f, "AAA", "1d").await.unwrap();
        assert!(c.ok && c.sample.iter().all(|s| (s.value.unwrap() - 1.0).abs() < 1e-12));
        // With no bars for the main symbol the formula is only checked for syntax.
        let c = check_expression_core(&st, f, "NOBARS", "1d").await.unwrap();
        assert!(c.ok && c.sample.is_empty());
    }

    #[tokio::test]
    async fn list_symbol_data_counts_stored_bars() {
        let st = storage().await;
        store(&st, "AAA", synthetic_bars(30)).await;
        let got = list_symbol_data_core(&st, &["aaa".into(), "NONE".into(), "AAA".into()], "1d").await.unwrap();
        assert_eq!(got.len(), 2, "duplicates collapse");
        assert_eq!((got[0].symbol.as_str(), got[0].timeframe.as_str(), got[0].bars), ("AAA", "1d", 30));
        assert!(got[0].first_time.as_deref().unwrap().starts_with("2024-01-01"));
        assert!(got[0].last_time.as_deref().unwrap().starts_with("2024-01-30"));
        assert_eq!((got[1].bars, got[1].first_time.clone(), got[1].last_time.clone()), (0, None, None));
        // Other timeframes of the same symbol are separate.
        assert_eq!(list_symbol_data_core(&st, &["AAA".into()], "1h").await.unwrap()[0].bars, 0);
        assert!(list_symbol_data_core(&st, &[], "1d").await.unwrap().is_empty());
        let many: Vec<String> = (0..=MAX_LISTED_SYMBOLS).map(|i| format!("S{i}")).collect();
        assert!(list_symbol_data_core(&st, &many, "1d").await.unwrap_err().contains("too many"));
        assert!(list_symbol_data_core(&st, &["bad symbol!".into()], "1d").await.is_err());
        assert!(list_symbol_data_core(&st, &["AAA".into()], "2d").await.is_err());
        assert_eq!(list_symbol_data_core(&st, &many[..MAX_LISTED_SYMBOLS], "1d").await.unwrap().len(), MAX_LISTED_SYMBOLS);
    }

    #[tokio::test]
    async fn stored_history_is_trimmed_relabelled_and_visible_to_the_loader_queries() {
        use super::super::data::{store_history, stored_bars};
        let st = storage().await;
        let tf = apex_core::domain::models::Timeframe::D1;
        let mut fetched = synthetic_bars(100);
        for b in &mut fetched {
            b.symbol = Symbol("some-other-name".into()); // providers may spell it differently
        }
        fetched[10].close = f64::NAN; // unusable bars are dropped
        fetched[11].close = 0.0;
        let kept = store_history(&st, "AAA", &tf, 50, fetched.clone()).await.unwrap();
        assert_eq!(kept, 50, "only the newest `bars`");
        let back = stored_bars(&st, "AAA", tf.clone(), 1000).await.unwrap();
        assert_eq!(back.len(), 50);
        assert_eq!(back.last().unwrap().time, fetched[99].time);
        assert!(back.iter().all(|b| b.symbol.0 == "AAA"));
        // Fetching again overlaps without duplicating.
        store_history(&st, "AAA", &tf, 100, fetched.clone()).await.unwrap();
        assert_eq!(stored_bars(&st, "AAA", tf.clone(), 1000).await.unwrap().len(), 98, "100 minus the 2 unusable bars");
        // Nothing usable is an error the user can act on.
        let e = store_history(&st, "ZZZ", &tf, 10, vec![]).await.unwrap_err();
        assert!(e.contains("ZZZ") && e.contains("no 1d history"), "{e}");
        let mut bad = synthetic_bars(5);
        for b in &mut bad {
            b.close = -1.0;
        }
        assert!(store_history(&st, "ZZZ", &tf, 10, bad).await.is_err());
    }

    #[test]
    fn history_window_is_wider_than_the_bar_count_and_capped() {
        use super::super::data::history_window;
        use apex_core::domain::models::Timeframe as Tf;
        let d = history_window(&Tf::D1, 250).num_days();
        assert!((350..=420).contains(&d), "250 daily bars need about a year and a half of calendar, got {d}");
        assert_eq!(history_window(&Tf::W1, 100).num_days(), 714);
        let h = history_window(&Tf::H1, 650).num_days();
        assert!((140..=160).contains(&h), "650 hourly bars at 6.5 per day is 100 sessions, got {h}");
        assert_eq!(history_window(&Tf::D1, 5000).num_days(), 7300, "capped at 20 years");
        assert!(history_window(&Tf::M1, 1).num_days() >= 1);
    }

    // ── Versioned datasets in the audit ledger ───────────────────────

    fn build(dir: &Path, name: &str, bars: usize) -> DatasetInfoDto {
        let r = normalize_request(&req(name)).unwrap();
        let (csv, info) =
            assemble(&r, &[("AAPL".into(), synthetic_bars(bars))], "user", &format!("data/datasets/{name}.csv"), Utc::now()).unwrap();
        write_dataset(dir, &csv, &info).unwrap()
    }

    fn the_ledger(dir: &Path) -> Ledger {
        ledger_for_dir(dir).unwrap()
    }

    #[test]
    fn two_builds_under_one_name_are_v1_and_v2_and_the_first_is_untouched() {
        let dir = temp("versions");
        let v1 = build(&dir, "spy", 100);
        let v1_csv = dir.join("spy").join("v1.csv");
        let v1_bytes = fs::read(&v1_csv).unwrap();
        let v1_hash = Ledger::sha256_hex(&v1_bytes);
        assert_eq!((v1.version, v1.csv_sha256.as_str(), v1.csv_bytes), (1, v1_hash.as_str(), v1_bytes.len() as u64));
        assert_eq!(v1.path, "data/datasets/spy/v1.csv");

        let v2 = build(&dir, "spy", 140);
        assert_eq!(v2.version, 2);
        assert_eq!(v2.path, "data/datasets/spy/v2.csv");
        assert_ne!(v2.csv_sha256, v1_hash);
        // v1 is exactly what it was: bytes, hash, read-only.
        assert_eq!(fs::read(&v1_csv).unwrap(), v1_bytes);
        assert_eq!(hash_file(&v1_csv).unwrap().0, v1_hash);
        assert!(fs::metadata(&v1_csv).unwrap().permissions().readonly());
        assert!(fs::metadata(dir.join("spy").join("v2.csv")).unwrap().permissions().readonly());
        assert!(dir.join("spy").join("v1.dataset.json").is_file() && dir.join("spy").join("v2.dataset.json").is_file());

        // One ledger object, two versions, each holding its own sidecar with its own hash.
        let ledger = the_ledger(&dir);
        let history = ledger.history(Kind::Dataset, "spy");
        assert_eq!(history.iter().map(|r| (r.version, r.action.as_str())).collect::<Vec<_>>(), vec![(1, "create"), (2, "update")]);
        let stored: DatasetInfoDto = serde_json::from_slice(&ledger.content(Kind::Dataset, "spy", 1).unwrap()).unwrap();
        assert_eq!((stored.version, stored.csv_sha256), (1, v1_hash.clone()));

        // The list shows the latest; older versions are still readable by number.
        let listed = list_datasets_in(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].version, listed[0].rows), (2, v2.rows));
        assert_eq!(read_info_version(&dir, "spy", Some(1)).unwrap().csv_sha256, v1_hash);
        assert!(read_info_version(&dir, "spy", Some(9)).is_err());
        assert_eq!(preview_in(&dir, "spy").unwrap().info.version, 2);
        assert!(ledger.verify().ok);
        cleanup(&dir);
    }

    #[test]
    fn a_delete_is_a_tombstone_the_files_stay_and_a_rebuild_continues_the_same_object() {
        let dir = temp("tombstone");
        build(&dir, "gone", 100);
        assert!(delete_in(&dir, "gone").unwrap());
        assert!(!delete_in(&dir, "gone").unwrap(), "already deleted");
        assert!(list_datasets_in(&dir).is_empty());
        assert!(read_info(&dir, "gone").unwrap_err().contains("unknown dataset"));
        assert!(preview_in(&dir, "gone").is_err());
        assert!(dir.join("gone").join("v1.csv").is_file(), "the data is not removed");
        let ledger = the_ledger(&dir);
        let history = ledger.history(Kind::Dataset, "gone");
        assert_eq!(history.last().unwrap().action, "delete");
        assert!(ledger.objects(Kind::Dataset, true).iter().any(|o| o.object_id == "gone" && o.deleted));
        assert!(ledger.content(Kind::Dataset, "gone", 1).is_ok(), "history stays readable");

        let again = build(&dir, "gone", 120);
        assert_eq!(again.version, 3, "the version number continues after the tombstone");
        assert!(dir.join("gone").join("v3.csv").is_file() && dir.join("gone").join("v1.csv").is_file());
        assert_eq!(list_datasets_in(&dir)[0].version, 3);
        assert_eq!(ledger.history(Kind::Dataset, "gone").len(), 3);
        cleanup(&dir);
    }

    #[test]
    fn a_dataset_from_before_versioning_becomes_version_1_in_place_when_listed() {
        let dir = temp("legacy");
        fs::create_dir_all(&dir).unwrap();
        let r = normalize_request(&req("old")).unwrap();
        let (csv, info) = assemble(&r, &[("AAPL".into(), synthetic_bars(100))], "user", "data/datasets/old.csv", Utc::now()).unwrap();
        fs::write(legacy_csv_path(&dir, "old"), &csv).unwrap();
        let side = serde_json::to_string_pretty(&info).unwrap();
        fs::write(legacy_sidecar_path(&dir, "old"), &side).unwrap();

        let listed = list_datasets_in(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, 1);
        assert_eq!(listed[0].csv_sha256, Ledger::sha256_hex(csv.as_bytes()));
        assert_eq!(listed[0].path, "data/datasets/old.csv", "the file stays where it is");
        assert_eq!(fs::read_to_string(legacy_csv_path(&dir, "old")).unwrap(), csv, "files are not rewritten");
        assert_eq!(fs::read_to_string(legacy_sidecar_path(&dir, "old")).unwrap(), side);
        let ledger = the_ledger(&dir);
        let history = ledger.history(Kind::Dataset, "old");
        assert_eq!(history.len(), 1);
        assert_eq!((history[0].action.as_str(), history[0].actor.as_str()), ("create", "system"));
        // Listing again adds nothing.
        list_datasets_in(&dir);
        assert_eq!(ledger.history(Kind::Dataset, "old").len(), 1);
        // The preview reads the old location.
        assert_eq!(preview_in(&dir, "old").unwrap().sample.len(), PREVIEW_ROWS);

        // A rebuild is version 2 in the new layout; the old file is still there, unchanged.
        let v2 = build(&dir, "old", 130);
        assert_eq!(v2.version, 2);
        assert!(dir.join("old").join("v2.csv").is_file());
        assert_eq!(fs::read_to_string(legacy_csv_path(&dir, "old")).unwrap(), csv);
        assert_eq!(read_info_version(&dir, "old", Some(1)).unwrap().path, "data/datasets/old.csv");
        cleanup(&dir);
    }

    #[test]
    fn a_legacy_dataset_can_be_deleted_without_listing_first() {
        let dir = temp("legacy-delete");
        fs::create_dir_all(&dir).unwrap();
        let r = normalize_request(&req("old")).unwrap();
        let (csv, info) = assemble(&r, &[("AAPL".into(), synthetic_bars(100))], "user", "data/datasets/old.csv", Utc::now()).unwrap();
        fs::write(legacy_csv_path(&dir, "old"), &csv).unwrap();
        fs::write(legacy_sidecar_path(&dir, "old"), serde_json::to_string(&info).unwrap()).unwrap();
        assert!(delete_in(&dir, "old").unwrap());
        assert!(list_datasets_in(&dir).is_empty(), "a deleted legacy dataset is not re-registered by listing");
        assert!(legacy_csv_path(&dir, "old").is_file());
        let history = the_ledger(&dir).history(Kind::Dataset, "old");
        assert_eq!(history.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(), vec!["create", "delete"]);
        cleanup(&dir);
    }

    #[test]
    fn leftovers_of_an_interrupted_build_are_set_aside_never_overwritten() {
        let dir = temp("orphan");
        fs::create_dir_all(dir.join("half")).unwrap();
        fs::write(dir.join("half").join("v1.csv"), "left by a crash").unwrap();
        let info = build(&dir, "half", 100);
        assert_eq!(info.version, 1);
        let aside: Vec<_> = fs::read_dir(dir.join("half"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("v1.csv.orphan-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(fs::read_to_string(aside[0].path()).unwrap(), "left by a crash");
        assert_eq!(hash_file(&dir.join("half").join("v1.csv")).unwrap().0, info.csv_sha256);
        cleanup(&dir);
    }

    #[test]
    fn concurrent_builds_of_one_name_get_distinct_versions() {
        let dir = temp("race");
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || build(&dir, "race", 90 + i * 7).version)
            })
            .collect();
        let mut versions: Vec<u32> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        versions.sort();
        assert_eq!(versions, vec![1, 2, 3, 4, 5, 6]);
        assert!(the_ledger(&dir).verify().ok);
        cleanup(&dir);
    }
}
