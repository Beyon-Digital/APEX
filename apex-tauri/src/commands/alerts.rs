use crate::state::AppState;
use crate::validation;
use apex_core::application::alert_engine::{AlertDelivery, AlertRule, StoredAlert};
use serde::Serialize;
use tauri::State;

/// Alert rule DTO for frontend.
#[derive(Debug, Clone, Serialize)]
pub struct AlertRuleDto {
    pub id: String,
    pub rule: String,
    pub enabled: bool,
}

/// Add a new alert rule.
#[tauri::command]
pub async fn add_alert(
    id: String,
    rule_json: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    add_alert_as(&state, id, rule_json, "user").await
}

/// [`add_alert`] for a named actor. The rule is versioned in the audit ledger first and the call is
/// an activity entry (`alert.create`), whether it worked or not.
pub(crate) async fn add_alert_as(state: &AppState, id: String, rule_json: String, actor: &str) -> Result<(), String> {
    let result = add_alert_inner(state, &id, &rule_json, actor).await;
    let (summary, details) = match &result {
        Ok(()) => (format!("Created alert {id}"), serde_json::from_str::<AlertRule>(&rule_json).ok().and_then(|r| serde_json::to_string(&r).ok())),
        Err(error) => (format!("Could not create alert {id}"), Some(crate::commands::settings::scrub_error(error, &[]))),
    };
    crate::activity::record(state, actor, "alert.create", "system", &id, &summary, result.is_ok(), details);
    result
}

async fn add_alert_inner(state: &AppState, id: &str, rule_json: &str, actor: &str) -> Result<(), String> {
    validation::validate_string_length(id, "alert id")?;
    validation::validate_alert_json(rule_json)?;

    let rule: AlertRule =
        serde_json::from_str(rule_json).map_err(|e| format!("Invalid alert rule: {}", e))?;

    if let AlertRule::PctChange { window_secs, .. } = &rule {
        if *window_secs > apex_core::application::alert_engine::MAX_WINDOW_SECS {
            return Err(format!(
                "window_secs exceeds the maximum of {} seconds (~366 days)",
                apex_core::application::alert_engine::MAX_WINDOW_SECS
            ));
        }
    }

    state
        .alert_store
        .add(
            StoredAlert {
                id: id.to_string(),
                rule,
                delivery: vec![AlertDelivery::InApp],
                enabled: true,
            },
            actor,
        )
        .await
}

/// Remove an alert rule.
#[tauri::command]
pub async fn remove_alert(rule_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    remove_alert_as(&state, &rule_id, "user").await
}

/// [`remove_alert`] for a named actor: a delete marker in the ledger plus an `alert.delete` entry.
pub(crate) async fn remove_alert_as(state: &AppState, rule_id: &str, actor: &str) -> Result<bool, String> {
    let result = state.alert_store.remove(rule_id, actor).await;
    let (summary, ok, details) = match &result {
        Ok(true) => (format!("Deleted alert {rule_id}"), true, None),
        Ok(false) => (format!("No alert {rule_id} to delete"), false, None),
        Err(error) => (format!("Could not delete alert {rule_id}"), false, Some(crate::commands::settings::scrub_error(error, &[]))),
    };
    crate::activity::record(state, actor, "alert.delete", "system", rule_id, &summary, ok, details);
    result
}

/// Get all alert rules.
#[tauri::command]
pub async fn get_alert_rules(state: State<'_, AppState>) -> Result<Vec<AlertRuleDto>, String> {
    let rules = state.alert_store.list().await?;
    Ok(rules
        .into_iter()
        .map(|r| AlertRuleDto {
            id: r.id,
            rule: serde_json::to_string(&r.rule).unwrap_or_default(),
            enabled: r.enabled,
        })
        .collect())
}
