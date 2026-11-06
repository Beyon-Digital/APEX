//! Automation engine — persisted, scheduled rules that run model signals
//! (or other actions) on an interval. The engine itself is a plain rule
//! store with due-time bookkeeping; the host app supplies the executor that
//! turns a due rule into a prediction + order.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a rule does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationKind {
    /// Compute the model's signal on `symbol` from the latest bars and place
    /// `quantity` on `broker_id` when confidence ≥ `threshold`.
    ModelSignal,
}

/// A scheduled automation rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationRule {
    pub id: String,
    pub name: String,
    pub kind: AutomationKind,
    pub symbol: String,
    /// Model registry id (for `ModelSignal`).
    pub model_id: String,
    /// Seconds between runs (min 30).
    pub interval_secs: u64,
    /// Order quantity when the signal fires.
    pub quantity: f64,
    /// Minimum class probability required to place the order (0–1).
    pub threshold: f64,
    pub broker_id: String,
    /// Opt-in to short selling: when true the rule may open a short instead of staying flat on
    /// a bearish signal. Rules saved before shorting existed load as false.
    #[serde(default)]
    pub allow_short: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
    /// Human-readable outcome of the most recent run.
    pub last_result: Option<String>,
    /// UTC day (YYYY-MM-DD) `orders_today` counts against.
    #[serde(default)]
    pub orders_day: Option<String>,
    /// Orders successfully placed on `orders_day`.
    #[serde(default)]
    pub orders_today: u32,
}

impl AutomationRule {
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_run_at {
            None => true,
            Some(last) => {
                (now - last).num_seconds() >= self.interval_secs.min(i64::MAX as u64) as i64
            }
        }
    }
}

/// File-backed rule store. Rules persist to `automations.json` in the app
/// data dir so they survive restarts.
pub struct AutomationEngine {
    rules: RwLock<HashMap<String, AutomationRule>>,
    persist_path: PathBuf,
}

impl AutomationEngine {
    pub fn new(persist_path: PathBuf) -> Self {
        let engine = Self {
            rules: RwLock::new(HashMap::new()),
            persist_path,
        };
        engine.load();
        engine
    }

    fn load(&self) {
        // Prefer the live file; fall back to the staged `.bak` copy a
        // save() may have left behind if it crashed mid-replace.
        for path in [
            self.persist_path.clone(),
            self.persist_path.with_extension("json.bak"),
        ] {
            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            match serde_json::from_str::<Vec<AutomationRule>>(&raw) {
                Ok(list) => {
                    let mut rules = self.rules.write().unwrap();
                    for r in list {
                        rules.insert(r.id.clone(), r);
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "Ignoring malformed automations file")
                }
            }
        }
    }

    fn save(&self) {
        let rules = self.rules.read().unwrap();
        let list: Vec<&AutomationRule> = rules.values().collect();
        if let Some(parent) = self.persist_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&list) {
            Ok(json) => {
                // Durable replace: full tmp write → stage dest as `.bak` →
                // install tmp over dest (rename; copy on Windows, which can't
                // rename over an existing file) → restore `.bak` on failure.
                // load() falls back to `.bak` if the live file is absent.
                let tmp = self.persist_path.with_extension("json.tmp");
                let bak = self.persist_path.with_extension("json.bak");
                if let Err(e) = std::fs::write(&tmp, &json) {
                    tracing::warn!(error = %e, "Failed to write automations tmp file");
                    return;
                }
                let mut staged_backup = false;
                if self.persist_path.exists() {
                    if bak.exists() {
                        let _ = std::fs::remove_file(&bak);
                    }
                    match std::fs::rename(&self.persist_path, &bak) {
                        Ok(()) => staged_backup = true,
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to stage automations backup")
                        }
                    }
                }
                let installed = std::fs::rename(&tmp, &self.persist_path)
                    .or_else(|_| std::fs::copy(&tmp, &self.persist_path).map(|_| ()))
                    .is_ok();
                if installed {
                    let _ = std::fs::remove_file(&tmp);
                    if staged_backup {
                        let _ = std::fs::remove_file(&bak);
                    }
                } else {
                    tracing::warn!("Failed to persist automations.json");
                    let _ = std::fs::remove_file(&tmp);
                    if staged_backup {
                        if let Err(e) = std::fs::rename(&bak, &self.persist_path) {
                            tracing::warn!(error = %e, "Failed to restore automations backup");
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to serialize automations"),
        }
    }

    pub fn list(&self) -> Vec<AutomationRule> {
        let rules = self.rules.read().unwrap();
        let mut v: Vec<AutomationRule> = rules.values().cloned().collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        v
    }

    pub fn get(&self, id: &str) -> Option<AutomationRule> {
        self.rules.read().unwrap().get(id).cloned()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_rule(
        &self,
        name: String,
        kind: AutomationKind,
        symbol: String,
        model_id: String,
        interval_secs: u64,
        quantity: f64,
        threshold: f64,
        broker_id: String,
        allow_short: bool,
    ) -> AutomationRule {
        let rule = AutomationRule {
            id: Uuid::new_v4().to_string(),
            name,
            kind,
            symbol,
            model_id,
            interval_secs: interval_secs.max(30),
            quantity,
            threshold: threshold.clamp(0.0, 1.0),
            broker_id,
            allow_short,
            enabled: true,
            created_at: Utc::now(),
            last_run_at: None,
            last_result: None,
            orders_day: None,
            orders_today: 0,
        };
        self.rules
            .write()
            .unwrap()
            .insert(rule.id.clone(), rule.clone());
        self.save();
        rule
    }

    pub fn remove_rule(&self, id: &str) -> bool {
        let removed = self.rules.write().unwrap().remove(id).is_some();
        if removed {
            self.save();
        }
        removed
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut rules = self.rules.write().unwrap();
        match rules.get_mut(id) {
            Some(r) => {
                r.enabled = enabled;
                drop(rules);
                self.save();
                true
            }
            None => false,
        }
    }

    /// Allow or forbid short selling for a rule. False when the rule does not exist.
    pub fn set_allow_short(&self, id: &str, allow: bool) -> bool {
        let mut rules = self.rules.write().unwrap();
        match rules.get_mut(id) {
            Some(r) => {
                r.allow_short = allow;
                drop(rules);
                self.save();
                true
            }
            None => false,
        }
    }

    /// Ids of rules currently due to run.
    pub fn due_rules(&self) -> Vec<AutomationRule> {
        let now = Utc::now();
        self.rules
            .read()
            .unwrap()
            .values()
            .filter(|r| r.is_due(now))
            .cloned()
            .collect()
    }

    /// Whether the rule may place another order today (UTC), under `cap`.
    pub fn can_place_today(&self, id: &str, cap: u32) -> bool {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        match self.rules.read().unwrap().get(id) {
            Some(r) => r.orders_day.as_deref() != Some(today.as_str()) || r.orders_today < cap,
            None => false,
        }
    }

    /// Increment the rule's successful-order count for the current UTC day
    /// (resets when the day rolls over).
    pub fn record_order(&self, id: &str) {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let mut rules = self.rules.write().unwrap();
        if let Some(r) = rules.get_mut(id) {
            if r.orders_day.as_deref() != Some(today.as_str()) {
                r.orders_day = Some(today);
                r.orders_today = 0;
            }
            r.orders_today += 1;
            drop(rules);
            self.save();
        }
    }

    /// Stamp a run outcome back onto the rule.
    pub fn record_run(&self, id: &str, result: String) {
        let mut rules = self.rules.write().unwrap();
        if let Some(r) = rules.get_mut(id) {
            r.last_run_at = Some(Utc::now());
            r.last_result = Some(result.chars().take(400).collect());
            drop(rules);
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> (AutomationEngine, PathBuf) {
        let dir = std::env::temp_dir().join(format!("apex-auto-test-{}", Uuid::new_v4()));
        let path = dir.join("automations.json");
        (AutomationEngine::new(path.clone()), path)
    }

    #[test]
    fn add_list_remove() {
        let (e, path) = engine();
        let r = e.add_rule(
            "r1".into(),
            AutomationKind::ModelSignal,
            "AAPL".into(),
            "m1".into(),
            60,
            5.0,
            0.6,
            "paper".into(),
            false,
        );
        assert_eq!(e.list().len(), 1);
        assert!(path.exists());
        assert!(e.remove_rule(&r.id));
        assert!(e.list().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn due_scheduling() {
        let (e, path) = engine();
        let r = e.add_rule(
            "r".into(),
            AutomationKind::ModelSignal,
            "AAPL".into(),
            "m".into(),
            30,
            1.0,
            0.5,
            "paper".into(),
            false,
        );
        assert!(r.is_due(Utc::now()));
        e.record_run(&r.id, "ok".into());
        let stored = e.get(&r.id).unwrap();
        assert!(!stored.is_due(Utc::now()));
        assert_eq!(stored.last_result.as_deref(), Some("ok"));
        e.set_enabled(&r.id, false);
        assert!(e.due_rules().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persists_across_instances() {
        let (e, path) = engine();
        e.add_rule(
            "persisted".into(),
            AutomationKind::ModelSignal,
            "MSFT".into(),
            "m".into(),
            120,
            2.0,
            0.5,
            "paper".into(),
            false,
        );
        let e2 = AutomationEngine::new(path.clone());
        assert_eq!(e2.list().len(), 1);
        assert_eq!(e2.list()[0].name, "persisted");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn daily_order_cap() {
        let (e, path) = engine();
        let r = e.add_rule(
            "capped".into(),
            AutomationKind::ModelSignal,
            "AAPL".into(),
            "m".into(),
            60,
            1.0,
            0.5,
            "paper".into(),
            false,
        );
        assert!(e.can_place_today(&r.id, 2));
        e.record_order(&r.id);
        e.record_order(&r.id);
        assert!(!e.can_place_today(&r.id, 2));
        // count survives reload
        let e2 = AutomationEngine::new(path.clone());
        assert!(!e2.can_place_today(&r.id, 2));
        assert!(e2.can_place_today(&r.id, 3));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rules_saved_before_shorting_load_as_not_allowed_to_short() {
        let dir = std::env::temp_dir().join(format!("apex-auto-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("automations.json");
        // A rule exactly as an older build wrote it: no `allow_short` key.
        std::fs::write(
            &path,
            r#"[{"id":"old","name":"legacy","kind":"model_signal","symbol":"AAPL","model_id":"m",
            "interval_secs":60,"quantity":2.0,"threshold":0.5,"broker_id":"paper","enabled":true,
            "created_at":"2025-01-01T00:00:00Z","last_run_at":null,"last_result":null}]"#,
        )
        .unwrap();
        let e = AutomationEngine::new(path);
        let rules = e.list();
        assert_eq!(rules.len(), 1);
        assert!(!rules[0].allow_short);
        assert_eq!(rules[0].orders_today, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn allow_short_is_stored_toggled_and_persisted() {
        let (e, path) = engine();
        let r = e.add_rule(
            "s".into(),
            AutomationKind::ModelSignal,
            "AAPL".into(),
            "m".into(),
            60,
            1.0,
            0.5,
            "paper".into(),
            true,
        );
        assert!(r.allow_short);
        assert!(e.set_allow_short(&r.id, false));
        assert!(!AutomationEngine::new(path.clone()).get(&r.id).unwrap().allow_short);
        assert!(e.set_allow_short(&r.id, true));
        assert!(AutomationEngine::new(path.clone()).get(&r.id).unwrap().allow_short);
        assert!(!e.set_allow_short("missing", true));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
