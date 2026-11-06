//! The one feature pipeline shared by the dataset builder, the trainer's inputs and inference.
//!
//! Training and scoring must see identical numbers, so every consumer goes through
//! [`model_matrix`] (or [`compute_matrix`] for catalog ids only). A value at bar `i` depends only
//! on bars `0..=i`: nothing here looks at later bars.
//!
//! Feature names a model may ask for:
//! * a catalog id (`ret_1`, `rsi_14`, ...),
//! * a lag of one: `<name>_lag<n>` is the value of `<name>` `n` bars earlier,
//! * a legacy column from models trained before the catalog existed (`sma_20`, `bb_upper`,
//!   `volume_lag_1`, `close`, ...). Their old definitions are kept so those models still score,
//! * a custom feature: a formula from [`super::ml_expr`] with a name of the user's choosing. It
//!   also accepts the `_lag<n>` suffix. Custom features and the context symbols they read are
//!   stored with the dataset and the model, so scoring computes them the same way.

use super::ml_expr::{self, Compiled, Context};
use crate::dto::{CustomFeatureDto, FeatureDefDto};
use apex_core::application::indicators as ind;
use apex_core::domain::models::{Timeframe, OHLCV};
use std::collections::HashMap;

struct Spec {
    id: &'static str,
    label: &'static str,
    group: &'static str,
    description: &'static str,
    /// Leading bars whose value is NaN.
    warmup: usize,
}

const CATALOG: &[Spec] = &[
    Spec { id: "ret_1", label: "1-bar return", group: "Price", description: "Close-to-close return over 1 bar.", warmup: 1 },
    Spec { id: "ret_5", label: "5-bar return", group: "Price", description: "Return over 5 bars.", warmup: 5 },
    Spec { id: "ret_10", label: "10-bar return", group: "Price", description: "Return over 10 bars.", warmup: 10 },
    Spec { id: "range_pct", label: "Bar range %", group: "Price", description: "(high - low) / close.", warmup: 0 },
    Spec { id: "sma_ratio_20", label: "Close / SMA 20", group: "Trend", description: "Close divided by its 20-bar average.", warmup: 19 },
    Spec { id: "sma_ratio_50", label: "Close / SMA 50", group: "Trend", description: "Close divided by its 50-bar average.", warmup: 49 },
    Spec { id: "ema_ratio_12", label: "Close / EMA 12", group: "Trend", description: "Close divided by its 12-bar EMA.", warmup: 11 },
    Spec { id: "macd_hist", label: "MACD histogram", group: "Momentum", description: "MACD line minus signal line, scaled by close.", warmup: 33 },
    Spec { id: "rsi_14", label: "RSI 14", group: "Momentum", description: "Relative strength index, 0-100.", warmup: 14 },
    Spec { id: "roc_10", label: "Rate of change 10", group: "Momentum", description: "Percent change over 10 bars.", warmup: 10 },
    Spec { id: "vol_10", label: "Volatility 10", group: "Volatility", description: "Std-dev of 1-bar returns over 10 bars.", warmup: 10 },
    Spec { id: "vol_20", label: "Volatility 20", group: "Volatility", description: "Std-dev of 1-bar returns over 20 bars.", warmup: 20 },
    Spec { id: "atr_pct_14", label: "ATR 14 %", group: "Volatility", description: "Average true range as a share of close.", warmup: 14 },
    Spec { id: "bb_pos_20", label: "Bollinger position", group: "Volatility", description: "Where close sits between the 20-bar bands (0-1).", warmup: 19 },
    Spec { id: "vol_ratio_20", label: "Volume / avg 20", group: "Volume", description: "Volume divided by its 20-bar average.", warmup: 19 },
];

/// The feature catalog, in display order.
pub fn catalog() -> Vec<FeatureDefDto> {
    CATALOG
        .iter()
        .map(|s| FeatureDefDto {
            id: s.id.into(),
            label: s.label.into(),
            group: s.group.into(),
            description: s.description.into(),
            warmup_bars: s.warmup,
        })
        .collect()
}

fn spec(id: &str) -> Option<&'static Spec> {
    CATALOG.iter().find(|s| s.id == id)
}

pub fn is_catalog_feature(id: &str) -> bool {
    spec(id).is_some()
}

/// Leading bars that have no value for `id` (None for ids outside the catalog).
pub fn warmup_of(id: &str) -> Option<usize> {
    spec(id).map(|s| s.warmup)
}

/// List feature command.
#[tauri::command]
pub async fn list_feature_catalog() -> Result<Vec<FeatureDefDto>, String> {
    Ok(catalog())
}

// ── Timeframes ───────────────────────────────────────────────────────

/// Canonical lowercase spelling of a timeframe, or None when it is not one the app supports.
pub fn canonical_timeframe(tf: &str) -> Option<&'static str> {
    match tf.trim().to_lowercase().as_str() {
        "1m" | "m1" => Some("1m"),
        "5m" | "m5" => Some("5m"),
        "15m" | "m15" => Some("15m"),
        "1h" | "h1" => Some("1h"),
        "4h" | "h4" => Some("4h"),
        "1d" | "d1" => Some("1d"),
        "1w" | "w1" => Some("1w"),
        _ => None,
    }
}

pub fn parse_timeframe(tf: &str) -> Option<Timeframe> {
    Some(match canonical_timeframe(tf)? {
        "1m" => Timeframe::M1,
        "5m" => Timeframe::M5,
        "15m" => Timeframe::M15,
        "1h" => Timeframe::H1,
        "4h" => Timeframe::H4,
        "1d" => Timeframe::D1,
        _ => Timeframe::W1,
    })
}

// ── Column computation ───────────────────────────────────────────────

struct Series {
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
}

impl Series {
    fn from_bars(bars: &[OHLCV]) -> Result<Self, String> {
        for (i, b) in bars.iter().enumerate() {
            let ok = [b.open, b.high, b.low, b.close].iter().all(|v| v.is_finite()) && b.close > 0.0;
            if !ok {
                return Err(format!(
                    "bad price data at bar {i} ({}): open/high/low/close must be finite and close must be > 0",
                    b.time.format("%Y-%m-%d %H:%M")
                ));
            }
        }
        Ok(Self {
            open: bars.iter().map(|b| b.open).collect(),
            high: bars.iter().map(|b| b.high).collect(),
            low: bars.iter().map(|b| b.low).collect(),
            close: bars.iter().map(|b| b.close).collect(),
            volume: bars.iter().map(|b| b.volume as f64).collect(),
        })
    }

    fn len(&self) -> usize {
        self.close.len()
    }
}

/// Place `vals` so `vals[0]` lands on bar `offset`; every other bar is NaN.
fn place(n: usize, offset: usize, vals: &[f64]) -> Vec<f64> {
    let mut out = vec![f64::NAN; n];
    for (k, v) in vals.iter().enumerate() {
        if offset + k < n {
            out[offset + k] = *v;
        }
    }
    out
}

fn ratio(a: f64, b: f64) -> f64 {
    if b == 0.0 || !a.is_finite() || !b.is_finite() {
        f64::NAN
    } else {
        a / b
    }
}

fn returns_over(close: &[f64], n: usize) -> Vec<f64> {
    (0..close.len())
        .map(|i| if i >= n { ratio(close[i], close[i - n]) - 1.0 } else { f64::NAN })
        .collect()
}

fn one_bar_volatility(close: &[f64], period: usize) -> Vec<f64> {
    let r1 = returns_over(close, 1);
    if r1.len() < 2 {
        return vec![f64::NAN; close.len()];
    }
    // std_dev output k covers returns r1[1+k ..= k+period] = bars up to k+period.
    let sd = ind::std_dev(&r1[1..], period).unwrap_or_default();
    place(close.len(), period, &sd)
}

fn catalog_column(id: &str, s: &Series) -> Option<Vec<f64>> {
    let n = s.len();
    let c = &s.close;
    Some(match id {
        "ret_1" => returns_over(c, 1),
        "ret_5" => returns_over(c, 5),
        "ret_10" => returns_over(c, 10),
        "range_pct" => (0..n).map(|i| ratio(s.high[i] - s.low[i], c[i])).collect(),
        "sma_ratio_20" | "sma_ratio_50" => {
            let p = if id == "sma_ratio_20" { 20 } else { 50 };
            let m = place(n, p - 1, &ind::sma(c, p).unwrap_or_default());
            (0..n).map(|i| ratio(c[i], m[i])).collect()
        }
        "ema_ratio_12" => {
            let m = place(n, 11, &ind::ema(c, 12).unwrap_or_default());
            (0..n).map(|i| ratio(c[i], m[i])).collect()
        }
        "macd_hist" => {
            let h = ind::macd(c, 12, 26, 9).map(|r| r.histogram).unwrap_or_default();
            let h = place(n, 33, &h);
            (0..n).map(|i| ratio(h[i], c[i])).collect()
        }
        "rsi_14" => place(n, 14, &ind::rsi(c, 14).unwrap_or_default()),
        "roc_10" => place(n, 10, &ind::roc(c, 10).unwrap_or_default()),
        "vol_10" => one_bar_volatility(c, 10),
        "vol_20" => one_bar_volatility(c, 20),
        "atr_pct_14" => {
            let a = place(n, 14, &ind::atr(&s.high, &s.low, c, 14).unwrap_or_default());
            (0..n).map(|i| ratio(a[i], c[i])).collect()
        }
        "bb_pos_20" => {
            let bb = ind::bollinger_bands(c, 20, 2.0).ok()?;
            let up = place(n, 19, &bb.upper);
            let lo = place(n, 19, &bb.lower);
            (0..n)
                .map(|i| {
                    let width = up[i] - lo[i];
                    if width.is_nan() {
                        f64::NAN
                    } else if width.abs() < 1e-12 {
                        0.5
                    } else {
                        (c[i] - lo[i]) / width
                    }
                })
                .collect()
        }
        "vol_ratio_20" => {
            let avg = place(n, 19, &ind::sma(&s.volume, 20).unwrap_or_default());
            (0..n)
                .map(|i| {
                    if avg[i].is_nan() {
                        f64::NAN
                    } else if avg[i] == 0.0 {
                        // No volume information at all (index, FX): neutral rather than undefined.
                        1.0
                    } else {
                        s.volume[i] / avg[i]
                    }
                })
                .collect()
        }
        _ => return None,
    })
}

/// Warmup (leading NaN bars) of a legacy column.
fn legacy_warmup(name: &str) -> Option<usize> {
    Some(match name {
        "close" | "open" | "high" | "low" | "volume" => 0,
        "volume_lag_1" => 1,
        "sma_5" => 4,
        "sma_20" | "bb_upper" | "bb_lower" | "bb_middle" | "stddev_20" => 19,
        "sma_50" => 49,
        "ema_12" => 11,
        "ema_26" => 25,
        "rsi_14" | "atr_14" => 14,
        "roc_10" => 10,
        "macd_signal" => 33,
        _ => return None,
    })
}

fn legacy_column(name: &str, s: &Series) -> Option<Vec<f64>> {
    let n = s.len();
    let c = &s.close;
    legacy_warmup(name)?;
    Some(match name {
        "close" => c.clone(),
        "open" => s.open.clone(),
        "high" => s.high.clone(),
        "low" => s.low.clone(),
        "volume" => s.volume.clone(),
        "volume_lag_1" => (0..n).map(|i| if i >= 1 { s.volume[i - 1] } else { f64::NAN }).collect(),
        "sma_5" => place(n, 4, &ind::sma(c, 5).unwrap_or_default()),
        "sma_20" => place(n, 19, &ind::sma(c, 20).unwrap_or_default()),
        "sma_50" => place(n, 49, &ind::sma(c, 50).unwrap_or_default()),
        "ema_12" => place(n, 11, &ind::ema(c, 12).unwrap_or_default()),
        "ema_26" => place(n, 25, &ind::ema(c, 26).unwrap_or_default()),
        "rsi_14" => place(n, 14, &ind::rsi(c, 14).unwrap_or_default()),
        "roc_10" => place(n, 10, &ind::roc(c, 10).unwrap_or_default()),
        "atr_14" => place(n, 14, &ind::atr(&s.high, &s.low, c, 14).unwrap_or_default()),
        "stddev_20" => place(n, 19, &ind::std_dev(c, 20).unwrap_or_default()),
        "macd_signal" => {
            let sig = ind::macd(c, 12, 26, 9).map(|r| r.signal_line).unwrap_or_default();
            place(n, 33, &sig)
        }
        "bb_upper" | "bb_middle" | "bb_lower" => {
            let bb = ind::bollinger_bands(c, 20, 2.0).ok()?;
            let v = match name {
                "bb_upper" => &bb.upper,
                "bb_middle" => &bb.middle,
                _ => &bb.lower,
            };
            place(n, 19, v)
        }
        _ => return None,
    })
}

// ── Name resolution ──────────────────────────────────────────────────

/// What a model's feature name refers to.
#[derive(Debug, Clone, PartialEq)]
enum Resolved {
    Catalog(String),
    Legacy(String),
    /// A user formula; `warmup` is its static warmup.
    Custom { name: String, warmup: usize },
    /// `base` shifted back by `lag` bars.
    Lag { base: Box<Resolved>, lag: usize },
}

impl Resolved {
    fn warmup(&self) -> usize {
        match self {
            Resolved::Catalog(id) => warmup_of(id).unwrap_or(0),
            Resolved::Legacy(name) => legacy_warmup(name).unwrap_or(0),
            Resolved::Custom { warmup, .. } => *warmup,
            Resolved::Lag { base, lag } => base.warmup() + lag,
        }
    }

    fn base_key(&self) -> String {
        match self {
            Resolved::Catalog(id) => format!("c:{id}"),
            Resolved::Legacy(name) => format!("l:{name}"),
            Resolved::Custom { name, .. } => format!("u:{name}"),
            Resolved::Lag { base, .. } => base.base_key(),
        }
    }
}

const MAX_LAG: usize = 250;

fn resolve(name: &str, customs: &Customs) -> Result<Resolved, String> {
    if let Some(base) = resolve_base(name, customs) {
        return Ok(base);
    }
    // `<base>_lag<n>`
    if let Some(pos) = name.rfind("_lag") {
        let (base, digits) = (&name[..pos], &name[pos + 4..]);
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let lag: usize = digits.parse().map_err(|_| format!("feature `{name}`: lag is too large"))?;
            if lag == 0 || lag > MAX_LAG {
                return Err(format!("feature `{name}`: lag must be between 1 and {MAX_LAG}"));
            }
            let inner = resolve_base(base, customs).ok_or_else(|| unknown_feature(name))?;
            return Ok(Resolved::Lag { base: Box::new(inner), lag });
        }
    }
    Err(unknown_feature(name))
}

fn resolve_base(name: &str, customs: &Customs) -> Option<Resolved> {
    if is_catalog_feature(name) {
        return Some(Resolved::Catalog(name.to_string()));
    }
    if legacy_warmup(name).is_some() {
        return Some(Resolved::Legacy(name.to_string()));
    }
    customs
        .get(name)
        .map(|c| Resolved::Custom { name: name.to_string(), warmup: c.compiled.warmup })
}

fn unknown_feature(name: &str) -> String {
    format!(
        "feature `{name}` cannot be computed: it is not in the feature catalog, not a custom feature of this model, \
         not a lag of one (`<feature>_lag<n>`) and not a legacy indicator column"
    )
}

// ── Custom features ──────────────────────────────────────────────────

/// Most custom features one dataset or model may carry.
pub const MAX_CUSTOM_FEATURES: usize = 20;
/// Most context symbols one dataset or model may read.
pub const MAX_CONTEXT_SYMBOLS: usize = 10;

/// Names that would collide with dataset columns.
const RESERVED_NAMES: &[&str] = &["time", "symbol", "fwd_ret", "target"];

/// Rules for a custom feature name: `[a-z][a-z0-9_]{0,40}`, not a catalog id, not a legacy column,
/// not a dataset column and not shaped like a lag (`x_lag3`), so it can never be mistaken for
/// something else.
pub fn validate_custom_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let shape_ok = name.len() <= 41
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !shape_ok {
        return Err(format!(
            "custom feature name `{name}` is not allowed: use a lowercase letter, then lowercase letters, digits or '_' (41 characters at most)"
        ));
    }
    if is_catalog_feature(name) {
        return Err(format!("custom feature name `{name}` is already a catalog feature; choose another name"));
    }
    if legacy_warmup(name).is_some() || RESERVED_NAMES.contains(&name) {
        return Err(format!("custom feature name `{name}` is reserved; choose another name"));
    }
    if let Some(pos) = name.rfind("_lag") {
        let digits = &name[pos + 4..];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "custom feature name `{name}` ends in _lag<number>, which is reserved for lagged copies of a feature"
            ));
        }
    }
    Ok(())
}

struct Custom {
    compiled: Compiled,
}

/// The compiled custom features of a dataset or model, looked up by name.
#[derive(Default)]
pub struct Customs {
    items: Vec<(String, Custom)>,
}

impl Customs {
    /// Check names (valid, unique) and compile every formula. Errors name the feature and, for
    /// formula errors, the character position.
    pub fn new(custom: &[CustomFeatureDto]) -> Result<Customs, String> {
        if custom.len() > MAX_CUSTOM_FEATURES {
            return Err(format!("too many custom features ({}); the limit is {MAX_CUSTOM_FEATURES}", custom.len()));
        }
        let mut items: Vec<(String, Custom)> = Vec::new();
        for c in custom {
            validate_custom_name(&c.name)?;
            if items.iter().any(|(n, _)| *n == c.name) {
                return Err(format!("custom feature `{}` is defined twice", c.name));
            }
            let compiled =
                ml_expr::compile(&c.expr).map_err(|e| format!("custom feature `{}`: {e}", c.name))?;
            items.push((c.name.clone(), Custom { compiled }));
        }
        Ok(Customs { items })
    }

    fn get(&self, name: &str) -> Option<&Custom> {
        self.items.iter().find(|(n, _)| n == name).map(|(_, c)| c)
    }

    /// Upper-case context symbols read by any formula, in order of first use.
    pub fn referenced_symbols(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, c) in &self.items {
            for s in &c.compiled.referenced_symbols {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    pub fn warmup(&self, name: &str) -> Option<usize> {
        self.get(name).map(|c| c.compiled.warmup)
    }
}

/// Custom features and context symbols recorded in a model's metadata JSON (`custom_features`:
/// `[{name, expr}]`, `context_symbols`: `["SPY"]`). Both are optional; older models have neither.
pub fn custom_spec_from_metadata(raw: &serde_json::Value) -> Result<(Vec<CustomFeatureDto>, Vec<String>), String> {
    let custom = match raw.get("custom_features") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("model metadata has unreadable custom_features: {e}"))?,
    };
    let context = match raw.get("context_symbols") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("model metadata has unreadable context_symbols: {e}"))?,
    };
    Ok((custom, context))
}

/// Context symbols a set of custom features reads (upper case, in order of first use).
pub fn referenced_symbols(custom: &[CustomFeatureDto]) -> Result<Vec<String>, String> {
    Ok(Customs::new(custom)?.referenced_symbols())
}

/// Canonical form of a context symbol list: trimmed, upper case, no duplicates.
pub fn normalize_symbols(symbols: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for s in symbols {
        let s = s.trim().to_uppercase();
        crate::validation::validate_symbol(&s)?;
        if !out.contains(&s) {
            out.push(s);
        }
    }
    if out.len() > MAX_CONTEXT_SYMBOLS {
        return Err(format!("too many context symbols ({}); the limit is {MAX_CONTEXT_SYMBOLS}", out.len()));
    }
    Ok(out)
}

// ── Public matrix API ────────────────────────────────────────────────

/// Bars needed before every feature in `names` has a value on the newest bar.
pub fn required_bars(names: &[String]) -> Result<usize, String> {
    required_bars_with(names, &[])
}

/// [`required_bars`] for a model that also has custom features.
pub fn required_bars_with(names: &[String], custom: &[CustomFeatureDto]) -> Result<usize, String> {
    let customs = Customs::new(custom)?;
    let mut need = 1;
    for name in names {
        need = need.max(resolve(name, &customs)?.warmup() + 1);
    }
    Ok(need)
}

/// Per-bar feature vectors for exactly `names` (in that order); NaN while a feature is warming
/// up. Handles catalog ids, `<feature>_lag<n>` and legacy columns. Fails with a message naming any
/// feature that cannot be computed.
pub fn model_matrix(bars: &[OHLCV], names: &[String]) -> Result<Vec<Vec<f64>>, String> {
    model_matrix_with(bars, names, &[], &Context::new())
}

/// [`model_matrix`] that also knows the model's custom features. `context` holds the bars of
/// every symbol the formulas read with `other(...)` (keys upper case).
pub fn model_matrix_with(
    bars: &[OHLCV],
    names: &[String],
    custom: &[CustomFeatureDto],
    context: &Context,
) -> Result<Vec<Vec<f64>>, String> {
    let customs = Customs::new(custom)?;
    model_matrix_compiled(bars, names, &customs, context)
}

/// [`model_matrix_with`] for callers that compiled the custom features once already.
pub fn model_matrix_compiled(
    bars: &[OHLCV],
    names: &[String],
    customs: &Customs,
    context: &Context,
) -> Result<Vec<Vec<f64>>, String> {
    let resolved: Vec<Resolved> = names.iter().map(|n| resolve(n, customs)).collect::<Result<_, _>>()?;
    let series = Series::from_bars(bars)?;
    let n = series.len();

    let mut bases: HashMap<String, Vec<f64>> = HashMap::new();
    for r in &resolved {
        let key = r.base_key();
        if bases.contains_key(&key) {
            continue;
        }
        let leaf = match r {
            Resolved::Lag { base, .. } => base.as_ref(),
            other => other,
        };
        let col = match leaf {
            Resolved::Catalog(id) => catalog_column(id, &series),
            Resolved::Legacy(name) => legacy_column(name, &series),
            Resolved::Custom { name, .. } => {
                let c = customs.get(name).ok_or_else(|| unknown_feature(name))?;
                Some(ml_expr::evaluate(&c.compiled, bars, context).map_err(|e| format!("custom feature `{name}`: {e}"))?)
            }
            Resolved::Lag { .. } => None,
        }
        .ok_or_else(|| unknown_feature(&r.base_key()))?;
        bases.insert(key, col);
    }

    let mut rows = vec![vec![f64::NAN; names.len()]; n];
    for (j, r) in resolved.iter().enumerate() {
        let col = &bases[&r.base_key()];
        let lag = if let Resolved::Lag { lag, .. } = r { *lag } else { 0 };
        for i in lag..n {
            rows[i][j] = col[i - lag];
        }
    }
    Ok(rows)
}

/// Catalog-only matrix.
#[allow(dead_code)]
pub fn compute_matrix(bars: &[OHLCV], feature_ids: &[String]) -> Result<Vec<Vec<f64>>, String> {
    for id in feature_ids {
        if !is_catalog_feature(id) {
            return Err(format!("unknown feature `{id}`"));
        }
    }
    model_matrix(bars, feature_ids)
}

/// Feature values of the newest bar as name -> value, failing when any is not yet defined.
pub fn latest_features(bars: &[OHLCV], names: &[String], symbol: &str) -> Result<HashMap<String, f64>, String> {
    latest_features_with(bars, names, &[], &Context::new(), symbol)
}

/// [`latest_features`] for a model with custom features and context symbols.
pub fn latest_features_with(
    bars: &[OHLCV],
    names: &[String],
    custom: &[CustomFeatureDto],
    context: &Context,
    symbol: &str,
) -> Result<HashMap<String, f64>, String> {
    let customs = Customs::new(custom)?;
    let mut need = 1;
    for name in names {
        need = need.max(resolve(name, &customs)?.warmup() + 1);
    }
    let matrix = model_matrix_compiled(bars, names, &customs, context)?;
    let row = matrix
        .last()
        .ok_or_else(|| format!("no bars for {symbol}"))?;
    let mut out = HashMap::new();
    for (name, v) in names.iter().zip(row) {
        if !v.is_finite() {
            return Err(format!(
                "feature `{name}` is not available for the latest bar of {symbol}: it needs about {need} bars and {} were loaded",
                bars.len()
            ));
        }
        out.insert(name.clone(), *v);
    }
    Ok(out)
}

// ── Context bars for scoring ─────────────────────────────────────────

/// The symbols to load for scoring: the model's recorded context symbols plus anything a formula
/// reads, so a model whose metadata lost a context symbol still gets its data.
pub fn context_symbols_needed(custom: &[CustomFeatureDto], recorded: &[String]) -> Result<Vec<String>, String> {
    let mut out = normalize_symbols(recorded)?;
    for s in referenced_symbols(custom)? {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

/// The custom formulas a model's features really use, and the context symbols those formulas
/// read. A feature is used when it is a custom feature's name or one of its lags (`name_lag<n>`).
/// Recorded context symbols that no used formula reads are dropped: they are not needed to score.
pub fn narrow_to_used(
    custom: &[CustomFeatureDto],
    recorded: &[String],
    feature_names: &[String],
) -> Result<(Vec<CustomFeatureDto>, Vec<String>), String> {
    let uses = |name: &str| {
        feature_names.iter().any(|f| {
            f == name
                || f.strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix("_lag"))
                    .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false)
        })
    };
    let used: Vec<CustomFeatureDto> = custom.iter().filter(|c| uses(&c.name)).cloned().collect();
    // A used formula may only be referenced through a symbol the metadata did not list.
    let symbols = context_symbols_needed(&used, &[])?;
    let _ = recorded; // kept in the signature so callers state what was recorded
    Ok((used, symbols))
}

/// Load the newest `limit` stored bars of each context symbol (same timeframe as the main
/// symbol) from storage. Fails naming a symbol that has no bars.
pub async fn load_context_from_storage(
    storage: &std::sync::Arc<dyn apex_core::ports::storage::StoragePort>,
    symbols: &[String],
    timeframe: Timeframe,
    limit: usize,
) -> Result<Context, String> {
    use apex_core::domain::models::{OHLCVQuery, Symbol};
    let mut ctx = Context::new();
    for sym in symbols {
        let bars = storage
            .query_ohlcv(OHLCVQuery {
                symbol: Symbol(sym.clone()),
                timeframe: timeframe.clone(),
                from: chrono::DateTime::UNIX_EPOCH,
                to: chrono::Utc::now(),
                limit: Some(limit),
            })
            .await
            .map_err(|e| format!("Failed to load bars for context symbol {sym}: {e}"))?;
        if bars.is_empty() {
            return Err(format!(
                "no stored bars for context symbol {sym}, which this model's custom features read; fetch its history first (fetch_symbol_history)"
            ));
        }
        ctx.insert(sym.clone(), bars);
    }
    Ok(ctx)
}

/// Refuse to score when a context symbol's newest bar is much older than the main symbol's. Forward
/// fill would otherwise quietly feed a stale value into a live decision.
pub fn check_context_fresh(
    context: &Context,
    main_last: chrono::DateTime<chrono::Utc>,
    max_age: chrono::Duration,
) -> Result<(), String> {
    for (sym, bars) in context {
        let newest = bars.iter().map(|b| b.time).max();
        match newest {
            None => return Err(format!("context symbol {sym} has no bars")),
            Some(t) if main_last - t > max_age => {
                return Err(format!(
                    "newest bar of context symbol {sym} is from {} but the symbol being scored is at {}; refusing to score on stale context data",
                    t.format("%Y-%m-%d %H:%M"),
                    main_last.format("%Y-%m-%d %H:%M")
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use apex_core::domain::models::Symbol;
    use chrono::{Duration, TimeZone, Utc};

    pub(crate) fn synthetic_bars(n: usize) -> Vec<OHLCV> {
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        (0..n)
            .map(|i| {
                let x = i as f64;
                let close = 100.0 + x * 0.2 + (x / 5.0).sin() * 3.0 + (x / 13.0).cos();
                OHLCV {
                    time: start + Duration::days(i as i64),
                    symbol: Symbol("TEST".into()),
                    timeframe: Timeframe::D1,
                    open: close - 0.3,
                    high: close + 1.0 + (x / 7.0).sin().abs(),
                    low: close - 1.0 - (x / 11.0).cos().abs(),
                    close,
                    volume: 10_000 + ((i * 37) % 500) as u64,
                }
            })
            .collect()
    }

    fn ids() -> Vec<String> {
        CATALOG.iter().map(|s| s.id.to_string()).collect()
    }

    fn bars_from_closes(closes: &[f64]) -> Vec<OHLCV> {
        let mut bars = synthetic_bars(closes.len());
        for (b, c) in bars.iter_mut().zip(closes) {
            b.close = *c;
            b.open = *c;
            b.high = *c + 1.0;
            b.low = *c - 1.0;
        }
        bars
    }

    #[test]
    fn catalog_has_the_fifteen_contract_ids() {
        let got: Vec<String> = catalog().into_iter().map(|f| f.id).collect();
        let want = [
            "ret_1", "ret_5", "ret_10", "range_pct", "sma_ratio_20", "sma_ratio_50", "ema_ratio_12",
            "macd_hist", "rsi_14", "roc_10", "vol_10", "vol_20", "atr_pct_14", "bb_pos_20", "vol_ratio_20",
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn warmup_nan_counts_match_the_catalog() {
        let bars = synthetic_bars(120);
        let m = compute_matrix(&bars, &ids()).unwrap();
        for (j, def) in catalog().iter().enumerate() {
            let nan_count = m.iter().take_while(|r| r[j].is_nan()).count();
            assert_eq!(nan_count, def.warmup_bars, "leading NaNs of {}", def.id);
            assert!(m.iter().skip(def.warmup_bars).all(|r| r[j].is_finite()), "{} has a NaN after warmup", def.id);
        }
    }

    #[test]
    fn values_on_a_known_series() {
        // Close 100, 101, ..., 129 (30 bars): every simple quantity can be computed by hand.
        let closes: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        let bars = bars_from_closes(&closes);
        let names: Vec<String> = ["ret_1", "ret_5", "range_pct", "sma_ratio_20", "roc_10", "rsi_14", "bb_pos_20"]
            .iter().map(|s| s.to_string()).collect();
        let m = compute_matrix(&bars, &names).unwrap();
        let last = m.last().unwrap();
        assert!((last[0] - 129.0 / 128.0 + 1.0).abs() < 1e-12, "ret_1");
        assert!((last[1] - (129.0 / 124.0 - 1.0)).abs() < 1e-12, "ret_5");
        assert!((last[2] - 2.0 / 129.0).abs() < 1e-12, "range_pct");
        let sma20: f64 = (110..=129).map(|v| v as f64).sum::<f64>() / 20.0;
        assert!((last[3] - 129.0 / sma20).abs() < 1e-12, "sma_ratio_20");
        assert!((last[4] - (129.0 - 119.0) / 119.0 * 100.0).abs() < 1e-9, "roc_10");
        assert!(last[5] > 99.0, "rsi of a monotone rise is saturated, got {}", last[5]);
        // Close above the upper half of its own band on a straight line.
        assert!(last[6] > 0.5 && last[6] < 1.0, "bb_pos_20 = {}", last[6]);
    }

    #[test]
    fn volatility_is_the_std_dev_of_one_bar_returns() {
        let bars = synthetic_bars(40);
        let m = compute_matrix(&bars, &["vol_10".to_string()]).unwrap();
        let i = 30;
        let r: Vec<f64> = (i - 9..=i).map(|k| bars[k].close / bars[k - 1].close - 1.0).collect();
        let mean = r.iter().sum::<f64>() / 10.0;
        let sd = (r.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / 10.0).sqrt();
        assert!((m[i][0] - sd).abs() < 1e-12);
    }

    #[test]
    fn no_lookahead_future_bars_do_not_change_earlier_values() {
        let bars = synthetic_bars(200);
        let base = compute_matrix(&bars, &ids()).unwrap();
        for cut in [60usize, 100, 150] {
            let mut perturbed = bars.clone();
            for (k, b) in perturbed.iter_mut().enumerate().skip(cut + 1) {
                b.close *= 1.0 + 0.3 * ((k % 5) as f64 - 2.0);
                b.high = b.close * 1.05;
                b.low = b.close * 0.9;
                b.open = b.close;
                b.volume = b.volume * 7 + 13;
            }
            let other = compute_matrix(&perturbed, &ids()).unwrap();
            for i in 0..=cut {
                for j in 0..base[i].len() {
                    let (a, b) = (base[i][j], other[i][j]);
                    assert!(
                        (a.is_nan() && b.is_nan()) || a == b,
                        "feature {} at bar {i} changed when bars after {cut} changed: {a} vs {b}",
                        CATALOG[j].id
                    );
                }
            }
        }
    }

    #[test]
    fn lag_features_take_the_value_n_bars_back() {
        let bars = synthetic_bars(120);
        let names: Vec<String> = ["ret_5", "ret_5_lag3"].iter().map(|s| s.to_string()).collect();
        let m = model_matrix(&bars, &names).unwrap();
        for i in 8..120 {
            assert_eq!(m[i][1], m[i - 3][0]);
        }
        assert!(m[7][1].is_nan(), "ret_5 first exists at bar 5, so its lag 3 first exists at bar 8");
        assert_eq!(required_bars(&names).unwrap(), 9);
    }

    #[test]
    fn legacy_names_keep_working() {
        let bars = synthetic_bars(120);
        let names: Vec<String> = [
            "sma_20", "sma_50", "ema_12", "ema_26", "rsi_14", "macd_signal", "bb_upper", "bb_lower", "atr_14",
            "volume_lag_1", "close", "open", "high", "low", "volume", "volume_lag1",
        ]
        .iter().map(|s| s.to_string()).collect();
        let m = model_matrix(&bars, &names).unwrap();
        let last = m.last().unwrap();
        assert!(last.iter().all(|v| v.is_finite()));
        let i = 119;
        assert_eq!(last[9], bars[i - 1].volume as f64, "volume_lag_1 is the previous bar's volume");
        assert_eq!(last[15], bars[i - 1].volume as f64, "volume_lag1 is a lag of volume");
        assert_eq!(last[10], bars[i].close);
        let sma20 = bars[i - 19..=i].iter().map(|b| b.close).sum::<f64>() / 20.0;
        assert!((last[0] - sma20).abs() < 1e-9);
        assert!(last[6] > last[7], "upper band above lower band");
    }

    #[test]
    fn unknown_feature_error_names_it() {
        let bars = synthetic_bars(80);
        let err = model_matrix(&bars, &["sma_ratio".to_string()]).unwrap_err();
        assert!(err.contains("`sma_ratio`"), "{err}");
        let err = model_matrix(&bars, &["bogus_lag2".to_string()]).unwrap_err();
        assert!(err.contains("`bogus_lag2`"), "{err}");
        let err = compute_matrix(&bars, &["sma_20".to_string()]).unwrap_err();
        assert!(err.contains("sma_20"), "{err}");
    }

    #[test]
    fn latest_features_names_the_feature_that_is_not_ready() {
        let bars = synthetic_bars(30);
        let err = latest_features(&bars, &["sma_ratio_50".to_string()], "TEST").unwrap_err();
        assert!(err.contains("sma_ratio_50") && err.contains("TEST"), "{err}");
        assert!(latest_features(&bars, &["sma_ratio_20".to_string()], "TEST").is_ok());
    }

    #[test]
    fn bad_prices_are_rejected_not_silently_propagated() {
        let mut bars = synthetic_bars(30);
        bars[10].close = 0.0;
        let err = compute_matrix(&bars, &["ret_1".to_string()]).unwrap_err();
        assert!(err.contains("bad price data"), "{err}");
    }

    #[test]
    fn timeframe_spellings() {
        assert_eq!(canonical_timeframe("D1"), Some("1d"));
        assert_eq!(canonical_timeframe("1H"), Some("1h"));
        assert_eq!(canonical_timeframe("7d"), None);
        assert_eq!(parse_timeframe("5m"), Some(Timeframe::M5));
    }

    // ── Custom features ──────────────────────────────────────────────

    fn custom(items: &[(&str, &str)]) -> Vec<CustomFeatureDto> {
        items.iter().map(|(n, e)| CustomFeatureDto { name: n.to_string(), expr: e.to_string() }).collect()
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Another symbol on a sparser calendar (every third bar missing).
    fn spy_context(n: usize) -> Context {
        use apex_core::domain::models::Symbol;
        let bars: Vec<OHLCV> = synthetic_bars(n)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 3 != 2)
            .map(|(_, mut b)| {
                b.symbol = Symbol("SPY".into());
                b.close = b.close * 2.0 + 5.0;
                b
            })
            .collect();
        let mut ctx = Context::new();
        ctx.insert("SPY".into(), bars);
        ctx
    }

    #[test]
    fn custom_columns_use_the_exact_name_and_equal_a_direct_evaluation() {
        let bars = synthetic_bars(150);
        let cf = custom(&[("mom5", "close / shift(close, 5) - 1"), ("trend", "close > sma(close, 20)")]);
        let wanted = names(&["ret_1", "mom5", "trend", "mom5_lag3"]);
        let m = model_matrix_with(&bars, &wanted, &cf, &Context::new()).unwrap();
        let direct = ml_expr::evaluate(&ml_expr::compile(&cf[0].expr).unwrap(), &bars, &Context::new()).unwrap();
        let trend = ml_expr::evaluate(&ml_expr::compile(&cf[1].expr).unwrap(), &bars, &Context::new()).unwrap();
        for i in 0..bars.len() {
            let eq = |a: f64, b: f64| (a.is_nan() && b.is_nan()) || a == b;
            assert!(eq(m[i][1], direct[i]), "mom5 at {i}");
            assert!(eq(m[i][2], trend[i]), "trend at {i}");
            if i >= 3 {
                assert!(eq(m[i][3], direct[i - 3]), "mom5_lag3 at {i}");
            }
        }
        assert!(m[7][3].is_nan() && m[8][3].is_finite(), "lag 3 of a warmup-5 feature first exists at bar 8");
        // The catalog column is unchanged by the presence of custom ones.
        let plain = model_matrix(&bars, &names(&["ret_1"])).unwrap();
        for i in 0..bars.len() {
            assert!((plain[i][0].is_nan() && m[i][0].is_nan()) || plain[i][0] == m[i][0]);
        }
        assert_eq!(required_bars_with(&names(&["mom5", "mom5_lag3"]), &cf).unwrap(), 5 + 3 + 1, "mom5_lag3 is the deepest");
        assert_eq!(required_bars_with(&wanted, &cf).unwrap(), 20, "trend needs the 20-bar average");
        assert_eq!(required_bars_with(&names(&["trend"]), &cf).unwrap(), 20);
    }

    #[test]
    fn custom_features_can_read_context_symbols() {
        let bars = synthetic_bars(120);
        let ctx = spy_context(120);
        let cf = custom(&[("rel", "close / other(\"SPY\", \"close\")")]);
        assert_eq!(referenced_symbols(&cf).unwrap(), vec!["SPY"]);
        let m = model_matrix_with(&bars, &names(&["rel"]), &cf, &ctx).unwrap();
        // Bar 5: SPY has bars 0,1,3,4,5 (2 is missing); bar 5 exists. Bar 8 is missing, so the
        // value of bar 7 is used.
        let spy = &ctx["SPY"];
        let at = |t: chrono::DateTime<chrono::Utc>| spy.iter().filter(|b| b.time <= t).last().unwrap().close;
        assert!((m[5][0] - bars[5].close / at(bars[5].time)).abs() < 1e-12);
        assert!((m[8][0] - bars[8].close / at(bars[7].time)).abs() < 1e-12, "forward-filled from bar 7");
    }

    #[test]
    fn missing_context_names_the_feature_and_the_symbol() {
        let bars = synthetic_bars(60);
        let cf = custom(&[("rel", "close / other(\"QQQ\", \"close\")")]);
        let err = model_matrix_with(&bars, &names(&["rel"]), &cf, &Context::new()).unwrap_err();
        assert!(err.contains("`rel`") && err.contains("QQQ"), "{err}");
        let err = latest_features_with(&bars, &names(&["rel"]), &cf, &Context::new(), "AAA").unwrap_err();
        assert!(err.contains("QQQ"), "{err}");
    }

    #[test]
    fn custom_name_rules() {
        for ok in ["a", "mom_5", "x1", &format!("a{}", "b".repeat(40))] {
            assert!(validate_custom_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "Mom", "1abc", "_x", "a-b", "a b", "naïve", &format!("a{}", "b".repeat(41)),
            "ret_1",        // catalog id
            "rsi_14",       // catalog id and legacy
            "close",        // legacy column
            "sma_20",       // legacy column
            "fwd_ret", "target", "time", "symbol", // dataset columns
            "x_lag3",       // looks like a lag
            "ret_1_lag2",
        ] {
            assert!(validate_custom_name(bad).is_err(), "{bad}");
        }
        assert!(validate_custom_name("x_lagged").is_ok(), "only _lag<digits> is reserved");
    }

    #[test]
    fn custom_set_validation_reports_the_feature_and_position() {
        let e = Customs::new(&custom(&[("a", "close"), ("a", "open")])).err().unwrap();
        assert!(e.contains("`a`") && e.contains("twice"), "{e}");
        let e = Customs::new(&custom(&[("good", "close"), ("bad", "close +")])).err().unwrap();
        assert!(e.contains("`bad`") && e.contains("character 8"), "{e}");
        let e = Customs::new(&custom(&[("ret_5", "close")])).err().unwrap();
        assert!(e.contains("catalog"), "{e}");
        let many: Vec<CustomFeatureDto> = (0..=MAX_CUSTOM_FEATURES)
            .map(|i| CustomFeatureDto { name: format!("f{i}"), expr: "close".into() })
            .collect();
        assert!(Customs::new(&many).err().unwrap().contains("too many"));
        assert!(Customs::new(&many[..MAX_CUSTOM_FEATURES]).is_ok());
    }

    #[test]
    fn a_lag_of_an_unknown_name_is_still_unknown() {
        let bars = synthetic_bars(60);
        let cf = custom(&[("mom", "ret(close, 3)")]);
        let err = model_matrix_with(&bars, &names(&["nope_lag2"]), &cf, &Context::new()).unwrap_err();
        assert!(err.contains("`nope_lag2`"), "{err}");
        // Without the custom definition the same name is unknown, with a message that says so.
        let err = model_matrix(&bars, &names(&["mom"])).unwrap_err();
        assert!(err.contains("`mom`") && err.contains("custom feature"), "{err}");
    }

    #[test]
    fn latest_features_with_custom_and_context_matches_the_matrix_row() {
        let bars = synthetic_bars(160);
        let ctx = spy_context(160);
        let cf = custom(&[("rel", "ret(close, 3) - ret(other('SPY', 'close'), 3)"), ("z", "zscore(close, 30)")]);
        let wanted = names(&["rel", "z", "rel_lag2", "rsi_14"]);
        let latest = latest_features_with(&bars, &wanted, &cf, &ctx, "AAA").unwrap();
        let m = model_matrix_with(&bars, &wanted, &cf, &ctx).unwrap();
        for (j, n) in wanted.iter().enumerate() {
            assert_eq!(latest[n], m[159][j], "{n}");
        }
        let err = latest_features_with(&bars[..20], &wanted, &cf, &ctx, "AAA").unwrap_err();
        assert!(err.contains("`z`") && err.contains("AAA"), "{err}");
    }

    #[test]
    fn context_symbols_needed_merges_recorded_and_referenced() {
        let cf = custom(&[("a", "other('qqq', 'close')"), ("b", "other('SPY', 'open')")]);
        let got = context_symbols_needed(&cf, &names(&["spy", "IWM"])).unwrap();
        assert_eq!(got, vec!["SPY", "IWM", "QQQ"]);
        assert!(context_symbols_needed(&[], &[]).unwrap().is_empty());
        let too_many: Vec<String> = (0..=MAX_CONTEXT_SYMBOLS).map(|i| format!("S{i}")).collect();
        assert!(normalize_symbols(&too_many).is_err());
    }

    #[test]
    fn stale_context_is_refused() {
        use chrono::Duration;
        let main_last = synthetic_bars(100).last().unwrap().time;
        let mut ctx = spy_context(100);
        assert!(check_context_fresh(&ctx, main_last, Duration::days(6)).is_ok());
        let stale: Vec<OHLCV> = ctx["SPY"].iter().filter(|b| b.time < main_last - Duration::days(30)).cloned().collect();
        ctx.insert("SPY".into(), stale);
        let err = check_context_fresh(&ctx, main_last, Duration::days(6)).unwrap_err();
        assert!(err.contains("SPY") && err.contains("stale"), "{err}");
        ctx.insert("SPY".into(), vec![]);
        assert!(check_context_fresh(&ctx, main_last, Duration::days(6)).is_err());
    }

    #[test]
    fn custom_features_have_no_lookahead() {
        let bars = synthetic_bars(160);
        let ctx = spy_context(160);
        let cf = custom(&[("a", "ema(close, 8) / sma(close, 30) - 1"), ("b", "ret(other('SPY', 'close'), 2)")]);
        let wanted = names(&["a", "b", "a_lag4"]);
        let base = model_matrix_with(&bars, &wanted, &cf, &ctx).unwrap();
        let cut = 90;
        let mut p = bars.clone();
        let mut p_ctx = ctx.clone();
        for b in p.iter_mut().skip(cut + 1) {
            b.close *= 1.7;
            b.high = b.close * 1.1;
            b.low = b.close * 0.9;
        }
        for b in p_ctx.get_mut("SPY").unwrap().iter_mut().filter(|b| b.time > bars[cut].time) {
            b.close *= 0.4;
        }
        let other = model_matrix_with(&p, &wanted, &cf, &p_ctx).unwrap();
        for i in 0..=cut {
            for j in 0..3 {
                assert!((base[i][j].is_nan() && other[i][j].is_nan()) || base[i][j] == other[i][j], "col {j} bar {i}");
            }
        }
    }

    #[test]
    fn metadata_spec_is_optional_and_strict_when_present() {
        let none = serde_json::json!({"feature_names": ["ret_1"]});
        assert_eq!(custom_spec_from_metadata(&none).unwrap(), (vec![], vec![]));
        let some = serde_json::json!({
            "custom_features": [{"name": "m", "expr": "ret(close, 3)"}],
            "context_symbols": ["SPY"],
        });
        let (c, s) = custom_spec_from_metadata(&some).unwrap();
        assert_eq!(c, custom(&[("m", "ret(close, 3)")]));
        assert_eq!(s, vec!["SPY"]);
        let bad = serde_json::json!({"custom_features": "nope"});
        assert!(custom_spec_from_metadata(&bad).unwrap_err().contains("custom_features"));
        assert_eq!(custom_spec_from_metadata(&serde_json::json!({"custom_features": null})).unwrap().0.len(), 0);
    }
}

#[cfg(test)]
mod narrow_tests {
    use super::*;

    fn cf(name: &str, expr: &str) -> CustomFeatureDto {
        CustomFeatureDto { name: name.into(), expr: expr.into() }
    }

    #[test]
    fn unused_formulas_and_their_context_symbols_are_dropped() {
        let custom = vec![cf("rel_qqq", "close / other(\"QQQ\", \"close\")"), cf("mom", "ret(close, 5)")];
        let recorded = vec!["QQQ".to_string(), "SPY".to_string()];
        // The model was trained on a subset that excludes rel_qqq.
        let names = vec!["ret_1".to_string(), "mom".to_string()];
        let (used, symbols) = narrow_to_used(&custom, &recorded, &names).unwrap();
        assert_eq!(used.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["mom"]);
        assert!(symbols.is_empty(), "no used formula reads another symbol: {symbols:?}");
    }

    #[test]
    fn a_used_formula_keeps_exactly_the_symbols_it_reads() {
        let custom = vec![cf("rel_qqq", "close / other(\"QQQ\", \"close\")"), cf("mom", "ret(close, 5)")];
        let names = vec!["rel_qqq".to_string()];
        let (used, symbols) = narrow_to_used(&custom, &["SPY".to_string()], &names).unwrap();
        assert_eq!(used.len(), 1);
        assert_eq!(symbols, vec!["QQQ".to_string()]);
    }

    #[test]
    fn a_lagged_custom_feature_counts_as_used_but_a_lookalike_name_does_not() {
        let custom = vec![cf("mom", "ret(close, 5)"), cf("mom2", "ret(close, 9)")];
        let names = vec!["mom_lag3".to_string()];
        let (used, _) = narrow_to_used(&custom, &[], &names).unwrap();
        assert_eq!(used.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["mom"]);
        let (none, _) = narrow_to_used(&custom, &[], &["mom_lagx".to_string(), "mom20".to_string()]).unwrap();
        assert!(none.is_empty());
    }
}
