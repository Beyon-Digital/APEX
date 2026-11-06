//! Durable alert rules shared by the UI and Copilot.
//!
//! The evaluator's trigger latch remains session-scoped; rule definitions do
//! not. Serialize writes and replace the file atomically before updating the
//! running engine, so a failed save cannot masquerade as a durable rule.
//!
//! Every rule is also a versioned object of the audit ledger (kind `alert`, object id = rule id,
//! content = canonical JSON of the stored rule). Creating or replacing a rule appends a version,
//! deleting one appends a delete marker, and an older version can be restored. The ledger is written
//! FIRST: when it cannot be, the change is refused and nothing else is touched. The alerts file stays
//! the live copy the engine is loaded from. When the file write fails after the ledger write, a
//! compensating version records that the change did not take effect.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use apex_core::application::alert_engine::{AlertEngine, AlertRule, StoredAlert};
use apex_core::application::market_data_aggregator::MarketDataAggregator;
use apex_core::domain::models::Symbol;
use tokio::sync::Mutex;

use super::versioned_files;
use crate::ledger::{Kind, Ledger, VersionRecord};

pub struct PersistentAlerts {
    engine: Arc<AlertEngine>,
    aggregator: Arc<MarketDataAggregator>,
    path: PathBuf,
    guard: Mutex<()>,
    load_error: Option<String>,
    /// Tests use a private ledger; the app uses the process-wide one.
    ledger: Option<Ledger>,
}

/// Canonical JSON of a stored rule: the ledger content of an alert.
pub(crate) fn alert_content(rule: &StoredAlert) -> Result<String, String> {
    let value = serde_json::to_value(rule).map_err(|e| format!("could not encode the alert: {e}"))?;
    Ok(versioned_files::canonical_json(&value))
}

fn describe(rule: &StoredAlert) -> String {
    let text = serde_json::to_string(&rule.rule).unwrap_or_default();
    let enabled = if rule.enabled { "" } else { " (disabled)" };
    format!("{text}{enabled}")
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
            ledger: None,
        }
    }

    /// Use a specific ledger instead of the process-wide one (tests).
    #[cfg(test)]
    fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = Some(ledger);
        self
    }

    fn ledger(&self) -> Result<Ledger, String> {
        match &self.ledger {
            Some(l) => Ok(l.clone()),
            None => versioned_files::ledger().cloned(),
        }
    }

    /// Make the ledger agree with a rule that exists in the store but has no (or a different)
    /// history: version 1 by actor `system` when there is none, an `external` version when the store
    /// holds something the ledger does not. Nothing changes when they already agree.
    fn sync_head(ledger: &Ledger, rule: &StoredAlert) -> Result<Option<VersionRecord>, String> {
        let content = alert_content(rule)?;
        let hash = Ledger::sha256_hex(content.as_bytes());
        match ledger.head(Kind::Alert, &rule.id) {
            None => ledger.put(Kind::Alert, &rule.id, content.as_bytes(), "system", &format!("Registered existing alert {}", describe(rule))).map(Some),
            Some(h) if !h.is_tombstone() && h.content_hash.as_deref() == Some(hash.as_str()) => Ok(None),
            Some(_) => ledger
                .put_as(Kind::Alert, &rule.id, content.as_bytes(), "external", "external", &format!("Alert found changed outside APEX: {}", describe(rule)))
                .map(Some),
        }
    }

    /// Startup: record rules that predate the ledger as version 1 (and any that changed while the app
    /// was not running as `external`; a live ledger rule missing from the file gets an `external` delete).
    /// Returns how many versions were added. Never needed for the app
    /// to work: an unregistered rule is registered the first time it is touched.
    pub async fn register_existing(&self) -> Result<usize, String> {
        let _guard = self.guard.lock().await;
        let ledger = self.ledger()?;
        let mut added = 0;
        let rules = self.engine.get_rules().await;
        for rule in &rules {
            if Self::sync_head(&ledger, rule)?.is_some() {
                added += 1;
            }
        }
        // The other direction: a rule the ledger holds as live that the alerts file does not have (it
        // was removed by hand, or the app stopped between the ledger write and the file write). Not
        // when the file could not be read: then it has no rules at all and every one would look deleted.
        // Not for the process-wide ledger in unit tests: many test AppStates share it while each has its own
        // alerts file, so one would mark the others' rules as removed (state.rs skips its startup reconcile too).
        let shared_test_ledger = cfg!(test) && self.ledger.is_none();
        if self.load_error.is_none() && !shared_test_ledger {
            for object in ledger.objects(Kind::Alert, false) {
                if !rules.iter().any(|rule| rule.id == object.object_id) {
                    ledger.tombstone(Kind::Alert, &object.object_id, "external", "Alert is not in the alerts file (removed outside APEX, or never saved)")?;
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    /// A ledger record for a change that then could not be written to the alerts file: put the
    /// object back as it was (or mark it deleted when it did not exist).
    fn compensate(ledger: &Ledger, id: &str, before: Option<&StoredAlert>, why: &str) {
        let summary = format!("Reverted: the change could not be saved ({why})");
        let result = match before {
            Some(rule) => alert_content(rule).and_then(|c| ledger.put(Kind::Alert, id, c.as_bytes(), "system", &summary)),
            None => ledger.tombstone(Kind::Alert, id, "system", &summary),
        };
        if let Err(error) = result {
            tracing::error!(%error, alert = id, "could not record the reverted alert change");
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

    /// Create or replace a rule. `actor` is "user" or "copilot". The ledger version is written first.
    pub async fn add(&self, rule: StoredAlert, actor: &str) -> Result<(), String> {
        let _guard = self.guard.lock().await;
        self.check_loaded()?;
        let ledger = self.ledger()?;
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
        let before = rules.iter().find(|existing| existing.id == rule.id).cloned();
        if let Some(existing) = &before {
            // A rule that predates the ledger is recorded before it is replaced, so it is not lost.
            Self::sync_head(&ledger, existing)?;
        }
        let content = alert_content(&rule)?;
        let verb = if before.is_some() { "Updated" } else { "Created" };
        ledger.put(Kind::Alert, &rule.id, content.as_bytes(), actor, &format!("{verb} alert {}", describe(&rule)))?;
        rules.retain(|existing| existing.id != rule.id);
        rules.push(rule.clone());
        if let Err(error) = persist(&self.path, &rules).await {
            Self::compensate(&ledger, &rule.id, before.as_ref(), &error);
            return Err(error);
        }
        self.engine.add_rule(rule).await;
        Ok(())
    }

    /// Delete a rule (a delete marker in the ledger; the history stays and can be restored).
    pub async fn remove(&self, id: &str, actor: &str) -> Result<bool, String> {
        let _guard = self.guard.lock().await;
        self.check_loaded()?;
        let mut rules = self.engine.get_rules().await;
        let Some(before) = rules.iter().find(|rule| rule.id == id).cloned() else {
            return Ok(false);
        };
        let ledger = self.ledger()?;
        Self::sync_head(&ledger, &before)?;
        ledger.tombstone(Kind::Alert, id, actor, &format!("Deleted alert {}", describe(&before)))?;
        rules.retain(|rule| rule.id != id);
        if let Err(error) = persist(&self.path, &rules).await {
            Self::compensate(&ledger, id, Some(&before), &error);
            return Err(error);
        }
        self.engine.remove_rule(id).await;
        Ok(true)
    }

    /// Bring an older version of a rule back as a NEW version and make it the live rule again.
    pub async fn restore(&self, ledger: &Ledger, id: &str, version: u32, actor: &str) -> Result<VersionRecord, String> {
        let _guard = self.guard.lock().await;
        self.check_loaded()?;
        // Read and check the old content first so a version that cannot be used fails before anything is written.
        let bytes = ledger.content(Kind::Alert, id, version)?;
        let rule: StoredAlert = serde_json::from_slice(&bytes).map_err(|e| format!("version {version} of alert {id} could not be read: {e}"))?;
        if rule.id != id {
            return Err(format!("version {version} of alert {id} holds a different rule id"));
        }
        let mut rules = self.engine.get_rules().await;
        let before = rules.iter().find(|existing| existing.id == id).cloned();
        if let Some(existing) = &before {
            Self::sync_head(ledger, existing)?;
        }
        if rule.enabled {
            if let Some(symbol) = quote_symbol(&rule.rule) {
                crate::validation::validate_symbol(symbol)?;
                self.aggregator
                    .start(&[Symbol(symbol.to_string())])
                    .await
                    .map_err(|error| format!("Could not subscribe to {symbol}: {error}"))?;
            }
        }
        let (record, _) = ledger.restore(Kind::Alert, id, version, actor)?;
        rules.retain(|existing| existing.id != id);
        rules.push(rule.clone());
        if let Err(error) = persist(&self.path, &rules).await {
            Self::compensate(ledger, id, before.as_ref(), &error);
            return Err(error);
        }
        self.engine.add_rule(rule).await;
        Ok(record)
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
    /// A private ledger next to the alerts file.
    fn ledger_for(path: &Path) -> Ledger {
        Ledger::open(&path.parent().unwrap().join("ledger")).unwrap()
    }
    async fn store(path: &Path, ledger: &Ledger) -> (PersistentAlerts, Arc<AlertEngine>) {
        let (engine, aggregator) = engines();
        let store = PersistentAlerts::load(engine.clone(), aggregator, path.to_path_buf()).await.with_ledger(ledger.clone());
        (store, engine)
    }
    fn rule(id: &str) -> StoredAlert {
        StoredAlert {
            id: id.into(),
            rule: AlertRule::DailyPnl { threshold: -100.0 },
            delivery: vec![AlertDelivery::InApp],
            enabled: true,
        }
    }
    fn rule_with(id: &str, threshold: f64) -> StoredAlert {
        StoredAlert { rule: AlertRule::DailyPnl { threshold }, ..rule(id) }
    }
    fn history(ledger: &Ledger, id: &str) -> Vec<(u32, String, String)> {
        ledger.history(Kind::Alert, id).into_iter().map(|r| (r.version, r.action, r.actor)).collect()
    }

    #[tokio::test]
    async fn survives_reload_and_deletion() {
        let path = path();
        let ledger = ledger_for(&path);
        let (store, _) = store(&path, &ledger).await;
        store.add(rule("one"), "user").await.unwrap();
        store.add(rule("one"), "user").await.unwrap();
        let (reloaded, _) = store_reload(&path, &ledger).await;
        assert_eq!(reloaded.list().await.unwrap().len(), 1);
        assert!(reloaded.remove("one", "user").await.unwrap());
        let (again, _) = store_reload(&path, &ledger).await;
        assert!(again.list().await.unwrap().is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    async fn store_reload(path: &Path, ledger: &Ledger) -> (PersistentAlerts, Arc<AlertEngine>) {
        store(path, ledger).await
    }

    #[tokio::test]
    async fn corrupted_file_is_preserved_and_writes_are_rejected() {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        let ledger = ledger_for(&path);
        let (store, _) = store(&path, &ledger).await;
        assert!(store.list().await.is_err());
        assert!(store.add(rule("one"), "user").await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        assert!(ledger.history(Kind::Alert, "one").is_empty(), "a refused change leaves no version");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn failed_save_does_not_update_running_rules() {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let ledger = ledger_for(&path);
        let (store, engine) = store(&path, &ledger).await;
        // Make the destination a directory after load so the atomic rename fails.
        std::fs::create_dir(&path).unwrap();
        assert!(store.add(rule("not-saved"), "user").await.is_err());
        assert!(engine.get_rules().await.is_empty());
        // The ledger was written first, so it also says what happened: created, then reverted.
        let h = history(&ledger, "not-saved");
        assert_eq!(h.iter().map(|(_, a, _)| a.as_str()).collect::<Vec<_>>(), ["create", "delete"]);
        assert_eq!(h[1].2, "system");
        assert!(ledger.history(Kind::Alert, "not-saved")[1].summary.starts_with("Reverted"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_ledger_failure_refuses_the_change_and_touches_nothing() {
        let path = path();
        let ledger = ledger_for(&path);
        let (store, engine) = store(&path, &ledger).await;
        store.add(rule("keep"), "user").await.unwrap();
        let before = std::fs::read(&path).unwrap();
        // Over the ledger's size limit, so the ledger write fails.
        let too_big = StoredAlert {
            rule: AlertRule::NewsKeyword { pattern: "x".repeat(crate::ledger::MAX_BLOB_BYTES + 1), symbols: vec![] },
            ..rule("huge")
        };
        let error = store.add(too_big, "user").await.unwrap_err();
        assert!(error.contains("ledger limit"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the alerts file is untouched");
        assert_eq!(engine.get_rules().await.len(), 1, "the running rules are untouched");
        assert!(ledger.history(Kind::Alert, "huge").is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn create_update_delete_and_restore_are_versions_with_their_actors() {
        let path = path();
        let ledger = ledger_for(&path);
        let (store, engine) = store(&path, &ledger).await;
        store.add(rule_with("a1", -100.0), "user").await.unwrap();
        store.add(rule_with("a1", -200.0), "copilot").await.unwrap();
        assert!(store.remove("a1", "user").await.unwrap());
        assert!(engine.get_rules().await.is_empty());
        assert_eq!(
            history(&ledger, "a1"),
            [(1, "create".into(), "user".into()), (2, "update".into(), "copilot".into()), (3, "delete".into(), "user".into())]
        );
        // The content is the canonical JSON of the stored rule, and old versions stay readable.
        let v1 = String::from_utf8(ledger.content(Kind::Alert, "a1", 1).unwrap()).unwrap();
        assert_eq!(v1, r#"{"delivery":["InApp"],"enabled":true,"id":"a1","rule":{"DailyPnl":{"threshold":-100.0}}}"#);
        // Restore version 1: a NEW version 4, and the rule is live again (also after a reload).
        let record = store.restore(&ledger, "a1", 1, "user").await.unwrap();
        assert_eq!((record.version, record.action.as_str(), record.restored_from), (4, "restore", Some(1)));
        assert_eq!(engine.get_rules().await.len(), 1);
        let (reloaded, _) = store_reload(&path, &ledger).await;
        let listed = reloaded.list().await.unwrap();
        assert_eq!(alert_content(&listed[0]).unwrap(), v1);
        assert_eq!(ledger.history(Kind::Alert, "a1").len(), 4, "nothing was erased");
        assert!(ledger.verify().ok);
        // A version that does not exist is an error, and changes nothing.
        assert!(store.restore(&ledger, "a1", 9, "user").await.is_err());
        assert_eq!(ledger.history(Kind::Alert, "a1").len(), 4);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn existing_rules_register_as_version_one_once_and_outside_changes_are_external() {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&vec![rule_with("old1", -1.0), rule_with("old2", -2.0)]).unwrap()).unwrap();
        let ledger = ledger_for(&path);
        let (store, _) = store(&path, &ledger).await;
        assert_eq!(store.register_existing().await.unwrap(), 2);
        assert_eq!(store.register_existing().await.unwrap(), 0, "registering twice adds nothing");
        assert_eq!(history(&ledger, "old1"), [(1, "create".into(), "system".into())]);
        // The file changes while the app is not running.
        std::fs::write(&path, serde_json::to_vec(&vec![rule_with("old1", -9.0), rule_with("old2", -2.0)]).unwrap()).unwrap();
        let (store, _) = store_reload(&path, &ledger).await;
        assert_eq!(store.register_existing().await.unwrap(), 1);
        assert_eq!(history(&ledger, "old1")[1], (2, "external".into(), "external".into()));
        assert_eq!(ledger.history(Kind::Alert, "old2").len(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_live_ledger_rule_missing_from_the_alerts_file_is_recorded_as_deleted_at_startup() {
        let path = path();
        let ledger = ledger_for(&path);
        let (store, _) = store(&path, &ledger).await;
        store.add(rule_with("hand", -1.0), "user").await.unwrap();
        store.add(rule_with("kept", -2.0), "user").await.unwrap();
        // Crash between the ledger write and the file write: a live head for a rule the file never got.
        ledger.put(Kind::Alert, "never-saved", alert_content(&rule("never-saved")).unwrap().as_bytes(), "user", "Created alert").unwrap();
        // The user removes a rule from the file by hand while the app is stopped.
        std::fs::write(&path, serde_json::to_vec(&vec![rule_with("kept", -2.0)]).unwrap()).unwrap();
        let (store, _) = store_reload(&path, &ledger).await;
        assert_eq!(store.register_existing().await.unwrap(), 2);
        for id in ["hand", "never-saved"] {
            let head = ledger.head(Kind::Alert, id).unwrap();
            assert!(head.is_tombstone(), "{id}");
            assert_eq!((head.action.as_str(), head.actor.as_str()), ("delete", "external"), "{id}");
        }
        assert_eq!(ledger.history(Kind::Alert, "hand").len(), 2, "the history stays");
        assert_eq!(ledger.history(Kind::Alert, "kept").len(), 1);
        assert_eq!(store.register_existing().await.unwrap(), 0, "once");
        // An alerts file that cannot be read says nothing about the rules: nothing is marked deleted.
        std::fs::write(&path, b"not json").unwrap();
        let (store, _) = store_reload(&path, &ledger).await;
        assert_eq!(store.register_existing().await.unwrap(), 0);
        assert!(!ledger.head(Kind::Alert, "kept").unwrap().is_tombstone());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn replacing_or_deleting_a_rule_that_predates_the_ledger_keeps_its_old_content() {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&vec![rule_with("pre", -1.0), rule_with("pre2", -2.0)]).unwrap()).unwrap();
        let ledger = ledger_for(&path);
        let (store, _) = store(&path, &ledger).await;
        store.add(rule_with("pre", -5.0), "user").await.unwrap();
        assert!(store.remove("pre2", "user").await.unwrap());
        assert_eq!(history(&ledger, "pre").iter().map(|(v, a, _)| (*v, a.as_str())).collect::<Vec<_>>(), [(1, "create"), (2, "update")]);
        assert!(String::from_utf8(ledger.content(Kind::Alert, "pre", 1).unwrap()).unwrap().contains("-1.0"));
        assert_eq!(history(&ledger, "pre2").iter().map(|(_, a, _)| a.as_str()).collect::<Vec<_>>(), ["create", "delete"]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
