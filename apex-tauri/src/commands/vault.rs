use std::collections::HashMap;

use crate::dto::EnvImportResultDto;
use crate::state::AppState;
use crate::vault::VaultStatus;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Clone, Serialize)]
pub struct VaultStatusDto {
    pub status: VaultStatus,
}

fn status_dto(state: &AppState) -> VaultStatusDto {
    VaultStatusDto {
        status: state.vault.status(),
    }
}

#[tauri::command]
pub async fn vault_status(state: State<'_, AppState>) -> Result<VaultStatusDto, String> {
    Ok(status_dto(&state))
}

/// Create the vault with a new passphrase and leave it unlocked.
#[tauri::command]
pub async fn vault_init(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<VaultStatusDto, String> {
    state.vault.init(&passphrase)?;
    state.apply_all_brokers().await;
    Ok(status_dto(&state))
}

/// Unlock the vault and bring every stored adapter online.
#[tauri::command]
pub async fn vault_unlock(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<VaultStatusDto, String> {
    state.vault.unlock(&passphrase)?;
    state.apply_all_brokers().await;
    Ok(status_dto(&state))
}

/// Lock the vault and drop every in-memory credential and live adapter.
#[tauri::command]
pub async fn vault_lock(state: State<'_, AppState>) -> Result<VaultStatusDto, String> {
    state.lock_vault().await;
    Ok(status_dto(&state))
}

#[tauri::command]
pub async fn vault_change_passphrase(
    old_passphrase: String,
    new_passphrase: String,
    state: State<'_, AppState>,
) -> Result<VaultStatusDto, String> {
    state
        .vault
        .change_passphrase(&old_passphrase, &new_passphrase)?;
    Ok(status_dto(&state))
}

/// `.env` variable -> (adapter id, field, is_secret)
const ENV_IMPORT_MAP: &[(&str, &str, &str, bool)] = &[
    ("ZERODHA_API_KEY", "zerodha", "api_key", true),
    ("ZERODHA_API_SECRET", "zerodha", "api_secret", true),
    ("ZERODHA_ACCESS_TOKEN", "zerodha", "access_token", true),
    ("ZERODHA_REDIRECT_URL", "zerodha", "redirect_url", false),
    ("ANGEL_ONE_API_KEY", "angel_one", "api_key", true),
    ("ANGEL_ONE_CLIENT_CODE", "angel_one", "client_code", false),
    ("ANGEL_ONE_JWT_TOKEN", "angel_one", "jwt_token", true),
    ("GROWW_API_KEY", "groww", "api_key", true),
    ("GROWW_ACCESS_TOKEN", "groww", "access_token", true),
    ("ROBINHOOD_CLIENT_ID", "robinhood", "client_id", true),
    ("ROBINHOOD_ACCESS_TOKEN", "robinhood", "access_token", true),
    ("BINANCE_API_KEY", "binance", "api_key", true),
    ("BINANCE_API_SECRET", "binance", "api_secret", true),
    ("COINBASE_API_KEY", "coinbase", "api_key", true),
    ("COINBASE_API_SECRET", "coinbase", "api_secret", true),
    ("COINBASE_PASSPHRASE", "coinbase", "passphrase", true),
    ("OPEN_ROUTER", "llm:copilot", "api_key", true),
    ("OPENROUTER_API_KEY", "llm:copilot", "api_key", true),
];

fn is_placeholder(value: &str) -> bool {
    value.is_empty() || (value.starts_with("your_") && value.ends_with("_here"))
}

/// One-time migration: copy credentials from a legacy `.env` file into the
/// vault. The file is only read; nothing is written to the process environment.
#[tauri::command]
pub async fn import_env_credentials(
    state: State<'_, AppState>,
) -> Result<EnvImportResultDto, String> {
    if !state.vault.is_unlocked() {
        return Err("Unlock the vault before importing credentials".into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let env_path = cwd
        .ancestors()
        .map(|dir| dir.join(".env"))
        .find(|path| path.is_file())
        .ok_or_else(|| "No .env file found".to_string())?;

    let vars: HashMap<String, String> = dotenvy::from_path_iter(&env_path)
        .map_err(|e| format!("Failed to read {}: {e}", env_path.display()))?
        .filter_map(Result::ok)
        .map(|(k, v)| (k, v.trim().to_string()))
        .collect();

    let mut imported = Vec::new();
    let mut touched: Vec<&str> = Vec::new();
    for (var, adapter_id, field, secret) in ENV_IMPORT_MAP {
        let Some(value) = vars.get(*var).filter(|v| !is_placeholder(v)) else {
            continue;
        };
        if *secret {
            state.vault.set_secret(adapter_id, field, value)?;
        } else {
            let mut settings: serde_json::Map<String, serde_json::Value> = state
                .vault
                .get_config(adapter_id)?
                .and_then(|c| serde_json::from_str(&c.settings_json).ok())
                .unwrap_or_default();
            settings.insert((*field).into(), serde_json::Value::String(value.clone()));
            let enabled = state
                .vault
                .get_config(adapter_id)?
                .map(|c| c.enabled)
                .unwrap_or(true);
            state
                .vault
                .set_config(adapter_id, enabled, &serde_json::Value::Object(settings).to_string())?;
        }
        imported.push((*var).to_string());
        if !touched.contains(adapter_id) {
            touched.push(adapter_id);
        }
    }
    for adapter_id in touched {
        if !adapter_id.starts_with("llm:") {
            state
                .apply_broker(adapter_id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(EnvImportResultDto {
        source: env_path.display().to_string(),
        imported,
    })
}
