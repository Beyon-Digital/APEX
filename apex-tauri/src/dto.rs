use serde::{Deserialize, Serialize};

use apex_core::domain::models;

/// Serialize a serde-enabled value to a bare string (strips surrounding quotes).
fn enum_to_string<T: Serialize>(val: &T) -> String {
    serde_json::to_string(val)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Quote DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteDto {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: u64,
    pub change_pct: f64,
    pub vwap: f64,
    pub updated_at: String,
}

impl From<&models::Quote> for QuoteDto {
    fn from(q: &models::Quote) -> Self {
        Self {
            symbol: q.symbol.0.clone(),
            bid: q.bid,
            ask: q.ask,
            last: q.last,
            open: q.open,
            high: q.high,
            low: q.low,
            volume: q.volume,
            change_pct: q.change_pct,
            vwap: q.vwap,
            updated_at: q.updated_at.to_rfc3339(),
        }
    }
}

/// OHLCV DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct OHLCVDto {
    pub time: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
}

impl From<&models::OHLCV> for OHLCVDto {
    fn from(bar: &models::OHLCV) -> Self {
        Self {
            time: bar.time.to_rfc3339(),
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        }
    }
}

/// Order DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderDto {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: f64,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub status: String,
    pub filled_qty: f64,
    pub avg_price: f64,
    pub created_at: String,
    pub updated_at: String,
    pub broker_id: String,
    pub source: String,
    pub rejection_reason: Option<String>,
}

impl From<&models::Order> for OrderDto {
    fn from(o: &models::Order) -> Self {
        Self {
            id: o.id.0.clone(),
            symbol: o.symbol.0.clone(),
            side: enum_to_string(&o.side),
            order_type: enum_to_string(&o.order_type),
            quantity: o.quantity,
            price: o.price,
            stop_price: o.stop_price,
            status: enum_to_string(&o.status),
            filled_qty: o.filled_qty,
            avg_price: o.avg_price,
            created_at: o.created_at.to_rfc3339(),
            updated_at: o.updated_at.to_rfc3339(),
            broker_id: o.broker_id.clone(),
            source: o.source.clone(),
            rejection_reason: o.rejection_reason.clone(),
        }
    }
}

/// Position DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionDto {
    pub symbol: String,
    pub quantity: f64,
    pub avg_price: f64,
    pub side: String,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub broker_id: String,
}

impl From<&models::Position> for PositionDto {
    fn from(p: &models::Position) -> Self {
        Self {
            symbol: p.symbol.0.clone(),
            quantity: p.quantity,
            avg_price: p.avg_price,
            side: enum_to_string(&p.side),
            pnl: p.pnl,
            pnl_pct: p.pnl_pct,
            broker_id: p.broker_id.clone(),
        }
    }
}

/// News item DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct NewsItemDto {
    pub id: String,
    pub headline: String,
    pub summary: String,
    pub source: String,
    pub url: String,
    pub published: String,
    pub symbols: Vec<String>,
    pub sentiment: Option<f32>,
}

impl From<&models::NewsItem> for NewsItemDto {
    fn from(n: &models::NewsItem) -> Self {
        Self {
            id: n.id.to_string(),
            headline: n.headline.clone(),
            summary: n.summary.clone(),
            source: n.source.clone(),
            url: n.url.clone(),
            published: n.published.to_rfc3339(),
            symbols: n.symbols.iter().map(|s| s.0.clone()).collect(),
            sentiment: n.sentiment,
        }
    }
}

/// Alert DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AlertDto {
    pub rule_id: String,
    pub message: String,
    pub severity: String,
}

/// New order request DTO from frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewOrderRequestDto {
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: f64,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub broker_id: String,
    pub tag: Option<String>,
    /// Broker margin product (e.g. Zerodha CNC/MIS/NRML); the broker default when absent.
    #[serde(default)]
    pub product: Option<String>,
}

/// Account balance DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AccountBalanceDto {
    pub total_value: f64,
    pub cash: f64,
    pub margin_used: f64,
    pub margin_available: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub currency: String,
}

impl From<&models::AccountBalance> for AccountBalanceDto {
    fn from(b: &models::AccountBalance) -> Self {
        Self {
            total_value: b.total_value,
            cash: b.cash,
            margin_used: b.margin_used,
            margin_available: b.margin_available,
            unrealized_pnl: b.unrealized_pnl,
            realized_pnl: b.realized_pnl,
            currency: b.currency.clone(),
        }
    }
}

/// Risk status DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskStatusDto {
    pub session_pnl: f64,
    pub is_halted: bool,
    pub max_daily_loss: f64,
}

/// General settings DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettingsDto {
    pub data_dir: String,
}

/// Adapter selection DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterPreferenceDto {
    pub adapter: String,
    #[serde(default)]
    pub available_adapters: Vec<String>,
}

/// Risk configuration DTO for frontend settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSettingsDto {
    pub max_daily_loss: f64,
    pub max_order_value: f64,
}

/// Storage settings DTO for frontend settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSettingsDto {
    pub backend: String,
    pub sqlite_path: String,
    pub postgres_url: String,
    pub wal_mode: bool,
    pub pool_size: usize,
    #[serde(default)]
    pub available_backends: Vec<String>,
}

/// Appearance settings DTO (theme + density).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettingsDto {
    /// "terminal" | "dark" | "light"
    pub theme: String,
    /// "comfortable" | "compact"
    pub density: String,
}

/// One configured OpenAI-compatible / ACP inference provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderDto {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    /// "chat" | "responses" | "acp"
    pub api_kind: String,
    /// Env var name holding the API key (never the key itself).
    pub api_key_env: String,
    pub max_tokens: u32,
    /// True when the referenced env var currently resolves to a non-empty value.
    pub key_configured: bool,
}

/// LLM provider registry settings DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettingsDto {
    /// Active provider id; empty = legacy [copilot] config.
    pub active: String,
    pub providers: Vec<LlmProviderDto>,
}

/// ACP connector settings DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSettingsDto {
    pub command: String,
    pub cwd: String,
}

/// App settings DTO for frontend settings panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettingsDto {
    pub config_path: String,
    pub runtime_storage_backend: String,
    pub runtime_storage_target: String,
    /// Set when the saved storage backend could not start and SQLite is in use instead.
    pub runtime_storage_notice: Option<String>,
    pub general: GeneralSettingsDto,
    pub market_data: AdapterPreferenceDto,
    pub execution: AdapterPreferenceDto,
    pub risk: RiskSettingsDto,
    pub storage: StorageSettingsDto,
    pub appearance: AppearanceSettingsDto,
    pub llm: LlmSettingsDto,
    pub acp: AcpSettingsDto,
}

/// App settings update request DTO from frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettingsUpdateDto {
    pub general: GeneralSettingsDto,
    pub market_data: AdapterPreferenceDto,
    pub execution: AdapterPreferenceDto,
    pub risk: RiskSettingsDto,
    pub storage: StorageSettingsDto,
    #[serde(default)]
    pub appearance: AppearanceSettingsDto,
    #[serde(default)]
    pub llm: LlmSettingsUpdateDto,
    #[serde(default)]
    pub acp: AcpSettingsDto,
}

/// LLM update payload — providers written without key_configured.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmSettingsUpdateDto {
    pub active: String,
    #[serde(default)]
    pub providers: Vec<LlmProviderWriteDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderWriteDto {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_write_api_kind")]
    pub api_kind: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub max_tokens: u32,
}

fn default_write_api_kind() -> String {
    "chat".to_string()
}

impl Default for AppearanceSettingsDto {
    fn default() -> Self {
        Self {
            theme: "terminal".to_string(),
            density: "compact".to_string(),
        }
    }
}

impl Default for AcpSettingsDto {
    fn default() -> Self {
        Self {
            command: String::new(),
            cwd: String::new(),
        }
    }
}

/// Research notebook cell DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookCellDto {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub output: Option<String>,
}

/// Research notebook document DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookDocumentDto {
    pub title: String,
    pub path: String,
    pub cells: Vec<NotebookCellDto>,
    pub updated_at: String,
}

/// Research notebook summary DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookSummaryDto {
    pub name: String,
    pub path: String,
    pub updated_at: String,
}

/// Run-request DTO for notebook cell execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookRunRequestDto {
    pub path: String,
    pub cells: Vec<NotebookCellDto>,
    pub cell_id: String,
}

/// Result DTO for a notebook cell execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookCellExecutionDto {
    pub cell_id: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub finished_at: String,
}

/// ML model DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MLModelDto {
    pub model_id: String,
    pub algorithm: String,
    pub status: String,
    pub metrics: std::collections::HashMap<String, f64>,
    pub feature_names: Vec<String>,
    pub created_at: String,
    pub data_path: String,
    pub target_column: String,
}

/// ML training request DTO from frontend.
///
/// Every field except `algorithm` has a default so older callers (and older saved JSON) keep
/// working. When `dataset` is set it supplies `data_path`, the feature columns, the target and
/// the return column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MLTrainingRequestDto {
    pub algorithm: String,
    #[serde(default)]
    pub data_path: String,
    #[serde(default)]
    pub target_column: String,
    #[serde(default)]
    pub feature_columns: Vec<String>,
    #[serde(default)]
    pub hyperparams: std::collections::HashMap<String, serde_json::Value>,
    #[serde(default = "default_n_splits")]
    pub n_splits: usize,
    #[serde(default)]
    pub lag_periods: Vec<i64>,
    /// Name of a dataset built in the workbench; when set it supplies `data_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    /// Column holding each row's forward return, used to score trading performance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_column: Option<String>,
    /// How the evaluation trades the predictions: "long_flat" (default) or "long_short".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trade_mode: Option<String>,
    /// Share (0 to 40) of the final time steps kept out of cross-validation and scored once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdout_pct: Option<f64>,
    /// Required when `algorithm` is "custom": a file saved with `save_estimator_file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimator_file: Option<String>,
    /// Required when `algorithm` is "recipe": the name of a saved recipe (see `ml_recipes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    /// Seed for estimators that take one. Omitted: a fixed default, so runs repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub random_state: Option<i64>,
    /// With `dataset`: train on this version of it. Omitted: the latest version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_version: Option<u32>,
}

impl Default for MLTrainingRequestDto {
    fn default() -> Self {
        Self {
            algorithm: String::new(),
            data_path: String::new(),
            target_column: String::new(),
            feature_columns: Vec::new(),
            hyperparams: Default::default(),
            n_splits: default_n_splits(),
            lag_periods: Vec::new(),
            dataset: None,
            return_column: None,
            trade_mode: None,
            holdout_pct: None,
            estimator_file: None,
            recipe: None,
            random_state: None,
            dataset_version: None,
        }
    }
}

fn default_n_splits() -> usize {
    5
}

/// ML training result DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MLTrainingResultDto {
    pub model_id: String,
    pub metrics: std::collections::HashMap<String, f64>,
    pub feature_names: Vec<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

// ── ML workbench: features, datasets, model detail ───────────────────

/// One entry of the shared feature catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureDefDto {
    pub id: String,
    pub label: String,
    /// "Price" | "Trend" | "Momentum" | "Volatility" | "Volume"
    pub group: String,
    pub description: String,
    /// Number of leading bars for which the value is not yet defined.
    pub warmup_bars: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LabelSpecDto {
    pub horizon_bars: usize,
    /// "direction" | "return_threshold"
    pub kind: String,
    #[serde(default)]
    pub threshold_pct: f64,
    /// return_threshold only: separate up/down thresholds (percent); fall back to `threshold_pct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_threshold_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_threshold_pct: Option<f64>,
}

/// A user-defined feature: a formula over bars (see `commands::ml_expr`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CustomFeatureDto {
    /// `[a-z][a-z0-9_]{0,40}`, not a catalog feature id.
    pub name: String,
    pub expr: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildDatasetRequestDto {
    pub name: String,
    pub symbols: Vec<String>,
    pub timeframe: String,
    pub bars: usize,
    pub features: Vec<String>,
    pub label: LabelSpecDto,
    /// Inclusive ISO dates (YYYY-MM-DD) bounding the bars; otherwise the latest `bars`.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub custom_features: Vec<CustomFeatureDto>,
    /// Extra symbols custom formulas may read with `other("SYMBOL", "field")`; not dataset rows.
    #[serde(default)]
    pub context_symbols: Vec<String>,
    /// Round-trip cost in percent, subtracted from every forward return before labelling.
    #[serde(default)]
    pub fee_pct: f64,
}

/// One sample value of a formula check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpressionSampleDto {
    pub time: String,
    /// null = not yet defined at that bar.
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpressionCheckDto {
    pub ok: bool,
    pub error: Option<String>,
    /// 0-based character offset of the error in the formula.
    pub error_pos: Option<usize>,
    pub warmup_bars: usize,
    /// The last few computed values for the chosen symbol.
    pub sample: Vec<ExpressionSampleDto>,
}

/// How much stored data exists for a symbol and timeframe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolDataDto {
    pub symbol: String,
    pub timeframe: String,
    pub bars: usize,
    pub first_time: Option<String>,
    pub last_time: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatasetInfoDto {
    pub name: String,
    /// Path usable as `data_path` when training (relative to the work root).
    pub path: String,
    pub rows: usize,
    pub symbols: Vec<String>,
    pub timeframe: String,
    pub features: Vec<String>,
    pub label: LabelSpecDto,
    pub class_counts: std::collections::BTreeMap<String, usize>,
    pub created_at: String,
    pub first_time: Option<String>,
    pub last_time: Option<String>,
    /// "user" | "copilot"
    pub source: String,
    #[serde(default)]
    pub custom_features: Vec<CustomFeatureDto>,
    #[serde(default)]
    pub context_symbols: Vec<String>,
    #[serde(default)]
    pub fee_pct: f64,
    /// Version of the dataset object in the audit ledger (1 for the first build under this name).
    /// Every build is a new version under the same name; earlier versions are never overwritten.
    #[serde(default)]
    pub version: u32,
    /// SHA-256 of this version's CSV file (the file is write-once).
    #[serde(default)]
    pub csv_sha256: String,
    /// Size of that CSV file in bytes.
    #[serde(default)]
    pub csv_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetPreviewDto {
    #[serde(flatten)]
    pub info: DatasetInfoDto,
    pub columns: Vec<String>,
    pub sample: Vec<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvFoldDto {
    pub fold: usize,
    pub train_rows: usize,
    pub test_rows: usize,
    pub accuracy: f64,
    pub f1: f64,
    pub precision: f64,
    pub recall: f64,
    #[serde(default)]
    pub baseline_accuracy: Option<f64>,
    #[serde(default)]
    pub trade_return_pct: Option<f64>,
    #[serde(default)]
    pub hit_rate: Option<f64>,
    #[serde(default)]
    pub trades: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingSummaryDto {
    pub strategy_return_pct: f64,
    pub buy_hold_return_pct: f64,
    pub hit_rate: f64,
    pub trades: usize,
    pub max_drawdown_pct: f64,
    pub sharpe: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureImportanceDto {
    pub feature: String,
    pub importance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsageDto {
    /// Always "automation".
    pub kind: String,
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub symbol: String,
}

/// Result on the final period that cross-validation never saw.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HoldoutDto {
    pub rows: usize,
    pub accuracy: f64,
    #[serde(default)]
    pub baseline_accuracy: Option<f64>,
    #[serde(default)]
    pub trading: Option<TradingSummaryDto>,
}

fn default_trade_mode() -> String {
    "long_flat".to_string()
}

/// Everything the workbench shows about one model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelDetailDto {
    pub model_id: String,
    pub algorithm: String,
    pub status: String,
    pub metrics: std::collections::HashMap<String, f64>,
    pub feature_names: Vec<String>,
    pub created_at: String,
    pub data_path: String,
    pub target_column: String,
    pub display_name: String,
    pub notes: String,
    pub tags: Vec<String>,
    /// "user" | "copilot" | "unknown"
    pub source: String,
    pub hyperparams: std::collections::HashMap<String, serde_json::Value>,
    pub n_splits: usize,
    pub lag_periods: Vec<i64>,
    pub dataset: Option<String>,
    pub symbols: Vec<String>,
    pub timeframe: Option<String>,
    pub label: Option<LabelSpecDto>,
    pub rows: Option<usize>,
    pub duration_secs: Option<f64>,
    pub artifact_bytes: Option<u64>,
    pub library_versions: std::collections::HashMap<String, String>,
    pub cv_folds: Vec<CvFoldDto>,
    pub feature_importance: Vec<FeatureImportanceDto>,
    pub trading: Option<TradingSummaryDto>,
    pub log_tail: String,
    pub used_by: Vec<ModelUsageDto>,
    /// "long_flat" | "long_short": how the evaluation traded the predictions.
    #[serde(default = "default_trade_mode")]
    pub trade_mode: String,
    pub holdout: Option<HoldoutDto>,
    #[serde(default)]
    pub custom_features: Vec<CustomFeatureDto>,
    #[serde(default)]
    pub context_symbols: Vec<String>,
    #[serde(default)]
    pub fee_pct: f64,
    /// Name of the custom estimator file when `algorithm` is "custom".
    pub estimator_file: Option<String>,
    /// The recipe this model was built from when `algorithm` is "recipe" (a copy).
    #[serde(default)]
    pub recipe: Option<RecipeDto>,
    /// Provenance. Version of the dataset the model was trained on (see `ledger_history('dataset', name)`).
    #[serde(default)]
    pub dataset_version: Option<u32>,
    /// SHA-256 of the dataset CSV the trainer read.
    #[serde(default)]
    pub dataset_csv_sha256: Option<String>,
    /// Ledger version of the custom estimator file whose code produced this model.
    #[serde(default)]
    pub estimator_version: Option<u32>,
    /// Ledger version of the recipe this model was built from.
    #[serde(default)]
    pub recipe_version: Option<u32>,
    /// Current version of this model's own metadata in the audit ledger (display name, notes, tags).
    #[serde(default)]
    pub ledger_version: Option<u32>,
}

// ── Algorithm catalog and custom estimators ──────────────────────────

/// One tunable constructor parameter of an algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDefDto {
    pub key: String,
    pub label: String,
    /// "int" | "float" | "bool" | "choice" | "text"
    #[serde(rename = "type")]
    pub kind: String,
    pub default: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
    #[serde(default)]
    pub help: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmDto {
    pub id: String,
    pub label: String,
    pub description: String,
    pub params: Vec<ParamDefDto>,
    pub accepts_extra_params: bool,
}

/// A custom estimator file under `<work root>/estimators/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstimatorFileDto {
    pub name: String,
    /// Relative to the work root, e.g. `estimators/my_model.py`.
    pub path: String,
    pub content: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSignalPointDto {
    pub time: String,
    pub close: f64,
    pub signal: i64,
    pub probability: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSignalHistoryDto {
    pub model_id: String,
    pub symbol: String,
    pub points: Vec<ModelSignalPointDto>,
}

/// Adapter health DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterHealthDto {
    pub adapter_id: String,
    pub adapter_type: String,
    pub status: String,
    pub message: String,
    pub last_check: String,
}

/// System health DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHealthDto {
    pub adapters: Vec<AdapterHealthDto>,
    pub uptime_secs: u64,
    pub memory_usage_mb: u64,
    pub active_subscriptions: usize,
    pub open_orders: usize,
    pub active_strategies: usize,
}

/// Broker connection snapshot DTO for frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerConnectionDto {
    pub broker_id: String,
    pub display_name: String,
    pub mode: String,
    pub status: String,
    pub configured: bool,
    pub authenticated: bool,
    pub execution_available: bool,
    pub market_data_available: bool,
    pub token_field_label: String,
    pub message: String,
    /// Whether an automation may short through this broker: paper always; a live broker only
    /// when `[automations] allow_live_shorting` is on and its adapter supports shorting.
    #[serde(default)]
    pub can_short: bool,
}

/// One point of an indicator/quant series, aligned to a bar time (RFC3339).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesPointDto {
    pub time: String,
    pub value: f64,
}

/// A named series of aligned points (e.g. "upper" band, "macd" line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedSeriesDto {
    pub name: String,
    pub points: Vec<SeriesPointDto>,
}

/// Result of a `compute_indicator` call.
/// `overlay` = draw on the price pane; false = dedicated oscillator pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndicatorResultDto {
    pub symbol: String,
    pub indicator: String,
    pub overlay: bool,
    pub series: Vec<NamedSeriesDto>,
}

/// Quant summary over a symbol's return series (OpenBB quantitative.summary).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantStatsDto {
    pub symbol: String,
    pub n: usize,
    pub mean: f64,
    pub std_dev: f64,
    pub variance: f64,
    pub skewness: f64,
    pub kurtosis: f64,
    pub min: f64,
    pub q05: f64,
    pub q25: f64,
    pub median: f64,
    pub q75: f64,
    pub q95: f64,
    pub max: f64,
    pub jarque_bera: f64,
    pub normal: bool,
    pub sharpe: f64,
    pub sortino: f64,
    pub omega: f64,
    pub max_drawdown: f64,
    pub ann_volatility: f64,
    /// ACF at lags 1..=10
    pub autocorr: Vec<f64>,
    /// Rolling stats series aligned to bar times
    pub rolling_vol: Vec<SeriesPointDto>,
    pub rolling_sharpe: Vec<SeriesPointDto>,
}

/// OLS regression y = alpha + beta*x over aligned return series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionDto {
    pub x_symbol: String,
    pub y_symbol: String,
    pub n: usize,
    pub alpha: f64,
    pub beta: f64,
    pub r_squared: f64,
    /// Per-observation residuals aligned to the shared timestamps
    pub residuals: Vec<SeriesPointDto>,
    /// Scatter of (x, y) return pairs for the regression plot
    pub scatter: Vec<[f64; 2]>,
    /// Fitted line endpoints for charting: [x_min, x_max] → [y_min, y_max]
    pub fit_line: Vec<[f64; 2]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_quote_dto_from_quote() {
        let quote = models::Quote {
            symbol: models::Symbol("AAPL".into()),
            bid: 150.0,
            ask: 150.05,
            last: 150.02,
            open: 149.0,
            high: 151.0,
            low: 148.5,
            volume: 10000,
            change_pct: 0.5,
            vwap: 149.8,
            updated_at: Utc::now(),
        };
        let dto = QuoteDto::from(&quote);
        assert_eq!(dto.symbol, "AAPL");
        assert!((dto.last - 150.02).abs() < 0.01);
    }

    #[test]
    fn test_order_dto_from_order() {
        let order = models::Order {
            id: models::OrderId("test-1".into()),
            symbol: models::Symbol("AAPL".into()),
            side: models::OrderSide::Buy,
            order_type: models::OrderType::Limit,
            quantity: 10.0,
            price: Some(150.0),
            stop_price: None,
            status: models::OrderStatus::Open,
            filled_qty: 0.0,
            avg_price: 0.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            broker_id: "paper".into(),
            source: "manual".into(),
            rejection_reason: None,
        };
        let dto = OrderDto::from(&order);
        assert_eq!(dto.id, "test-1");
        assert_eq!(dto.side, "Buy");
    }

    #[test]
    fn test_position_dto_from_position() {
        let pos = models::Position {
            symbol: models::Symbol("AAPL".into()),
            quantity: 10.0,
            avg_price: 150.0,
            side: models::OrderSide::Buy,
            pnl: 50.0,
            pnl_pct: 3.33,
            broker_id: "paper".into(),
        };
        let dto = PositionDto::from(&pos);
        assert_eq!(dto.symbol, "AAPL");
        assert!((dto.pnl - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_new_order_request_dto_deserialization() {
        let json = r#"{
            "symbol": "AAPL",
            "side": "Buy",
            "order_type": "Limit",
            "quantity": 10.0,
            "price": 150.0,
            "stop_price": null,
            "broker_id": "paper",
            "tag": null
        }"#;
        let dto: NewOrderRequestDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.symbol, "AAPL");
        assert_eq!(dto.side, "Buy");
    }

    #[test]
    fn test_risk_status_dto_serialization() {
        let dto = RiskStatusDto {
            session_pnl: -500.0,
            is_halted: false,
            max_daily_loss: 50000.0,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("session_pnl"));
    }

    #[test]
    fn test_broker_connection_dto_serialization() {
        let dto = BrokerConnectionDto {
            broker_id: "paper".into(),
            display_name: "Paper Trading".into(),
            mode: "paper".into(),
            status: "ready".into(),
            configured: true,
            authenticated: true,
            execution_available: true,
            market_data_available: false,
            token_field_label: String::new(),
            message: "Paper trading is available".into(),
            can_short: true,
        };

        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"can_short\":true"));
        assert!(json.contains("Paper Trading"));
        assert!(json.contains("ready"));
    }

    #[test]
    fn test_app_settings_update_dto_deserialization() {
        let json = r#"{
            "general": { "data_dir": "data" },
            "market_data": { "adapter": "yahoo_finance" },
            "execution": { "adapter": "paper" },
            "risk": { "max_daily_loss": 50000, "max_order_value": 500000 },
            "storage": {
                "backend": "sqlite",
                "sqlite_path": "apex.db",
                "postgres_url": "",
                "wal_mode": true,
                "pool_size": 4
            }
        }"#;

        let dto: AppSettingsUpdateDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.execution.adapter, "paper");
        assert_eq!(dto.storage.backend, "sqlite");
    }

    #[test]
    fn test_notebook_document_dto_serialization() {
        let notebook = NotebookDocumentDto {
            title: "Research".into(),
            path: "notebooks/research.apexnb.json".into(),
            cells: vec![NotebookCellDto {
                id: "cell-1".into(),
                kind: "code".into(),
                content: "print('hello')".into(),
                output: Some("hello".into()),
            }],
            updated_at: Utc::now().to_rfc3339(),
        };

        let json = serde_json::to_string(&notebook).unwrap();
        assert!(json.contains("Research"));
        assert!(json.contains("cell-1"));
    }
}

/// One input field in an adapter's configuration form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterFieldDto {
    pub key: String,
    pub label: String,
    /// "text" | "secret" | "toggle"
    pub kind: String,
    pub required: bool,
    pub help: String,
}

/// Describes an adapter so the UI can render its configuration form generically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterSchemaDto {
    pub adapter_id: String,
    pub display_name: String,
    /// "broker" | "data" | "llm"
    pub category: String,
    pub description: String,
    pub fields: Vec<AdapterFieldDto>,
}

/// Stored (non-secret) configuration for one adapter. Secrets are reported only
/// as the names of fields that have a stored value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterConfigDto {
    pub adapter_id: String,
    pub enabled: bool,
    pub settings: std::collections::HashMap<String, String>,
    pub secrets_configured: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveAdapterConfigRequest {
    pub adapter_id: String,
    pub enabled: bool,
    #[serde(default)]
    pub settings: std::collections::HashMap<String, String>,
    /// Only fields being set or replaced; omitted/empty values keep the stored secret.
    #[serde(default)]
    pub secrets: std::collections::HashMap<String, String>,
    /// Secret fields to delete.
    #[serde(default)]
    pub clear_secrets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterTestResultDto {
    pub adapter_id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvImportResultDto {
    pub source: String,
    pub imported: Vec<String>,
}

/// State of the live-trading safeguards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveStatusDto {
    pub armed: bool,
    pub orders_today: u32,
    pub max_orders_per_day: u32,
    pub max_qty_per_order: f64,
    pub halted: bool,
}

/// Result of previewing an order. Live orders carry a short-lived confirmation id
/// that `place_order` requires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPreviewDto {
    pub broker_id: String,
    pub live: bool,
    pub estimated_value: Option<f64>,
    pub confirmation_id: Option<String>,
    pub expires_in_secs: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillSwitchResultDto {
    pub cancelled: Vec<String>,
    pub failed: Vec<String>,
}

/// One match in the symbol picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolSearchResultDto {
    pub symbol: String,
    pub name: String,
    pub exchange: String,
    /// Where the match came from: "zerodha" or "yahoo".
    pub source: String,
}

/// One data feed that could serve a symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedOptionDto {
    pub adapter_id: String,
    pub name: String,
    /// "delayed" | "live" | "exchange"
    pub quality: String,
    pub supports: bool,
    pub ready: bool,
    /// Why it can't be chosen right now, if so.
    pub unavailable_reason: Option<String>,
}

/// Which feed is serving a symbol and which others could.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolFeedDto {
    pub symbol: String,
    /// The feed currently supplying quotes.
    pub active: Option<String>,
    /// The feed the user pinned, if any (None = automatic).
    pub preferred: Option<String>,
    pub options: Vec<FeedOptionDto>,
}

// ── Algorithm recipes ────────────────────────────────────────────────

/// Parameters of a step or member: a JSON object, keys sorted when written.
pub type RecipeParams = serde_json::Map<String, serde_json::Value>;

/// `#[serde(default)]` only covers a missing key. The Python validator also accepts an explicit
/// `null` (and language models often send one), so a `null` reads as the default too.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_as_none_calibration<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(default_calibrate))
}

/// One preprocessing step, e.g. `{"kind": "standard_scaler", "params": {}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeStepDto {
    pub kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub params: RecipeParams,
}

/// A member model of a voting or stacking recipe (or the final estimator of a stack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeMemberDto {
    pub algorithm: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub params: RecipeParams,
    /// Voting only; the trainer treats a missing weight as 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeModelDto {
    Single {
        algorithm: String,
        #[serde(default, deserialize_with = "null_as_default")]
        params: RecipeParams,
    },
    Voting {
        /// "soft" | "hard"
        voting: String,
        members: Vec<RecipeMemberDto>,
    },
    /// Time-aware stacking: members are fitted on expanding windows and their out-of-fold
    /// predictions train `final`.
    Stacking {
        members: Vec<RecipeMemberDto>,
        #[serde(rename = "final")]
        final_estimator: RecipeMemberDto,
        blocks: usize,
    },
}

fn default_calibrate() -> String {
    "none".into()
}

/// An algorithm assembled from library parts, saved as `<work root>/algorithms/<name>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeDto {
    /// `[a-z][a-z0-9_]{0,40}`
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub label: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub steps: Vec<RecipeStepDto>,
    pub model: RecipeModelDto,
    /// "none" | "sigmoid" | "isotonic"
    #[serde(default = "default_calibrate", deserialize_with = "null_as_none_calibration")]
    pub calibrate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeIssueDto {
    /// Where in the recipe, e.g. `steps[1].params.k` or `model.members[2]`.
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeCheckDto {
    pub ok: bool,
    #[serde(default)]
    pub errors: Vec<RecipeIssueDto>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub steps: usize,
    /// Number of fitted models (members plus the final estimator for stacking).
    #[serde(default)]
    pub models: usize,
    /// Pessimistic size estimate for a typical dataset; null when unknown.
    #[serde(default)]
    pub est_model_mb: Option<f64>,
}
