use crate::dto::{OHLCVDto, QuoteDto, SymbolSearchResultDto};
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
    let syms: Vec<Symbol> = symbols.into_iter().map(Symbol).collect();
    state
        .aggregator
        .start(&syms)
        .await
        .map_err(|e| e.to_string())
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
    state.aggregator.unsubscribe(&syms).await;
    Ok(())
}

/// Symbol picker search: Zerodha instruments when connected, plus Yahoo Finance.
#[tauri::command]
pub async fn search_symbols(
    query: String,
    state: State<'_, AppState>,
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
