use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::commands::python_runtime::RuntimePaths;

const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../config/apex.example.toml");

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub market_data: MarketDataConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub risk: RiskSection,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub news: NewsConfig,
    #[serde(default)]
    pub copilot: CopilotConfig,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub acp: AcpConfig,
    #[serde(default)]
    pub automations: AutomationsConfig,
    #[serde(default)]
    pub live: LiveConfig,
}

impl AppConfig {
    pub fn load(runtime_paths: &RuntimePaths) -> Result<Self> {
        ensure_config_file(runtime_paths)?;

        let config_path = runtime_paths.config_file();
        let raw = fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file {}", config_path.display()))?;

        toml::from_str(&raw)
            .with_context(|| format!("Failed to parse config file {}", config_path.display()))
    }

    pub fn resolved_data_dir(&self, runtime_paths: &RuntimePaths) -> Result<PathBuf> {
        resolve_runtime_dir(
            runtime_paths,
            self.general.data_dir.trim(),
            runtime_paths.data_dir(),
        )
    }

    pub fn resolved_sqlite_path(&self, runtime_paths: &RuntimePaths) -> Result<PathBuf> {
        let data_dir = self.resolved_data_dir(runtime_paths)?;
        let configured = self.storage.sqlite_path.trim();
        let sqlite_path = if configured.is_empty() {
            data_dir.join("apex.db")
        } else {
            let path = PathBuf::from(configured);
            if path.is_absolute() {
                path
            } else {
                data_dir.join(path)
            }
        };

        if let Some(parent) = sqlite_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create SQLite parent directory {}",
                    parent.display()
                )
            })?;
        }

        Ok(sqlite_path)
    }

    pub fn risk_config(&self) -> apex_core::application::risk_engine::RiskConfig {
        let defaults = apex_core::application::risk_engine::RiskConfig::default();
        apex_core::application::risk_engine::RiskConfig {
            max_daily_loss: self.risk.max_daily_loss,
            max_order_value: self.risk.max_order_value,
            ..defaults
        }
    }
}

fn ensure_config_file(runtime_paths: &RuntimePaths) -> Result<()> {
    let config_path = runtime_paths.config_file();
    if config_path.exists() {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory {}", parent.display()))?;
    }

    fs::write(config_path, DEFAULT_CONFIG_TEMPLATE).with_context(|| {
        format!(
            "Failed to create default config file at {}",
            config_path.display()
        )
    })?;

    Ok(())
}

fn resolve_runtime_dir(
    runtime_paths: &RuntimePaths,
    configured: &str,
    fallback: &Path,
) -> Result<PathBuf> {
    let path = if configured.is_empty() {
        fallback.to_path_buf()
    } else {
        runtime_paths.resolve_user_relative_path(configured)
    };

    fs::create_dir_all(&path)
        .with_context(|| format!("Failed to create runtime directory {}", path.display()))?;

    Ok(path)
}

/// UI appearance: theme + density.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// "terminal", "dark" or "light".
    #[serde(default = "default_theme")]
    pub theme: String,
    /// "comfortable" or "compact".
    #[serde(default = "default_density")]
    pub density: String,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            density: default_density(),
        }
    }
}

/// One OpenAI-compatible inference provider (chat completions or responses API).
/// API keys are never stored in config — `api_key_env` names the env var / OS
/// keychain entry to read at call time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    /// Stable identifier, e.g. "openrouter", "ollama", "acp:claude".
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Base URL (e.g. https://api.openai.com/v1) — empty for ACP providers.
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// "chat" (OpenAI /chat/completions), "responses" (/responses), or "acp"
    /// (spawn an ACP agent process — see [acp]).
    #[serde(default = "default_api_kind")]
    pub api_kind: String,
    /// Env var holding the API key (no secrets in config).
    #[serde(default)]
    pub api_key_env: String,
    /// Optional per-provider token cap; 0 = use copilot.max_tokens.
    #[serde(default)]
    pub max_tokens: u32,
}

/// Inference provider registry + selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmConfig {
    /// id of the active provider; empty = legacy [copilot] config.
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub providers: Vec<LlmProviderConfig>,
}

/// Agent Client Protocol connector — spawn an external agent (IDE/CLI) and
/// talk to it over stdio JSON-RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpConfig {
    /// Command to spawn, e.g. "npx @zed-industries/claude-code-acp".
    #[serde(default)]
    pub command: String,
    /// Working directory for the agent; empty = repo root at runtime.
    #[serde(default)]
    pub cwd: String,
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            cwd: String::new(),
        }
    }
}

/// Limits applied to every order sent to a live (non-paper) broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveConfig {
    /// Orders allowed per UTC day across all live brokers.
    #[serde(default = "default_live_max_orders")]
    pub max_orders_per_day: u32,
    /// Largest quantity allowed in a single live order.
    #[serde(default = "default_live_max_qty")]
    pub max_qty_per_order: f64,
}

fn default_live_max_orders() -> u32 {
    50
}

fn default_live_max_qty() -> f64 {
    1000.0
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            max_orders_per_day: default_live_max_orders(),
            max_qty_per_order: default_live_max_qty(),
        }
    }
}

/// Scheduled automations (model-signal trading rules etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationsConfig {
    /// Master switch — false disables the scheduler entirely.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When false, automations and copilot order tools may only target the
    /// paper adapter; live broker ids are rejected.
    #[serde(default)]
    pub allow_live_trading: bool,
    /// Cap on orders a single rule may place per UTC day.
    #[serde(default = "default_max_orders_per_day")]
    pub max_orders_per_day: u32,
}

impl Default for AutomationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_live_trading: false,
            max_orders_per_day: default_max_orders_per_day(),
        }
    }
}

fn default_max_orders_per_day() -> u32 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDataConfig {
    #[serde(default = "default_market_data_adapter")]
    pub adapter: String,
}

impl Default for MarketDataConfig {
    fn default() -> Self {
        Self {
            adapter: default_market_data_adapter(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_execution_adapter")]
    pub adapter: String,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            adapter: default_execution_adapter(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSection {
    #[serde(default = "default_max_daily_loss")]
    pub max_daily_loss: f64,
    #[serde(default = "default_max_order_value")]
    pub max_order_value: f64,
}

impl Default for RiskSection {
    fn default() -> Self {
        Self {
            max_daily_loss: default_max_daily_loss(),
            max_order_value: default_max_order_value(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_storage_backend")]
    pub backend: String,
    #[serde(default = "default_sqlite_path")]
    pub sqlite_path: String,
    #[serde(default)]
    pub postgres_url: Option<String>,
    #[serde(default = "default_wal_mode")]
    pub wal_mode: bool,
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: default_storage_backend(),
            sqlite_path: default_sqlite_path(),
            postgres_url: None,
            wal_mode: default_wal_mode(),
            pool_size: default_pool_size(),
        }
    }
}

impl StorageConfig {
    pub fn backend_kind(&self) -> Result<StorageBackendKind> {
        let normalized = self.backend.trim().to_ascii_lowercase();

        match normalized.as_str() {
            "" | "sqlite" => Ok(StorageBackendKind::Sqlite),
            "timescale" | "timescaledb" | "postgres" | "postgresql" => {
                Ok(StorageBackendKind::Timescale)
            }
            other => Err(anyhow::anyhow!(
                "Unsupported storage backend `{other}`. Expected `sqlite` or `timescale`."
            )),
        }
    }

    pub fn postgres_url(&self) -> Option<String> {
        preferred_env_var("APEX_POSTGRES_URL")
            .or_else(|| preferred_env_var("DATABASE_URL"))
            .or_else(|| {
                self.postgres_url
                    .clone()
                    .and_then(normalize_optional_string)
            })
    }
}

/// RSS/Atom feed source configuration for the news engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsFeedConfig {
    pub name: String,
    pub url: String,
    /// "rss" or "atom"
    #[serde(default = "default_feed_type")]
    pub feed_type: String,
    /// 1-10, higher = more important
    #[serde(default = "default_feed_priority")]
    pub priority: u8,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// News engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How often feeds are polled, in seconds.
    #[serde(default = "default_news_poll_secs")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub feeds: Vec<NewsFeedConfig>,
}

impl Default for NewsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: default_news_poll_secs(),
            feeds: Vec::new(),
        }
    }
}

/// AI copilot (OpenRouter) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// OpenRouter model slug. "openrouter/free" auto-routes to a free model.
    #[serde(default = "default_copilot_model")]
    pub model: String,
    /// OpenRouter API base URL.
    #[serde(default = "default_copilot_base_url")]
    pub base_url: String,
    /// Max tokens per response.
    #[serde(default = "default_copilot_max_tokens")]
    pub max_tokens: u32,
    /// When true (default), write-class copilot tool calls (orders,
    /// automations, strategy writes, training) pause for explicit user
    /// approval in the chat UI before executing.
    #[serde(default = "default_true")]
    pub require_approval: bool,
}

impl Default for CopilotConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_copilot_model(),
            base_url: default_copilot_base_url(),
            max_tokens: default_copilot_max_tokens(),
            require_approval: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackendKind {
    Sqlite,
    Timescale,
}

fn preferred_env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|value| normalize_optional_string(value))
}

fn normalize_optional_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_data_dir() -> String {
    "data".to_string()
}

fn default_true() -> bool {
    true
}

fn default_feed_type() -> String {
    "rss".to_string()
}

fn default_feed_priority() -> u8 {
    5
}

fn default_news_poll_secs() -> u64 {
    300
}

fn default_copilot_model() -> String {
    "openrouter/free".to_string()
}

fn default_copilot_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

fn default_copilot_max_tokens() -> u32 {
    1024
}

fn default_max_daily_loss() -> f64 {
    50_000.0
}

fn default_theme() -> String {
    "terminal".to_string()
}

fn default_density() -> String {
    "compact".to_string()
}

fn default_api_kind() -> String {
    "chat".to_string()
}

fn default_market_data_adapter() -> String {
    "yahoo_finance".to_string()
}

fn default_execution_adapter() -> String {
    "paper".to_string()
}

fn default_max_order_value() -> f64 {
    500_000.0
}

fn default_storage_backend() -> String {
    "sqlite".to_string()
}

fn default_sqlite_path() -> String {
    "apex.db".to_string()
}

fn default_wal_mode() -> bool {
    true
}

fn default_pool_size() -> usize {
    4
}
