use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, warn};

use crate::domain::models::*;

/// Data quality checker for market data
///
/// Validates incoming ticks and OHLCV bars to ensure data integrity
/// and detect anomalies before storage or processing.
pub struct DataQualityChecker {
    /// Maximum allowed price (to detect data errors)
    max_price: f64,
    /// Minimum allowed price (to detect data errors)
    min_price: f64,
    /// Maximum allowed volume (to detect data errors)
    max_volume: u64,
    /// Maximum allowed timestamp drift (seconds)
    max_timestamp_drift: i64,
}

impl Default for DataQualityChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl DataQualityChecker {
    /// Create a new data quality checker with default thresholds
    pub fn new() -> Self {
        Self {
            max_price: 1_000_000_000.0,        // 1 billion
            min_price: 0.00000001,             // 1 satoshi
            max_volume: 1_000_000_000_000_000, // 1 quadrillion
            max_timestamp_drift: 3600,         // 1 hour
        }
    }

    /// Create a new data quality checker with custom thresholds
    pub fn with_thresholds(
        max_price: f64,
        min_price: f64,
        max_volume: u64,
        max_timestamp_drift: i64,
    ) -> Self {
        Self {
            max_price,
            min_price,
            max_volume,
            max_timestamp_drift,
        }
    }

    /// Validate a single tick
    pub fn validate_tick(&self, tick: &Tick) -> Result<()> {
        // Check timestamp is reasonable
        let now = Utc::now();
        let time_diff = (now - tick.time).num_seconds().abs();

        if time_diff > self.max_timestamp_drift {
            return Err(anyhow!(
                "Tick timestamp drift too large: {} seconds (max: {})",
                time_diff,
                self.max_timestamp_drift
            ));
        }

        self.validate_tick_values(tick)
    }

    /// Validate the contents of a tick independently of its age. A market
    /// close can be shown as a dated reference, but never as a live tick.
    pub fn validate_tick_values(&self, tick: &Tick) -> Result<()> {
        if tick
            .open
            .is_some_and(|value| !value.is_finite() || value <= 0.0 || value > self.max_price)
            || tick.change_pct.is_some_and(|value| !value.is_finite())
        {
            return Err(anyhow!("Invalid optional quote values"));
        }
        // Check prices are reasonable
        if !tick.last.is_finite() || tick.last <= 0.0 || tick.last > self.max_price {
            return Err(anyhow!(
                "Invalid last price: {} (must be > {} and < {})",
                tick.last,
                self.min_price,
                self.max_price
            ));
        }

        if !tick.bid.is_finite() || tick.bid <= 0.0 || tick.bid > self.max_price {
            return Err(anyhow!(
                "Invalid bid price: {} (must be > {} and < {})",
                tick.bid,
                self.min_price,
                self.max_price
            ));
        }

        if !tick.ask.is_finite() || tick.ask <= 0.0 || tick.ask > self.max_price {
            return Err(anyhow!(
                "Invalid ask price: {} (must be > {} and < {})",
                tick.ask,
                self.min_price,
                self.max_price
            ));
        }

        // Check bid-ask spread is reasonable (ask should be >= bid)
        if tick.ask < tick.bid {
            return Err(anyhow!(
                "Invalid bid-ask spread: ask {} < bid {}",
                tick.ask,
                tick.bid
            ));
        }

        // Check volume is reasonable
        if tick.volume > self.max_volume {
            return Err(anyhow!(
                "Invalid volume: {} (max: {})",
                tick.volume,
                self.max_volume
            ));
        }

        // Check symbol is not empty
        if tick.symbol.0.is_empty() {
            return Err(anyhow!("Empty symbol in tick"));
        }

        // Check source is not empty
        if tick.source.is_empty() {
            return Err(anyhow!("Empty source in tick"));
        }

        Ok(())
    }

    /// Validate a batch of ticks, returning only valid ones
    pub fn validate_ticks(&self, ticks: &[Tick]) -> Result<Vec<Tick>> {
        let mut valid_ticks = Vec::with_capacity(ticks.len());
        let mut rejected_count = 0;

        for tick in ticks {
            match self.validate_tick(tick) {
                Ok(_) => valid_ticks.push(tick.clone()),
                Err(e) => {
                    rejected_count += 1;
                    warn!("Rejected tick for {}: {}", tick.symbol.0, e);
                }
            }
        }

        if rejected_count > 0 {
            debug!("Rejected {} out of {} ticks", rejected_count, ticks.len());
        }

        Ok(valid_ticks)
    }

    /// Validate a single OHLCV bar
    pub fn validate_ohlcv(&self, bar: &OHLCV) -> Result<()> {
        // Check timestamp is reasonable
        let now = Utc::now();
        let time_diff = (now - bar.time).num_seconds().abs();

        if time_diff > self.max_timestamp_drift {
            return Err(anyhow!(
                "OHLCV timestamp drift too large: {} seconds (max: {})",
                time_diff,
                self.max_timestamp_drift
            ));
        }

        // Check prices are reasonable
        if bar.open <= 0.0 || bar.open > self.max_price {
            return Err(anyhow!(
                "Invalid open price: {} (must be > {} and < {})",
                bar.open,
                self.min_price,
                self.max_price
            ));
        }

        if bar.high <= 0.0 || bar.high > self.max_price {
            return Err(anyhow!(
                "Invalid high price: {} (must be > {} and < {})",
                bar.high,
                self.min_price,
                self.max_price
            ));
        }

        if bar.low <= 0.0 || bar.low > self.max_price {
            return Err(anyhow!(
                "Invalid low price: {} (must be > {} and < {})",
                bar.low,
                self.min_price,
                self.max_price
            ));
        }

        if bar.close <= 0.0 || bar.close > self.max_price {
            return Err(anyhow!(
                "Invalid close price: {} (must be > {} and < {})",
                bar.close,
                self.min_price,
                self.max_price
            ));
        }

        // Check OHLC relationships: high >= open, high >= close, high >= low
        if bar.high < bar.open || bar.high < bar.close || bar.high < bar.low {
            return Err(anyhow!(
                "Invalid OHLC relationship: high {} < open/close/low",
                bar.high
            ));
        }

        // Check OHLC relationships: low <= open, low <= close, low <= high
        if bar.low > bar.open || bar.low > bar.close || bar.low > bar.high {
            return Err(anyhow!(
                "Invalid OHLC relationship: low {} > open/close/high",
                bar.low
            ));
        }

        // Check volume is reasonable
        if bar.volume > self.max_volume {
            return Err(anyhow!(
                "Invalid volume: {} (max: {})",
                bar.volume,
                self.max_volume
            ));
        }

        // Check symbol is not empty
        if bar.symbol.0.is_empty() {
            return Err(anyhow!("Empty symbol in OHLCV"));
        }

        Ok(())
    }

    /// Validate a batch of OHLCV bars, returning only valid ones
    pub fn validate_ohlcv_bars(&self, bars: &[OHLCV]) -> Result<Vec<OHLCV>> {
        let mut valid_bars = Vec::with_capacity(bars.len());
        let mut rejected_count = 0;

        for bar in bars {
            match self.validate_ohlcv(bar) {
                Ok(_) => valid_bars.push(bar.clone()),
                Err(e) => {
                    rejected_count += 1;
                    warn!("Rejected OHLCV bar for {}: {}", bar.symbol.0, e);
                }
            }
        }

        if rejected_count > 0 {
            debug!(
                "Rejected {} out of {} OHLCV bars",
                rejected_count,
                bars.len()
            );
        }

        Ok(valid_bars)
    }

    /// Check for gaps in tick timestamps
    pub fn detect_tick_gaps(
        &self,
        ticks: &[Tick],
        max_gap_seconds: i64,
    ) -> Vec<(DateTime<Utc>, DateTime<Utc>, i64)> {
        let mut gaps = Vec::new();

        if ticks.len() < 2 {
            return gaps;
        }

        let mut sorted_ticks = ticks.to_vec();
        sorted_ticks.sort_by_key(|t| t.time);

        for window in sorted_ticks.windows(2) {
            let gap = (window[1].time - window[0].time).num_seconds();
            if gap > max_gap_seconds {
                gaps.push((window[0].time, window[1].time, gap));
            }
        }

        gaps
    }

    /// Check for duplicate ticks
    pub fn detect_duplicate_ticks(&self, ticks: &[Tick]) -> Vec<Tick> {
        let mut seen = std::collections::HashSet::new();
        let mut duplicates = Vec::new();

        for tick in ticks {
            // Use price as integer for hashing (multiply by 100 for 2 decimal precision)
            let price_int = (tick.last * 100.0) as i64;
            let key = (tick.time, tick.symbol.0.clone(), price_int);
            if seen.contains(&key) {
                duplicates.push(tick.clone());
            } else {
                seen.insert(key);
            }
        }

        duplicates
    }

    /// Check for price anomalies (sudden large changes)
    pub fn detect_price_anomalies(&self, ticks: &[Tick], max_change_pct: f64) -> Vec<(Tick, f64)> {
        let mut anomalies = Vec::new();

        if ticks.len() < 2 {
            return anomalies;
        }

        let mut sorted_ticks = ticks.to_vec();
        sorted_ticks.sort_by_key(|t| t.time);

        for window in sorted_ticks.windows(2) {
            // Only compare same symbol
            if window[0].symbol != window[1].symbol {
                continue;
            }

            if window[0].last > 0.0 {
                let change_pct = ((window[1].last - window[0].last) / window[0].last).abs();
                if change_pct > max_change_pct {
                    anomalies.push((window[1].clone(), change_pct));
                }
            }
        }

        anomalies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_tick() {
        let checker = DataQualityChecker::new();
        let tick = Tick {
            time: Utc::now(),
            symbol: Symbol("BTC/USDT".into()),
            bid: 50000.0,
            ask: 50001.0,
            last: 50000.5,
            volume: 1000,
            source: "binance".into(),

            open: None,

            change_pct: None,
        };

        assert!(checker.validate_tick(&tick).is_ok());
    }

    #[test]
    fn test_validate_invalid_price() {
        let checker = DataQualityChecker::new();
        let mut tick = Tick {
            time: Utc::now(),
            symbol: Symbol("BTC/USDT".into()),
            bid: 50000.0,
            ask: 50001.0,
            last: -1.0,
            volume: 1000,
            source: "binance".into(),

            open: None,

            change_pct: None,
        };

        assert!(checker.validate_tick(&tick).is_err());

        tick.last = 1_000_000_001.0;
        assert!(checker.validate_tick(&tick).is_err());
    }

    #[test]
    fn test_validate_invalid_bid_ask() {
        let checker = DataQualityChecker::new();
        let tick = Tick {
            time: Utc::now(),
            symbol: Symbol("BTC/USDT".into()),
            bid: 50001.0,
            ask: 50000.0,
            last: 50000.5,
            volume: 1000,
            source: "binance".into(),

            open: None,

            change_pct: None,
        };

        assert!(checker.validate_tick(&tick).is_err());
    }

    #[test]
    fn test_validate_valid_ohlcv() {
        let checker = DataQualityChecker::new();
        let bar = OHLCV {
            time: Utc::now(),
            symbol: Symbol("BTC/USDT".into()),
            timeframe: Timeframe::D1,
            open: 50000.0,
            high: 50100.0,
            low: 49900.0,
            close: 50050.0,
            volume: 1000,
        };

        assert!(checker.validate_ohlcv(&bar).is_ok());
    }

    #[test]
    fn test_validate_invalid_ohlcv() {
        let checker = DataQualityChecker::new();
        let mut bar = OHLCV {
            time: Utc::now(),
            symbol: Symbol("BTC/USDT".into()),
            timeframe: Timeframe::D1,
            open: 50000.0,
            high: 49800.0, // Invalid: high < open
            low: 49900.0,
            close: 50050.0,
            volume: 1000,
        };

        assert!(checker.validate_ohlcv(&bar).is_err());

        bar.high = 50100.0;
        bar.low = 50200.0; // Invalid: low > high
        assert!(checker.validate_ohlcv(&bar).is_err());
    }

    #[test]
    fn test_detect_tick_gaps() {
        let checker = DataQualityChecker::new();
        let ticks = vec![
            Tick {
                time: Utc::now(),
                symbol: Symbol("BTC/USDT".into()),
                bid: 50000.0,
                ask: 50001.0,
                last: 50000.5,
                volume: 1000,
                source: "binance".into(),
                open: None,
                change_pct: None,
            },
            Tick {
                time: Utc::now() + chrono::Duration::seconds(100),
                symbol: Symbol("BTC/USDT".into()),
                bid: 50001.0,
                ask: 50002.0,
                last: 50001.5,
                volume: 1000,
                source: "binance".into(),
                open: None,
                change_pct: None,
            },
        ];

        let gaps = checker.detect_tick_gaps(&ticks, 10);
        assert_eq!(gaps.len(), 1);
    }

    #[test]
    fn test_detect_duplicates() {
        let checker = DataQualityChecker::new();
        let time = Utc::now();
        let ticks = vec![
            Tick {
                time,
                symbol: Symbol("BTC/USDT".into()),
                bid: 50000.0,
                ask: 50001.0,
                last: 50000.5,
                volume: 1000,
                source: "binance".into(),
                open: None,
                change_pct: None,
            },
            Tick {
                time,
                symbol: Symbol("BTC/USDT".into()),
                bid: 50000.0,
                ask: 50001.0,
                last: 50000.5,
                volume: 1000,
                source: "binance".into(),
                open: None,
                change_pct: None,
            },
        ];

        let duplicates = checker.detect_duplicate_ticks(&ticks);
        assert_eq!(duplicates.len(), 1);
    }

    #[test]
    fn test_detect_price_anomalies() {
        let checker = DataQualityChecker::new();
        let ticks = vec![
            Tick {
                time: Utc::now(),
                symbol: Symbol("BTC/USDT".into()),
                bid: 50000.0,
                ask: 50001.0,
                last: 50000.0,
                volume: 1000,
                source: "binance".into(),
                open: None,
                change_pct: None,
            },
            Tick {
                time: Utc::now() + chrono::Duration::seconds(1),
                symbol: Symbol("BTC/USDT".into()),
                bid: 60000.0,
                ask: 60001.0,
                last: 60000.0,
                volume: 1000,
                source: "binance".into(),
                open: None,
                change_pct: None,
            },
        ];

        let anomalies = checker.detect_price_anomalies(&ticks, 0.10); // 10% threshold
        assert_eq!(anomalies.len(), 1);
        assert!(anomalies[0].1 > 0.10); // 20% change
    }
}
