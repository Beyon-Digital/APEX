use crate::dto::{FeedOptionDto, OHLCVDto, QuoteDto, SymbolFeedDto, SymbolSearchResultDto};
use crate::state::AppState;
use crate::validation;
use apex_core::domain::models::Symbol;
use chrono::{DateTime, Utc};
use tauri::State;

/// Get a quote for a symbol from the cache.
#[tauri::command]
pub async fn get_quote(symbol: String, state: State<'_, AppState>) -> Result<QuoteDto, String> {
    validation::validate_symbol(&symbol)?;

    state
        .aggregator
        .get_display_quote(&symbol)
        .map(|q| QuoteDto::from(&q))
        .ok_or_else(|| format!("No quote available for {}", symbol))
}

/// Get historical OHLCV data for a symbol.
#[tauri::command]
pub async fn get_ohlcv(
    symbol: String,
    timeframe: String,
    from: i64,
    to: i64,
    state: State<'_, AppState>,
) -> Result<Vec<OHLCVDto>, String> {
    use apex_core::domain::models::Timeframe;

    let tf = match timeframe.to_lowercase().as_str() {
        "1s" | "s1" => Timeframe::S1,
        "5s" | "s5" => Timeframe::S5,
        "15s" | "s15" => Timeframe::S15,
        "1m" | "m1" => Timeframe::M1,
        "3m" | "m3" => Timeframe::M3,
        "5m" | "m5" => Timeframe::M5,
        "15m" | "m15" => Timeframe::M15,
        "30m" | "m30" => Timeframe::M30,
        "1h" | "h1" => Timeframe::H1,
        "4h" | "h4" => Timeframe::H4,
        "1d" | "d1" => Timeframe::D1,
        "1w" | "w1" => Timeframe::W1,
        _ => return Err(format!("Invalid timeframe: {}", timeframe)),
    };

    let from_dt = DateTime::<Utc>::from_timestamp(from, 0)
        .ok_or_else(|| "Invalid from timestamp".to_string())?;
    let to_dt =
        DateTime::<Utc>::from_timestamp(to, 0).ok_or_else(|| "Invalid to timestamp".to_string())?;

    // Query from storage
    let params = apex_core::domain::models::OHLCVQuery {
        symbol: Symbol(symbol.clone()),
        timeframe: tf,
        from: from_dt,
        to: to_dt,
        limit: Some(1000),
    };

    match state.storage.query_ohlcv(params).await {
        Ok(bars) if !bars.is_empty() => Ok(bars.iter().map(OHLCVDto::from).collect()),
        _ => {
            // Fall back to aggregator adapter for historical data
            Err(format!(
                "No historical data available for {} in timeframe {}",
                symbol, timeframe
            ))
        }
    }
}

/// Subscribe to real-time market data for symbols.
#[tauri::command]
pub async fn subscribe_symbols(
    symbols: Vec<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if symbols.len() > 500 {
        return Err("Too many symbols in one request".into());
    }
    for symbol in &symbols {
        validation::validate_symbol(symbol)?;
    }
    let syms: Vec<Symbol> = symbols.into_iter().map(Symbol).collect();
    state
        .aggregator
        .start(&syms)
        .await
        .map_err(|e| e.to_string())
}

/// Symbols something other than the watchlist still needs live: running strategy
/// sessions, price alerts and automations. Removing a ticker from the watchlist
/// must not cut their data.
async fn symbols_in_use(state: &AppState) -> std::collections::HashSet<String> {
    let mut keep = std::collections::HashSet::new();
    for session in crate::commands::strategy_worker::session_details() {
        if session.state == "running" || session.state == "starting" {
            keep.insert(session.symbol);
        }
    }
    for rule in state.automations.list() {
        if rule.enabled {
            keep.insert(rule.symbol);
        }
    }
    if let Ok(alerts) = state.alert_store.list().await {
        for alert in alerts {
            if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(&alert.rule) {
                // AlertRule serializes as { "Variant": { "symbol": ... } }.
                for inner in map.values() {
                    if let Some(symbol) = inner.get("symbol").and_then(|s| s.as_str()) {
                        keep.insert(symbol.to_string());
                    }
                }
            }
        }
    }
    keep
}

/// Stop streaming symbols that were removed from the watchlist.
#[tauri::command]
pub async fn unsubscribe_symbols(
    symbols: Vec<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    for symbol in &symbols {
        validation::validate_symbol(symbol)?;
    }
    let syms: Vec<Symbol> = symbols.into_iter().map(Symbol).collect();
    let keep = symbols_in_use(&state).await;
    state.aggregator.unsubscribe_except(&syms, &keep).await;
    Ok(())
}

/// Symbol picker search: Zerodha instruments when connected, plus Yahoo Finance.
#[tauri::command]
pub async fn search_symbols(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<SymbolSearchResultDto>, String> {
    search_symbols_inner(state.inner(), &query).await
}

/// Shared by the IPC command and the AI copilot.
pub(crate) async fn search_symbols_inner(
    state: &AppState,
    query: &str,
) -> Result<Vec<SymbolSearchResultDto>, String> {
    const LIMIT: usize = 12;
    let query = query.trim().to_string();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    validation::validate_string_length(&query, "query")?;

    let mut results: Vec<SymbolSearchResultDto> = state
        .search_zerodha_instruments(&query, LIMIT)
        .await
        .into_iter()
        .map(|i| SymbolSearchResultDto {
            symbol: i.apex_symbol(),
            name: i.name.clone(),
            exchange: i.exchange.clone(),
            source: "zerodha".to_string(),
        })
        .collect();

    if let Ok(response) = state
        .http
        .get("https://query1.finance.yahoo.com/v1/finance/search")
        .query(&[("q", query.as_str()), ("quotesCount", "10"), ("newsCount", "0")])
        .timeout(std::time::Duration::from_secs(4))
        .send()
        .await
    {
        if let Ok(body) = response.json::<serde_json::Value>().await {
            for quote in body.get("quotes").and_then(|q| q.as_array()).into_iter().flatten() {
                let Some(symbol) = quote.get("symbol").and_then(|s| s.as_str()) else { continue };
                if validation::validate_symbol(symbol).is_err() {
                    continue;
                }
                let name = ["longname", "shortname"]
                    .iter()
                    .find_map(|k| quote.get(*k).and_then(|v| v.as_str()))
                    .unwrap_or_default();
                let exchange = quote.get("exchDisp").and_then(|v| v.as_str()).unwrap_or_default();
                results.push(SymbolSearchResultDto {
                    symbol: symbol.to_string(),
                    name: name.to_string(),
                    exchange: exchange.to_string(),
                    source: "yahoo".to_string(),
                });
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    results.retain(|r| seen.insert(r.symbol.to_uppercase()));
    results.truncate(LIMIT);
    Ok(results)
}

/// Friendly name and data quality for a feed's adapter id.
pub(crate) fn describe_feed(adapter_id: &str) -> (String, &'static str) {
    match adapter_id {
        "yahoo" | "yahoo_finance" => ("Yahoo Finance".into(), "delayed"),
        "zerodha_kite" => ("Zerodha Kite".into(), "live"),
        "angel_one" => ("Angel One".into(), "live"),
        "groww" => ("Groww".into(), "live"),
        "robinhood" => ("Robinhood".into(), "live"),
        "binance" => ("Binance".into(), "exchange"),
        "coinbase" => ("Coinbase".into(), "exchange"),
        "polymarket" => ("Polymarket".into(), "exchange"),
        other => (other.replace('_', " "), "live"),
    }
}

/// For each symbol: the feed serving it now, the user's pinned choice, and every
/// feed that could serve it (with the reason when one can't).
#[tauri::command]
pub async fn get_symbol_feeds(
    symbols: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<SymbolFeedDto>, String> {
    if symbols.len() > 200 {
        return Err("Too many symbols".into());
    }
    let mut out = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        validation::validate_symbol(&symbol)?;
        let options = state
            .aggregator
            .feed_options(&Symbol(symbol.clone()))
            .into_iter()
            .map(|o| {
                let (name, quality) = describe_feed(&o.adapter_id);
                let unavailable_reason = if !o.ready {
                    Some("Not connected. Add or unlock it under Settings > Accounts.".to_string())
                } else if !o.supports {
                    Some("Doesn't cover this symbol.".to_string())
                } else {
                    None
                };
                FeedOptionDto {
                    adapter_id: o.adapter_id,
                    name,
                    quality: quality.to_string(),
                    supports: o.supports,
                    ready: o.ready,
                    unavailable_reason,
                }
            })
            .collect();
        out.push(SymbolFeedDto {
            active: state.aggregator.quote_source(&symbol),
            preferred: state.aggregator.provider_override(&symbol),
            symbol,
            options,
        });
    }
    Ok(out)
}

/// Pin a symbol to one feed, or pass `None` to return to automatic selection.
#[tauri::command]
pub async fn set_symbol_provider(
    symbol: String,
    adapter_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validation::validate_symbol(&symbol)?;
    if let Some(id) = &adapter_id {
        validation::validate_string_length(id, "adapter_id")?;
        let known = state
            .aggregator
            .feed_options(&Symbol(symbol.clone()))
            .iter()
            .any(|o| &o.adapter_id == id);
        if !known {
            return Err(format!("Unknown data feed: {id}"));
        }
    }
    state.aggregator.set_provider_override(&symbol, adapter_id.as_deref());
    // Make sure the chosen feed is actually streaming this symbol.
    state
        .aggregator
        .start(&[Symbol(symbol)])
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod feed_tests {
    use super::describe_feed;

    #[test]
    fn feeds_have_friendly_names_and_quality() {
        assert_eq!(describe_feed("yahoo"), ("Yahoo Finance".to_string(), "delayed"));
        assert_eq!(describe_feed("zerodha_kite").1, "live");
        assert_eq!(describe_feed("binance").1, "exchange");
        assert_eq!(describe_feed("some_custom").0, "some custom");
    }
}
