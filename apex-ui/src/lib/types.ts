// DTO types matching the Rust backend
export interface QuoteDto {
  symbol: string;
  bid: number;
  ask: number;
  last: number;
  open: number;
  high: number;
  low: number;
  volume: number;
  change_pct: number;
  vwap: number;
  updated_at: string;
}

export interface OHLCVDto {
  time: string;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

export interface OrderDto {
  id: string;
  symbol: string;
  side: string;
  order_type: string;
  quantity: number;
  price: number | null;
  stop_price: number | null;
  status: string;
  rejection_reason?: string | null;
  filled_qty: number;
  avg_price: number;
  created_at: string;
  updated_at: string;
  broker_id: string;
  source: string;
}

export interface PositionDto {
  symbol: string;
  quantity: number;
  avg_price: number;
  side: string;
  pnl: number;
  pnl_pct: number;
  broker_id: string;
}

export interface AlertDto {
  rule_id: string;
  message: string;
  severity: string;
}

export interface AlertRuleDto {
  id: string;
  rule: string;
  enabled: boolean;
}

export interface RiskStatusDto {
  session_pnl: number;
  is_halted: boolean;
  max_daily_loss: number;
}

export interface GeneralSettingsDto {
  data_dir: string;
}

export interface AdapterPreferenceDto {
  adapter: string;
  available_adapters: string[];
}

export interface RiskSettingsDto {
  max_daily_loss: number;
  max_order_value: number;
}

export interface StorageSettingsDto {
  backend: string;
  sqlite_path: string;
  postgres_url: string;
  wal_mode: boolean;
  pool_size: number;
  available_backends: string[];
}

export interface AppearanceSettingsDto {
  theme: string;
  density: string;
}

export interface LlmProviderDto {
  id: string;
  name: string;
  base_url: string;
  model: string;
  api_kind: string;
  api_key_env: string;
  max_tokens: number;
  key_configured: boolean;
}

export interface LlmProviderWriteDto {
  id: string;
  name: string;
  base_url: string;
  model: string;
  api_kind: string;
  api_key_env: string;
  max_tokens: number;
}

export interface LlmSettingsDto {
  active: string;
  providers: LlmProviderDto[];
}

export interface LlmSettingsWriteDto {
  active: string;
  providers: LlmProviderWriteDto[];
}

export interface AcpSettingsDto {
  command: string;
  cwd: string;
}

export interface AppSettingsDto {
  config_path: string;
  runtime_storage_backend: string;
  runtime_storage_target: string;
  general: GeneralSettingsDto;
  market_data: AdapterPreferenceDto;
  execution: AdapterPreferenceDto;
  risk: RiskSettingsDto;
  storage: StorageSettingsDto;
  appearance: AppearanceSettingsDto;
  llm: LlmSettingsDto;
  acp: AcpSettingsDto;
}

export interface AppSettingsUpdateDto {
  general: GeneralSettingsDto;
  market_data: Pick<AdapterPreferenceDto, 'adapter'>;
  execution: Pick<AdapterPreferenceDto, 'adapter'>;
  risk: RiskSettingsDto;
  storage: Omit<StorageSettingsDto, 'available_backends'>;
  appearance: AppearanceSettingsDto;
  llm: LlmSettingsWriteDto;
  acp: AcpSettingsDto;
}

export interface NewOrderRequestDto {
  symbol: string;
  side: string;
  order_type: string;
  quantity: number;
  price: number | null;
  stop_price: number | null;
  broker_id: string;
  tag: string | null;
  /** Broker margin product (Zerodha: CNC/MIS/NRML); broker default when omitted. */
  product?: string | null;
}

export interface AccountBalanceDto {
  total_value: number;
  cash: number;
  margin_used: number;
  margin_available: number;
  unrealized_pnl: number;
  realized_pnl: number;
  currency: string;
}

export interface StrategyFileDto {
  name: string;
  path: string;
  content: string;
}

export interface StrategyExecutionResultDto {
  success: boolean;
  output: string[];
  error: string | null;
  exit_code: number | null;
  started_at: string;
  finished_at: string;
}

export interface StrategyBacktestRequestDto {
  path: string;
  symbol: string;
  timeframe: string;
  from: string;
  to: string;
  initial_capital?: number;
  commission_bps?: number;
  slippage_bps?: number;
  quantity?: number;
}

export interface BacktestMetricsDto {
  total_return: number;
  total_return_pct: number;
  annualized_return_pct: number;
  sharpe_ratio: number;
  max_drawdown: number;
  max_drawdown_pct: number;
  total_trades: number;
  winning_trades: number;
  losing_trades: number;
  win_rate: number;
  profit_factor: number | null;
  avg_trade_pnl: number;
  avg_win: number;
  avg_loss: number;
  max_consecutive_wins: number;
  max_consecutive_losses: number;
  final_equity: number;
}

export interface BacktestTradeDto {
  symbol: string;
  side: string;
  entry_time: string;
  entry_price: number;
  exit_time: string | null;
  exit_price: number | null;
  quantity: number;
  pnl: number;
  commission: number;
}

export interface EquityPointDto {
  time: string;
  equity: number;
  drawdown: number;
}

export interface StrategyBacktestResultDto {
  strategy_name: string;
  strategy_path: string;
  inferred_strategy: string;
  symbol: string;
  timeframe: string;
  bars_analyzed: number;
  metrics: BacktestMetricsDto;
  trades: BacktestTradeDto[];
  equity_curve: EquityPointDto[];
  notes: string[];
}

export interface NotebookCellDto {
  id: string;
  kind: 'code' | 'markdown';
  content: string;
  output: string | null;
}

export interface NotebookDocumentDto {
  title: string;
  path: string;
  cells: NotebookCellDto[];
  updated_at: string;
}

export interface NotebookSummaryDto {
  name: string;
  path: string;
  updated_at: string;
}

export interface NotebookRunRequestDto {
  path: string;
  cells: NotebookCellDto[];
  cell_id: string;
}

export interface NotebookCellExecutionDto {
  cell_id: string;
  success: boolean;
  stdout: string;
  stderr: string;
  finished_at: string;
}

// ML Workbench types
export interface MLModelDto {
  model_id: string;
  algorithm: string;
  status: 'idle' | 'training' | 'completed' | 'failed';
  metrics: Record<string, number>;
  feature_names: string[];
  created_at: string;
  data_path: string;
  target_column: string;
}

export interface ModelSignalDto {
  model_id: string;
  symbol: string;
  signal: number;
  probability: number | null;
  features_used: string[];
}

export interface AutomationDto {
  id: string;
  name: string;
  kind: string;
  symbol: string;
  model_id: string;
  interval_secs: number;
  quantity: number;
  threshold: number;
  broker_id: string;
  enabled: boolean;
  created_at: string;
  last_run_at: string | null;
  last_result: string | null;
  orders_today: number;
}

export interface CreateAutomationDto {
  name: string;
  kind: string;
  symbol: string;
  model_id: string;
  interval_secs?: number;
  quantity?: number;
  threshold?: number;
  broker_id?: string;
}

export interface MLTrainingRequestDto {
  algorithm: string;
  data_path: string;
  target_column: string;
  feature_columns: string[];
  hyperparams: Record<string, number | string>;
  n_splits: number;
  lag_periods: number[];
}

export interface MLTrainingResultDto {
  model_id: string;
  metrics: Record<string, number>;
  feature_names: string[];
  status: string;
}

// Health Monitor types
export interface AdapterHealthDto {
  adapter_id: string;
  adapter_type: string;
  status: 'healthy' | 'degraded' | 'unhealthy';
  message: string;
  last_check: string;
}

export interface SystemHealthDto {
  adapters: AdapterHealthDto[];
  uptime_secs: number;
  memory_usage_mb: number;
  active_subscriptions: number;
  open_orders: number;
  active_strategies: number;
}

export interface BrokerConnectionDto {
  broker_id: string;
  display_name: string;
  mode: 'paper' | 'live';
  status: 'ready' | 'connected' | 'auth_required' | 'not_configured' | 'degraded' | 'unhealthy' | string;
  configured: boolean;
  authenticated: boolean;
  execution_available: boolean;
  market_data_available: boolean;
  token_field_label: string;
  message: string;
}

// News
export interface NewsItemDto {
  id: string;
  headline: string;
  summary: string;
  source: string;
  url: string;
  published: string;
  symbols: string[];
  sentiment: number | null;
}

export interface NewsFeedDto {
  name: string;
  url: string;
  enabled: boolean;
}

// Order book
export interface OrderBookLevelDto {
  price: number;
  quantity: number;
}

export interface OrderBookDto {
  symbol: string;
  source: 'binance' | 'synthetic' | string;
  bids: OrderBookLevelDto[];
  asks: OrderBookLevelDto[];
}

// Graph
export interface GraphNodeDto {
  id: string;
  node_type: 'Instrument' | 'Sector' | 'MacroVariable' | 'NewsEvent' | 'CustomVariable' | string;
  label: string;
  symbol: string | null;
  properties: Record<string, unknown>;
}

export interface GraphEdgeDto {
  source: string;
  target: string;
  data: {
    edge_type: unknown;
    weight: number;
    metadata: Record<string, unknown>;
  };
}

export interface GraphDto {
  nodes: GraphNodeDto[];
  edges: GraphEdgeDto[];
}

// Scanner
export interface ScanCriterionDto {
  kind: string;
  value?: number;
  value2?: number;
  period?: number;
}

export interface ScanRequestDto {
  name?: string;
  symbols: string[];
  criteria: ScanCriterionDto[];
  timeframe?: string;
  lookback_bars?: number;
}

export interface ScanResultDto {
  symbol: string;
  last_price: number;
  change_pct: number;
  volume: number;
  matched_at: string;
}

export interface ScanOutputDto {
  config_name: string;
  scanned_count: number;
  matched_count: number;
  results: ScanResultDto[];
  completed_at: string;
}

// AI Copilot
export interface CopilotMessageDto {
  role: 'user' | 'assistant';
  content: string;
}

export interface ToolCallTraceDto {
  name: string;
  detail: string;
  ok: boolean;
}

export interface PendingApprovalDto {
  key: string;
  name: string;
  detail: string;
}

export interface CopilotReplyDto {
  reply: string;
  model: string;
  provider: string;
  tool_calls: ToolCallTraceDto[];
  pending_approvals: PendingApprovalDto[];
}

// Quant / indicators (OpenBB-style analytics)
export interface SeriesPointDto {
  time: string;
  value: number;
}

export interface NamedSeriesDto {
  name: string;
  points: SeriesPointDto[];
}

export interface IndicatorResultDto {
  symbol: string;
  indicator: string;
  overlay: boolean;
  series: NamedSeriesDto[];
}

export interface QuantStatsDto {
  symbol: string;
  n: number;
  mean: number;
  std_dev: number;
  variance: number;
  skewness: number;
  kurtosis: number;
  min: number;
  q05: number;
  q25: number;
  median: number;
  q75: number;
  q95: number;
  max: number;
  jarque_bera: number;
  normal: boolean;
  sharpe: number;
  sortino: number;
  omega: number;
  max_drawdown: number;
  ann_volatility: number;
  autocorr: number[];
  rolling_vol: SeriesPointDto[];
  rolling_sharpe: SeriesPointDto[];
}

export interface RegressionDto {
  x_symbol: string;
  y_symbol: string;
  n: number;
  alpha: number;
  beta: number;
  r_squared: number;
  residuals: SeriesPointDto[];
  scatter: [number, number][];
  fit_line: [number, number][];
}

// ── Credential vault & adapter configuration ─────────────────────────

export type VaultStatus = 'uninitialized' | 'locked' | 'unlocked';

export interface VaultStatusDto {
  status: VaultStatus;
}

export type AdapterFieldKind = 'text' | 'secret' | 'toggle';

export interface AdapterFieldDto {
  key: string;
  label: string;
  kind: AdapterFieldKind;
  required: boolean;
  help: string;
}

export interface AdapterSchemaDto {
  adapter_id: string;
  display_name: string;
  /** "broker" | "data" | "llm" */
  category: string;
  description: string;
  fields: AdapterFieldDto[];
}

export interface AdapterConfigDto {
  adapter_id: string;
  enabled: boolean;
  settings: Record<string, string>;
  /** Names of secret fields that have a stored value (values are never returned). */
  secrets_configured: string[];
}

export interface SaveAdapterConfigRequest {
  adapter_id: string;
  enabled: boolean;
  settings: Record<string, string>;
  /** Only secrets being set or replaced; omitted fields keep their stored value. */
  secrets: Record<string, string>;
  clear_secrets: string[];
}

export interface AdapterTestResultDto {
  adapter_id: string;
  ok: boolean;
  message: string;
}

export interface EnvImportResultDto {
  source: string;
  imported: string[];
}

// ── Live trading safeguards ──────────────────────────────────────────

export interface LiveStatusDto {
  armed: boolean;
  orders_today: number;
  max_orders_per_day: number;
  max_qty_per_order: number;
  halted: boolean;
}

export interface OrderPreviewDto {
  broker_id: string;
  live: boolean;
  estimated_value: number | null;
  confirmation_id: string | null;
  expires_in_secs: number;
  warnings: string[];
}

export interface KillSwitchResultDto {
  cancelled: string[];
  failed: string[];
}

export interface SymbolSearchResultDto {
  symbol: string;
  name: string;
  exchange: string;
  /** "zerodha" | "yahoo" | "local" */
  source: string;
}

// ── Strategy sessions ────────────────────────────────────────────────

export interface StrategySessionInfoDto {
  path: string;
  symbol: string;
  timeframe: string;
  broker_id: string;
  quantity: number;
  started_at: string;
  /** "starting" | "running" | "stopped" | "error" */
  state: string;
  held: boolean;
  bars_processed: number;
  orders_placed: number;
  last_error: string | null;
  recent_logs: string[];
}

// ── ChatGPT plan sign-in ─────────────────────────────────────────────

export interface ChatGptStatusDto {
  /** "locked" | "signed_out" | "waiting" | "connected" | "error" */
  state: string;
  email: string | null;
  /** False when signed in but plan usage was not granted. */
  plan_usage: boolean;
  message: string | null;
  /** The copilot is set to run on the ChatGPT plan. */
  use_for_copilot: boolean;
  model: string | null;
}

export interface ChatGptModelDto {
  slug: string;
  name: string;
}

// ── Data feeds per ticker ────────────────────────────────────────────

export interface FeedOptionDto {
  adapter_id: string;
  name: string;
  /** "delayed" | "live" | "exchange" */
  quality: string;
  supports: boolean;
  ready: boolean;
  unavailable_reason: string | null;
}

export interface SymbolFeedDto {
  symbol: string;
  /** The feed currently supplying quotes. */
  active: string | null;
  /** The feed the user pinned (null = automatic). */
  preferred: string | null;
  options: FeedOptionDto[];
}
