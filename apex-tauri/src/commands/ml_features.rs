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
//!   `volume_lag_1`, `close`, ...). Their old definitions are kept so those models still score.

use crate::dto::FeatureDefDto;
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
    /// `base` shifted back by `lag` bars.
    Lag { base: Box<Resolved>, lag: usize },
}

impl Resolved {
    fn warmup(&self) -> usize {
        match self {
            Resolved::Catalog(id) => warmup_of(id).unwrap_or(0),
            Resolved::Legacy(name) => legacy_warmup(name).unwrap_or(0),
            Resolved::Lag { base, lag } => base.warmup() + lag,
        }
    }

    fn base_key(&self) -> String {
        match self {
            Resolved::Catalog(id) => format!("c:{id}"),
            Resolved::Legacy(name) => format!("l:{name}"),
            Resolved::Lag { base, .. } => base.base_key(),
        }
    }
}

const MAX_LAG: usize = 250;

fn resolve(name: &str) -> Result<Resolved, String> {
    if is_catalog_feature(name) {
        return Ok(Resolved::Catalog(name.to_string()));
    }
    if legacy_warmup(name).is_some() {
        return Ok(Resolved::Legacy(name.to_string()));
    }
    // `<base>_lag<n>`
    if let Some(pos) = name.rfind("_lag") {
        let (base, digits) = (&name[..pos], &name[pos + 4..]);
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let lag: usize = digits.parse().map_err(|_| format!("feature `{name}`: lag is too large"))?;
            if lag == 0 || lag > MAX_LAG {
                return Err(format!("feature `{name}`: lag must be between 1 and {MAX_LAG}"));
            }
            let inner = if is_catalog_feature(base) {
                Resolved::Catalog(base.to_string())
            } else if legacy_warmup(base).is_some() {
                Resolved::Legacy(base.to_string())
            } else {
                return Err(unknown_feature(name));
            };
            return Ok(Resolved::Lag { base: Box::new(inner), lag });
        }
    }
    Err(unknown_feature(name))
}

fn unknown_feature(name: &str) -> String {
    format!(
        "feature `{name}` cannot be computed: it is not in the feature catalog, not a lag of one (`<feature>_lag<n>`) \
         and not a legacy indicator column"
    )
}

/// Bars needed before every feature in `names` has a value on the newest bar.
pub fn required_bars(names: &[String]) -> Result<usize, String> {
    let mut need = 1;
    for name in names {
        need = need.max(resolve(name)?.warmup() + 1);
    }
    Ok(need)
}

// ── Public matrix API ────────────────────────────────────────────────

/// Per-bar feature vectors for exactly `names` (in that order); NaN while a feature is warming
/// up. Handles catalog ids, `<feature>_lag<n>` and legacy columns. Fails with a message naming any
/// feature that cannot be computed.
pub fn model_matrix(bars: &[OHLCV], names: &[String]) -> Result<Vec<Vec<f64>>, String> {
    let resolved: Vec<Resolved> = names.iter().map(|n| resolve(n)).collect::<Result<_, _>>()?;
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

/// Catalog-only matrix, used by the dataset builder.
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
    let need = required_bars(names)?;
    let matrix = model_matrix(bars, names)?;
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
}
