use crate::dto::OHLCVDto;
use crate::state::AppState;
use apex_core::domain::models::{OHLCVQuery, Symbol, Timeframe, OHLCV};
use chrono::Utc;
use tauri::State;

/// Load OHLCV bars for a symbol: storage first, falling back to the market
/// data adapter (persisting what it returns). Shared by chart/history,
/// indicator, and quant commands — OpenBB-style "fetch then cache" pipeline.
pub(crate) async fn load_bars(
    state: &AppState,
    symbol: &str,
    timeframe: Option<&str>,
    limit: usize,
) -> Result<Vec<OHLCV>, String> {
    let tf = match timeframe.unwrap_or("d1").to_lowercase().as_str() {
        "1m" | "m1" => Timeframe::M1,
        "5m" | "m5" => Timeframe::M5,
        "15m" | "m15" => Timeframe::M15,
        "1h" | "h1" => Timeframe::H1,
        "4h" | "h4" => Timeframe::H4,
        "1d" | "d1" => Timeframe::D1,
        "1w" | "w1" => Timeframe::W1,
        _ => Timeframe::D1,
    };

    let now = Utc::now();

    let params = OHLCVQuery {
        symbol: Symbol(symbol.to_string()),
        timeframe: tf.clone(),
        from: chrono::DateTime::UNIX_EPOCH,
        to: now,
        limit: Some(limit),
    };

    let mut bars = state
        .storage
        .query_ohlcv(params)
        .await
        .map_err(|e| format!("Failed to query historical data for {}: {}", symbol, e))?;

    // Refresh when the cache is empty, undersized, or its newest bar is older
    // than the requested timeframe's bucket — a partial/old cache must not
    // starve intraday charts or analytics of recent data.
    let bucket_secs: i64 = match tf {
        Timeframe::S1 => 1,
        Timeframe::S5 => 5,
        Timeframe::S15 => 15,
        Timeframe::M1 => 60,
        Timeframe::M3 => 180,
        Timeframe::M5 => 300,
        Timeframe::M15 => 900,
        Timeframe::M30 => 1800,
        Timeframe::H1 => 3600,
        Timeframe::H4 => 14400,
        Timeframe::D1 => 86_400,
        Timeframe::W1 => 604_800,
    };
    let newest_is_stale = bars
        .last()
        .map(|b| (now - b.time).num_seconds() > bucket_secs * 2)
        .unwrap_or(true);
    if bars.is_empty() || bars.len() < limit || newest_is_stale {
        let lookback_days = match tf {
            Timeframe::S1 | Timeframe::S5 | Timeframe::S15 => 1,
            Timeframe::M1 | Timeframe::M3 | Timeframe::M5 | Timeframe::M15 | Timeframe::M30 => 7,
            Timeframe::H1 | Timeframe::H4 => 60,
            Timeframe::D1 | Timeframe::W1 => 3650,
        };
        let from = now - chrono::Duration::days(lookback_days);

        // Storage-first: a provider outage must not blank charts that already
        // have cached history — only an empty cache makes the error fatal.
        let fetched = match state
            .history_source
            .get_historical_ohlcv(&Symbol(symbol.to_string()), tf, from, now)
            .await
        {
            Ok(b) => b,
            Err(e) if bars.is_empty() => {
                return Err(format!("Failed to fetch historical data for {symbol}: {e}"));
            }
            Err(e) => {
                tracing::warn!(
                    symbol = %symbol,
                    error = %e,
                    "OHLCV refresh failed; serving cached bars"
                );
                Vec::new()
            }
        };

        if !fetched.is_empty() {
            // Merge cached + fetched on bar time (fetched wins) so a larger
            // request backfills the cache instead of being trimmed by it.
            let mut merged: std::collections::BTreeMap<chrono::DateTime<Utc>, OHLCV> =
                bars.iter().map(|b| (b.time, b.clone())).collect();
            for b in &fetched {
                merged.insert(b.time, b.clone());
            }
            let merged: Vec<OHLCV> = merged.into_values().collect();
            if let Err(e) = state.storage.write_ohlcv(&merged).await {
                tracing::warn!(symbol = %symbol, error = %e, "Failed to persist fetched OHLCV bars");
            }
            bars = merged;
        }
    }

    let start = bars.len().saturating_sub(limit);
    Ok(bars.split_off(start))
}

/// Most bars one `fetch_history` call asks the provider for.
pub(crate) const MAX_FETCH_BARS: usize = 5000;
/// Most stored bars one summary reads per symbol.
pub(crate) const MAX_STORED_BARS: usize = 100_000;

/// Stored bars of a symbol, oldest first, with no provider call. At most the newest `limit`.
pub(crate) async fn stored_bars(
    storage: &std::sync::Arc<dyn apex_core::ports::storage::StoragePort>,
    symbol: &str,
    timeframe: Timeframe,
    limit: usize,
) -> Result<Vec<OHLCV>, String> {
    storage
        .query_ohlcv(OHLCVQuery {
            symbol: Symbol(symbol.to_string()),
            timeframe,
            from: chrono::DateTime::UNIX_EPOCH,
            to: Utc::now(),
            limit: Some(limit),
        })
        .await
        .map_err(|e| format!("Failed to read stored bars for {symbol}: {e}"))
}

/// "1d", "1h", ... as the UI spells timeframes.
fn timeframe_label(tf: &Timeframe) -> String {
    match tf {
        Timeframe::M1 => "1m".into(),
        Timeframe::M5 => "5m".into(),
        Timeframe::M15 => "15m".into(),
        Timeframe::H1 => "1h".into(),
        Timeframe::H4 => "4h".into(),
        Timeframe::D1 => "1d".into(),
        Timeframe::W1 => "1w".into(),
        other => other.as_str().to_lowercase(),
    }
}

fn bucket_seconds(tf: &Timeframe) -> i64 {
    match tf {
        Timeframe::S1 => 1,
        Timeframe::S5 => 5,
        Timeframe::S15 => 15,
        Timeframe::M1 => 60,
        Timeframe::M3 => 180,
        Timeframe::M5 => 300,
        Timeframe::M15 => 900,
        Timeframe::M30 => 1800,
        Timeframe::H1 => 3600,
        Timeframe::H4 => 14_400,
        Timeframe::D1 => 86_400,
        Timeframe::W1 => 604_800,
    }
}

/// How far back to ask for `bars` bars: market calendars skip weekends, nights and holidays, so
/// the span is wider than `bars` times the bar length. Capped at 20 years.
pub(crate) fn history_window(tf: &Timeframe, bars: usize) -> chrono::Duration {
    let bucket = bucket_seconds(tf) as f64;
    let days = if bucket >= 604_800.0 {
        bars as f64 * 7.0 + 14.0
    } else if bucket >= 86_400.0 {
        bars as f64 * 1.4 * 1.1 + 7.0
    } else {
        // About 6.5 trading hours a day; 24/7 markets are simply over-asked.
        let per_day = (6.5 * 3600.0 / bucket).max(1.0);
        bars as f64 / per_day * 1.4 * 1.1 + 3.0
    };
    chrono::Duration::days((days.ceil() as i64).clamp(1, 7300))
}

/// Download up to `bars` bars of history from the provider the chart uses and store them, so
/// [`load_bars`] and the dataset builder see them. Returns how many bars were stored.
pub(crate) async fn fetch_history(
    storage: &std::sync::Arc<dyn apex_core::ports::storage::StoragePort>,
    source: &std::sync::Arc<dyn apex_core::ports::market_data::MarketDataPort>,
    symbol: &str,
    timeframe: Timeframe,
    bars: usize,
) -> Result<usize, String> {
    let now = Utc::now();
    let from = now - history_window(&timeframe, bars);
    let fetched = source
        .get_historical_ohlcv(&Symbol(symbol.to_string()), timeframe.clone(), from, now)
        .await
        .map_err(|e| format!("Could not download history for {symbol}: {e}"))?;
    store_history(storage, symbol, &timeframe, bars, fetched).await
}

/// Keep the newest `bars` usable bars of what a provider returned, label them with the requested
/// symbol and timeframe (so [`load_bars`] finds them) and write them. Errors when nothing usable
/// came back.
pub(crate) async fn store_history(
    storage: &std::sync::Arc<dyn apex_core::ports::storage::StoragePort>,
    symbol: &str,
    timeframe: &Timeframe,
    bars: usize,
    mut fetched: Vec<OHLCV>,
) -> Result<usize, String> {
    fetched.retain(|b| {
        [b.open, b.high, b.low, b.close].iter().all(|v| v.is_finite()) && b.close > 0.0
    });
    if fetched.is_empty() {
        return Err(format!(
            "The data provider has no {} history for {symbol}. Check the symbol spelling and that the provider covers it.",
            timeframe_label(timeframe)
        ));
    }
    fetched.sort_by_key(|b| b.time);
    fetched.dedup_by_key(|b| b.time);
    let start = fetched.len().saturating_sub(bars);
    let mut kept = fetched.split_off(start);
    for b in &mut kept {
        b.symbol = Symbol(symbol.to_string());
        b.timeframe = timeframe.clone();
    }
    storage
        .write_ohlcv(&kept)
        .await
        .map_err(|e| format!("Could not store history for {symbol}: {e}"))?;
    Ok(kept.len())
}

/// Get historical data for a symbol from storage.
#[tauri::command]
pub async fn get_historical_data(
    symbol: String,
    timeframe: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<OHLCVDto>, String> {
    let bars = load_bars(&state, &symbol, timeframe.as_deref(), limit.unwrap_or(500)).await?;
    Ok(bars.iter().map(OHLCVDto::from).collect())
}

/// Get watchlist symbols from storage.
#[tauri::command]
pub async fn get_watchlist_symbols(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    // Return symbols from the aggregator's quote cache
    let symbols: Vec<String> = state
        .aggregator
        .quote_cache()
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    Ok(symbols)
}
