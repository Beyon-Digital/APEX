//! Durable alert rules shared by the UI and Copilot.
//!
//! The evaluator's trigger latch remains session-scoped; rule definitions do
//! not. Serialize writes and replace the file atomically before updating the
//! running engine, so a failed save cannot masquerade as a durable rule.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use apex_core::application::alert_engine::{AlertEngine, AlertRule, StoredAlert};
use apex_core::application::market_data_aggregator::MarketDataAggregator;
use apex_core::domain::models::Symbol;
use tokio::sync::Mutex;

pub struct PersistentAlerts {
    engine: Arc<AlertEngine>,
    aggregator: Arc<MarketDataAggregator>,
    path: PathBuf,
    guard: Mutex<()>,
    load_error: Option<String>,
}

fn quote_symbol(rule: &AlertRule) -> Option<&str> {
    match rule {
        AlertRule::PriceAbove { symbol, .. }
        | AlertRule::PriceBelow { symbol, .. }
        | AlertRule::PctChange { symbol, .. }
        | AlertRule::VwapCross { symbol } => Some(symbol),
        _ => None,
    }
}

impl PersistentAlerts {
    pub async fn load(
        engine: Arc<AlertEngine>,
        aggregator: Arc<MarketDataAggregator>,
        path: PathBuf,
    ) -> Self {
        let (rules, load_error) = match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice::<Vec<StoredAlert>>(&bytes) {
                Ok(rules) => (rules, None),
                Err(err) => (
                    Vec::new(),
                    Some(format!(
                        "Could not read saved alerts: {err}. The saved file was preserved."
                    )),
                ),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(err) => (
                Vec::new(),
                Some(format!(
                    "Could not read saved alerts: {err}. The saved file was preserved."
                )),
            ),
        };
        if let Some(error) = &load_error {
            tracing::error!(%error);
        }
        let mut symbols = Vec::new();
        for rule in rules {
            if rule.enabled {
                if let Some(symbol) = quote_symbol(&rule.rule) {
                    if !symbols.iter().any(|s: &Symbol| s.0 == symbol) {
                        symbols.push(Symbol(symbol.to_string()));
                    }
                }
            }
            engine.add_rule(rule).await;
        }
        if !symbols.is_empty() {
            if let Err(error) = aggregator.start(&symbols).await {
                tracing::warn!(%error, "Could not restore alert quote subscriptions");
            }
        }
        Self {
            engine,
            aggregator,
            path,
            guard: Mutex::new(()),
            load_error,
        }
    }

    fn check_loaded(&self) -> Result<(), String> {
        match &self.load_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    pub async fn list(&self) -> Result<Vec<StoredAlert>, String> {
        self.check_loaded()?;
        Ok(self.engine.get_rules().await)
    }

    pub async fn add(&self, rule: StoredAlert) -> Result<(), String> {
        let _guard = self.guard.lock().await;
        self.check_loaded()?;
        if rule.enabled {
            if let Some(symbol) = quote_symbol(&rule.rule) {
                crate::validation::validate_symbol(symbol)?;
                self.aggregator
                    .start(&[Symbol(symbol.to_string())])
                    .await
                    .map_err(|error| format!("Could not subscribe to {symbol}: {error}"))?;
            }
        }
        let mut rules = self.engine.get_rules().await;
        rules.retain(|existing| existing.id != rule.id);
        rules.push(rule.clone());
        persist(&self.path, &rules).await?;
        self.engine.add_rule(rule).await;
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> Result<bool, String> {
        let _guard = self.guard.lock().await;
        self.check_loaded()?;
        let mut rules = self.engine.get_rules().await;
        let previous = rules.len();
        rules.retain(|rule| rule.id != id);
        if rules.len() == previous {
            return Ok(false);
        }
        persist(&self.path, &rules).await?;
        self.engine.remove_rule(id).await;
        Ok(true)
    }
}

async fn persist(path: &Path, rules: &[StoredAlert]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid alerts storage path")?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("Could not save alerts: {e}"))?;
    let bytes =
        serde_json::to_vec_pretty(rules).map_err(|e| format!("Could not save alerts: {e}"))?;
    let temp = parent.join(format!(".alerts-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(error) = tokio::fs::write(&temp, bytes).await {
        return Err(format!("Could not save alerts: {error}"));
    }
    if let Err(error) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(format!("Could not save alerts: {error}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_core::application::alert_engine::AlertDelivery;
    use apex_core::bus::message_bus::MessageBus;

    fn engines() -> (Arc<AlertEngine>, Arc<MarketDataAggregator>) {
        let bus = Arc::new(MessageBus::new());
        (
            Arc::new(AlertEngine::new(bus.clone())),
            Arc::new(MarketDataAggregator::new(bus)),
        )
    }
    fn path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("apex-alert-test-{}", uuid::Uuid::new_v4()))
            .join("alerts.json")
    }
    fn rule(id: &str) -> StoredAlert {
        StoredAlert {
            id: id.into(),
            rule: AlertRule::DailyPnl { threshold: -100.0 },
            delivery: vec![AlertDelivery::InApp],
            enabled: true,
        }
    }

    #[tokio::test]
    async fn survives_reload_and_deletion() {
        let path = path();
        let (engine, aggregator) = engines();
        let store = PersistentAlerts::load(engine, aggregator, path.clone()).await;
        store.add(rule("one")).await.unwrap();
        store.add(rule("one")).await.unwrap();
        let (engine, aggregator) = engines();
        let reloaded = PersistentAlerts::load(engine, aggregator, path.clone()).await;
        assert_eq!(reloaded.list().await.unwrap().len(), 1);
        assert!(reloaded.remove("one").await.unwrap());
        let (engine, aggregator) = engines();
        assert!(PersistentAlerts::load(engine, aggregator, path.clone())
            .await
            .list()
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn corrupted_file_is_preserved_and_writes_are_rejected() {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        let (engine, aggregator) = engines();
        let store = PersistentAlerts::load(engine, aggregator, path.clone()).await;
        assert!(store.list().await.is_err());
        assert!(store.add(rule("one")).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn failed_save_does_not_update_running_rules() {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let (engine, aggregator) = engines();
        let store = PersistentAlerts::load(engine.clone(), aggregator, path.clone()).await;
        // Make the destination a directory after load so the atomic rename fails.
        std::fs::create_dir(&path).unwrap();
        assert!(store.add(rule("not-saved")).await.is_err());
        assert!(engine.get_rules().await.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
