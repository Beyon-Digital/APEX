use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Failed attempts in a row since the last success, per action. Only these counts are ever logged:
/// never a passphrase, a length or a hint.
static FAILED_INIT: AtomicU32 = AtomicU32::new(0);
static FAILED_UNLOCK: AtomicU32 = AtomicU32::new(0);
static FAILED_CHANGE: AtomicU32 = AtomicU32::new(0);

/// One finished vault action as an activity entry. A failure records `ok=false` and the running count,
/// with no details (an error text could hint at the passphrase).
fn record_attempt(state: &AppState, actor: &str, action: &str, label: &str, counter: &AtomicU32, ok: bool) {
    if ok {
        let before = counter.swap(0, Ordering::SeqCst);
        let details = (before > 0).then(|| format!("failed_attempts_before={before}"));
        crate::activity::record(state, actor, action, "system", "vault", &format!("Vault {label}"), true, details);
    } else {
        let n = counter.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        crate::activity::record(
            state,
            actor,
            action,
            "system",
            "vault",
            &format!("Vault {label} failed (failed attempt {n} in a row)"),
            false,
            Some(format!("failed_attempts={n}")),
        );
    }
}

/// Create the vault with a new passphrase and leave it unlocked.
#[tauri::command]
pub async fn vault_init(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<VaultStatusDto, String> {
    vault_init_as(&state, &passphrase, "user").await
}

pub(crate) async fn vault_init_as(state: &AppState, passphrase: &str, actor: &str) -> Result<VaultStatusDto, String> {
    let result = state.vault.init(passphrase);
    record_attempt(state, actor, "vault.init", "created", &FAILED_INIT, result.is_ok());
    result?;
    state.apply_all_brokers().await;
    Ok(status_dto(state))
}

/// Unlock the vault and bring every stored adapter online.
#[tauri::command]
pub async fn vault_unlock(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<VaultStatusDto, String> {
    vault_unlock_as(&state, &passphrase, "user").await
}

pub(crate) async fn vault_unlock_as(state: &AppState, passphrase: &str, actor: &str) -> Result<VaultStatusDto, String> {
    let result = state.vault.unlock(passphrase);
    record_attempt(state, actor, "vault.unlock", "unlocked", &FAILED_UNLOCK, result.is_ok());
    result?;
    state.apply_all_brokers().await;
    Ok(status_dto(state))
}

/// Lock the vault and drop every in-memory credential and live adapter.
#[tauri::command]
pub async fn vault_lock(state: State<'_, AppState>) -> Result<VaultStatusDto, String> {
    vault_lock_as(&state, "user").await
}

pub(crate) async fn vault_lock_as(state: &AppState, actor: &str) -> Result<VaultStatusDto, String> {
    state.lock_vault().await;
    crate::activity::record(state, actor, "vault.lock", "system", "vault", "Vault locked", true, None);
    Ok(status_dto(state))
}

#[tauri::command]
pub async fn vault_change_passphrase(
    old_passphrase: String,
    new_passphrase: String,
    state: State<'_, AppState>,
) -> Result<VaultStatusDto, String> {
    vault_change_passphrase_as(&state, &old_passphrase, &new_passphrase, "user").await
}

pub(crate) async fn vault_change_passphrase_as(
    state: &AppState,
    old_passphrase: &str,
    new_passphrase: &str,
    actor: &str,
) -> Result<VaultStatusDto, String> {
    let result = state.vault.change_passphrase(old_passphrase, new_passphrase);
    record_attempt(state, actor, "vault.change_passphrase", "passphrase changed", &FAILED_CHANGE, result.is_ok());
    result?;
    // Changing the passphrase leaves the vault unlocked. If it was locked before,
    // bring the adapters online like a normal unlock would.
    state.apply_all_brokers().await;
    Ok(status_dto(state))
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
    // `llm:@active` resolves at import time to the provider the copilot really uses.
    ("OPEN_ROUTER", "llm:@active", "api_key", true),
    ("OPENROUTER_API_KEY", "llm:@active", "api_key", true),
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
    let mut touched: Vec<String> = Vec::new();
    // The copilot reads its key from `llm:<active provider>`, or `llm:copilot` when
    // none is selected. Importing to any other slot would look saved but never be read.
    let llm_slot = if state.llm.active.trim().is_empty() {
        "llm:copilot".to_string()
    } else {
        format!("llm:{}", state.llm.active)
    };
    for (var, adapter_id, field, secret) in ENV_IMPORT_MAP {
        let adapter_id: &str = if *adapter_id == "llm:@active" { llm_slot.as_str() } else { adapter_id };
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
        if !touched.iter().any(|t| t == adapter_id) {
            touched.push(adapter_id.to_string());
        }
    }
    for adapter_id in &touched {
        if !adapter_id.starts_with("llm:") {
            state
                .apply_broker(adapter_id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    // The names of the variables, never their values.
    crate::activity::record(
        &state,
        "user",
        "adapter.import_env",
        "system",
        "vault",
        &format!("Imported {} credential(s) from a .env file into the vault", imported.len()),
        true,
        Some(format!("variables={}", imported.join(","))),
    );
    Ok(EnvImportResultDto {
        source: env_path.display().to_string(),
        imported,
    })
}
