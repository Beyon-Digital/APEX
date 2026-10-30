//! Kite instruments master: maps trading symbols to the numeric instrument
//! tokens that the historical-data and KiteTicker APIs require.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use super::{parse_symbol, Bucket, KiteSession};
use apex_core::domain::models::Symbol;

const REFRESH_AFTER_HOURS: i64 = 12;

#[derive(Debug, Clone)]
pub struct Instrument {
    pub token: u32,
    pub tradingsymbol: String,
    pub name: String,
    pub exchange: String,
    pub segment: String,
    pub instrument_type: String,
    pub lot_size: u32,
    pub tick_size: f64,
}

impl Instrument {
    pub fn key(&self) -> String {
        format!("{}:{}", self.exchange, self.tradingsymbol)
    }

    /// The APEX-facing symbol: `RELIANCE.NS`, `TCS.BO`, or `NFO:NIFTY…`.
    pub fn apex_symbol(&self) -> String {
        super::to_apex_symbol(&self.exchange, &self.tradingsymbol)
    }

    /// Kite price divisor for binary ticker packets (currency pairs use 1e7).
    pub fn price_divisor(&self) -> f64 {
        match self.token & 0xff {
            3 => 10_000_000.0,
            6 => 10_000.0,
            _ => 100.0,
        }
    }
}

#[derive(Default)]
pub struct InstrumentMaster {
    by_key: RwLock<HashMap<String, Instrument>>,
    by_token: RwLock<HashMap<u32, String>>,
    loaded_at: RwLock<Option<DateTime<Utc>>>,
}

impl InstrumentMaster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded_at.read().unwrap().is_some()
    }

    fn is_fresh(&self) -> bool {
        self.loaded_at
            .read()
            .unwrap()
            .map(|at| Utc::now() - at < Duration::hours(REFRESH_AFTER_HOURS))
            .unwrap_or(false)
    }

    /// Download the master if it is missing or stale.
    pub async fn ensure_loaded(&self, session: &KiteSession) -> Result<()> {
        if self.is_fresh() {
            return Ok(());
        }
        let csv = session.get_text("/instruments", Bucket::General).await?;
        let count = self.load_csv(&csv);
        tracing::info!(count, "Loaded Kite instruments master");
        Ok(())
    }

    /// Parse the instruments CSV, replacing the current contents.
    pub fn load_csv(&self, csv: &str) -> usize {
        let mut by_key = HashMap::new();
        let mut by_token = HashMap::new();
        let mut lines = csv.lines();
        let Some(header) = lines.next() else { return 0 };
        let columns: Vec<&str> = header.split(',').collect();
        let idx = |name: &str| columns.iter().position(|c| *c == name);
        let (Some(i_token), Some(i_sym), Some(i_exch)) =
            (idx("instrument_token"), idx("tradingsymbol"), idx("exchange"))
        else {
            return 0;
        };
        let i_name = idx("name");
        let i_seg = idx("segment");
        let i_type = idx("instrument_type");
        let i_lot = idx("lot_size");
        let i_tick = idx("tick_size");

        for line in lines {
            let fields = split_csv_line(line);
            let get = |i: Option<usize>| i.and_then(|i| fields.get(i)).cloned().unwrap_or_default();
            let Some(token) = fields.get(i_token).and_then(|t| t.parse::<u32>().ok()) else {
                continue;
            };
            let instrument = Instrument {
                token,
                tradingsymbol: fields.get(i_sym).cloned().unwrap_or_default(),
                name: get(i_name),
                exchange: fields.get(i_exch).cloned().unwrap_or_default(),
                segment: get(i_seg),
                instrument_type: get(i_type),
                lot_size: get(i_lot).parse().unwrap_or(1),
                tick_size: get(i_tick).parse().unwrap_or(0.05),
            };
            if instrument.tradingsymbol.is_empty() {
                continue;
            }
            let key = instrument.key();
            by_token.insert(token, key.clone());
            by_key.insert(key, instrument);
        }
        let count = by_key.len();
        *self.by_key.write().unwrap() = by_key;
        *self.by_token.write().unwrap() = by_token;
        *self.loaded_at.write().unwrap() = Some(Utc::now());
        count
    }

    /// Resolve an APEX symbol. Falls back to a NSE→BSE lookup for bare symbols.
    pub fn resolve(&self, symbol: &Symbol) -> Option<Instrument> {
        let parsed = parse_symbol(&symbol.0);
        let by_key = self.by_key.read().unwrap();
        by_key.get(&parsed.key()).cloned().or_else(|| {
            let bare = !symbol.0.contains(':') && !symbol.0.contains('.');
            bare.then(|| by_key.get(&format!("BSE:{}", parsed.tradingsymbol)).cloned())
                .flatten()
        })
    }

    pub fn by_token(&self, token: u32) -> Option<Instrument> {
        let key = self.by_token.read().unwrap().get(&token).cloned()?;
        self.by_key.read().unwrap().get(&key).cloned()
    }

    /// Search tradable instruments for the symbol picker: prefix matches on the
    /// trading symbol first, then name matches. Equities and indices are preferred.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Instrument> {
        let q = query.trim().to_uppercase();
        if q.is_empty() {
            return Vec::new();
        }
        let by_key = self.by_key.read().unwrap();
        let mut scored: Vec<(u8, &Instrument)> = by_key
            .values()
            .filter(|i| matches!(i.exchange.as_str(), "NSE" | "BSE"))
            .filter(|i| matches!(i.instrument_type.as_str(), "EQ" | ""))
            .filter_map(|i| {
                let sym = i.tradingsymbol.to_uppercase();
                let rank = if sym == q {
                    0
                } else if sym.starts_with(&q) {
                    1
                } else if i.name.to_uppercase().contains(&q) {
                    2
                } else {
                    return None;
                };
                let exchange_penalty = if i.exchange == "NSE" { 0 } else { 3 };
                Some((rank + exchange_penalty, i))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.tradingsymbol.cmp(&b.1.tradingsymbol)));
        scored.into_iter().take(limit).map(|(_, i)| i.clone()).collect()
    }
}

/// Split one CSV line, honouring double-quoted fields (instrument names contain commas).
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n\
738561,2885,RELIANCE,\"RELIANCE INDUSTRIES, LTD\",0,,0,0.05,1,EQ,NSE,NSE\n\
128083204,500325,RELIANCE,RELIANCE INDUSTRIES,0,,0,0.05,1,EQ,BSE,BSE\n\
256265,1001,NIFTY 50,NIFTY 50,0,,0,0,1,EQ,INDICES,NSE\n";

    #[test]
    fn loads_and_resolves() {
        let master = InstrumentMaster::new();
        assert_eq!(master.load_csv(CSV), 3);
        let nse = master.resolve(&Symbol("RELIANCE.NS".into())).unwrap();
        assert_eq!(nse.token, 738561);
        assert_eq!(nse.name, "RELIANCE INDUSTRIES, LTD");
        assert_eq!(master.resolve(&Symbol("RELIANCE.BO".into())).unwrap().token, 128083204);
        assert_eq!(master.by_token(738561).unwrap().apex_symbol(), "RELIANCE.NS");
        assert!(master.resolve(&Symbol("AAPL".into())).is_none());
    }

    #[test]
    fn search_prefers_nse_prefix_matches() {
        let master = InstrumentMaster::new();
        master.load_csv(CSV);
        let hits = master.search("reli", 5);
        assert_eq!(hits[0].exchange, "NSE");
        assert_eq!(hits.len(), 2);
        assert!(master.search("industries", 5).len() == 2);
    }

    #[test]
    fn csv_quotes() {
        assert_eq!(split_csv_line("a,\"b,c\",d"), vec!["a", "b,c", "d"]);
    }
}
