import type { QuoteDto, OrderDto, PositionDto, NewOrderRequestDto, RiskStatusDto, MLModelDto, MLTrainingRequestDto, MLTrainingResultDto, SystemHealthDto, AdapterHealthDto, AlertRuleDto, StrategyFileDto, StrategyExecutionResultDto, StrategyBacktestRequestDto, StrategyBacktestResultDto, AccountBalanceDto, BrokerConnectionDto, AppSettingsDto, AppSettingsUpdateDto, NotebookCellExecutionDto, NotebookDocumentDto, NotebookRunRequestDto, NotebookSummaryDto, OHLCVDto, NewsItemDto, NewsFeedDto, OrderBookDto, GraphDto, ScanOutputDto, ScanRequestDto, CopilotMessageDto, CopilotReplyDto, IndicatorResultDto, QuantStatsDto, RegressionDto, ModelSignalDto, AutomationDto, CreateAutomationDto, StrategySessionInfoDto, ChatGptStatusDto, ChatGptModelDto, SymbolFeedDto, VaultStatusDto, AdapterSchemaDto, AdapterConfigDto, SaveAdapterConfigRequest, AdapterTestResultDto, EnvImportResultDto, LiveStatusDto, OrderPreviewDto, KillSwitchResultDto, SymbolSearchResultDto } from './types';

const IS_TAURI = typeof window !== 'undefined' && '__TAURI__' in window;

const DEFAULT_STRATEGY_TEMPLATE = `"""
APEX Strategy Template
Subclass Strategy and override on_bar / on_tick.
"""
from apex_sdk import Strategy, Bar, Signal, Timeframe


class MyStrategy(Strategy):
    def on_init(self, params: dict) -> None:
        self.subscribe(["RELIANCE.NS"], Timeframe.M5)
        self.log("Strategy initialized")

    def on_stop(self) -> None:
        self.log("Strategy stopped")
`;

const mockPositions: PositionDto[] = [];
const mockOrders: OrderDto[] = [];
let mockModels: MLModelDto[] = [];
let mockAlertRules: AlertRuleDto[] = [];
let mockStrategyFiles: StrategyFileDto[] = [
  {
    name: 'my_strategy.py',
    path: 'strategies/my_strategy.py',
    content: DEFAULT_STRATEGY_TEMPLATE,
  },
];
let mockNotebooks: NotebookDocumentDto[] = [
  {
    title: 'Market Research',
    path: 'notebooks/market-research.apexnb.json',
    updated_at: new Date().toISOString(),
    cells: [
      {
        id: 'nb-cell-1',
        kind: 'markdown',
        content: '# Market Research\n\nUse this notebook for quick experiments and notes.',
        output: null,
      },
      {
        id: 'nb-cell-2',
        kind: 'code',
        content: 'prices = [101.5, 102.2, 104.1]\nprint("Average", sum(prices) / len(prices))',
        output: 'Average 102.6',
      },
    ],
  },
];
let mockTrainingCounter = 0;
let mockAutomations: AutomationDto[] = [];
const mockFeedPrefs = new Map<string, string>();
let mockChatGpt: ChatGptStatusDto = { state: 'signed_out', email: null, plan_usage: false, message: null, use_for_copilot: false, model: null };
// Browser mock: vault starts unlocked so web-mode specs are not gated.
let mockVaultStatus: VaultStatusDto['status'] = 'unlocked';
const mockAdapterConfigs = new Map<string, AdapterConfigDto>();
const mockLive: LiveStatusDto = { armed: false, orders_today: 0, max_orders_per_day: 50, max_qty_per_order: 1000, halted: false };
const mockConfirmations = new Map<string, string>();
const MOCK_MASTER: SymbolSearchResultDto[] = [
  { symbol: 'RELIANCE.NS', name: 'Reliance Industries', exchange: 'NSE', source: 'local' },
  { symbol: 'TCS.NS', name: 'Tata Consultancy Services', exchange: 'NSE', source: 'local' },
  { symbol: 'INFY.NS', name: 'Infosys', exchange: 'NSE', source: 'local' },
  { symbol: 'HDFCBANK.NS', name: 'HDFC Bank', exchange: 'NSE', source: 'local' },
  { symbol: 'SBIN.NS', name: 'State Bank of India', exchange: 'NSE', source: 'local' },
  { symbol: 'AAPL', name: 'Apple Inc.', exchange: 'NASDAQ', source: 'local' },
  { symbol: 'MSFT', name: 'Microsoft Corporation', exchange: 'NASDAQ', source: 'local' },
  { symbol: 'GOOGL', name: 'Alphabet Inc.', exchange: 'NASDAQ', source: 'local' },
  { symbol: 'TSLA', name: 'Tesla, Inc.', exchange: 'NASDAQ', source: 'local' },
  { symbol: 'BTC-USD', name: 'Bitcoin USD', exchange: 'CCC', source: 'local' },
];
const MOCK_ADAPTER_SCHEMAS: AdapterSchemaDto[] = [
  { adapter_id: 'zerodha', display_name: 'Zerodha Kite', category: 'broker', description: 'Indian equities and derivatives. Create a Kite Connect app, then log in daily to get an access token.', fields: [
    { key: 'api_key', label: 'API key', kind: 'secret', required: true, help: 'From the Kite Connect developer console.' },
    { key: 'api_secret', label: 'API secret', kind: 'secret', required: true, help: 'Used once per login to exchange the request token.' },
    { key: 'redirect_url', label: 'Redirect URL', kind: 'text', required: false, help: 'Must match the redirect URL registered on your Kite Connect app.' },
  ] },
  { adapter_id: 'binance', display_name: 'Binance', category: 'broker', description: 'Crypto spot trading and market data.', fields: [
    { key: 'api_key', label: 'API key', kind: 'secret', required: false, help: 'Required for order execution.' },
    { key: 'api_secret', label: 'API secret', kind: 'secret', required: false, help: '' },
    { key: 'testnet', label: 'Use testnet', kind: 'toggle', required: false, help: 'Route orders to the Binance testnet.' },
    { key: 'market_data_feed', label: 'Stream public market data', kind: 'toggle', required: false, help: 'Opens a live Binance price feed for matching watchlist symbols.' },
  ] },
  { adapter_id: 'llm:copilot', display_name: 'Copilot (OpenRouter)', category: 'llm', description: 'API key for this inference provider.', fields: [
    { key: 'api_key', label: 'API key', kind: 'secret', required: false, help: '' },
  ] },
];
let mockBrokerConnections: BrokerConnectionDto[] = [
  {
    broker_id: 'paper',
    display_name: 'Paper Trading',
    mode: 'paper',
    status: 'ready',
    configured: true,
    authenticated: true,
    execution_available: true,
    market_data_available: false,
    token_field_label: '',
    message: 'Paper trading is active for safe simulated execution.',
  },
  {
    broker_id: 'zerodha',
    display_name: 'Zerodha Kite',
    mode: 'live',
    status: 'auth_required',
    configured: true,
    authenticated: false,
    execution_available: true,
    market_data_available: true,
    token_field_label: 'Access Token',
    message: 'Zerodha Kite is configured for live orders, account access, and market data. Paste an Access Token to enable live connectivity.',
  },
  {
    broker_id: 'angel_one',
    display_name: 'Angel One',
    mode: 'live',
    status: 'auth_required',
    configured: true,
    authenticated: false,
    execution_available: true,
    market_data_available: true,
    token_field_label: 'JWT Token',
    message: 'Angel One is configured for live orders, account access, and market data. Paste a JWT Token to enable live connectivity.',
  },
  {
    broker_id: 'groww',
    display_name: 'Groww',
    mode: 'live',
    status: 'auth_required',
    configured: true,
    authenticated: false,
    execution_available: true,
    market_data_available: true,
    token_field_label: 'Access Token',
    message: 'Groww is configured for live orders, account access, and market data. Paste an Access Token to enable live connectivity.',
  },
  {
    broker_id: 'robinhood',
    display_name: 'Robinhood',
    mode: 'live',
    status: 'auth_required',
    configured: true,
    authenticated: false,
    execution_available: true,
    market_data_available: true,
    token_field_label: 'Access Token',
    message: 'Robinhood is configured for live orders, account access, and market data. Paste an Access Token to enable live connectivity.',
  },
];

let mockAppSettings: AppSettingsDto = {
  config_path: 'config/apex.toml',
  runtime_storage_backend: 'sqlite',
  runtime_storage_target: 'data/apex.db',
  general: {
    data_dir: 'data',
  },
  market_data: {
    adapter: 'yahoo_finance',
    available_adapters: ['yahoo_finance', 'zerodha', 'angel_one', 'groww', 'robinhood'],
  },
  execution: {
    adapter: 'paper',
    available_adapters: ['paper', 'zerodha', 'angel_one', 'groww', 'robinhood'],
  },
  risk: {
    max_daily_loss: 50_000,
    max_order_value: 500_000,
  },
  storage: {
    backend: 'sqlite',
    sqlite_path: 'apex.db',
    postgres_url: '',
    wal_mode: true,
    pool_size: 4,
    available_backends: ['sqlite', 'timescale'],
  },
  appearance: { theme: 'terminal', density: 'compact' },
  llm: {
    active: 'openrouter',
    providers: [
      {
        id: 'openrouter',
        name: 'OpenRouter',
        base_url: 'https://openrouter.ai/api/v1',
        model: 'openrouter/free',
        api_kind: 'chat',
        api_key_env: 'OPEN_ROUTER',
        max_tokens: 1024,
        key_configured: false,
      },
    ],
  },
  acp: { command: '', cwd: '' },
};

function cloneMockSettings(): AppSettingsDto {
  return JSON.parse(JSON.stringify(mockAppSettings)) as AppSettingsDto;
}

function buildMockHealthAdapters(): AdapterHealthDto[] {
  const now = new Date().toISOString();
  const adapters: AdapterHealthDto[] = [
    { adapter_id: 'yahoo_finance', adapter_type: 'market_data', status: 'healthy', message: 'Connected', last_check: now },
    { adapter_id: 'paper', adapter_type: 'execution', status: 'healthy', message: 'Active', last_check: now },
  ];

  for (const broker of mockBrokerConnections) {
    if (broker.mode !== 'live' || !broker.configured) {
      continue;
    }

    const status = broker.authenticated ? 'healthy' : 'degraded';
    const message = broker.authenticated ? 'Connected' : 'Awaiting session token';

    if (broker.market_data_available) {
      adapters.push({
        adapter_id: broker.broker_id === 'zerodha' ? 'zerodha_kite' : broker.broker_id,
        adapter_type: 'market_data',
        status,
        message,
        last_check: now,
      });
    }

    if (broker.execution_available) {
      adapters.push({
        adapter_id: broker.broker_id,
        adapter_type: 'execution',
        status,
        message,
        last_check: now,
      });
    }
  }

  return adapters;
}

function buildMockBacktestResult(request: StrategyBacktestRequestDto): StrategyBacktestResultDto {
  const baseEquity = request.initial_capital ?? 100_000;
  const symbol = request.symbol || 'RELIANCE.NS';
  const timeframe = request.timeframe || '1d';
  const strategyName = request.path.split('/').pop() ?? 'strategy.py';
  const now = Date.now();

  return {
    strategy_name: strategyName,
    strategy_path: request.path,
    inferred_strategy: 'Price vs SMA(20)',
    symbol,
    timeframe,
    bars_analyzed: 180,
    metrics: {
      total_return: 1_723.5,
      total_return_pct: 1_723.5 / baseEquity * 100,
      annualized_return_pct: 18.42,
      sharpe_ratio: 1.34,
      max_drawdown: 4_250,
      max_drawdown_pct: 4.25,
      total_trades: 2,
      winning_trades: 2,
      losing_trades: 0,
      win_rate: 100,
      profit_factor: null,
      avg_trade_pnl: 861.75,
      avg_win: 861.75,
      avg_loss: 0,
      max_consecutive_wins: 2,
      max_consecutive_losses: 0,
      final_equity: baseEquity + 1_723.5,
    },
    trades: [
      {
        symbol,
        side: 'Buy',
        entry_time: new Date(now - 1000 * 60 * 60 * 24 * 20).toISOString(),
        entry_price: 2430.25,
        exit_time: null,
        exit_price: null,
        quantity: request.quantity ?? 10,
        pnl: 0,
        commission: 7.3,
      },
      {
        symbol,
        side: 'Sell',
        entry_time: new Date(now - 1000 * 60 * 60 * 24 * 20).toISOString(),
        entry_price: 2430.25,
        exit_time: new Date(now - 1000 * 60 * 60 * 24 * 15).toISOString(),
        exit_price: 2518.4,
        quantity: request.quantity ?? 10,
        pnl: 852.1,
        commission: 17.8,
      },
      {
        symbol,
        side: 'Sell',
        entry_time: new Date(now - 1000 * 60 * 60 * 24 * 10).toISOString(),
        entry_price: 2498.6,
        exit_time: new Date(now - 1000 * 60 * 60 * 24 * 4).toISOString(),
        exit_price: 2587.9,
        quantity: request.quantity ?? 10,
        pnl: 871.4,
        commission: 18.2,
      },
    ],
    equity_curve: Array.from({ length: 12 }, (_, index) => ({
      time: new Date(now - (11 - index) * 1000 * 60 * 60 * 24 * 7).toISOString(),
      equity: baseEquity + index * 1100 + (index % 3 === 0 ? -320 : 0),
      drawdown: index % 3 === 0 ? 0.022 : 0.0,
    })),
    notes: [
      'Browser-mode mock backtest: using deterministic sample historical bars.',
      'In the desktop app, this runs against cached or live Yahoo Finance OHLCV data.',
    ],
  };
}

function buildMockHistoricalData(symbol: string, timeframe = '1d', limit = 100): OHLCVDto[] {
  const now = Date.now();
  const stepMs = timeframe.endsWith('m')
    ? 60_000 * Math.max(1, Number.parseInt(timeframe, 10) || 1)
    : timeframe.endsWith('h')
      ? 60 * 60_000 * Math.max(1, Number.parseInt(timeframe, 10) || 1)
      : 24 * 60 * 60_000;

  return Array.from({ length: Math.min(limit, 250) }, (_, index) => {
    const base = 150 + Math.sin(index / 4) * 4 + index * 0.15;
    const open = Number((base + Math.cos(index) * 0.8).toFixed(2));
    const close = Number((base + Math.sin(index / 2) * 1.1).toFixed(2));
    const high = Number((Math.max(open, close) + 1.25).toFixed(2));
    const low = Number((Math.min(open, close) - 1.15).toFixed(2));
    return {
      time: new Date(now - (Math.min(limit, 250) - index) * stepMs).toISOString(),
      open,
      high,
      low,
      close,
      volume: 1000 + index * 17 + symbol.length * 23,
    };
  });
}

function cloneNotebook(doc: NotebookDocumentDto): NotebookDocumentDto {
  return JSON.parse(JSON.stringify(doc)) as NotebookDocumentDto;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (IS_TAURI) {
    const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
    try {
      return await tauriInvoke<T>(cmd, args);
    } catch (error) {
      // Rust command errors arrive as strings; keep their actionable message.
      throw error instanceof Error ? error : new Error(typeof error === 'string' ? error : String(error));
    }
  }
  
  if (cmd === 'list_strategy_sessions' || cmd === 'list_strategy_session_details') return [] as any;
  if (cmd === 'list_automations') return [...mockAutomations] as any;
  if (cmd === 'create_automation') {
    const req = args?.request as CreateAutomationDto;
    const created: AutomationDto = {
      id: `auto_${mockAutomations.length + 1}`, name: req.name, kind: req.kind, symbol: req.symbol,
      model_id: req.model_id, interval_secs: req.interval_secs ?? 300, quantity: req.quantity ?? 1, threshold: req.threshold ?? 0.5,
      broker_id: req.broker_id ?? 'paper', enabled: true, created_at: new Date().toISOString(), last_run_at: null, last_result: null, orders_today: 0,
    };
    mockAutomations.push(created);
    return created as any;
  }
  if (cmd === 'set_automation_enabled') {
    const rule = mockAutomations.find((a) => a.id === args?.id);
    if (rule) rule.enabled = Boolean(args?.enabled);
    return Boolean(rule) as any;
  }
  if (cmd === 'delete_automation') {
    const before = mockAutomations.length;
    mockAutomations = mockAutomations.filter((a) => a.id !== args?.id);
    return (mockAutomations.length < before) as any;
  }
  if (cmd === 'get_symbol_feeds') {
    const symbols = (args?.symbols as string[] | undefined) ?? [];
    return symbols.map((symbol) => {
      const indian = symbol.endsWith('.NS') || symbol.endsWith('.BO');
      const preferred = mockFeedPrefs.get(symbol) ?? null;
      return {
        symbol,
        active: preferred ?? 'yahoo',
        preferred,
        options: [
          { adapter_id: 'yahoo', name: 'Yahoo Finance', quality: 'delayed', supports: true, ready: true, unavailable_reason: null },
          { adapter_id: 'zerodha_kite', name: 'Zerodha Kite', quality: 'live', supports: indian, ready: false,
            unavailable_reason: 'Not connected. Add or unlock it under Settings > Accounts.' },
        ],
      };
    }) as any;
  }
  if (cmd === 'set_symbol_provider') {
    const symbol = args?.symbol as string;
    if (typeof args?.adapterId === 'string') mockFeedPrefs.set(symbol, args.adapterId);
    else mockFeedPrefs.delete(symbol);
    return undefined as any;
  }
  if (cmd === 'chatgpt_status') return { ...mockChatGpt } as any;
  if (cmd === 'chatgpt_sign_in') {
    mockChatGpt = { ...mockChatGpt, state: 'waiting', message: 'Waiting for you to finish signing in in the browser…' };
    setTimeout(() => { mockChatGpt = { ...mockChatGpt, state: 'connected', email: 'demo@example.com', plan_usage: true, message: null }; }, 700);
    return 'https://auth.openai.com/api/accounts/authorize?client_id=dynamic_agent_client' as any;
  }
  if (cmd === 'chatgpt_sign_out') {
    mockChatGpt = { state: 'signed_out', email: null, plan_usage: false, message: null, use_for_copilot: false, model: null };
    return 'Signed out.' as any;
  }
  if (cmd === 'chatgpt_list_models') return [{ slug: 'gpt-demo', name: 'GPT Demo' }, { slug: 'gpt-demo-mini', name: 'GPT Demo Mini' }] as any;
  if (cmd === 'chatgpt_set_preferences') {
    if (args?.useForCopilot === true && mockChatGpt.state !== 'connected') throw new Error('Sign in with ChatGPT (and allow plan usage) first.');
    mockChatGpt = {
      ...mockChatGpt,
      ...(typeof args?.useForCopilot === 'boolean' ? { use_for_copilot: args.useForCopilot } : {}),
      ...(typeof args?.model === 'string' ? { model: args.model } : {}),
    };
    return { ...mockChatGpt } as any;
  }
  if (cmd === 'open_external_url') return undefined as any;
  if (cmd === 'vault_status') return { status: mockVaultStatus } as any;
  if (cmd === 'vault_init' || cmd === 'vault_unlock') { mockVaultStatus = 'unlocked'; return { status: mockVaultStatus } as any; }
  if (cmd === 'vault_lock') { mockVaultStatus = 'locked'; return { status: mockVaultStatus } as any; }
  if (cmd === 'vault_change_passphrase') return { status: mockVaultStatus } as any;
  if (cmd === 'import_env_credentials') return { source: '.env', imported: [] } as any;
  if (cmd === 'list_adapter_schemas') return MOCK_ADAPTER_SCHEMAS as any;
  if (cmd === 'get_adapter_configs') {
    return MOCK_ADAPTER_SCHEMAS.map((schema) => mockAdapterConfigs.get(schema.adapter_id)
      ?? { adapter_id: schema.adapter_id, enabled: true, settings: {}, secrets_configured: [] }) as any;
  }
  if (cmd === 'save_adapter_config') {
    const request = args?.request as SaveAdapterConfigRequest;
    const previous = mockAdapterConfigs.get(request.adapter_id);
    const secrets = new Set(previous?.secrets_configured ?? []);
    Object.entries(request.secrets).forEach(([key, value]) => { if (value.trim()) secrets.add(key); });
    request.clear_secrets.forEach((key) => secrets.delete(key));
    const saved: AdapterConfigDto = { adapter_id: request.adapter_id, enabled: request.enabled, settings: { ...(previous?.settings ?? {}), ...request.settings }, secrets_configured: [...secrets].sort() };
    mockAdapterConfigs.set(request.adapter_id, saved);
    return saved as any;
  }
  if (cmd === 'set_adapter_enabled') {
    const id = args?.adapterId as string;
    const current = mockAdapterConfigs.get(id) ?? { adapter_id: id, enabled: true, settings: {}, secrets_configured: [] };
    const updated = { ...current, enabled: Boolean(args?.enabled) };
    mockAdapterConfigs.set(id, updated);
    return updated as any;
  }
  if (cmd === 'delete_adapter_config') { mockAdapterConfigs.delete(args?.adapterId as string); return undefined as any; }
  if (cmd === 'test_adapter_connection') {
    const id = args?.adapterId as string;
    const configured = (mockAdapterConfigs.get(id)?.secrets_configured.length ?? 0) > 0;
    return { adapter_id: id, ok: configured, message: configured ? 'Credentials stored (web mock).' : 'No credentials stored.' } as any;
  }
  if (cmd === 'zerodha_login_url') return 'https://kite.zerodha.com/connect/login?v=3&api_key=mock' as any;
  if (cmd === 'get_live_status') return { ...mockLive } as any;
  if (cmd === 'set_live_armed') { mockLive.armed = Boolean(args?.armed); return { ...mockLive } as any; }
  if (cmd === 'kill_switch') { mockLive.armed = false; mockLive.halted = true; return { cancelled: [], failed: [] } as any; }
  if (cmd === 'preview_order') {
    const req = args?.request as NewOrderRequestDto;
    const live = req.broker_id !== 'paper';
    const warnings = live && !mockLive.armed ? ['Live trading is not armed.'] : [];
    const ref = req.price ?? req.stop_price ?? 150.2;
    let confirmation: string | null = null;
    if (live && warnings.length === 0) {
      confirmation = Math.random().toString(36).slice(2);
      mockConfirmations.set(confirmation, req.broker_id);
    }
    return { broker_id: req.broker_id, live, estimated_value: ref * req.quantity, confirmation_id: confirmation, expires_in_secs: live ? 60 : 0, warnings } as any;
  }
  if (cmd === 'search_symbols') {
    const q = String(args?.query ?? '').trim().toUpperCase();
    if (!q) return [] as any;
    return MOCK_MASTER.filter((r) => r.symbol.toUpperCase().includes(q) || r.name.toUpperCase().includes(q)).slice(0, 12) as any;
  }
  if (cmd === 'unsubscribe_symbols') return undefined as any;
  if (cmd === 'get_holdings') return [] as any;
  if (cmd === 'get_quote') {
    const sym = (args as any).symbol;
    return { symbol: sym, bid: 150, ask: 150.5, last: 150.2, open: 149, high: 151, low: 148, volume: 1000, change_pct: 0.5, vwap: 150, updated_at: new Date().toISOString() } as any;
  }
  if (cmd === 'place_order') {
    const req = (args?.request as NewOrderRequestDto);
    if (req.broker_id !== 'paper') {
      const confirmation = args?.confirmationId as string | undefined;
      if (!mockLive.armed) throw new Error('Live trading is not armed. Arm it in the Live panel before sending live orders.');
      if (!confirmation || mockConfirmations.get(confirmation) !== req.broker_id) throw new Error('Order was not confirmed. Review and confirm it first.');
      mockConfirmations.delete(confirmation);
      mockLive.orders_today += 1;
    }
    const isMarket = req.order_type.toLowerCase() === 'market';
    const order: OrderDto = { id: Math.random().toString(), symbol: req.symbol, side: req.side, order_type: req.order_type, quantity: req.quantity, price: req.price, stop_price: req.stop_price, status: isMarket ? 'Filled' : 'Open', filled_qty: isMarket ? req.quantity : 0, avg_price: isMarket ? 150.2 : 0, created_at: new Date().toISOString(), updated_at: new Date().toISOString(), broker_id: req.broker_id, source: 'mock' };
    mockOrders.push(order);
    if (!isMarket) return order.id as any;

    let p = mockPositions.find(x => x.symbol === req.symbol);
    if (!p) {
        p = { symbol: req.symbol, quantity: 0, avg_price: 150.2, side: req.side, pnl: Math.random()*100, pnl_pct: Math.random(), broker_id: req.broker_id };
        mockPositions.push(p);
    }
    p.quantity += (req.side.toUpperCase() === 'BUY' ? Number(req.quantity) : -Number(req.quantity));
    return order.id as any;
  }
  if (cmd === 'get_positions') return [...mockPositions] as any;
  if (cmd === 'get_open_orders') return mockOrders.filter((order) => ['New', 'Pending', 'Open', 'PartiallyFilled'].includes(order.status)) as any;
  if (cmd === 'get_account_balance') {
    const brokerId = (args?.brokerId as string) || 'paper';
    const broker = mockBrokerConnections.find((connection) => connection.broker_id === brokerId);

    if (broker?.mode === 'live' && !broker.authenticated) {
      throw new Error(`${broker.display_name} is not authenticated yet`);
    }

    const balance: AccountBalanceDto = brokerId === 'robinhood'
      ? {
          total_value: 25000,
          cash: 12500,
          margin_used: 0,
          margin_available: 12500,
          unrealized_pnl: 420,
          realized_pnl: 180,
          currency: 'USD',
        }
      : {
          total_value: 1_250_000,
          cash: 845_000,
          margin_used: 120_000,
          margin_available: 725_000,
          unrealized_pnl: 12_400,
          realized_pnl: 3_250,
          currency: 'INR',
        };

    return balance as any;
  }
  if (cmd === 'get_risk_status') return { session_pnl: mockPositions.reduce((a,b)=>a+b.pnl, 0), is_halted: false, max_daily_loss: 50000 } as any;
  if (cmd === 'get_historical_data') {
    const symbol = (args?.symbol as string) || 'RELIANCE.NS';
    const timeframe = args?.timeframe as string | undefined;
    const limit = Number(args?.limit ?? 100);
    return buildMockHistoricalData(symbol, timeframe, limit) as any;
  }
  if (cmd === 'get_app_settings') return cloneMockSettings() as any;
  if (cmd === 'save_app_settings') {
    const request = args?.request as AppSettingsUpdateDto;
    mockAppSettings = {
      ...mockAppSettings,
      general: { ...request.general },
      market_data: {
        adapter: request.market_data.adapter,
        available_adapters: [...mockAppSettings.market_data.available_adapters],
      },
      execution: {
        adapter: request.execution.adapter,
        available_adapters: [...mockAppSettings.execution.available_adapters],
      },
      risk: { ...request.risk },
      storage: {
        ...request.storage,
        available_backends: [...mockAppSettings.storage.available_backends],
      },
    };

    return cloneMockSettings() as any;
  }
  if (cmd === 'list_notebooks') {
    return mockNotebooks.map((notebook) => ({
      name: notebook.title,
      path: notebook.path,
      updated_at: notebook.updated_at,
    })) as any;
  }
  if (cmd === 'load_notebook') {
    const path = args?.path as string;
    const notebook = mockNotebooks.find((entry) => entry.path === path);
    if (!notebook) {
      throw new Error(`Notebook not found: ${path}`);
    }
    return cloneNotebook(notebook) as any;
  }
  if (cmd === 'create_notebook') {
    const rawPath = (args?.path as string | undefined)?.trim() || `notebooks/notebook-${mockNotebooks.length + 1}.apexnb.json`;
    const normalizedPath = rawPath.endsWith('.apexnb.json') ? rawPath : `${rawPath}.apexnb.json`;
    const created: NotebookDocumentDto = {
      title: normalizedPath.split('/').pop()?.replace(/\.apexnb\.json$/, '') || 'Research Notebook',
      path: normalizedPath,
      updated_at: new Date().toISOString(),
      cells: [
        {
          id: `nb-cell-${Date.now()}`,
          kind: 'markdown',
          content: '# Research Notebook',
          output: null,
        },
      ],
    };
    mockNotebooks = [...mockNotebooks.filter((entry) => entry.path !== normalizedPath), created];
    return cloneNotebook(created) as any;
  }
  if (cmd === 'save_notebook') {
    const notebook = args?.notebook as NotebookDocumentDto;
    const saved = {
      ...notebook,
      updated_at: new Date().toISOString(),
    };
    const existingIndex = mockNotebooks.findIndex((entry) => entry.path === saved.path);
    if (existingIndex >= 0) {
      mockNotebooks = mockNotebooks.map((entry, index) => index === existingIndex ? saved : entry);
    } else {
      mockNotebooks = [...mockNotebooks, saved];
    }
    return cloneNotebook(saved) as any;
  }
  if (cmd === 'run_notebook_cell') {
    const request = args?.request as NotebookRunRequestDto;
    const targetCell = request.cells.find((cell) => cell.id === request.cell_id);
    if (!targetCell) {
      throw new Error(`Notebook cell not found: ${request.cell_id}`);
    }
    const sourceCells = request.cells.filter((cell) => cell.kind === 'code');
    const stdout = [`Executed ${targetCell.id}`, `Code cells in scope: ${sourceCells.length}`];
    return {
      cell_id: request.cell_id,
      success: true,
      stdout: stdout.join('\n'),
      stderr: '',
      finished_at: new Date().toISOString(),
    } as NotebookCellExecutionDto as any;
  }
  if (cmd === 'list_strategy_files') return [...mockStrategyFiles] as any;
  if (cmd === 'create_strategy_file') {
    const path = args?.path as string;
    const name = path.split('/').pop() ?? 'strategy.py';
    const content = (args?.content as string | undefined) ?? DEFAULT_STRATEGY_TEMPLATE;
    const created: StrategyFileDto = { name, path, content };
    mockStrategyFiles = [...mockStrategyFiles.filter((file) => file.path !== path), created];
    return created as any;
  }
  if (cmd === 'save_strategy_file') {
    const path = args?.path as string;
    const name = path.split('/').pop() ?? 'strategy.py';
    const content = args?.content as string;
    const saved: StrategyFileDto = { name, path, content };
    const existing = mockStrategyFiles.findIndex((file) => file.path === path);
    if (existing >= 0) {
      mockStrategyFiles = mockStrategyFiles.map((file, index) => index === existing ? saved : file);
    } else {
      mockStrategyFiles = [...mockStrategyFiles, saved];
    }
    return saved as any;
  }
  if (cmd === 'delete_strategy_file') {
    const path = args?.path as string;
    const before = mockStrategyFiles.length;
    mockStrategyFiles = mockStrategyFiles.filter((file) => file.path !== path);
    return (before !== mockStrategyFiles.length) as any;
  }
  if (cmd === 'run_strategy_file') {
    const path = args?.path as string;
    const name = path.split('/').pop() ?? 'strategy.py';
    return {
      success: true,
      output: [
        `Initializing strategy ${name}`,
        'Strategy initialized',
        `Strategy ${name} stopped`,
      ],
      error: null,
      exit_code: 0,
      started_at: new Date().toISOString(),
      finished_at: new Date().toISOString(),
    } as StrategyExecutionResultDto as any;
  }
  if (cmd === 'run_strategy_backtest') {
    return buildMockBacktestResult(args?.request as StrategyBacktestRequestDto) as any;
  }
  if (cmd === 'add_alert') {
    const id = args?.id as string;
    const rule = args?.ruleJson as string;
    mockAlertRules = [
      ...mockAlertRules.filter((existing) => existing.id !== id),
      { id, rule, enabled: true },
    ];
    return undefined as any;
  }
  if (cmd === 'remove_alert') {
    const ruleId = args?.ruleId as string;
    const before = mockAlertRules.length;
    mockAlertRules = mockAlertRules.filter((rule) => rule.id !== ruleId);
    return (mockAlertRules.length < before) as any;
  }
  if (cmd === 'get_alert_rules') return [...mockAlertRules] as any;
  if (cmd === 'list_ml_models') return [...mockModels] as any;
  if (cmd === 'train_ml_model') {
    const req = args?.request as MLTrainingRequestDto;
    mockTrainingCounter++;
    const modelId = `model_${mockTrainingCounter}`;
    const model: MLModelDto = {
      model_id: modelId,
      algorithm: req?.algorithm ?? 'random_forest',
      status: 'completed',
      metrics: { accuracy: 0.72, f1: 0.68, precision: 0.74, recall: 0.65 },
      feature_names: req?.feature_columns ?? ['sma_20', 'rsi_14', 'volume_lag_1'],
      created_at: new Date().toISOString(),
      data_path: req?.data_path ?? 'data/sample.csv',
      target_column: req?.target_column ?? 'signal',
    };
    mockModels.push(model);
    return { model_id: modelId, metrics: model.metrics, feature_names: model.feature_names, status: 'completed' } as any;
  }
  if (cmd === 'delete_ml_model') {
    const id = args?.modelId as string;
    mockModels = mockModels.filter(m => m.model_id !== id);
    return true as any;
  }
  if (cmd === 'list_broker_connections') {
    return [...mockBrokerConnections] as any;
  }
  if (cmd === 'set_broker_session') {
    const brokerId = args?.brokerId as string;
    const sessionToken = (args?.sessionToken as string | undefined)?.trim();

    if (!sessionToken) {
      throw new Error('Session token must not be empty');
    }

    let updated: BrokerConnectionDto | undefined;
    mockBrokerConnections = mockBrokerConnections.map((connection) => {
      if (connection.broker_id !== brokerId) {
        return connection;
      }

      updated = {
        ...connection,
        authenticated: true,
        status: connection.mode === 'paper' ? 'ready' : 'connected',
        message: `${connection.display_name} is authenticated and ready for safe live data and account checks.`,
      };

      return updated;
    });

    if (!updated) {
      throw new Error(`Unknown broker: ${brokerId}`);
    }

    return updated as any;
  }
  if (cmd === 'clear_broker_session') {
    const brokerId = args?.brokerId as string;

    let updated: BrokerConnectionDto | undefined;
    mockBrokerConnections = mockBrokerConnections.map((connection) => {
      if (connection.broker_id !== brokerId) {
        return connection;
      }

      updated = connection.mode === 'paper'
        ? connection
        : {
            ...connection,
            authenticated: false,
            status: connection.configured ? 'auth_required' : 'not_configured',
            message: connection.configured
              ? `${connection.display_name} is configured for live connectivity. Paste a ${connection.token_field_label || 'session token'} to enable it.`
              : connection.message,
          };

      return updated;
    });

    if (!updated) {
      throw new Error(`Unknown broker: ${brokerId}`);
    }

    return updated as any;
  }
  if (cmd === 'get_system_health') {
    return {
      adapters: buildMockHealthAdapters(),
      uptime_secs: Math.floor((Date.now() - performance.timeOrigin) / 1000),
      memory_usage_mb: 128,
      active_subscriptions: 5,
      open_orders: mockOrders.length,
      active_strategies: 0,
    } as any;
  }
  if (cmd === 'get_orders') {
    return [...mockOrders].sort((a, b) => b.created_at.localeCompare(a.created_at)) as any;
  }
  if (cmd === 'get_news' || cmd === 'search_news') {
    return buildMockNews(args?.symbol as string | undefined) as any;
  }
  if (cmd === 'list_news_feeds') {
    return [
      { name: 'CNBC Markets', url: 'https://www.cnbc.com/id/100003114/device/rss/rss.html', enabled: true },
      { name: 'MarketWatch Top Stories', url: 'https://feeds.marketwatch.com/marketwatch/topstories', enabled: true },
    ] as NewsFeedDto[] as any;
  }
  if (cmd === 'get_order_book') {
    return buildMockOrderBook((args?.symbol as string) || 'RELIANCE.NS') as any;
  }
  if (cmd === 'get_graph' || cmd === 'compute_correlations') {
    return buildMockGraph() as any;
  }
  if (cmd === 'run_scan') {
    const request = args?.request as ScanRequestDto | undefined;
    const universe = request?.symbols?.length ? request.symbols : ['RELIANCE.NS', 'TCS.NS', 'HDFCBANK.NS', 'INFY.NS', 'AAPL', 'MSFT', 'GOOGL'];
    const results = universe.map((symbol, i) => ({
      symbol,
      last_price: 140 + i * 37.5,
      change_pct: ((i % 5) - 1) * 1.3,
      volume: 800_000 + i * 120_000,
      matched_at: new Date().toISOString(),
    }));
    return {
      config_name: request?.name ?? 'Ad-hoc scan',
      scanned_count: universe.length,
      matched_count: results.length,
      results,
      completed_at: new Date().toISOString(),
    } as ScanOutputDto as any;
  }
  if (cmd === 'copilot_chat') {
    return {
      reply: '[web-only mode] Copilot needs the desktop app + OPEN_ROUTER key. In-browser responses are stubbed.',
      model: 'mock',
      provider: 'mock',
      tool_calls: [],
      pending_approvals: [],
    } as CopilotReplyDto as any;
  }

  console.log('[Mock IPC]', cmd, args);
  return {} as T;
}

function buildMockNews(symbolFilter?: string): NewsItemDto[] {
  const now = Date.now();
  const feed = [
    { headline: 'Sensex, Nifty hit fresh highs as IT stocks rally', source: 'CNBC Markets', symbols: ['TCS.NS', 'INFY.NS'], sentiment: 0.62 },
    { headline: 'Reliance Industries weighs new energy capex plan', source: 'MarketWatch Top Stories', symbols: ['RELIANCE.NS'], sentiment: 0.41 },
    { headline: 'Fed officials signal patience on rate cuts', source: 'CNBC Markets', symbols: ['AAPL', 'MSFT'], sentiment: -0.18 },
    { headline: 'Bitcoin steadies near highs; altcoins mixed', source: 'CoinDesk', symbols: ['BTC-USD', 'ETH-USD'], sentiment: 0.12 },
    { headline: 'HDFC Bank raises deposit rates amid liquidity push', source: 'MarketWatch Top Stories', symbols: ['HDFCBANK.NS'], sentiment: 0.35 },
    { headline: 'Alphabet unveils new AI accelerator roadmap', source: 'CNBC Markets', symbols: ['GOOGL'], sentiment: 0.55 },
  ];
  return feed
    .map((item, i) => ({
      id: `mock-news-${i}`,
      headline: item.headline,
      summary: `${item.headline} — extended coverage and analysis from ${item.source}.`,
      source: item.source,
      url: '',
      published: new Date(now - i * 11 * 60_000).toISOString(),
      symbols: item.symbols,
      sentiment: item.sentiment,
    }))
    .filter((item) => !symbolFilter || item.symbols.includes(symbolFilter));
}

function buildMockOrderBook(symbol: string): OrderBookDto {
  const mid = 150.0;
  const bids = Array.from({ length: 16 }, (_, i) => ({
    price: Number((mid - 0.02 - i * 0.03).toFixed(2)),
    quantity: 40 + ((i * 73 + symbol.length * 29) % 160),
  }));
  const asks = Array.from({ length: 16 }, (_, i) => ({
    price: Number((mid + 0.02 + i * 0.03).toFixed(2)),
    quantity: 40 + ((i * 97 + symbol.length * 31) % 160),
  }));
  return { symbol, source: 'synthetic', bids, asks };
}

function buildMockGraph(): GraphDto {
  const nodeIds = ['RELIANCE.NS', 'TCS.NS', 'HDFCBANK.NS', 'INFY.NS', 'AAPL', 'MSFT'];
  const nodes = nodeIds.map((id) => ({
    id,
    node_type: 'Instrument',
    label: id,
    symbol: id,
    properties: {},
  }));
  const edges = [
    { source: 'TCS.NS', target: 'INFY.NS', data: { edge_type: { CorrelatedWith: { coefficient: 0.82, window: '90d' } }, weight: 0.82, metadata: {} } },
    { source: 'AAPL', target: 'MSFT', data: { edge_type: { CorrelatedWith: { coefficient: 0.71, window: '90d' } }, weight: 0.71, metadata: {} } },
    { source: 'RELIANCE.NS', target: 'HDFCBANK.NS', data: { edge_type: { CorrelatedWith: { coefficient: 0.44, window: '90d' } }, weight: 0.44, metadata: {} } },
    { source: 'AAPL', target: 'RELIANCE.NS', data: { edge_type: { CorrelatedWith: { coefficient: -0.22, window: '90d' } }, weight: 0.22, metadata: {} } },
  ];
  return { nodes, edges };
}

export async function getQuote(s: string): Promise<QuoteDto> { return invoke<QuoteDto>('get_quote', { symbol: s }); }
export async function subscribeSymbols(s: string[]): Promise<void> { return invoke<void>('subscribe_symbols', { symbols: s }); }
export async function unsubscribeSymbols(s: string[]): Promise<void> { return invoke<void>('unsubscribe_symbols', { symbols: s }); }
export async function searchSymbols(query: string): Promise<SymbolSearchResultDto[]> { return invoke<SymbolSearchResultDto[]>('search_symbols', { query }); }
export async function placeOrder(r: NewOrderRequestDto, confirmationId?: string | null): Promise<string> {
  return invoke<string>('place_order', { request: r, confirmationId: confirmationId ?? null });
}
export async function previewOrder(r: NewOrderRequestDto): Promise<OrderPreviewDto> { return invoke<OrderPreviewDto>('preview_order', { request: r }); }
export async function getHoldings(brokerId: string): Promise<PositionDto[]> { return invoke<PositionDto[]>('get_holdings', { brokerId }); }
export async function cancelOrder(o: string, b: string): Promise<void> { return invoke<void>('cancel_order', { orderId: o, brokerId: b }); }
export async function getPositions(): Promise<PositionDto[]> { return invoke<PositionDto[]>('get_positions'); }
export async function getOpenOrders(): Promise<OrderDto[]> { return invoke<OrderDto[]>('get_open_orders'); }
export async function getAccountBalance(brokerId: string): Promise<AccountBalanceDto> { return invoke<AccountBalanceDto>('get_account_balance', { brokerId }); }
export async function getRiskStatus(): Promise<RiskStatusDto> { return invoke<RiskStatusDto>('get_risk_status'); }
export async function resetHalt(): Promise<void> { return invoke<void>('reset_halt'); }
export async function getHistoricalData(symbol: string, timeframe?: string, limit?: number): Promise<OHLCVDto[]> {
  return invoke<OHLCVDto[]>('get_historical_data', { symbol, timeframe, limit });
}
export async function getAppSettings(): Promise<AppSettingsDto> { return invoke<AppSettingsDto>('get_app_settings'); }
export async function saveAppSettings(request: AppSettingsUpdateDto): Promise<AppSettingsDto> {
  return invoke<AppSettingsDto>('save_app_settings', { request });
}
export async function listNotebooks(): Promise<NotebookSummaryDto[]> { return invoke<NotebookSummaryDto[]>('list_notebooks'); }
export async function loadNotebook(path: string): Promise<NotebookDocumentDto> {
  return invoke<NotebookDocumentDto>('load_notebook', { path });
}
export async function createNotebook(path: string): Promise<NotebookDocumentDto> {
  return invoke<NotebookDocumentDto>('create_notebook', { path });
}
export async function saveNotebook(notebook: NotebookDocumentDto): Promise<NotebookDocumentDto> {
  return invoke<NotebookDocumentDto>('save_notebook', { notebook });
}
export async function runNotebookCell(request: NotebookRunRequestDto): Promise<NotebookCellExecutionDto> {
  return invoke<NotebookCellExecutionDto>('run_notebook_cell', { request });
}
export async function listStrategyFiles(): Promise<StrategyFileDto[]> { return invoke<StrategyFileDto[]>('list_strategy_files'); }
export async function createStrategyFile(path: string, content?: string): Promise<StrategyFileDto> { return invoke<StrategyFileDto>('create_strategy_file', { path, content }); }
export async function saveStrategyFile(path: string, content: string): Promise<StrategyFileDto> { return invoke<StrategyFileDto>('save_strategy_file', { path, content }); }
export async function deleteStrategyFile(path: string): Promise<boolean> { return invoke<boolean>('delete_strategy_file', { path }); }
export async function runStrategyFile(path: string, params?: Record<string, unknown>): Promise<StrategyExecutionResultDto> { return invoke<StrategyExecutionResultDto>('run_strategy_file', { path, paramsJson: JSON.stringify(params ?? {}) }); }
export async function runStrategyBacktest(request: StrategyBacktestRequestDto): Promise<StrategyBacktestResultDto> { return invoke<StrategyBacktestResultDto>('run_strategy_backtest', { request }); }
export async function addAlert(i: string, r: string): Promise<void> { return invoke<void>('add_alert', { id: i, ruleJson: r }); }
export async function removeAlert(r: string): Promise<boolean> { return invoke<boolean>('remove_alert', { ruleId: r }); }
export async function getAlertRules(): Promise<AlertRuleDto[]> { return invoke<AlertRuleDto[]>('get_alert_rules'); }

// ML Workbench
export async function listMLModels(): Promise<MLModelDto[]> { return invoke<MLModelDto[]>('list_ml_models'); }
export async function trainMLModel(r: MLTrainingRequestDto): Promise<MLTrainingResultDto> { return invoke<MLTrainingResultDto>('train_ml_model', { request: r }); }
export async function deleteMLModel(id: string): Promise<boolean> { return invoke<boolean>('delete_ml_model', { modelId: id }); }
export async function predictModelSignal(modelId: string, symbol: string): Promise<ModelSignalDto> { return invoke<ModelSignalDto>('predict_model_signal', { modelId, symbol }); }

// Automations — persisted scheduled model-signal trading rules
export async function listAutomations(): Promise<AutomationDto[]> { return invoke<AutomationDto[]>('list_automations'); }
export async function createAutomation(request: CreateAutomationDto): Promise<AutomationDto> { return invoke<AutomationDto>('create_automation', { request }); }
export async function deleteAutomation(id: string): Promise<boolean> { return invoke<boolean>('delete_automation', { id }); }
export async function setAutomationEnabled(id: string, enabled: boolean): Promise<boolean> { return invoke<boolean>('set_automation_enabled', { id, enabled }); }

// Broker connectivity
export async function listBrokerConnections(): Promise<BrokerConnectionDto[]> { return invoke<BrokerConnectionDto[]>('list_broker_connections'); }
export async function setBrokerSession(brokerId: string, sessionToken: string): Promise<BrokerConnectionDto> { return invoke<BrokerConnectionDto>('set_broker_session', { brokerId, sessionToken }); }
export async function clearBrokerSession(brokerId: string): Promise<BrokerConnectionDto> { return invoke<BrokerConnectionDto>('clear_broker_session', { brokerId }); }
export async function zerodhaLogin(requestToken: string): Promise<BrokerConnectionDto> { return invoke<BrokerConnectionDto>('zerodha_login', { requestToken }); }

// Health Monitor
export async function getSystemHealth(): Promise<SystemHealthDto> { return invoke<SystemHealthDto>('get_system_health'); }

// Orders — blotter
export async function getOrders(symbol?: string, limit?: number): Promise<OrderDto[]> {
  return invoke<OrderDto[]>('get_orders', { symbol, limit });
}

// News
export async function getNews(limit?: number, symbol?: string): Promise<NewsItemDto[]> {
  return invoke<NewsItemDto[]>('get_news', { limit, symbol });
}
export async function searchNews(query: string, symbol?: string, limit?: number): Promise<NewsItemDto[]> {
  return invoke<NewsItemDto[]>('search_news', { query, symbol, limit });
}
export async function listNewsFeeds(): Promise<NewsFeedDto[]> { return invoke<NewsFeedDto[]>('list_news_feeds'); }

// Order book
export async function getOrderBook(symbol: string): Promise<OrderBookDto> {
  return invoke<OrderBookDto>('get_order_book', { symbol });
}

// Relationship graph
export async function getGraph(): Promise<GraphDto> { return invoke<GraphDto>('get_graph'); }
export async function computeCorrelations(symbols: string[], windowDays?: number): Promise<GraphDto> {
  return invoke<GraphDto>('compute_correlations', { symbols, windowDays });
}

// Market scanner
export async function runScan(request: ScanRequestDto): Promise<ScanOutputDto> {
  return invoke<ScanOutputDto>('run_scan', { request });
}

// AI Copilot (OpenRouter — desktop only)
/** `turnId` identifies one user turn; re-send the same id when replaying after an approval. */
export async function copilotChat(message: string, history?: CopilotMessageDto[], turnId?: string): Promise<CopilotReplyDto> {
  return invoke<CopilotReplyDto>('copilot_chat', { message, history, turnId });
}

export async function approveCopilotTools(keys: string[]): Promise<number> {
  return invoke<number>('approve_copilot_tools', { keys });
}

// Quant analytics (OpenBB-style)
export async function computeIndicator(
  symbol: string,
  indicator: string,
  timeframe?: string,
  params?: Record<string, number>,
): Promise<IndicatorResultDto> {
  return invoke<IndicatorResultDto>('compute_indicator', { symbol, indicator, timeframe, params });
}
export async function getQuantStats(symbol: string, timeframe?: string, window?: number): Promise<QuantStatsDto> {
  return invoke<QuantStatsDto>('get_quant_stats', { symbol, timeframe, window });
}
export async function getRegression(xSymbol: string, ySymbol: string, timeframe?: string): Promise<RegressionDto> {
  return invoke<RegressionDto>('get_regression', { xSymbol, ySymbol, timeframe });
}

export async function stopStrategyFile(path: string): Promise<void> { return invoke<void>('stop_strategy_file', { path }); }

export async function listStrategySessions(): Promise<string[]> { return invoke<string[]>('list_strategy_sessions'); }
export async function listStrategySessionDetails(): Promise<StrategySessionInfoDto[]> { return invoke<StrategySessionInfoDto[]>('list_strategy_session_details'); }

// Credential vault
export async function vaultStatus(): Promise<VaultStatusDto> { return invoke<VaultStatusDto>('vault_status'); }
export async function vaultInit(passphrase: string): Promise<VaultStatusDto> { return invoke<VaultStatusDto>('vault_init', { passphrase }); }
export async function vaultUnlock(passphrase: string): Promise<VaultStatusDto> { return invoke<VaultStatusDto>('vault_unlock', { passphrase }); }
export async function vaultLock(): Promise<VaultStatusDto> { return invoke<VaultStatusDto>('vault_lock'); }
export async function vaultChangePassphrase(oldPassphrase: string, newPassphrase: string): Promise<VaultStatusDto> {
  return invoke<VaultStatusDto>('vault_change_passphrase', { oldPassphrase, newPassphrase });
}
export async function importEnvCredentials(): Promise<EnvImportResultDto> { return invoke<EnvImportResultDto>('import_env_credentials'); }

// Adapter configuration
export async function listAdapterSchemas(): Promise<AdapterSchemaDto[]> { return invoke<AdapterSchemaDto[]>('list_adapter_schemas'); }
export async function getAdapterConfigs(): Promise<AdapterConfigDto[]> { return invoke<AdapterConfigDto[]>('get_adapter_configs'); }
export async function saveAdapterConfig(request: SaveAdapterConfigRequest): Promise<AdapterConfigDto> {
  return invoke<AdapterConfigDto>('save_adapter_config', { request });
}
export async function setAdapterEnabled(adapterId: string, enabled: boolean): Promise<AdapterConfigDto> {
  return invoke<AdapterConfigDto>('set_adapter_enabled', { adapterId, enabled });
}
export async function deleteAdapterConfig(adapterId: string): Promise<void> { return invoke<void>('delete_adapter_config', { adapterId }); }
export async function testAdapterConnection(adapterId: string): Promise<AdapterTestResultDto> {
  return invoke<AdapterTestResultDto>('test_adapter_connection', { adapterId });
}
export async function zerodhaLoginUrl(): Promise<string> { return invoke<string>('zerodha_login_url'); }

// Live trading safeguards
export async function getLiveStatus(): Promise<LiveStatusDto> { return invoke<LiveStatusDto>('get_live_status'); }
export async function setLiveArmed(armed: boolean): Promise<LiveStatusDto> { return invoke<LiveStatusDto>('set_live_armed', { armed }); }
export async function killSwitch(): Promise<KillSwitchResultDto> { return invoke<KillSwitchResultDto>('kill_switch'); }

// ChatGPT plan sign-in (Copilot)
export async function chatgptStatus(): Promise<ChatGptStatusDto> { return invoke<ChatGptStatusDto>('chatgpt_status'); }
export async function chatgptSignIn(): Promise<string> { return invoke<string>('chatgpt_sign_in'); }
export async function chatgptSignOut(): Promise<string> { return invoke<string>('chatgpt_sign_out'); }
export async function chatgptListModels(): Promise<ChatGptModelDto[]> { return invoke<ChatGptModelDto[]>('chatgpt_list_models'); }
export async function chatgptSetPreferences(useForCopilot?: boolean, model?: string): Promise<ChatGptStatusDto> {
  return invoke<ChatGptStatusDto>('chatgpt_set_preferences', { useForCopilot, model });
}
export async function openExternalUrl(url: string): Promise<void> {
  if (!IS_TAURI) { window.open(url, '_blank', 'noopener'); return; }
  return invoke<void>('open_external_url', { url });
}

// Per-ticker data feed choice
export async function getSymbolFeeds(symbols: string[]): Promise<SymbolFeedDto[]> { return invoke<SymbolFeedDto[]>('get_symbol_feeds', { symbols }); }
export async function setSymbolProvider(symbol: string, adapterId: string | null): Promise<void> {
  return invoke<void>('set_symbol_provider', { symbol, adapterId });
}
