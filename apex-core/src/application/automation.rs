//! Automation engine — persisted, scheduled rules that run model signals
//! (or other actions) on an interval. The engine itself is a plain rule
//! store with due-time bookkeeping; the host app supplies the executor that
//! turns a due rule into a prediction + order.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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
    /// The rule as it is versioned in the audit ledger: everything that defines what the rule does,
    /// and nothing that changes on every run. `orders_today`, `orders_day`, `last_run_at` and
    /// `last_result` are left out (they live in the order event stream), so a run does not create a
    /// new version. Keys are written in a fixed order so equal rules give equal bytes.
    pub fn audit_content(&self) -> String {
        let kind = serde_json::to_value(self.kind).unwrap_or(serde_json::Value::Null);
        let mut map = serde_json::Map::new();
        map.insert("allow_short".into(), self.allow_short.into());
        map.insert("broker_id".into(), self.broker_id.clone().into());
        map.insert("created_at".into(), self.created_at.to_rfc3339().into());
        map.insert("enabled".into(), self.enabled.into());
        map.insert("id".into(), self.id.clone().into());
        map.insert("interval_secs".into(), self.interval_secs.into());
        map.insert("kind".into(), kind);
        map.insert("model_id".into(), self.model_id.clone().into());
        map.insert("name".into(), self.name.clone().into());
        map.insert("quantity".into(), self.quantity.into());
        map.insert("symbol".into(), self.symbol.clone().into());
        map.insert("threshold".into(), self.threshold.into());
        serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_default()
    }

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

/// A change to a rule's definition, reported to the [`RuleVersionHook`] so it can be versioned.
pub struct RuleChange<'a> {
    /// "create" | "enable" | "disable" | "allow_short" | "delete" | "restore" | "edit"
    pub action: &'static str,
    /// The rule after the change (for a delete, the rule as it was).
    pub rule: &'a AutomationRule,
    pub deleted: bool,
    /// Who made the change; see [`with_actor`].
    pub actor: String,
}

/// Receives every change to a rule's definition (not run counters). The host app uses it to append
/// a version to the audit ledger. An `Err` is logged; the change itself still applies, and the
/// startup check of the rule file against the ledger records anything that was missed.
pub trait RuleVersionHook: Send + Sync {
    fn on_change(&self, change: &RuleChange<'_>) -> Result<(), String>;
}

thread_local! {
    static ACTOR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `f` with `actor` named as the author of any rule change it makes on this thread. The engine
/// methods are synchronous, so the scope covers exactly the calls inside `f`.
pub fn with_actor<R>(actor: &str, f: impl FnOnce() -> R) -> R {
    let previous = ACTOR.with(|a| a.replace(Some(actor.to_string())));
    let result = f();
    ACTOR.with(|a| *a.borrow_mut() = previous);
    result
}

/// The actor of the current [`with_actor`] scope; "unattributed" outside one.
pub fn current_actor() -> String {
    ACTOR.with(|a| a.borrow().clone()).unwrap_or_else(|| "unattributed".to_string())
}

/// File-backed rule store. Rules persist to `automations.json` in the app
/// data dir so they survive restarts.
pub struct AutomationEngine {
    rules: RwLock<HashMap<String, AutomationRule>>,
    persist_path: PathBuf,
    hook: RwLock<Option<Arc<dyn RuleVersionHook>>>,
}

impl AutomationEngine {
    pub fn new(persist_path: PathBuf) -> Self {
        let engine = Self {
            rules: RwLock::new(HashMap::new()),
            persist_path,
            hook: RwLock::new(None),
        };
        engine.load();
        engine
    }

    /// Version every change to a rule's definition through `hook`.
    pub fn set_version_hook(&self, hook: Arc<dyn RuleVersionHook>) {
        *self.hook.write().unwrap() = Some(hook);
    }

    fn notify(&self, action: &'static str, rule: &AutomationRule, deleted: bool) {
        let Some(hook) = self.hook.read().unwrap().clone() else { return };
        let change = RuleChange { action, rule, deleted, actor: current_actor() };
        if let Err(error) = hook.on_change(&change) {
            tracing::error!(rule = %rule.id, action, %error, "could not record an automation version in the audit ledger");
        }
    }

    /// Put a rule back to a stored definition (audit restore). The run counters of an existing rule
    /// are kept; a rule that was deleted comes back with fresh counters.
    pub fn apply_restored(&self, restored: AutomationRule) -> AutomationRule {
        let merged = {
            let mut rules = self.rules.write().unwrap();
            let merged = match rules.get(&restored.id) {
                Some(current) => AutomationRule {
                    last_run_at: current.last_run_at,
                    last_result: current.last_result.clone(),
                    orders_day: current.orders_day.clone(),
                    orders_today: current.orders_today,
                    ..restored
                },
                None => AutomationRule {
                    last_run_at: None,
                    last_result: None,
                    orders_day: None,
                    orders_today: 0,
                    ..restored
                },
            };
            rules.insert(merged.id.clone(), merged.clone());
            merged
        };
        self.save();
        self.notify("restore", &merged, false);
        merged
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
        self.notify("create", &rule, false);
        rule
    }

    pub fn remove_rule(&self, id: &str) -> bool {
        let removed = self.rules.write().unwrap().remove(id);
        if let Some(rule) = &removed {
            self.save();
            self.notify("delete", rule, true);
        }
        removed.is_some()
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut rules = self.rules.write().unwrap();
        match rules.get_mut(id) {
            Some(r) => {
                let changed = r.enabled != enabled;
                r.enabled = enabled;
                let rule = r.clone();
                drop(rules);
                self.save();
                if changed {
                    self.notify(if enabled { "enable" } else { "disable" }, &rule, false);
                }
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
                let changed = r.allow_short != allow;
                r.allow_short = allow;
                let rule = r.clone();
                drop(rules);
                self.save();
                if changed {
                    self.notify("allow_short", &rule, false);
                }
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

    struct Recording(std::sync::Mutex<Vec<(String, String, bool, String)>>);

    impl RuleVersionHook for Recording {
        fn on_change(&self, change: &RuleChange<'_>) -> Result<(), String> {
            self.0.lock().unwrap().push((
                change.action.to_string(),
                change.rule.id.clone(),
                change.deleted,
                change.actor.clone(),
            ));
            Ok(())
        }
    }

    fn sample(e: &AutomationEngine) -> AutomationRule {
        e.add_rule("r".into(), AutomationKind::ModelSignal, "AAPL".into(), "m".into(), 60, 5.0, 0.6, "paper".into(), false)
    }

    #[test]
    fn the_hook_sees_each_definition_change_with_its_actor_and_not_run_counters() {
        let (e, path) = engine();
        let hook = Arc::new(Recording(Default::default()));
        e.set_version_hook(hook.clone());
        let r = with_actor("user", || sample(&e));
        with_actor("user", || e.set_enabled(&r.id, false));
        with_actor("user", || e.set_enabled(&r.id, false)); // unchanged
        with_actor("copilot", || e.set_allow_short(&r.id, true));
        e.record_order(&r.id);
        e.record_run(&r.id, "x".into());
        e.remove_rule(&r.id); // no scope
        e.remove_rule(&r.id); // already gone
        let seen = hook.0.lock().unwrap().clone();
        let summary: Vec<(&str, bool, &str)> = seen.iter().map(|(a, _, d, who)| (a.as_str(), *d, who.as_str())).collect();
        assert_eq!(
            summary,
            [
                ("create", false, "user"),
                ("disable", false, "user"),
                ("allow_short", false, "copilot"),
                ("delete", true, "unattributed"),
            ]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn audit_content_ignores_counters_and_is_stable() {
        let (e, path) = engine();
        let r = sample(&e);
        let before = r.audit_content();
        e.record_order(&r.id);
        e.record_run(&r.id, "done".into());
        let after = e.get(&r.id).unwrap();
        assert!(after.orders_today > 0 && after.last_result.is_some());
        assert_eq!(before, after.audit_content());
        assert!(!before.contains("orders_today") && !before.contains("last_run_at") && !before.contains("last_result"));
        // The stored form reads back as a rule.
        let back: AutomationRule = serde_json::from_str(&before).unwrap();
        assert_eq!(back.id, r.id);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_actor_scope_ends_with_the_closure() {
        assert_eq!(current_actor(), "unattributed");
        with_actor("user", || {
            assert_eq!(current_actor(), "user");
            with_actor("copilot", || assert_eq!(current_actor(), "copilot"));
            assert_eq!(current_actor(), "user");
        });
        assert_eq!(current_actor(), "unattributed");
    }
}
