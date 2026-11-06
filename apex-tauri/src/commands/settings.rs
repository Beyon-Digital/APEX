//! Application settings (`config/apex.toml`).
//!
//! The file is a versioned object of the audit ledger (kind `settings`, object id
//! [`SETTINGS_ID`]). The ledger never holds a secret: what it stores is [`redact_config`] of the
//! file. A save writes the ledger first (fail closed: when that fails the file is not touched) and
//! then replaces the file atomically. Edits made outside the app are noticed by comparing
//! `redact_config(file)` with the head and recorded as an `external` version.
//!
//! HONEST LIMITS. (1) A change that only touches a redacted value (a database password, an API
//! token) produces the same redacted text, so it adds no ledger version; the activity entry says the
//! key changed, never the value. (2) A settings version cannot be restored: the redacted copy lacks
//! the secrets, so `ledger_restore` refuses settings and the history is view-only.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context};
use tauri::State;
use toml_edit::{value, DocumentMut, Item, Table};

use crate::commands::python_runtime::RuntimePaths;
use crate::commands::versioned_files;
use crate::ledger::{Kind, Ledger, VersionRecord};
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
    // Out-of-app edits are recorded the moment the settings are looked at. Never fails the read.
    note_external_edit(&state, runtime_paths.config_file());
    let config = AppConfig::load(&runtime_paths).map_err(|error| error.to_string())?;
    Ok(build_settings_dto(&config, &runtime_paths, &state))
}

#[tauri::command]
pub async fn save_app_settings(
    request: AppSettingsUpdateDto,
    runtime_paths: State<'_, RuntimePaths>,
    state: State<'_, AppState>,
) -> Result<AppSettingsDto, String> {
    save_settings_audited(&state, &runtime_paths, request, "user")
}

/// Validate, version (ledger first), write the file, record the activity entry, and return the
/// settings as the UI shows them. Every outcome, including a refusal, is an activity entry.
pub(crate) fn save_settings_audited(
    state: &AppState,
    runtime_paths: &RuntimePaths,
    request: AppSettingsUpdateDto,
    actor: &str,
) -> Result<AppSettingsDto, String> {
    let outcome = validate_settings_request(&request)
        .map_err(|error| error.to_string())
        .and_then(|_| {
            let ledger = versioned_files::ledger()?;
            save_settings_inner(ledger, runtime_paths.config_file(), &request, actor)
        });
    match outcome {
        Ok(change) => {
            if let Some(found) = &change.external {
                let e = external_edit_entry(found);
                crate::activity::record(state, &e.actor, &e.action, &e.kind, &e.target_id, &e.summary, e.ok, e.details);
            }
            let summary = if change.changed.is_empty() {
                "Settings saved (no values changed)".to_string()
            } else {
                format!("Settings saved: changed {}", list_keys(&change.changed))
            };
            crate::activity::record(state, actor, "settings.save", "system", SETTINGS_ID, &summary, true, Some(change.details.clone()).filter(|d| !d.is_empty()));
            let config = AppConfig::load(runtime_paths).map_err(|error| error.to_string())?;
            Ok(build_settings_dto(&config, runtime_paths, state))
        }
        Err(error) => {
            crate::activity::record(
                state,
                actor,
                "settings.save",
                "system",
                SETTINGS_ID,
                "Settings were not saved",
                false,
                Some(scrub_error(&error, &[])),
            );
            Err(error)
        }
    }
}

/// Record an out-of-app edit of the settings file as a version, if there is one.
fn note_external_edit(state: &AppState, config_path: &Path) {
    let Some(ledger) = versioned_files::try_ledger() else { return };
    match reconcile_settings(ledger, config_path) {
        Ok(Some(found)) if found.record.actor == "external" => {
            let e = external_edit_entry(&found);
            crate::activity::record(state, &e.actor, &e.action, &e.kind, &e.target_id, &e.summary, e.ok, e.details);
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not compare the settings file with the audit ledger"),
    }
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

/// The new text of the settings file: the current `raw` text with the requested values applied
/// (everything else, including comments and layout, stays as it was).
fn build_settings_text(raw: &str, request: &AppSettingsUpdateDto) -> anyhow::Result<String> {
    let mut doc = raw
        .parse::<DocumentMut>()
        .context("Failed to parse the config file")?;

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

    Ok(doc.to_string())
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

// ── Versioning ───────────────────────────────────────────────────────

/// Object id of the settings file in the audit ledger.
pub const SETTINGS_ID: &str = "config/apex.toml";

/// Serializes saves and reconciles of the settings file.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

fn settings_lock() -> std::sync::MutexGuard<'static, ()> {
    SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a save did.
#[derive(Debug, Clone, Default)]
pub(crate) struct SettingsChange {
    /// Dotted keys whose value changed (added, removed or modified), sorted.
    pub changed: Vec<String>,
    /// `key: old -> new` for non-secret values; secret keys say `changed` only.
    pub details: String,
    /// The ledger version holding the saved text (the head when the redacted text did not change).
    pub version: Option<u32>,
    /// Set when an edit made outside the app was found and recorded before this save.
    pub external: Option<Reconciled>,
}

/// Up to a dozen keys, then a count.
fn list_keys(keys: &[String]) -> String {
    const SHOWN: usize = 12;
    if keys.len() <= SHOWN {
        keys.join(", ")
    } else {
        format!("{} and {} more", keys[..SHOWN].join(", "), keys.len() - SHOWN)
    }
}

/// A version appended by [`reconcile_settings`], with what differs from the version before it.
#[derive(Debug, Clone)]
pub(crate) struct Reconciled {
    pub record: VersionRecord,
    /// Keys that differ from the previous head (empty for the first registration).
    pub changed: Vec<String>,
    /// `key: old -> new` for those keys (secrets are already `<redacted>` in both, so never shown).
    pub details: String,
}

/// Bring the ledger in step with the file. No history yet: version 1 by actor `system`. File differs
/// from the head (comparing `redact_config(file)` with the head, never the raw file): a new version by
/// actor `external`. Returns what was appended, if anything. A missing file is not an error.
pub(crate) fn reconcile_settings(ledger: &Ledger, config_path: &Path) -> Result<Option<Reconciled>, String> {
    let _guard = settings_lock();
    reconcile_locked(ledger, config_path)
}

fn reconcile_locked(ledger: &Ledger, config_path: &Path) -> Result<Option<Reconciled>, String> {
    let bytes = match fs::read(config_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", config_path.display())),
    };
    let redacted = redact_config(&String::from_utf8_lossy(&bytes));
    let hash = Ledger::sha256_hex(redacted.as_bytes());
    match ledger.head(Kind::Settings, SETTINGS_ID) {
        None => {
            let record = ledger.put(Kind::Settings, SETTINGS_ID, redacted.as_bytes(), "system", "Registered the settings file (secrets redacted)")?;
            Ok(Some(Reconciled { record, changed: Vec::new(), details: String::new() }))
        }
        Some(head) if !head.is_tombstone() && head.content_hash.as_deref() == Some(hash.as_str()) => Ok(None),
        Some(head) => {
            let previous = if head.is_tombstone() { String::new() } else { String::from_utf8_lossy(&ledger.content(Kind::Settings, SETTINGS_ID, head.version)?).to_string() };
            let (changed, details) = describe_settings_change(&previous, &redacted);
            let summary = if changed.is_empty() { "The settings file was changed outside APEX (secrets redacted)".to_string() } else { format!("The settings file was changed outside APEX: {}", list_keys(&changed)) };
            let record = ledger.put_as(Kind::Settings, SETTINGS_ID, redacted.as_bytes(), "external", "external", &summary)?;
            Ok(Some(Reconciled { record, changed, details }))
        }
    }
}

/// The activity entry for an outside edit found by [`reconcile_settings`].
pub(crate) fn external_edit_entry(found: &Reconciled) -> crate::activity::ActivityDto {
    crate::activity::ActivityDto {
        id: uuid::Uuid::new_v4().to_string(),
        ts: chrono::Utc::now().to_rfc3339(),
        actor: "external".into(),
        action: "settings.external_edit".into(),
        kind: "system".into(),
        target_id: SETTINGS_ID.into(),
        summary: if found.changed.is_empty() {
            format!("The settings file was changed outside APEX (recorded as version {})", found.record.version)
        } else {
            format!("The settings file was changed outside APEX (version {}): changed {}", found.record.version, list_keys(&found.changed))
        },
        ok: true,
        details: Some(found.details.clone()).filter(|d| !d.is_empty()),
    }
}

/// Save settings: pick up any outside edit, build the new text, write the ledger version FIRST (when
/// that fails nothing is written), then replace the file atomically.
pub(crate) fn save_settings_inner(
    ledger: &Ledger,
    config_path: &Path,
    request: &AppSettingsUpdateDto,
    actor: &str,
) -> Result<SettingsChange, String> {
    let _guard = settings_lock();
    let external = reconcile_locked(ledger, config_path)?.filter(|r| r.record.actor == "external");
    let raw = fs::read_to_string(config_path).map_err(|e| format!("Failed to read config file {}: {e}", config_path.display()))?;
    let new_text = build_settings_text(&raw, request).map_err(|e| e.to_string())?;
    let (changed, details) = describe_settings_change(&raw, &new_text);
    let mut change = SettingsChange { changed, details, version: None, external };
    if new_text == raw {
        change.version = ledger.head(Kind::Settings, SETTINGS_ID).map(|h| h.version);
        return Ok(change);
    }
    let summary = if change.changed.is_empty() { "Settings saved (only formatting or hidden values changed)".to_string() } else { format!("Changed {}", list_keys(&change.changed)) };
    let rec = ledger.put(Kind::Settings, SETTINGS_ID, redact_config(&new_text).as_bytes(), actor, &summary)?;
    change.version = Some(rec.version);
    write_atomic(config_path, &new_text).map_err(|e| {
        format!(
            "Settings were recorded as version {} in the audit ledger, but the file {} could not be updated ({e}). The file is unchanged; the difference is recorded the next time APEX looks at it.",
            rec.version,
            config_path.display()
        )
    })?;
    Ok(change)
}

/// Write `text` to a temporary file next to `path` and rename it into place.
///
/// A symlinked `path` is followed: the file it points to is replaced and the link stays. The
/// temporary file is created readable by its owner only, so the text (which can hold a password) is
/// never visible to other users while it is written; the original file's permissions are applied
/// afterwards, just before the rename.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let path = &fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "config".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let write = || -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

// ── What changed ─────────────────────────────────────────────────────

/// path -> (value text, whether any key on the path is secret-named)
type Flat = BTreeMap<String, (String, bool)>;

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn flatten_table(table: &Table, prefix: &str, secret: bool, out: &mut Flat) {
    for (key, item) in table.iter() {
        let path = join_path(prefix, key);
        flatten_item(item, &path, secret || is_secret_key_name(key), out);
    }
}

fn flatten_item(item: &Item, path: &str, secret: bool, out: &mut Flat) {
    match item {
        Item::Table(t) => flatten_table(t, path, secret, out),
        Item::ArrayOfTables(array) => {
            for (n, t) in array.iter().enumerate() {
                let label = t.get("id").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| n.to_string());
                flatten_table(t, &format!("{path}[{label}]"), secret, out);
            }
        }
        Item::Value(v) => flatten_value(v, path, secret, out),
        Item::None => {}
    }
}

fn flatten_value(v: &toml_edit::Value, path: &str, secret: bool, out: &mut Flat) {
    match v {
        toml_edit::Value::InlineTable(t) => {
            for (key, inner) in t.iter() {
                flatten_value(inner, &join_path(path, key), secret || is_secret_key_name(key), out);
            }
        }
        other => {
            // Numbers are compared by value, so `50_000.0` and `50000.0` are the same setting.
            let text = if let Some(f) = other.as_float() {
                format!("{f:?}")
            } else if let Some(i) = other.as_integer() {
                i.to_string()
            } else {
                other.as_str().map(String::from).unwrap_or_else(|| other.to_string().trim().to_string())
            };
            out.insert(path.to_string(), (text, secret));
        }
    }
}

fn flatten_text(text: &str) -> Flat {
    let mut out = Flat::new();
    if let Ok(doc) = text.parse::<DocumentMut>() {
        flatten_table(doc.as_table(), "", false, &mut out);
    }
    out
}

/// A value as it may appear in the activity log: URL credentials and query strings removed, clipped.
fn display_value(raw: &str) -> String {
    let mut text = redact_urls(raw);
    if text.contains("://") {
        if let Some(q) = text.find('?') {
            text.truncate(q);
            text.push_str("?<query omitted>");
        }
    }
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if flat.is_empty() {
        return "(empty)".to_string();
    }
    if flat.chars().count() > 120 {
        format!("{}…", flat.chars().take(120).collect::<String>())
    } else {
        flat
    }
}

/// (changed keys, `key: old -> new; ...`). Secret keys show `changed`, added or removed, never a value.
pub(crate) fn describe_settings_change(old_text: &str, new_text: &str) -> (Vec<String>, String) {
    let (mut old, mut new) = (flatten_text(old_text), flatten_text(new_text));
    // A key that appears or disappears holding an empty string is not a change of setting.
    old.retain(|k, v| !(v.0.is_empty() && !new.contains_key(k)));
    new.retain(|k, v| !(v.0.is_empty() && !old.contains_key(k)));
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();
    let (mut changed, mut lines) = (Vec::new(), Vec::new());
    for key in keys {
        let (o, n) = (old.get(key), new.get(key));
        if o.map(|v| &v.0) == n.map(|v| &v.0) {
            continue;
        }
        changed.push(key.clone());
        let secret = o.map(|v| v.1).unwrap_or(false) || n.map(|v| v.1).unwrap_or(false);
        lines.push(match (o, n) {
            _ if secret => format!("{key}: changed"),
            (Some((a, _)), Some((b, _))) => {
                let (da, db) = (display_value(a), display_value(b));
                if da == db {
                    format!("{key}: changed (credentials or query only)")
                } else {
                    format!("{key}: {da} -> {db}")
                }
            }
            (None, Some((b, _))) => format!("{key}: added {}", display_value(b)),
            (Some((a, _)), None) => format!("{key}: removed (was {})", display_value(a)),
            (None, None) => unreachable!(),
        });
    }
    (changed, lines.join("; "))
}

/// An error text safe for the activity log: URL credentials and secret query values removed, any
/// `secrets` given removed wherever they appear, clipped.
pub(crate) fn scrub_error(text: &str, secrets: &[&str]) -> String {
    let mut out = redact_urls(text);
    for secret in secrets.iter().filter(|s| !s.trim().is_empty()) {
        out = out.replace(*secret, REDACTED);
    }
    if out.chars().count() > 300 {
        format!("{}…", out.chars().take(300).collect::<String>())
    } else {
        out
    }
}

// ── Redaction ────────────────────────────────────────────────────────

/// What replaces a secret.
pub const REDACTED: &str = "<redacted>";

const SECRET_MARKERS: [&str; 11] = [
    "password", "passwd", "passphrase", "secret", "token", "api_key", "apikey", "private_key", "key_material",
    "authorization", "credential",
];

/// Words that, in front of `_tokens`, make it a token COUNT (`max_tokens`), not a credential.
const TOKEN_COUNT_WORDS: [&str; 9] = ["max", "min", "num", "total", "prompt", "completion", "context", "output", "input"];

/// Whether a key holds a secret, by its name.
///
/// The name is lowercased (and `-` read as `_`). It is secret when it CONTAINS one of: password,
/// passwd, passphrase, secret, token, api_key, apikey, private_key, key_material. It is a deliberate
/// substring rule that errs towards hiding: `tokenizer` or `secret_santa` are hidden too.
///
/// Exceptions, because they name or count something instead of holding a credential and the audit
/// needs to see them: a name ending in `_env` or `_env_var` (the NAME of an environment variable, as
/// in `api_key_env`), and token counts such as `max_tokens` or `max_output_tokens`.
///
/// Not matched on purpose: a bare `key` or a name that only contains the word (`keyboard_shortcut_key`,
/// `monkey`, `sort_key`). Those stay readable.
pub fn is_secret_key_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase().replace('-', "_");
    if n.is_empty() || n.ends_with("_env") || n.ends_with("_env_var") {
        return false;
    }
    if let Some(stem) = n.strip_suffix("_tokens") {
        if TOKEN_COUNT_WORDS.contains(&stem.rsplit('_').next().unwrap_or(stem)) {
            return false;
        }
    }
    SECRET_MARKERS.iter().any(|m| n.contains(m))
}

/// A URL query parameter name whose value is hidden: secret-named, or one of the usual bare names.
fn is_secret_query_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    matches!(n.as_str(), "key" | "sig" | "signature" | "auth" | "authorization" | "sas" | "code") || is_secret_key_name(&n)
}

/// The text of a config file with every secret replaced by `<redacted>`; everything else is kept
/// byte for byte (comments, layout, line endings).
///
/// Hidden: (1) the value of a key whose name is secret (see [`is_secret_key_name`]), including a
/// dotted key with a secret part, a key inside a table whose header has a secret part, and keys of
/// inline tables; the whole value goes, whatever its type. (2) The password in any URL's userinfo
/// (`scheme://user:PASSWORD@host`), in values AND comments. A userinfo without a colon is left as
/// is. (3) The value of a secret-named parameter in a URL query (`?password=…`, `?key=…`).
///
/// Pure, never panics (malformed text is scanned leniently), and idempotent:
/// `redact_config(redact_config(x)) == redact_config(x)`.
pub fn redact_config(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut r = Redactor { c: &chars, out: String::with_capacity(text.len()) };
    r.run();
    redact_urls(&r.out)
}

/// The URL rules and the `name=value` rule of [`redact_config`] on any text.
pub fn redact_urls(text: &str) -> String {
    redact_bearer(&redact_pairs(&redact_url_parts(text)))
}

/// `Bearer <token>` wherever it appears, whatever the key is called: an authorization header copied
/// into a note or a value is a credential even when its key name does not look secret. The word
/// `Bearer` stays so the text still reads; the token after it is replaced. Idempotent.
fn redact_bearer(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(rel) = lower[from..].find("bearer") {
        let word = from + rel;
        let end = word + "bearer".len();
        // Only the whole word, followed by whitespace and then a token.
        let before_ok = word == 0 || !lower.as_bytes()[word - 1].is_ascii_alphanumeric();
        let rest = &text[end..];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let token_start = end + ws;
        let token_len = text[token_start..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == ';')
            .unwrap_or(text.len() - token_start);
        let token = &text[token_start..token_start + token_len];
        // A real token is long and made of token characters, and either has a digit or symbol in it or
        // is very long. Ordinary words ("the bearer of this key") are not tokens.
        let looks_like_token = token.len() >= 8
            && token.chars().all(|c| c.is_ascii_alphanumeric() || "._~+/=-".contains(c))
            && (token.chars().any(|c| !c.is_ascii_alphabetic()) || token.len() >= 20);
        if before_ok && ws > 0 && looks_like_token && token != "<redacted>" {
            out.push_str(&text[last..token_start]);
            out.push_str("<redacted>");
            last = token_start + token_len;
            from = last;
        } else {
            from = end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// `name=value` pairs whose name is secret (see [`is_secret_key_name`]), anywhere in the text: a
/// key=value connection string (`host=db user=u password=hunter2`), a commented-out setting
/// (`# api_key = "old"`), or a note after a value. The value is the quoted text after `=` or, unquoted,
/// everything up to whitespace or a quote. A value that is already `<redacted>` is left alone, so the
/// rule is idempotent and does not eat what follows a redacted URL parameter (`?token=<redacted>&x=1`).
fn redact_pairs(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let is_ident = |ch: char| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < c.len() {
        if !is_ident(c[i]) || (i > 0 && is_ident(c[i - 1])) {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let mut name_end = i;
        while name_end < c.len() && is_ident(c[name_end]) {
            name_end += 1;
        }
        let name: String = c[i..name_end].iter().collect();
        out.push_str(&name);
        i = name_end;
        if !is_secret_key_name(&name) {
            continue;
        }
        // name, an optional closing quote of a quoted key, spaces, `=`, spaces, then the value.
        let mut k = i;
        if matches!(c.get(k), Some('"' | '\'')) {
            k += 1;
        }
        while matches!(c.get(k), Some(' ' | '\t')) {
            k += 1;
        }
        if c.get(k) != Some(&'=') || c.get(k + 1) == Some(&'=') {
            continue;
        }
        k += 1;
        while matches!(c.get(k), Some(' ' | '\t')) {
            k += 1;
        }
        // The value: quoted (`"x"`, `'x'`, or `\"x\"` inside a TOML string) or bare.
        let (open, close): (&[char], &[char]) = match (c.get(k), c.get(k + 1)) {
            (Some('\\'), Some('"')) => (&['\\', '"'], &['\\', '"']),
            (Some('"'), _) => (&['"'], &['"']),
            (Some('\''), _) => (&['\''], &['\'']),
            _ => (&[], &[]),
        };
        let value_start = k + open.len();
        let (value_end, resume) = if open.is_empty() {
            let mut e = value_start;
            while e < c.len() && !c[e].is_whitespace() && !matches!(c[e], '"' | '\'') {
                e += 1;
            }
            (e, e)
        } else {
            let mut e = value_start;
            while e < c.len() && c[e] != '\n' && !c[e..].starts_with(close) {
                // A backslash escapes the next character inside a double-quoted string.
                e += if c[e] == '\\' && close == ['"'] { 2 } else { 1 };
            }
            let e = e.min(c.len());
            let closed = e < c.len() && c[e..].starts_with(close);
            (e, if closed { e + close.len() } else { e })
        };
        let value: String = c[value_start..value_end].iter().collect();
        out.extend(&c[i..value_start]);
        if value.is_empty() || value.starts_with(REDACTED) {
            out.push_str(&value);
        } else {
            out.push_str(REDACTED);
        }
        out.extend(&c[value_end..resume]);
        i = resume;
    }
    out
}

fn redact_url_parts(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == ':' && c.get(i + 1) == Some(&'/') && c.get(i + 2) == Some(&'/') {
            out.push_str("://");
            i += 3;
            let mut end = i;
            while end < c.len() {
                let ch = c[end];
                if ch.is_whitespace() || ((ch == '"' || ch == '\'') && !(end > i && c[end - 1] == '\\')) {
                    break;
                }
                end += 1;
            }
            out.push_str(&redact_url_body(&c[i..end]));
            i = end;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `userinfo@host/path?query#fragment` (everything after `://`).
fn redact_url_body(body: &[char]) -> String {
    let auth_end = body.iter().position(|c| matches!(c, '/' | '?' | '#')).unwrap_or(body.len());
    let auth = &body[..auth_end];
    let mut out = String::new();
    match auth.iter().rposition(|c| *c == '@') {
        Some(at) => {
            let userinfo = &auth[..at];
            match userinfo.iter().position(|c| *c == ':') {
                Some(colon) if colon + 1 < userinfo.len() => {
                    out.extend(&userinfo[..=colon]);
                    out.push_str(REDACTED);
                    out.extend(&auth[at..]);
                }
                _ => out.extend(auth),
            }
        }
        None => out.extend(auth),
    }
    let rest = &body[auth_end..];
    let Some(q) = rest.iter().position(|c| *c == '?') else {
        out.extend(rest);
        return out;
    };
    out.extend(&rest[..=q]);
    let query_end = rest[q..].iter().position(|c| *c == '#').map(|p| q + p).unwrap_or(rest.len());
    let query: String = rest[q + 1..query_end].iter().collect();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        match token.split_once('=') {
            Some((name, value)) if !value.is_empty() && is_secret_query_name(name) => {
                out.push_str(name);
                out.push('=');
                out.push_str(REDACTED);
            }
            _ => out.push_str(token),
        }
        token.clear();
    };
    for ch in query.chars() {
        if ch == '&' || ch == ';' {
            flush(&mut token, &mut out);
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush(&mut token, &mut out);
    out.extend(&rest[query_end..]);
    out
}

/// Lenient TOML scanner: finds the keys and values, copies everything else through unchanged.
struct Redactor<'a> {
    c: &'a [char],
    out: String,
}

const REDACTED_VALUE: &str = "\"<redacted>\"";

impl Redactor<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.c.get(i).copied()
    }

    fn starts(&self, i: usize, pattern: &str) -> bool {
        pattern.chars().enumerate().all(|(k, p)| self.c.get(i + k) == Some(&p))
    }

    fn copy(&mut self, from: usize, to: usize) {
        let to = to.min(self.c.len());
        if from < to {
            self.out.extend(&self.c[from..to]);
        }
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while matches!(self.at(i), Some(' ') | Some('\t')) {
            i += 1;
        }
        i
    }

    /// Spaces, tabs, line breaks, commas and comments (between array items and inline-table fields).
    fn skip_trivia(&self, mut i: usize) -> usize {
        loop {
            match self.at(i) {
                Some(' ' | '\t' | '\n' | '\r' | ',') => i += 1,
                Some('#') => {
                    while !matches!(self.at(i), None | Some('\n')) {
                        i += 1;
                    }
                }
                _ => return i,
            }
        }
    }

    /// Copy from `i` through the end of the line; returns the start of the next one.
    fn copy_line(&mut self, i: usize) -> usize {
        let mut j = i;
        while j < self.c.len() && self.c[j] != '\n' {
            j += 1;
        }
        let end = (j + 1).min(self.c.len());
        self.copy(i, end);
        end
    }

    /// End (exclusive) of the string starting at `i` (a quote character).
    fn skip_string(&self, i: usize) -> usize {
        let n = self.c.len();
        let q = self.c[i];
        let triple: String = std::iter::repeat(q).take(3).collect();
        if self.starts(i, &triple) {
            let mut j = i + 3;
            while j < n {
                if q == '"' && self.c[j] == '\\' {
                    j += 2;
                } else if self.starts(j, &triple) {
                    j += 3;
                    let mut extra = 0;
                    while extra < 2 && self.at(j) == Some(q) {
                        j += 1;
                        extra += 1;
                    }
                    return j.min(n);
                } else {
                    j += 1;
                }
            }
            return n;
        }
        let mut j = i + 1;
        while j < n {
            match self.c[j] {
                '\n' => return j,
                '\\' if q == '"' => j += 2,
                ch if ch == q => return j + 1,
                _ => j += 1,
            }
        }
        n
    }

    /// A dotted key at `i`: its parts (quotes removed, escapes left as written) and the end of the key.
    fn key_path(&self, i: usize) -> Option<(Vec<String>, usize)> {
        let mut parts = Vec::new();
        let mut j = i;
        loop {
            j = self.skip_ws(j);
            let Some(ch) = self.at(j) else { break };
            if ch == '"' || ch == '\'' {
                let end = self.skip_string(j);
                let inner_end = if end > j + 1 && self.c[end - 1] == ch { end - 1 } else { end };
                parts.push(self.c[(j + 1).min(inner_end)..inner_end].iter().collect());
                j = end;
            } else if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                let start = j;
                while matches!(self.at(j), Some(b) if b.is_ascii_alphanumeric() || b == '_' || b == '-') {
                    j += 1;
                }
                parts.push(self.c[start..j].iter().collect());
            } else {
                break;
            }
            let k = self.skip_ws(j);
            if self.at(k) == Some('.') {
                j = k + 1;
            } else {
                break;
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some((parts, j))
        }
    }

    fn value_present(&self, i: usize) -> bool {
        matches!(self.at(i), Some(ch) if !matches!(ch, '\n' | '\r' | '#' | ',' | '}' | ']'))
    }

    /// End (exclusive) of the value starting at `i`.
    fn value_end(&self, i: usize) -> usize {
        let n = self.c.len();
        match self.at(i) {
            Some('"' | '\'') => self.skip_string(i),
            Some('[') => {
                let mut j = i + 1;
                loop {
                    j = self.skip_trivia(j);
                    match self.at(j) {
                        None => return n,
                        Some(']') => return j + 1,
                        _ => {
                            let e = self.value_end(j);
                            j = if e > j { e } else { j + 1 };
                        }
                    }
                }
            }
            Some('{') => {
                let mut j = i + 1;
                loop {
                    j = self.skip_trivia(j);
                    match self.at(j) {
                        None => return n,
                        Some('}') => return j + 1,
                        _ => match self.key_path(j) {
                            Some((_, kend)) => {
                                let k = self.skip_ws(kend);
                                if self.at(k) == Some('=') {
                                    let v = self.skip_ws(k + 1);
                                    j = if self.value_present(v) { self.value_end(v).max(v) } else { v };
                                    j = j.max(kend);
                                } else {
                                    j = kend.max(j + 1);
                                }
                            }
                            None => j += 1,
                        },
                    }
                }
            }
            _ => {
                let mut j = i;
                while j < n && !matches!(self.c[j], ',' | ']' | '}' | '#' | '\n' | '\r') {
                    j += 1;
                }
                while j > i && matches!(self.c[j - 1], ' ' | '\t') {
                    j -= 1;
                }
                j
            }
        }
    }

    /// Copy the value at `i`, redacting secret fields of inline tables inside it. Returns the end.
    fn emit_value(&mut self, i: usize) -> usize {
        match self.at(i) {
            Some('[') => {
                self.out.push('[');
                let mut j = i + 1;
                loop {
                    let t = self.skip_trivia(j);
                    self.copy(j, t);
                    j = t;
                    match self.at(j) {
                        None => return j,
                        Some(']') => {
                            self.out.push(']');
                            return j + 1;
                        }
                        Some(ch) => {
                            let e = self.emit_value(j);
                            if e > j {
                                j = e;
                            } else {
                                self.out.push(ch);
                                j += 1;
                            }
                        }
                    }
                }
            }
            Some('{') => {
                self.out.push('{');
                let mut j = i + 1;
                loop {
                    let t = self.skip_trivia(j);
                    self.copy(j, t);
                    j = t;
                    match self.at(j) {
                        None => return j,
                        Some('}') => {
                            self.out.push('}');
                            return j + 1;
                        }
                        Some(ch) => match self.key_path(j) {
                            Some((parts, kend)) => {
                                self.copy(j, kend);
                                let k = self.skip_ws(kend);
                                self.copy(kend, k);
                                if self.at(k) == Some('=') {
                                    self.out.push('=');
                                    let v = self.skip_ws(k + 1);
                                    self.copy(k + 1, v);
                                    j = v;
                                    if self.value_present(v) {
                                        if parts.iter().any(|p| is_secret_key_name(p)) {
                                            let e = self.value_end(v).max(v);
                                            self.out.push_str(REDACTED_VALUE);
                                            j = e;
                                        } else {
                                            j = self.emit_value(v);
                                        }
                                    }
                                } else {
                                    j = k;
                                }
                            }
                            None => {
                                self.out.push(ch);
                                j += 1;
                            }
                        },
                    }
                }
            }
            _ => {
                let e = self.value_end(i);
                self.copy(i, e);
                e
            }
        }
    }

    fn run(&mut self) {
        let n = self.c.len();
        let mut i = 0;
        let mut table_secret = false;
        while i < n {
            let ws = self.skip_ws(i);
            self.copy(i, ws);
            i = ws;
            match self.at(i) {
                None => break,
                Some(ch @ ('\n' | '\r')) => {
                    self.out.push(ch);
                    i += 1;
                }
                Some('#') => i = self.copy_line(i),
                Some('[') => {
                    let start = if self.at(i + 1) == Some('[') { i + 2 } else { i + 1 };
                    table_secret = self.key_path(start).map(|(parts, _)| parts.iter().any(|p| is_secret_key_name(p))).unwrap_or(false);
                    i = self.copy_line(i);
                }
                Some(_) => match self.key_path(i) {
                    None => i = self.copy_line(i),
                    Some((parts, kend)) => {
                        self.copy(i, kend);
                        let k = self.skip_ws(kend);
                        self.copy(kend, k);
                        if self.at(k) != Some('=') {
                            i = self.copy_line(k);
                            continue;
                        }
                        self.out.push('=');
                        let v = self.skip_ws(k + 1);
                        self.copy(k + 1, v);
                        i = v;
                        if self.value_present(v) {
                            if table_secret || parts.iter().any(|p| is_secret_key_name(p)) {
                                let e = self.value_end(v).max(v);
                                self.out.push_str(REDACTED_VALUE);
                                i = e;
                            } else {
                                i = self.emit_value(v);
                            }
                        }
                        i = self.copy_line(i);
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    const CANARY: &str = "S3CR3T-CANARY-9f2c";

    #[test]
    fn authorization_headers_and_credential_keys_are_redacted() {
        let out = redact_config(&format!("[x]\nauthorization = \"Bearer {CANARY}\"\ncredentials = \"{CANARY}\"\n"));
        assert!(!out.contains(CANARY), "{out}");
        // A Bearer token is redacted wherever it appears, whatever the key is called.
        let out = redact_config(&format!("[x]\nnote = \"curl -H 'Authorization: Bearer {CANARY}' https://h\"\nheader = \"bearer {CANARY}\"\n"));
        assert!(!out.contains(CANARY), "{out}");
        assert!(out.contains("Bearer <redacted>") || out.contains("bearer <redacted>"), "{out}");
        assert_eq!(redact_config(&out), out, "idempotent");
    }

    #[test]
    fn bearer_pass_leaves_ordinary_text_alone() {
        for text in [
            "[x]\nnote = \"the bearer of this key\"\n",
            "[x]\nnote = \"bearer authentication scheme\"\n",
            "[x]\nbearerless = 1\n",
            "[x]\nname = \"pallbearer x\"\n",
        ] {
            assert_eq!(redact_config(text), text, "{text}");
        }
    }

    #[test]
    fn a_wide_set_of_hostile_config_shapes_never_keeps_a_secret() {
        let cases: Vec<(&str, String)> = vec![
            ("dsn pairs", format!("[storage]\npostgres_url = \"host=db user=u password={CANARY} dbname=d\"\n")),
            ("dsn literal string", format!("[storage]\npostgres_url = 'host=db user=u password={CANARY}'\n")),
            ("url with @ in the password", format!("[storage]\npostgres_url = \"postgresql://u:{CANARY}@x@host:5432/db\"\n")),
            ("percent-encoded password", format!("[storage]\npostgres_url = \"postgresql://u:p%40{CANARY}@host/db\"\n")),
            ("srv scheme", format!("[storage]\npostgres_url = \"mongodb+srv://u:{CANARY}@cluster/db\"\n")),
            ("upper-case key", format!("[llm]\nPASSWORD = \"{CANARY}\"\n")),
            ("mixed-case key", format!("[llm]\nApi_Key = \"{CANARY}\"\n")),
            ("commented pair", format!("# password = \"{CANARY}\"\n[general]\ndata_dir = \"data\"\n")),
            ("trailing comment dsn", format!("[storage]\npostgres_url = \"\" # was host=db password={CANARY}\n")),
            ("multi-line string", format!("[llm]\nnote = \"\"\"\nline\npassword = {CANARY}\n\"\"\"\n")),
            ("inline table", format!("[llm]\nprovider = {{ name = \"x\", api_key = \"{CANARY}\" }}\n")),
            ("array of tables", format!("[[llm.providers]]\nid = \"a\"\napi_key = \"{CANARY}\"\n")),
            ("dotted key", format!("llm.providers.a.token = \"{CANARY}\"\n")),
            ("quoted key", format!("\"api_key\" = \"{CANARY}\"\n")),
            ("padded equals", format!("[llm]\nsecret   =   \"{CANARY}\"\n")),
            ("url query token", format!("[x]\nendpoint = \"https://h/p?token={CANARY}&a=1\"\n")),
            ("bearer value", format!("[x]\nauthorization = \"Bearer {CANARY}\"\n")),
            ("access token", format!("[zerodha]\naccess_token = \"{CANARY}\"\n")),
        ];
        for (name, text) in &cases {
            let out = redact_config(text);
            assert!(!out.contains(CANARY), "{name} leaked: {out}");
            assert_eq!(&redact_config(&out), &out, "{name}: not idempotent");
        }
    }

    use super::*;
    use crate::commands::versioned_files::testutil::{open, tmp};

    const EXAMPLE: &str = include_str!("../../../config/apex.example.toml");

    fn red(text: &str) -> String {
        redact_config(text)
    }

    // ── URLs ──

    #[test]
    fn the_password_in_url_userinfo_is_hidden_with_and_without_port_path_and_query() {
        let cases = [
            ("postgresql://apex:hunter2@db.example.com/apex", "postgresql://apex:<redacted>@db.example.com/apex"),
            ("postgresql://apex:hunter2@db.example.com:5432/apex", "postgresql://apex:<redacted>@db.example.com:5432/apex"),
            ("postgres://apex:hunter2@127.0.0.1:5432/apex?sslmode=require&application_name=apex", "postgres://apex:<redacted>@127.0.0.1:5432/apex?sslmode=require&application_name=apex"),
            ("postgres://apex:hunter2@[::1]:5432/apex#frag", "postgres://apex:<redacted>@[::1]:5432/apex#frag"),
            // An '@' inside the password: the LAST '@' ends the userinfo.
            ("postgres://apex:p@ss@host/db", "postgres://apex:<redacted>@host/db"),
            // Nothing secret: left alone.
            ("postgresql://apex@db.example.com/apex", "postgresql://apex@db.example.com/apex"),
            ("postgresql://db.example.com:5432/apex", "postgresql://db.example.com:5432/apex"),
            ("postgresql://apex:@db.example.com/apex", "postgresql://apex:@db.example.com/apex"),
            ("https://example.com/@user/page", "https://example.com/@user/page"),
            ("not a url: a:b@c", "not a url: a:b@c"),
        ];
        for (input, expected) in cases {
            assert_eq!(red(&format!("postgres_url = \"{input}\"\n")), format!("postgres_url = \"{expected}\"\n"), "{input}");
        }
    }

    #[test]
    fn secret_query_parameters_are_hidden_and_ordinary_ones_are_not() {
        assert_eq!(
            red("u = \"postgresql://h/db?user=a&password=pw1&sslmode=require&api_key=k1&key=k2&sort=asc\"\n"),
            "u = \"postgresql://h/db?user=a&password=<redacted>&sslmode=require&api_key=<redacted>&key=<redacted>&sort=asc\"\n"
        );
        assert_eq!(red("u = \"https://h/p?token=t1#frag\"\n"), "u = \"https://h/p?token=<redacted>#frag\"\n");
        assert_eq!(red("u = \"https://h/p?monkey=1&keyboard=2\"\n"), "u = \"https://h/p?monkey=1&keyboard=2\"\n");
    }

    #[test]
    fn urls_in_comments_and_unquoted_text_are_covered_too() {
        assert_eq!(red("# old: postgresql://postgres:postgres@localhost:5432/apex\n"), "# old: postgresql://postgres:<redacted>@localhost:5432/apex\n");
        assert_eq!(red("x = 1 # see https://u:pw@h/\n"), "x = 1 # see https://u:<redacted>@h/\n");
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_url_early() {
        // The password contains an escaped quote; every character of it must go.
        assert_eq!(red(r#"u = "postgres://a:pa\"ss@host/db""#), r#"u = "postgres://a:<redacted>@host/db""#);
    }

    #[test]
    fn a_key_value_connection_string_loses_its_password() {
        let cases = [
            ("host=db user=u password=hunter2 dbname=d", "host=db user=u password=<redacted> dbname=d"),
            ("password=hunter2 host=db", "password=<redacted> host=db"),
            ("host=db password = hunter2 dbname=d", "host=db password = <redacted> dbname=d"),
            ("host=db password='a b c' dbname=d", "host=db password='<redacted>' dbname=d"),
            ("host=db PASSWORD=hunter2", "host=db PASSWORD=<redacted>"),
            ("host=db password=hunter2", "host=db password=<redacted>"),
            // Nothing secret: left alone.
            ("host=db user=u dbname=d sslmode=require", "host=db user=u dbname=d sslmode=require"),
        ];
        for (input, expected) in cases {
            let text = format!("postgres_url = \"{input}\"\n");
            assert_eq!(red(&text), format!("postgres_url = \"{expected}\"\n"), "{input}");
            assert_eq!(red(&red(&text)), red(&text), "idempotent: {input}");
        }
        // Single-quoted TOML string holding a double-quoted libpq value.
        assert_eq!(red("u = 'host=db password=\"p w\" x=1'\n"), "u = 'host=db password=\"<redacted>\" x=1'\n");
        // A redacted URL parameter does not swallow what follows it.
        assert_eq!(red("u = \"https://h/p?token=t1&x=1\"\n"), "u = \"https://h/p?token=<redacted>&x=1\"\n");
    }

    #[test]
    fn commented_out_credentials_and_trailing_notes_are_hidden() {
        assert_eq!(red("# password = \"oldpw\"\n"), "# password = \"<redacted>\"\n");
        assert_eq!(red("# api_key = \"sk-old\"\n"), "# api_key = \"<redacted>\"\n");
        assert_eq!(red("#token='t0'\n"), "#token='<redacted>'\n");
        assert_eq!(red("postgres_url = \"\" # was host=db password=oldpw\n"), "postgres_url = \"\" # was host=db password=<redacted>\n");
        // Names that are not credentials keep their comments.
        assert_eq!(red("# max_tokens = 4096\n# api_key_env = \"MY_KEY\"\n"), "# max_tokens = 4096\n# api_key_env = \"MY_KEY\"\n");
        assert_eq!(red("# why: the password policy changed\n"), "# why: the password policy changed\n");
    }

    #[test]
    fn the_activity_log_shows_a_connection_string_without_its_password() {
        let (keys, details) = describe_settings_change("", "[storage]\npostgres_url = \"host=db user=u password=hunter2 dbname=d\"\n");
        assert_eq!(keys, vec!["storage.postgres_url".to_string()]);
        assert!(!details.contains("hunter2"), "{details}");
        assert!(details.contains("host=db") && details.contains("password=<redacted>"), "{details}");
        assert!(!scrub_error("connect failed: host=db password=hunter2", &[]).contains("hunter2"));
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_keeps_permissions_follows_symlinks_and_creates_private_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let tmp_dir = tmp("write-atomic");
        let dir = tmp_dir.0.clone();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // The original's mode is kept, whether strict or open.
        for want in [0o600, 0o644] {
            let file = dir.join(format!("mode-{want:o}.toml"));
            fs::write(&file, "old").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(want)).unwrap();
            write_atomic(&file, "new").unwrap();
            assert_eq!((fs::read_to_string(&file).unwrap(), mode(&file)), ("new".to_string(), want));
        }
        // A new file is private to its owner.
        let fresh = dir.join("fresh.toml");
        write_atomic(&fresh, "x").unwrap();
        assert_eq!(mode(&fresh), 0o600);
        // A symlinked config stays a link and the file it points to is updated.
        let target = dir.join("dotfiles-apex.toml");
        let link = dir.join("linked.toml");
        fs::write(&target, "old").unwrap();
        symlink(&target, &link).unwrap();
        write_atomic(&link, "new").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link was replaced by a file");
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        // No temporary file is left behind.
        assert!(fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    }

    // ── Key names ──

    #[test]
    fn key_names_are_matched_by_substring_and_the_exceptions_are_deliberate() {
        for name in [
            "password", "db_password", "PASSWORD", "passwd", "passphrase", "client_secret", "secret", "access_token", "refresh-token", "token",
            "api_key", "API_KEY", "apikey", "openrouter_api_key", "private_key", "key_material", "session_token_value",
        ] {
            assert!(is_secret_key_name(name), "{name} should be secret");
        }
        for name in [
            "keyboard_shortcut_key", "command_key", "key", "sort_key", "monkey", "hotkey", "primary_key", "max_tokens", "max_output_tokens", "min_tokens",
            "api_key_env", "token_env", "password_env", "adapter", "max_daily_loss", "base_url", "",
        ] {
            assert!(!is_secret_key_name(name), "{name:?} should stay readable");
        }
        // The conservative direction: a name that merely contains a marker is hidden (documented).
        assert!(is_secret_key_name("tokenizer"));
        assert!(is_secret_key_name("tokens"), "a list of tokens is not a count");
    }

    #[test]
    fn values_of_secret_keys_are_replaced_whatever_their_type() {
        let text = "\
[storage]
postgres_password = \"hunter2\"
api_key = 'single'
token = 12345
secret = true
client_secret = \"\"\"multi
line\"\"\"
refresh_tokens = [\"a\", \"b\"]
";
        let out = red(text);
        for leaked in ["hunter2", "single", "12345", "multi", "\"a\"", "\"b\""] {
            assert!(!out.contains(leaked), "{leaked} leaked: {out}");
        }
        assert_eq!(out.matches("\"<redacted>\"").count(), 6);
        assert!(out.starts_with("[storage]\npostgres_password = \"<redacted>\"\napi_key = \"<redacted>\"\n"));
    }

    #[test]
    fn harmless_keys_with_key_or_token_in_a_longer_name_keep_their_values() {
        let text = "keyboard_shortcut_key = \"Ctrl+K\"\nmax_tokens = 1024\napi_key_env = \"OPENROUTER_API_KEY\"\ncommand_key = \"Space\"\n";
        assert_eq!(red(text), text);
    }

    #[test]
    fn tables_inline_tables_arrays_of_tables_quoted_and_dotted_keys() {
        let text = r#"
[llm]
active = "x"

[[llm.providers]]
id = "openai"
api_key_env = "OPENAI_API_KEY"
api_key = "sk-live-AAA"
max_tokens = 4096

[[llm.providers]]
id = "other"
"Api Token" = "tok-BBB"

[db]
connection = { host = "h", password = "pw-CCC", options = { token = "tok-DDD", ssl = true } }
list = [ { name = "a", secret = "sec-EEE" }, { name = "b", secret = "sec-FFF" } ]
credentials.password = "pw-GGG"
"quoted key"."client_secret" = "sec-HHH"
other = "visible"

[secrets]
anything = "sec-III"
number = 7

[[tokens]]
value = "tok-JJJ"
"#;
        let out = red(text);
        for leaked in ["AAA", "BBB", "CCC", "DDD", "EEE", "FFF", "GGG", "HHH", "III", "JJJ", "= 7"] {
            assert!(!out.contains(leaked), "{leaked} leaked:\n{out}");
        }
        for kept in ["id = \"openai\"", "api_key_env = \"OPENAI_API_KEY\"", "max_tokens = 4096", "host = \"h\"", "ssl = true", "name = \"a\"", "name = \"b\"", "other = \"visible\"", "active = \"x\"", "[[llm.providers]]", "[secrets]", "[[tokens]]"] {
            assert!(out.contains(kept), "{kept} lost:\n{out}");
        }
        assert!(out.contains("connection = { host = \"h\", password = \"<redacted>\", options = { token = \"<redacted>\", ssl = true } }"), "{out}");
        assert!(out.contains("list = [ { name = \"a\", secret = \"<redacted>\" }, { name = \"b\", secret = \"<redacted>\" } ]"), "{out}");
        // Still valid TOML.
        assert!(out.parse::<DocumentMut>().is_ok(), "{out}");
    }

    #[test]
    fn multi_line_arrays_with_comments_and_strings_containing_hashes() {
        let text = "keys = [\n  \"a#b\", # note ] with bracket\n  \"c\",\n]\ntoken = [\n  \"t1\", # c\n  \"t2\"\n]\nnext = \"after\"\n";
        let out = red(text);
        assert_eq!(out, "keys = [\n  \"a#b\", # note ] with bracket\n  \"c\",\n]\ntoken = \"<redacted>\"\nnext = \"after\"\n");
    }

    #[test]
    fn comments_layout_line_endings_and_unicode_survive() {
        let text = "# héllo ✓\r\n[a]\r\n  key   =   \"v\"   # trailing\r\n\r\n\tpassword =\t\"p\" # why\r\nname = \"日本語\"\r\n";
        let out = red(text);
        assert_eq!(out, "# héllo ✓\r\n[a]\r\n  key   =   \"v\"   # trailing\r\n\r\n\tpassword =\t\"<redacted>\" # why\r\nname = \"日本語\"\r\n");
    }

    #[test]
    fn text_without_secrets_comes_back_byte_for_byte_and_the_example_config_changes_in_one_comment_only() {
        let plain = "[risk]\nmax_daily_loss = 50_000.0 # limit\n\n[execution]\nadapter = \"paper\"\n";
        assert_eq!(red(plain), plain);
        assert_eq!(red(""), "");
        assert_eq!(red("\n\n"), "\n\n");
        let out = red(EXAMPLE);
        let changed: Vec<(&str, &str)> = EXAMPLE.lines().zip(out.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(out.lines().count(), EXAMPLE.lines().count());
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0], ("# postgresql://postgres:postgres@localhost:5432/apex_market", "# postgresql://postgres:<redacted>@localhost:5432/apex_market"));
    }

    #[test]
    fn redaction_is_idempotent() {
        let corpus = [
            EXAMPLE.to_string(),
            "postgres_url = \"postgresql://u:pw@h:5432/d?password=x&sslmode=require\"\npassword = \"a\"\n[secrets]\nk = 1\n".to_string(),
            "x = { password = \"a\", y = [ { token = \"t\" } ] }\n".to_string(),
            "password = \"<redacted>\"\n".to_string(),
            "garbage [[[ = = \"unterminated\npassword = \"x\n".to_string(),
        ];
        for text in corpus {
            let once = red(&text);
            assert_eq!(red(&once), once, "not idempotent for:\n{text}");
        }
    }

    #[test]
    fn malformed_text_never_panics_and_still_hides_what_it_can() {
        let nasty = [
            "password = ", "password", "= \"x\"", "[", "[[", "[a", "a = [", "a = {", "a = [ { b = ", "\"unterminated", "'''", "\"\"\" never closed",
            "password = \"unterminated\npassword = \"second\"", "a.b.c.", "a..b = 1", "\u{0}\u{1}", "token = \"\\", "x = \"\\\"", "http://", "://", "a://@", "a://:@:",
        ];
        for text in nasty {
            let out = red(text);
            assert_eq!(red(&out), out, "{text:?}");
        }
        assert!(!red("password = \"unterminated\npassword = \"second\"").contains("second"));
        // Every prefix of the example survives too (no index out of range anywhere).
        for end in (0..EXAMPLE.len()).step_by(37).filter(|e| EXAMPLE.is_char_boundary(*e)) {
            let _ = red(&EXAMPLE[..end]);
        }
    }

    // ── Describing a change ──

    #[test]
    fn a_change_lists_the_keys_and_shows_old_and_new_for_plain_values_only() {
        let old = "[risk]\nmax_daily_loss = 1000.0\n[execution]\nadapter = \"paper\"\n[storage]\npostgres_url = \"postgresql://u:oldpw@h/db\"\n[llm]\n[[llm.providers]]\nid = \"a\"\nmodel = \"m1\"\napi_key_env = \"E1\"\nmax_tokens = 10\n";
        let new = "[risk]\nmax_daily_loss = 2500.0\n[execution]\nadapter = \"zerodha\"\n[storage]\npostgres_url = \"postgresql://u:newpw@h/db\"\n[llm]\n[[llm.providers]]\nid = \"a\"\nmodel = \"m2\"\napi_key_env = \"E1\"\nmax_tokens = 10\napi_key = \"sk-NEW\"\n";
        let (keys, details) = describe_settings_change(old, new);
        assert_eq!(keys, ["execution.adapter", "llm.providers[a].api_key", "llm.providers[a].model", "risk.max_daily_loss", "storage.postgres_url"]);
        assert!(details.contains("risk.max_daily_loss: 1000.0 -> 2500.0"), "{details}");
        assert!(details.contains("execution.adapter: paper -> zerodha"), "{details}");
        assert!(details.contains("llm.providers[a].model: m1 -> m2"), "{details}");
        assert!(details.contains("llm.providers[a].api_key: changed"), "{details}");
        assert!(details.contains("storage.postgres_url: changed (credentials or query only)"), "{details}");
        for leaked in ["oldpw", "newpw", "sk-NEW"] {
            assert!(!details.contains(leaked), "{leaked} leaked: {details}");
        }
        assert_eq!(describe_settings_change(old, old), (vec![], String::new()));
    }

    #[test]
    fn a_changed_url_shows_host_and_path_without_credentials_or_query() {
        let (_, details) = describe_settings_change("u = \"https://a.example/v1?key=K1\"\n", "u = \"https://b.example/v1?key=K2\"\n");
        assert_eq!(details, "u: https://a.example/v1?<query omitted> -> https://b.example/v1?<query omitted>");
        let (_, details) = describe_settings_change("", "u = \"https://x:pw@b.example/v1\"\n");
        assert_eq!(details, "u: added https://x:<redacted>@b.example/v1");
    }

    #[test]
    fn scrub_error_removes_urls_credentials_and_given_secrets() {
        let out = scrub_error("error sending request for url (https://api.x.com/v1?api_key=SEKRET1&x=1): token SEKRET2 bad, user:SEKRET3", &["SEKRET2", "SEKRET3", ""]);
        assert!(!out.contains("SEKRET"), "{out}");
        assert!(out.contains("x=1"));
        assert!(scrub_error(&"y".repeat(1000), &[]).chars().count() <= 301);
    }

    // ── Saving and drift ──

    fn config_with_password(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("config").join("apex.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = EXAMPLE.replace("postgres_url = \"\"", "postgres_url = \"postgresql://apex:CANARY-PG-PASSWORD@db.example.com:5432/apex\"");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn request(config_path: &Path, edit: impl FnOnce(&mut serde_json::Value)) -> AppSettingsUpdateDto {
        let raw = std::fs::read_to_string(config_path).unwrap();
        let cfg: crate::config::AppConfig = toml::from_str(&raw).unwrap();
        let mut value = serde_json::json!({
            "general": { "data_dir": cfg.general.data_dir },
            "market_data": { "adapter": cfg.market_data.adapter },
            "execution": { "adapter": cfg.execution.adapter },
            "risk": { "max_daily_loss": cfg.risk.max_daily_loss, "max_order_value": cfg.risk.max_order_value },
            "storage": {
                "backend": cfg.storage.backend, "sqlite_path": cfg.storage.sqlite_path,
                "postgres_url": cfg.storage.postgres_url.clone().unwrap_or_default(), "wal_mode": cfg.storage.wal_mode, "pool_size": cfg.storage.pool_size,
            },
        });
        edit(&mut value);
        serde_json::from_value(value).unwrap()
    }

    fn head_text(ledger: &Ledger) -> String {
        let head = ledger.head(Kind::Settings, SETTINGS_ID).unwrap();
        String::from_utf8(ledger.content(Kind::Settings, SETTINGS_ID, head.version).unwrap()).unwrap()
    }

    fn all_ledger_bytes(ledger: &Ledger) -> String {
        fn walk(dir: &Path, out: &mut String) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if let Ok(b) = std::fs::read(&p) {
                    out.push_str(&String::from_utf8_lossy(&b));
                }
            }
        }
        let mut out = String::new();
        walk(ledger.dir(), &mut out);
        out
    }

    #[test]
    fn a_save_is_a_new_redacted_version_and_the_file_keeps_its_secrets() {
        let dir = tmp("settings-save");
        let ledger = open(&dir);
        let path = config_with_password(&dir.0);
        let req = request(&path, |v| v["risk"]["max_daily_loss"] = serde_json::json!(75000.0));
        let change = save_settings_inner(&ledger, &path, &req, "user").unwrap();
        assert_eq!(change.changed, ["risk.max_daily_loss"]);
        assert_eq!(change.details, "risk.max_daily_loss: 50000.0 -> 75000.0");
        // The real file has the new value and still its password and comments.
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(file.contains("max_daily_loss = 75000.0") && file.contains("CANARY-PG-PASSWORD") && file.contains("# Maximum daily loss"));
        // History: 1 = the file as it was (registered by the system), 2 = the save by the user.
        let h = ledger.history(Kind::Settings, SETTINGS_ID);
        assert_eq!(h.iter().map(|r| (r.version, r.actor.as_str(), r.action.as_str())).collect::<Vec<_>>(), [(1, "system", "create"), (2, "user", "update")]);
        assert_eq!(h[1].summary, "Changed risk.max_daily_loss");
        assert!(String::from_utf8(ledger.content(Kind::Settings, SETTINGS_ID, 1).unwrap()).unwrap().contains("max_daily_loss = 50_000.0"));
        assert!(head_text(&ledger).contains("max_daily_loss = 75000.0"));
        // No secret anywhere in the ledger.
        let everything = all_ledger_bytes(&ledger);
        assert!(!everything.contains("CANARY-PG-PASSWORD"), "the password reached the ledger");
        assert!(head_text(&ledger).contains("postgresql://apex:<redacted>@db.example.com:5432/apex"));
        // No temporary file is left next to the config.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(leftovers, ["apex.toml"]);
        assert!(ledger.verify().ok);
    }

    #[test]
    fn saving_the_same_values_again_adds_nothing() {
        let dir = tmp("settings-same");
        let ledger = open(&dir);
        let path = config_with_password(&dir.0);
        let req = request(&path, |_| {});
        let first = save_settings_inner(&ledger, &path, &req, "user").unwrap();
        // The first save only normalizes layout (no setting changes); it is still a recorded version.
        assert!(first.changed.is_empty(), "{:?}", first.changed);
        assert_eq!(ledger.history(Kind::Settings, SETTINGS_ID).len(), 2);
        let second = save_settings_inner(&ledger, &path, &req, "user").unwrap();
        assert!(second.changed.is_empty());
        assert_eq!(ledger.history(Kind::Settings, SETTINGS_ID).len(), 2, "saving the same values again adds nothing");
    }

    #[test]
    fn a_password_only_change_is_reported_without_a_value_and_adds_no_version() {
        let dir = tmp("settings-pw");
        let ledger = open(&dir);
        let path = config_with_password(&dir.0);
        save_settings_inner(&ledger, &path, &request(&path, |_| {}), "user").unwrap();
        let req = request(&path, |v| v["storage"]["postgres_url"] = serde_json::json!("postgresql://apex:CANARY-NEW-PASSWORD@db.example.com:5432/apex"));
        let change = save_settings_inner(&ledger, &path, &req, "user").unwrap();
        assert_eq!(change.changed, ["storage.postgres_url"]);
        assert_eq!(change.details, "storage.postgres_url: changed (credentials or query only)");
        assert!(std::fs::read_to_string(&path).unwrap().contains("CANARY-NEW-PASSWORD"), "the file did change");
        assert_eq!(ledger.history(Kind::Settings, SETTINGS_ID).len(), 2, "the redacted text is the same, so no new version");
        assert!(!all_ledger_bytes(&ledger).contains("CANARY"));
    }

    #[test]
    fn if_the_ledger_cannot_record_the_save_the_file_is_not_touched() {
        let dir = tmp("settings-failclosed");
        let ledger = open(&dir);
        let path = config_with_password(&dir.0);
        save_settings_inner(&ledger, &path, &request(&path, |_| {}), "user").unwrap();
        let before = std::fs::read(&path).unwrap();
        // A redacted text over the ledger's size limit makes the ledger write fail.
        let req = request(&path, |v| {
            v["execution"]["adapter"] = serde_json::json!("paper");
            v["general"]["data_dir"] = serde_json::json!("d".repeat(crate::ledger::MAX_BLOB_BYTES + 10));
        });
        let error = save_settings_inner(&ledger, &path, &req, "user").unwrap_err();
        assert!(error.contains("ledger limit"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the file must be exactly as it was");
        assert_eq!(ledger.history(Kind::Settings, SETTINGS_ID).len(), 2);
        let leftovers = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "no temp file");
    }

    #[test]
    fn an_edit_made_outside_the_app_is_recorded_and_not_lost_by_the_next_save() {
        let dir = tmp("settings-drift");
        let ledger = open(&dir);
        let path = config_with_password(&dir.0);
        // First run: version 1 by the system.
        let first = reconcile_settings(&ledger, &path).unwrap().unwrap();
        assert_eq!((first.record.version, first.record.actor.as_str()), (1, "system"));
        assert!(reconcile_settings(&ledger, &path).unwrap().is_none(), "nothing changed");
        // An editor raises the loss limit and swaps the password.
        let edited = std::fs::read_to_string(&path).unwrap().replace("50_000.0", "900_000.0").replace("CANARY-PG-PASSWORD", "CANARY-OTHER");
        std::fs::write(&path, edited).unwrap();
        let found = reconcile_settings(&ledger, &path).unwrap().unwrap();
        assert_eq!((found.record.version, found.record.actor.as_str(), found.record.action.as_str()), (2, "external", "external"));
        assert_eq!(found.changed, ["risk.max_daily_loss"]);
        assert_eq!(found.details, "risk.max_daily_loss: 50000.0 -> 900000.0");
        let entry = external_edit_entry(&found);
        assert_eq!((entry.actor.as_str(), entry.action.as_str(), entry.ok), ("external", "settings.external_edit", true));
        assert!(entry.summary.contains("risk.max_daily_loss"));
        // A save after an outside edit keeps it: the value comes from the file, not from an old copy.
        let edited_again = std::fs::read_to_string(&path).unwrap().replace("900_000.0", "123.0");
        std::fs::write(&path, edited_again).unwrap();
        let change = save_settings_inner(&ledger, &path, &request(&path, |v| v["risk"]["max_order_value"] = serde_json::json!(1.0)), "user").unwrap();
        assert!(change.external.is_some());
        let versions: Vec<(u32, String)> = ledger.history(Kind::Settings, SETTINGS_ID).into_iter().map(|r| (r.version, r.actor)).collect();
        assert_eq!(versions, [(1, "system".into()), (2, "external".into()), (3, "external".into()), (4, "user".into())]);
        assert!(!all_ledger_bytes(&ledger).contains("CANARY"));
    }

    #[test]
    fn drift_is_judged_on_the_redacted_text_never_on_the_raw_file() {
        // If the raw file were compared with the (redacted) head, every check would see a difference
        // and append a version forever. Checking repeatedly must add nothing.
        let dir = tmp("settings-drift-redacted");
        let ledger = open(&dir);
        let path = config_with_password(&dir.0);
        reconcile_settings(&ledger, &path).unwrap();
        for _ in 0..5 {
            assert!(reconcile_settings(&ledger, &path).unwrap().is_none());
        }
        assert_eq!(ledger.history(Kind::Settings, SETTINGS_ID).len(), 1);
    }

    #[test]
    fn a_missing_settings_file_is_not_an_error() {
        let dir = tmp("settings-missing");
        let ledger = open(&dir);
        assert!(reconcile_settings(&ledger, &dir.0.join("nope.toml")).unwrap().is_none());
    }
}
