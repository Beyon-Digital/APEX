use std::fs;

use anyhow::{anyhow, Context};
use tauri::State;
use toml_edit::{value, DocumentMut, Item, Table};

use crate::commands::python_runtime::RuntimePaths;
use crate::config::{AppConfig, StorageConfig};
use crate::dto::{
    AcpSettingsDto, AdapterPreferenceDto, AppSettingsDto, AppSettingsUpdateDto,
    AppearanceSettingsDto, GeneralSettingsDto, LlmProviderDto, LlmSettingsDto, RiskSettingsDto,
    StorageSettingsDto,
};
use crate::state::AppState;

const EXECUTION_ADAPTERS: &[&str] = &[
    "paper",
    "zerodha",
    "angel_one",
    "groww",
    "robinhood",
    "binance",
    "coinbase",
];
const MARKET_DATA_ADAPTERS: &[&str] = &[
    "yahoo_finance",
    "zerodha",
    "angel_one",
    "groww",
    "robinhood",
    "binance",
    "coinbase",
    "polymarket",
];
const STORAGE_BACKENDS: &[&str] = &["sqlite", "timescale"];
const THEMES: &[&str] = &["terminal", "dark", "light"];
const DENSITIES: &[&str] = &["comfortable", "compact"];
const LLM_API_KINDS: &[&str] = &["chat", "responses", "acp"];

/// Whether the vault holds an API key for this LLM provider (never exposes it).
fn key_stored(state: &AppState, provider_id: &str) -> bool {
    state
        .vault
        .secret_fields(&format!("llm:{provider_id}"))
        .map(|fields| fields.iter().any(|f| f == "api_key"))
        .unwrap_or(false)
}

#[tauri::command]
pub async fn get_app_settings(
    runtime_paths: State<'_, RuntimePaths>,
    state: State<'_, AppState>,
) -> Result<AppSettingsDto, String> {
    let config = AppConfig::load(&runtime_paths).map_err(|error| error.to_string())?;
    Ok(build_settings_dto(&config, &runtime_paths, &state))
}

#[tauri::command]
pub async fn save_app_settings(
    request: AppSettingsUpdateDto,
    runtime_paths: State<'_, RuntimePaths>,
    state: State<'_, AppState>,
) -> Result<AppSettingsDto, String> {
    validate_settings_request(&request).map_err(|error| error.to_string())?;
    write_settings_to_file(runtime_paths.config_file(), &request)
        .map_err(|error| error.to_string())?;
    let config = AppConfig::load(&runtime_paths).map_err(|error| error.to_string())?;
    Ok(build_settings_dto(&config, &runtime_paths, &state))
}

fn build_settings_dto(
    config: &AppConfig,
    runtime_paths: &RuntimePaths,
    state: &AppState,
) -> AppSettingsDto {
    AppSettingsDto {
        config_path: runtime_paths.config_file().display().to_string(),
        runtime_storage_backend: state.storage_backend.clone(),
        runtime_storage_target: state.storage_target.clone(),
        runtime_storage_notice: state.storage_notice.clone(),
        general: GeneralSettingsDto {
            data_dir: config.general.data_dir.clone(),
        },
        market_data: AdapterPreferenceDto {
            adapter: config.market_data.adapter.clone(),
            available_adapters: MARKET_DATA_ADAPTERS
                .iter()
                .map(|adapter| (*adapter).to_string())
                .collect(),
        },
        execution: AdapterPreferenceDto {
            adapter: config.execution.adapter.clone(),
            available_adapters: EXECUTION_ADAPTERS
                .iter()
                .map(|adapter| (*adapter).to_string())
                .collect(),
        },
        risk: RiskSettingsDto {
            max_daily_loss: config.risk.max_daily_loss,
            max_order_value: config.risk.max_order_value,
        },
        storage: StorageSettingsDto {
            backend: config.storage.backend.clone(),
            sqlite_path: config.storage.sqlite_path.clone(),
            postgres_url: config.storage.postgres_url.clone().unwrap_or_default(),
            wal_mode: config.storage.wal_mode,
            pool_size: config.storage.pool_size,
            available_backends: STORAGE_BACKENDS
                .iter()
                .map(|backend| (*backend).to_string())
                .collect(),
        },
        appearance: AppearanceSettingsDto {
            theme: config.appearance.theme.clone(),
            density: config.appearance.density.clone(),
        },
        llm: LlmSettingsDto {
            active: config.llm.active.clone(),
            providers: config
                .llm
                .providers
                .iter()
                .map(|p| LlmProviderDto {
                    id: p.id.clone(),
                    name: if p.name.is_empty() {
                        p.id.clone()
                    } else {
                        p.name.clone()
                    },
                    base_url: p.base_url.clone(),
                    model: p.model.clone(),
                    api_kind: p.api_kind.clone(),
                    api_key_env: p.api_key_env.clone(),
                    max_tokens: p.max_tokens,
                    key_configured: key_stored(state, &p.id),
                })
                .collect(),
        },
        acp: AcpSettingsDto {
            command: config.acp.command.clone(),
            cwd: config.acp.cwd.clone(),
        },
    }
}

fn validate_settings_request(request: &AppSettingsUpdateDto) -> anyhow::Result<()> {
    ensure_allowed_adapter(
        request.execution.adapter.trim(),
        EXECUTION_ADAPTERS,
        "execution.adapter",
    )?;
    ensure_allowed_adapter(
        request.market_data.adapter.trim(),
        MARKET_DATA_ADAPTERS,
        "market_data.adapter",
    )?;

    let storage = StorageConfig {
        backend: request.storage.backend.clone(),
        sqlite_path: request.storage.sqlite_path.clone(),
        postgres_url: Some(request.storage.postgres_url.clone()),
        wal_mode: request.storage.wal_mode,
        pool_size: request.storage.pool_size,
    };
    let backend = storage.backend_kind()?;

    if matches!(backend, crate::config::StorageBackendKind::Timescale)
        && storage.postgres_url().is_none()
    {
        return Err(anyhow!(
            "A PostgreSQL/Timescale connection URL is required when the storage backend is set to timescale."
        ));
    }

    if request.risk.max_daily_loss <= 0.0 {
        return Err(anyhow!("Risk max daily loss must be greater than 0"));
    }

    if request.risk.max_order_value <= 0.0 {
        return Err(anyhow!("Risk max order value must be greater than 0"));
    }

    if request.storage.pool_size == 0 {
        return Err(anyhow!("Storage pool size must be at least 1"));
    }

    ensure_allowed_adapter(request.appearance.theme.trim(), THEMES, "appearance.theme")?;
    ensure_allowed_adapter(
        request.appearance.density.trim(),
        DENSITIES,
        "appearance.density",
    )?;

    let mut seen_ids = std::collections::HashSet::new();
    for p in &request.llm.providers {
        if p.id.trim().is_empty() {
            return Err(anyhow!("LLM provider id must not be empty"));
        }
        if !seen_ids.insert(p.id.trim().to_string()) {
            return Err(anyhow!("Duplicate LLM provider id `{}`", p.id));
        }
        ensure_allowed_adapter(p.api_kind.trim(), LLM_API_KINDS, "llm.providers.api_kind")?;
        if p.api_kind != "acp" && p.base_url.trim().is_empty() {
            return Err(anyhow!(
                "LLM provider `{}` needs a base_url (OpenAI-compatible endpoint)",
                p.id
            ));
        }
        if p.api_kind != "acp" {
            crate::validation::validate_provider_url(&p.base_url)
                .map_err(|e| anyhow!("LLM provider `{}`: {}", p.id, e))?;
        }
    }
    if !request.llm.active.trim().is_empty()
        && !request
            .llm
            .providers
            .iter()
            .any(|p| p.id == request.llm.active)
    {
        return Err(anyhow!(
            "Active LLM provider `{}` is not in the provider list",
            request.llm.active
        ));
    }

    Ok(())
}

fn ensure_allowed_adapter(value: &str, allowed: &[&str], field: &str) -> anyhow::Result<()> {
    if allowed.iter().any(|candidate| *candidate == value) {
        Ok(())
    } else {
        Err(anyhow!(
            "Unsupported value `{value}` for {field}. Allowed values: {}",
            allowed.join(", ")
        ))
    }
}

fn write_settings_to_file(
    config_path: &std::path::Path,
    request: &AppSettingsUpdateDto,
) -> anyhow::Result<()> {
    let raw = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file {}", config_path.display()))?;
    let mut doc = raw
        .parse::<DocumentMut>()
        .with_context(|| format!("Failed to parse config file {}", config_path.display()))?;

    ensure_table(&mut doc, "general");
    ensure_table(&mut doc, "market_data");
    ensure_table(&mut doc, "execution");
    ensure_table(&mut doc, "risk");
    ensure_table(&mut doc, "storage");

    doc["general"]["data_dir"] = value(non_empty_or_default(&request.general.data_dir, "data"));
    doc["market_data"]["adapter"] = value(request.market_data.adapter.trim());
    doc["execution"]["adapter"] = value(request.execution.adapter.trim());
    doc["risk"]["max_daily_loss"] = value(request.risk.max_daily_loss);
    doc["risk"]["max_order_value"] = value(request.risk.max_order_value);
    doc["storage"]["backend"] = value(request.storage.backend.trim());
    doc["storage"]["sqlite_path"] = value(non_empty_or_default(
        &request.storage.sqlite_path,
        "apex.db",
    ));
    doc["storage"]["postgres_url"] = value(request.storage.postgres_url.trim());
    doc["storage"]["wal_mode"] = value(request.storage.wal_mode);
    doc["storage"]["pool_size"] = value(request.storage.pool_size as i64);

    ensure_table(&mut doc, "appearance");
    doc["appearance"]["theme"] = value(request.appearance.theme.trim());
    doc["appearance"]["density"] = value(request.appearance.density.trim());

    ensure_table(&mut doc, "acp");
    doc["acp"]["command"] = value(request.acp.command.trim());
    doc["acp"]["cwd"] = value(request.acp.cwd.trim());

    ensure_table(&mut doc, "llm");
    doc["llm"]["active"] = value(request.llm.active.trim());
    let mut providers_table = toml_edit::ArrayOfTables::new();
    for p in &request.llm.providers {
        let mut t = Table::new();
        t["id"] = value(p.id.trim());
        t["name"] = value(p.name.trim());
        t["base_url"] = value(p.base_url.trim());
        t["model"] = value(p.model.trim());
        t["api_kind"] = value(p.api_kind.trim());
        t["api_key_env"] = value(p.api_key_env.trim());
        t["max_tokens"] = value(p.max_tokens as i64);
        providers_table.push(t);
    }
    doc["llm"]["providers"] = Item::ArrayOfTables(providers_table);

    fs::write(config_path, doc.to_string())
        .with_context(|| format!("Failed to write config file {}", config_path.display()))?;

    Ok(())
}

fn ensure_table(doc: &mut DocumentMut, section: &str) {
    let present = doc.get(section).map(|i| i.is_table()).unwrap_or(false);
    if !present {
        doc[section] = Item::Table(Table::new());
    }
}

fn non_empty_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}
