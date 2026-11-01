use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::bus::message_bus::{AlertMessage, AlertSeverity, BusMessage, MessageBus, Topic};
use crate::domain::models::*;

/// Alert rule definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertRule {
    PriceAbove {
        symbol: String,
        threshold: f64,
    },
    PriceBelow {
        symbol: String,
        threshold: f64,
    },
    PctChange {
        symbol: String,
        pct: f64,
        window_secs: u64,
    },
    VwapCross {
        symbol: String,
    },
    DailyPnl {
        threshold: f64,
    },
    NewsKeyword {
        pattern: String,
        symbols: Vec<String>,
    },
}

/// Alert delivery method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertDelivery {
    InApp,
    Sound,
    OsNotification,
    Telegram(String),
}

/// Stored alert rule with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAlert {
    pub id: String,
    pub rule: AlertRule,
    pub delivery: Vec<AlertDelivery>,
    pub enabled: bool,
}

/// Upper bound on a PctChange rule's `window_secs` (~1 year). Enforced in
/// `add_rule` and clamped at evaluation — an out-of-range stored rule can
/// never overflow chrono's duration range into negative retention.
pub const MAX_WINDOW_SECS: u64 = 366 * 24 * 3600;

/// Max retained tick samples per symbol before the window compacts.
const WINDOW_SAMPLE_CAP: usize = 50_000;

/// Alert Engine — evaluates rules against market data and emits alerts
pub struct AlertEngine {
    bus: Arc<MessageBus>,
    rules: Arc<tokio::sync::RwLock<Vec<StoredAlert>>>,
    /// Per-rule "currently triggered" latch — an alert fires on the
    /// false→true edge and re-arms once the condition clears, so a
    /// sustained breach emits exactly one notification.
    triggered: Arc<dashmap::DashMap<String, bool>>,
    /// Rolling per-symbol price history for windowed PctChange rules
    /// (pruned to max(24h, largest configured window)).
    price_windows: dashmap::DashMap<String, std::collections::VecDeque<(DateTime<Utc>, f64)>>,
}

impl AlertEngine {
    /// Create a new alert engine
    pub fn new(bus: Arc<MessageBus>) -> Self {
        Self {
            bus,
            rules: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            triggered: Arc::new(dashmap::DashMap::new()),
            price_windows: dashmap::DashMap::new(),
        }
    }

    /// Percentage change of `quote.last` against the price at the start of
    /// the rolling window (`window_secs` back from `quote.time`).
    fn windowed_pct_change(&self, quote: &Quote, window_secs: u64) -> Option<f64> {
        let window = self.price_windows.get(&quote.symbol.0)?;
        let cutoff =
            quote.updated_at - chrono::Duration::seconds(window_secs.min(MAX_WINDOW_SECS) as i64);
        // Reference = newest sample at-or-before the window start.
        // No sample at-or-before the window start means the rolling window
        // isn't filled yet — return None rather than anchoring to the oldest
        // tick (that would let a short move trip a long-window alert).
        let reference = window
            .iter()
            .rev()
            .find(|(t, _)| *t <= cutoff)
            .map(|(_, p)| *p)?;
        if reference.abs() < f64::EPSILON {
            return None;
        }
        Some((quote.last - reference) / reference * 100.0)
    }

    /// Add a new alert rule
    pub async fn add_rule(&self, alert: StoredAlert) {
        info!("Adding alert rule: {:?}", alert.id);
        let mut alert = alert;
        if let AlertRule::PctChange { window_secs, .. } = &mut alert.rule {
            // Clamp defensively — callers validate, but rules also arrive via
            // stored rows / hand-edited input; an overflowing window must not
            // corrupt retention for other symbols.
            *window_secs = (*window_secs).min(MAX_WINDOW_SECS);
        }
        let mut rules = self.rules.write().await;
        rules.retain(|r| r.id != alert.id);
        self.triggered.remove(&alert.id);
        rules.push(alert);
    }

    /// Remove an alert rule by ID
    pub async fn remove_rule(&self, rule_id: &str) -> bool {
        let mut rules = self.rules.write().await;
        let len_before = rules.len();
        rules.retain(|r| r.id != rule_id);
        self.triggered.remove(rule_id);
        rules.len() < len_before
    }

    /// Get all configured rules
    pub async fn get_rules(&self) -> Vec<StoredAlert> {
        self.rules.read().await.clone()
    }

    /// Spawn the background evaluation loop.
    ///
    /// Subscribes to the quote wildcard topic on the message bus and evaluates
    /// every enabled rule against each incoming quote. Without this, rules are
    /// stored but never evaluated.
    pub fn start(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = engine.bus.subscribe(Topic::Quote("*".into()));
            loop {
                match rx.recv().await {
                    Ok(BusMessage::QuoteData(quote)) => {
                        engine.evaluate_quote(&quote).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "Alert engine lagging behind quote stream");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Record a quote into the rolling price window used by PctChange rules.
    /// Retains at least 24h of ticks per symbol, extended to the largest
    /// configured rule window so long-window alerts can always be measured.
    fn record_price_window(&self, quote: &Quote, max_rule_window_secs: u64) {
        let retention = max_rule_window_secs.min(MAX_WINDOW_SECS).max(24 * 3600);
        let cutoff = quote.updated_at - chrono::Duration::seconds(retention as i64);
        let mut window = self
            .price_windows
            .entry(quote.symbol.0.clone())
            .or_default();
        window.push_back((quote.updated_at, quote.last));
        while window.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
            window.pop_front();
        }
        if window.len() > WINDOW_SAMPLE_CAP {
            // Compact rather than pop the front: halve the density of the
            // oldest quarter so a sample still exists at-or-before long rule
            // window starts (blind eviction would make them unmeasurable).
            let quarter = window.len() / 4;
            let mut compacted =
                std::collections::VecDeque::with_capacity(window.len() - quarter / 2 + 1);
            compacted.extend(window.iter().take(quarter).step_by(2).copied());
            compacted.extend(window.iter().skip(quarter).copied());
            *window = compacted;
        }
    }

    /// Evaluate all rules against a quote update
    pub async fn evaluate_quote(&self, quote: &Quote) {
        let rules = self.rules.read().await;
        let max_window = rules
            .iter()
            .filter(|a| a.enabled)
            .filter_map(|a| match &a.rule {
                AlertRule::PctChange { window_secs, .. } => Some(*window_secs),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        self.record_price_window(quote, max_window);
        for stored_alert in rules.iter() {
            if !stored_alert.enabled {
                continue;
            }

            // Quote-driven rules only evaluate against their own symbol — a
            // foreign tick must not touch the trigger latch.
            let rule_symbol = match &stored_alert.rule {
                AlertRule::PriceAbove { symbol, .. }
                | AlertRule::PriceBelow { symbol, .. }
                | AlertRule::PctChange { symbol, .. }
                | AlertRule::VwapCross { symbol } => Some(symbol.as_str()),
                _ => None,
            };
            let symbol = match rule_symbol {
                Some(s) => s,
                None => continue,
            };
            if symbol != quote.symbol.0 {
                continue;
            }

            let fired = match &stored_alert.rule {
                AlertRule::PriceAbove { threshold, .. } => quote.last > *threshold,
                AlertRule::PriceBelow { threshold, .. } => quote.last < *threshold,
                AlertRule::VwapCross { .. } => (quote.last - quote.vwap).abs() < 0.01,
                AlertRule::PctChange {
                    pct, window_secs, ..
                } => {
                    if *window_secs == 0 {
                        // Degenerate zero window = provider's session change.
                        quote.change_pct.abs() >= *pct
                    } else {
                        // A configured window that can't be measured yet must
                        // not fire — never substitute the daily change.
                        self.windowed_pct_change(quote, *window_secs)
                            .map(|change| change.abs() >= *pct)
                            .unwrap_or(false)
                    }
                }
                _ => false,
            };

            let was_triggered = self
                .triggered
                .get(&stored_alert.id)
                .map(|v| *v)
                .unwrap_or(false);
            if fired != was_triggered {
                self.triggered.insert(stored_alert.id.clone(), fired);
                if fired {
                    self.fire_alert(&stored_alert.id, &stored_alert.rule);
                }
            }
        }
    }

    /// Evaluate NewsKeyword rules against an incoming news item — the pattern
    /// matches case-insensitively against headline+summary; `symbols` filters
    /// the item's symbol tags (empty = match any).
    pub async fn evaluate_news(&self, item: &crate::domain::models::NewsItem) {
        let rules = self.rules.read().await;
        let haystack = format!("{} {}", item.headline, item.summary).to_lowercase();
        for stored_alert in rules.iter() {
            if !stored_alert.enabled {
                continue;
            }
            if let AlertRule::NewsKeyword { pattern, symbols } = &stored_alert.rule {
                if !haystack.contains(&pattern.to_lowercase()) {
                    continue;
                }
                // symbols filter: when populated, require a tag intersection
                if !symbols.is_empty()
                    && !symbols
                        .iter()
                        .any(|s| item.symbols.iter().any(|is| is.0.eq_ignore_ascii_case(s)))
                {
                    continue;
                }
                self.fire_alert(&stored_alert.id, &stored_alert.rule);
            }
        }
    }

    /// Evaluate P&L-based alerts
    pub async fn evaluate_pnl(&self, pnl: f64) {
        let rules = self.rules.read().await;
        for stored_alert in rules.iter() {
            if !stored_alert.enabled {
                continue;
            }

            if let AlertRule::DailyPnl { threshold } = &stored_alert.rule {
                let fired = pnl < *threshold;
                let was_triggered = self
                    .triggered
                    .get(&stored_alert.id)
                    .map(|v| *v)
                    .unwrap_or(false);
                if fired != was_triggered {
                    self.triggered.insert(stored_alert.id.clone(), fired);
                    if fired {
                        self.fire_alert(&stored_alert.id, &stored_alert.rule);
                    }
                }
            }
        }
    }

    /// Fire an alert — emit to message bus
    fn fire_alert(&self, rule_id: &str, rule: &AlertRule) {
        let message = match rule {
            AlertRule::PriceAbove { symbol, threshold } => {
                format!("{} price above {:.2}", symbol, threshold)
            }
            AlertRule::PriceBelow { symbol, threshold } => {
                format!("{} price below {:.2}", symbol, threshold)
            }
            AlertRule::VwapCross { symbol } => {
                format!("{} crossed VWAP", symbol)
            }
            AlertRule::DailyPnl { threshold } => {
                format!("Daily P&L below {:.2}", threshold)
            }
            AlertRule::PctChange { symbol, pct, .. } => {
                format!("{} changed by {:.2}%", symbol, pct)
            }
            AlertRule::NewsKeyword { pattern, .. } => {
                format!("News keyword match: {}", pattern)
            }
        };

        info!("Alert fired: {}", message);
        self.bus.publish(
            Topic::Alert,
            BusMessage::AlertFired(AlertMessage {
                rule_id: rule_id.to_string(),
                message,
                severity: AlertSeverity::Warning,
            }),
        );
    }

    /// Get the number of configured rules
    pub async fn rule_count(&self) -> usize {
        self.rules.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_quote(symbol: &str, last: f64) -> Quote {
        Quote {
            symbol: Symbol(symbol.into()),
            bid: last - 0.05,
            ask: last + 0.05,
            last,
            open: last,
            high: last + 1.0,
            low: last - 1.0,
            volume: 10000,
            change_pct: 0.0,
            vwap: last,
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_add_and_get_rules() {
        let bus = Arc::new(MessageBus::new());
        let engine = AlertEngine::new(bus);

        let alert = StoredAlert {
            id: "test-1".into(),
            rule: AlertRule::PriceAbove {
                symbol: "AAPL".into(),
                threshold: 200.0,
            },
            delivery: vec![AlertDelivery::InApp],
            enabled: true,
        };

        engine.add_rule(alert).await;
        assert_eq!(engine.rule_count().await, 1);

        let rules = engine.get_rules().await;
        assert_eq!(rules[0].id, "test-1");
    }

    #[tokio::test]
    async fn test_remove_rule() {
        let bus = Arc::new(MessageBus::new());
        let engine = AlertEngine::new(bus);

        engine
            .add_rule(StoredAlert {
                id: "test-1".into(),
                rule: AlertRule::PriceAbove {
                    symbol: "AAPL".into(),
                    threshold: 200.0,
                },
                delivery: vec![AlertDelivery::InApp],
                enabled: true,
            })
            .await;

        assert!(engine.remove_rule("test-1").await);
        assert_eq!(engine.rule_count().await, 0);
        assert!(!engine.remove_rule("nonexistent").await);
    }

    #[tokio::test]
    async fn test_price_above_alert_fires() {
        let bus = Arc::new(MessageBus::new());
        let mut rx = bus.subscribe(Topic::Alert);
        let engine = AlertEngine::new(bus);

        engine
            .add_rule(StoredAlert {
                id: "price-above-1".into(),
                rule: AlertRule::PriceAbove {
                    symbol: "AAPL".into(),
                    threshold: 150.0,
                },
                delivery: vec![AlertDelivery::InApp],
                enabled: true,
            })
            .await;

        // Quote above threshold — should fire
        let quote = test_quote("AAPL", 155.0);
        engine.evaluate_quote(&quote).await;

        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(msg.is_ok());
    }

    #[tokio::test]
    async fn test_price_below_no_fire() {
        let bus = Arc::new(MessageBus::new());
        let mut rx = bus.subscribe(Topic::Alert);
        let engine = AlertEngine::new(bus);

        engine
            .add_rule(StoredAlert {
                id: "price-above-1".into(),
                rule: AlertRule::PriceAbove {
                    symbol: "AAPL".into(),
                    threshold: 200.0,
                },
                delivery: vec![AlertDelivery::InApp],
                enabled: true,
            })
            .await;

        // Quote below threshold — should NOT fire
        let quote = test_quote("AAPL", 150.0);
        engine.evaluate_quote(&quote).await;

        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(msg.is_err()); // Timeout — no message
    }

    #[tokio::test]
    async fn test_disabled_rule_does_not_fire() {
        let bus = Arc::new(MessageBus::new());
        let mut rx = bus.subscribe(Topic::Alert);
        let engine = AlertEngine::new(bus);

        engine
            .add_rule(StoredAlert {
                id: "disabled-1".into(),
                rule: AlertRule::PriceAbove {
                    symbol: "AAPL".into(),
                    threshold: 100.0,
                },
                delivery: vec![AlertDelivery::InApp],
                enabled: false, // Disabled
            })
            .await;

        let quote = test_quote("AAPL", 155.0);
        engine.evaluate_quote(&quote).await;

        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(msg.is_err()); // No alert for disabled rule
    }

    #[tokio::test]
    async fn test_pnl_alert() {
        let bus = Arc::new(MessageBus::new());
        let mut rx = bus.subscribe(Topic::Alert);
        let engine = AlertEngine::new(bus);

        engine
            .add_rule(StoredAlert {
                id: "pnl-1".into(),
                rule: AlertRule::DailyPnl { threshold: -5000.0 },
                delivery: vec![AlertDelivery::InApp],
                enabled: true,
            })
            .await;

        engine.evaluate_pnl(-6000.0).await;

        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(msg.is_ok());
    }

    #[tokio::test]
    async fn test_huge_window_clamped_not_corrupting() {
        // A u64::MAX window must be clamped at add time — it can never panic
        // the duration math or wipe a symbol's window via negative retention.
        let bus = Arc::new(MessageBus::new());
        let engine = AlertEngine::new(bus);

        engine
            .add_rule(StoredAlert {
                id: "huge-window".into(),
                rule: AlertRule::PctChange {
                    symbol: "AAPL".into(),
                    pct: 1.0,
                    window_secs: u64::MAX,
                },
                delivery: vec![AlertDelivery::InApp],
                enabled: true,
            })
            .await;

        let stored = engine.get_rules().await;
        let window = match &stored[0].rule {
            AlertRule::PctChange { window_secs, .. } => *window_secs,
            _ => panic!("wrong rule"),
        };
        assert_eq!(window, MAX_WINDOW_SECS);

        // Recording a quote must not wipe the window (negative-retention bug).
        engine.evaluate_quote(&test_quote("AAPL", 100.0)).await;
        engine.evaluate_quote(&test_quote("AAPL", 101.0)).await;
        assert!(
            engine
                .price_windows
                .get("AAPL")
                .map(|w| w.len())
                .unwrap_or(0)
                >= 1
        );
    }

    #[test]
    fn test_compaction_keeps_reference_for_long_windows() {
        // >50k ticks inside a long window: compaction downsamples the oldest
        // quarter instead of evicting the reference a long rule needs.
        let bus = Arc::new(MessageBus::new());
        let engine = AlertEngine::new(bus);

        let window_secs = 100 * 3600; // 100h rule window
        let start = Utc::now() - chrono::Duration::hours(100);
        for i in 0..60_000i64 {
            let mut q = test_quote("AAPL", 100.0);
            q.updated_at = start + chrono::Duration::seconds(i * 6); // ~1 tick/6s
            engine.record_price_window(&q, window_secs);
        }

        let len = engine
            .price_windows
            .get("AAPL")
            .map(|w| w.len())
            .unwrap_or(0);
        assert!(len <= 60_000 && len > 0);
        // Compaction must have left coverage at the rule's window start.
        let mut q = test_quote("AAPL", 200.0);
        q.updated_at = Utc::now();
        let change = engine.windowed_pct_change(&q, window_secs);
        assert!(change.is_some(), "long window lost its reference sample");
    }
}
