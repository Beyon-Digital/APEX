use std::collections::HashMap;

use crate::dto::{
    AdapterConfigDto, AdapterFieldDto, AdapterSchemaDto, AdapterTestResultDto,
    SaveAdapterConfigRequest,
};
use crate::state::AppState;
use crate::validation;
use tauri::State;

const MAX_FIELD_LEN: usize = 4096;

fn field(key: &str, label: &str, kind: &str, required: bool, help: &str) -> AdapterFieldDto {
    AdapterFieldDto {
        key: key.into(),
        label: label.into(),
        kind: kind.into(),
        required,
        help: help.into(),
    }
}

fn feed_toggle(exchange: &str) -> AdapterFieldDto {
    field(
        "market_data_feed",
        "Stream public market data",
        "toggle",
        false,
        &format!("Opens a live {exchange} price feed for matching watchlist symbols."),
    )
}

fn broker_schemas() -> Vec<AdapterSchemaDto> {
    let schema = |id: &str, name: &str, category: &str, description: &str, fields| AdapterSchemaDto {
        adapter_id: id.into(),
        display_name: name.into(),
        category: category.into(),
        description: description.into(),
        fields,
    };
    vec![
        schema(
            "zerodha",
            "Zerodha Kite",
            "broker",
            "Indian equities and derivatives. Create a Kite Connect app, then log in daily to get an access token.",
            vec![
                field("api_key", "API key", "secret", true, "From the Kite Connect developer console."),
                field("api_secret", "API secret", "secret", true, "Used once per login to exchange the request token. Never shown again."),
                field("redirect_url", "Redirect URL", "text", false, "Must match the redirect URL registered on your Kite Connect app."),
            ],
        ),
        schema(
            "angel_one",
            "Angel One",
            "broker",
            "SmartAPI access to Indian markets.",
            vec![
                field("api_key", "API key", "secret", true, ""),
                field("client_code", "Client code", "text", true, "Your Angel One login id."),
            ],
        ),
        schema(
            "groww",
            "Groww",
            "broker",
            "Groww trading API.",
            vec![field("api_key", "API key", "secret", true, "")],
        ),
        schema(
            "robinhood",
            "Robinhood",
            "broker",
            "US equities and crypto.",
            vec![field("client_id", "Client id", "secret", false, "Required for order execution; market data only needs an access token.")],
        ),
        schema(
            "binance",
            "Binance",
            "broker",
            "Crypto spot trading and market data.",
            vec![
                field("api_key", "API key", "secret", false, "Required for order execution."),
                field("api_secret", "API secret", "secret", false, ""),
                field("testnet", "Use testnet", "toggle", false, "Route orders to the Binance testnet."),
                feed_toggle("Binance"),
            ],
        ),
        schema(
            "coinbase",
            "Coinbase Pro",
            "broker",
            "Crypto trading and market data.",
            vec![
                field("api_key", "API key", "secret", false, "Required for order execution."),
                field("api_secret", "API secret", "secret", false, ""),
                field("passphrase", "Passphrase", "secret", false, ""),
                feed_toggle("Coinbase"),
            ],
        ),
        schema(
            "polymarket",
            "Polymarket",
            "data",
            "Prediction-market data (experimental).",
            vec![feed_toggle("Polymarket")],
        ),
    ]
}

fn llm_schema(provider_id: &str, name: &str) -> AdapterSchemaDto {
    AdapterSchemaDto {
        adapter_id: format!("llm:{provider_id}"),
        display_name: name.into(),
        category: "llm".into(),
        description: "API key for this inference provider.".into(),
        fields: vec![field("api_key", "API key", "secret", false, "")],
    }
}

fn allowed_fields(state: &AppState, adapter_id: &str) -> Vec<AdapterFieldDto> {
    adapter_schemas(state)
        .into_iter()
        .find(|s| s.adapter_id == adapter_id)
        .map(|s| s.fields)
        .unwrap_or_default()
}

fn adapter_schemas(state: &AppState) -> Vec<AdapterSchemaDto> {
    let mut schemas = broker_schemas();
    for provider in &state.llm.providers {
        if provider.api_kind != "acp" {
            let name = if provider.name.is_empty() { &provider.id } else { &provider.name };
            schemas.push(llm_schema(&provider.id, name));
        }
    }
    if !schemas.iter().any(|s| s.adapter_id == "llm:copilot") {
        schemas.push(llm_schema("copilot", "Copilot (OpenRouter)"));
    }
    schemas
}

fn config_dto(state: &AppState, adapter_id: &str) -> Result<AdapterConfigDto, String> {
    let record = state.vault.get_config(adapter_id)?;
    let (enabled, settings) = match record {
        Some(record) => {
            let settings: HashMap<String, String> =
                serde_json::from_str::<HashMap<String, serde_json::Value>>(&record.settings_json)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(k, _)| !k.ends_with("_expires_at"))
                    .map(|(k, v)| {
                        let text = match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        };
                        (k, text)
                    })
                    .collect();
            (record.enabled, settings)
        }
        None => (true, HashMap::new()),
    };
    Ok(AdapterConfigDto {
        adapter_id: adapter_id.to_string(),
        enabled,
        settings,
        secrets_configured: state.vault.secret_fields(adapter_id)?,
    })
}

/// Field definitions for every adapter, used to render the configuration forms.
#[tauri::command]
pub async fn list_adapter_schemas(
    state: State<'_, AppState>,
) -> Result<Vec<AdapterSchemaDto>, String> {
    Ok(adapter_schemas(&state))
}

/// Stored configuration for every adapter. Secrets are never returned, only
/// which fields have a value. Works while the vault is locked (names only).
#[tauri::command]
pub async fn get_adapter_configs(
    state: State<'_, AppState>,
) -> Result<Vec<AdapterConfigDto>, String> {
    adapter_schemas(&state)
        .iter()
        .map(|schema| config_dto(&state, &schema.adapter_id))
        .collect()
}

/// Save an adapter's settings and secrets, then hot-apply them.
#[tauri::command]
pub async fn save_adapter_config(
    request: SaveAdapterConfigRequest,
    state: State<'_, AppState>,
) -> Result<AdapterConfigDto, String> {
    let mut request = request;
    // Keys the app manages itself (such as a login token's expiry) can come back from
    // the form; they are not editable fields, so drop them instead of rejecting the save.
    request.settings.retain(|key, _| !key.ends_with("_expires_at"));
    let adapter_id = request.adapter_id.as_str();
    validation::validate_adapter_id(adapter_id)?;
    if !state.vault.is_unlocked() {
        return Err("Vault is locked. Unlock it to change adapter credentials.".into());
    }

    let fields = allowed_fields(&state, adapter_id);
    let kind_of = |key: &str| fields.iter().find(|f| f.key == key).map(|f| f.kind.as_str());

    for (key, value) in request.settings.iter().chain(request.secrets.iter()) {
        if kind_of(key).is_none() {
            return Err(format!("Unknown field `{key}` for {adapter_id}"));
        }
        if value.len() > MAX_FIELD_LEN {
            return Err(format!("Field `{key}` is too long"));
        }
    }
    for key in request.settings.keys() {
        if kind_of(key) == Some("secret") {
            return Err(format!("`{key}` is a secret and cannot be saved as a setting"));
        }
    }
    for key in request.secrets.keys().chain(request.clear_secrets.iter()) {
        if kind_of(key) != Some("secret") {
            return Err(format!("`{key}` is not a secret field"));
        }
    }

    // Merge with stored settings so partial updates keep other keys (e.g. the
    // login-managed token expiry).
    let mut merged: serde_json::Map<String, serde_json::Value> = state
        .vault
        .get_config(adapter_id)?
        .and_then(|c| serde_json::from_str(&c.settings_json).ok())
        .unwrap_or_default();
    for (key, value) in &request.settings {
        merged.insert(key.clone(), serde_json::Value::String(value.trim().to_string()));
    }
    state
        .vault
        .set_config(adapter_id, request.enabled, &serde_json::Value::Object(merged).to_string())?;

    for (key, value) in &request.secrets {
        let value = value.trim();
        if !value.is_empty() {
            state.vault.set_secret(adapter_id, key, value)?;
        }
    }
    for key in &request.clear_secrets {
        state.vault.delete_secret(adapter_id, key)?;
    }

    if !adapter_id.starts_with("llm:") {
        state.apply_broker(adapter_id).await.map_err(|e| e.to_string())?;
    }
    config_dto(&state, adapter_id)
}

#[tauri::command]
pub async fn set_adapter_enabled(
    adapter_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<AdapterConfigDto, String> {
    validation::validate_adapter_id(&adapter_id)?;
    if !state.vault.is_unlocked() {
        return Err("Vault is locked".into());
    }
    let settings = state
        .vault
        .get_config(&adapter_id)?
        .map(|c| c.settings_json)
        .unwrap_or_else(|| "{}".into());
    state.vault.set_config(&adapter_id, enabled, &settings)?;
    if !adapter_id.starts_with("llm:") {
        state.apply_broker(&adapter_id).await.map_err(|e| e.to_string())?;
    }
    config_dto(&state, &adapter_id)
}

/// Remove an adapter's configuration and every stored secret.
#[tauri::command]
pub async fn delete_adapter_config(
    adapter_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validation::validate_adapter_id(&adapter_id)?;
    if !state.vault.is_unlocked() {
        return Err("Vault is locked. Unlock it to remove credentials.".into());
    }
    state.vault.delete_adapter(&adapter_id)?;
    if !adapter_id.starts_with("llm:") {
        state.reset_broker(&adapter_id).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Check that an adapter's saved credentials work, without placing orders.
#[tauri::command]
pub async fn test_adapter_connection(
    adapter_id: String,
    state: State<'_, AppState>,
) -> Result<AdapterTestResultDto, String> {
    validation::validate_adapter_id(&adapter_id)?;
    let result = |ok: bool, message: String| AdapterTestResultDto {
        adapter_id: adapter_id.clone(),
        ok,
        message,
    };

    if adapter_id.starts_with("llm:") {
        let stored = state
            .vault
            .secret_fields(&adapter_id)?
            .iter()
            .any(|f| f == "api_key");
        return Ok(if stored {
            result(true, "API key is stored.".into())
        } else {
            result(false, "No API key stored.".into())
        });
    }

    let Some(connection) = state.broker_connection(&adapter_id) else {
        return Ok(result(false, "Unknown adapter".into()));
    };
    if !connection.configured {
        return Ok(result(false, connection.message));
    }
    if connection.execution_available && connection.authenticated {
        return Ok(match state.otm.get_account_balance(&adapter_id).await {
            Ok(balance) => result(
                true,
                format!("Connected. Available balance {:.2}.", balance.cash),
            ),
            Err(error) => result(false, format!("Account check failed: {error}")),
        });
    }
    Ok(result(
        connection.authenticated,
        connection.message,
    ))
}
