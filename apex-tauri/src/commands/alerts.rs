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
    validation::validate_string_length(&id, "alert id")?;
    validation::validate_alert_json(&rule_json)?;

    let rule: AlertRule =
        serde_json::from_str(&rule_json).map_err(|e| format!("Invalid alert rule: {}", e))?;

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
        .add(StoredAlert {
            id,
            rule,
            delivery: vec![AlertDelivery::InApp],
            enabled: true,
        })
        .await?;

    Ok(())
}

/// Remove an alert rule.
#[tauri::command]
pub async fn remove_alert(rule_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    state.alert_store.remove(&rule_id).await
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
