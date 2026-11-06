import { AdapterManager } from '../settings/AdapterManager';
import { ChatGptCard } from '../settings/ChatGptCard';
import { CopilotTile } from '../copilot/CopilotTile';
import { ActivityPanel } from '../activity/ActivityPanel';
import { ProviderKeyField } from '../settings/ProviderKeyField';
import { SymbolField } from '../common/SymbolField';
import { DataFeedsOverview } from '../settings/DataFeedsOverview';
import { useLiveStore } from '../../stores/liveStore';
import React, { useCallback, useEffect, useState } from 'react';
import { Watchlist } from '../trading/Watchlist';
import { DataBrowser } from '../trading/DataBrowser';
import { OrderEntry } from '../trading/OrderEntry';
import { PositionsPanel } from '../trading/PositionsPanel';
import { CandleChart } from '../charts/CandleChart';
import { MLWorkbench } from '../ml/MLWorkbench';
import { HealthMonitor } from '../monitor/HealthMonitor';
import { AutomationsPanel } from '../monitor/AutomationsPanel';
import { NotebookEditor } from './NotebookEditor';
import { NewsPanel } from '../news/NewsPanel';
import { OrderBlotter } from '../trading/OrderBlotter';
import { OrderBookPanel } from '../charts/OrderBookPanel';
import { GraphPanel } from '../graph/GraphPanel';
import { ScannerPanel } from '../scanner/ScannerPanel';
import { MarketOverview } from '../market/MarketOverview';
import { AnalyticsPanel } from '../quant/AnalyticsPanel';
import { BacktestPanel } from '../strategy/BacktestPanel';
import { ErrorBoundary } from '../common/ErrorBoundary';
import { addAlert, clearBrokerSession, getAlertRules, getAppSettings, listBrokerConnections, removeAlert, saveAppSettings, setBrokerSession, subscribeSymbols, zerodhaLogin } from '../../lib/tauri';
import { applyAppearance } from '../../lib/appearance';
import type { AlertRuleDto, AppSettingsDto, AppSettingsUpdateDto, BrokerConnectionDto, LlmProviderWriteDto } from '../../lib/types';
import { useMarketStore } from '../../stores/marketStore';
import { useBrokerStore } from '../../stores/brokerStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { VALID_TABS, type CenterTab } from './workspaceTabs';
import { TiledWorkspace } from './TiledWorkspace';
import type { PanelId, TileItem } from './panels';

// The locally bundled code editor is sizeable; load it when the strategy
// workspace is first opened while keeping it mounted across tab changes.
const StrategyIDE = React.lazy(() => import('../strategy/StrategyIDE').then((module) => ({ default: module.StrategyIDE })));

type AlertCondition = 'price_above' | 'price_below';

type SettingsTab = 'accounts' | 'data' | 'ai' | 'trading' | 'general';

const SETTINGS_TABS: { id: SettingsTab; label: string; hint: string }[] = [
  { id: 'accounts', label: 'Accounts', hint: 'Brokers, data keys, login' },
  { id: 'data', label: 'Market data', hint: 'Feeds and providers' },
  { id: 'ai', label: 'AI Copilot', hint: 'ChatGPT, API providers' },
  { id: 'trading', label: 'Trading', hint: 'Live safeguards, risk' },
  { id: 'general', label: 'General', hint: 'Look and storage' },
];

const CENTER_TABS: { id: CenterTab; label: string }[] = [
  { id: 'chart', label: 'Chart' },
  { id: 'market', label: 'Market' },
  { id: 'strategy', label: 'Strategy IDE' },
  { id: 'backtest', label: 'Backtest' },
  { id: 'blotter', label: 'Blotter' },
  { id: 'book', label: 'Book' },
  { id: 'news', label: 'News' },
  { id: 'scanner', label: 'Scanner' },
  { id: 'graph', label: 'Graph' },
  { id: 'analytics', label: 'Analytics' },
  { id: 'ml', label: 'ML Workbench' },
  { id: 'data', label: 'Stored Data' },
  { id: 'automations', label: 'Automations' },
  { id: 'notebook', label: 'Notebook' },
  { id: 'activity', label: 'Activity' },
  { id: 'copilot', label: 'Copilot' },
  { id: 'health', label: 'Health' },
];

const SETTINGS_LABELS: Record<string, string> = {
  paper: 'Paper Trading',
  yahoo_finance: 'Yahoo Finance',
  zerodha: 'Zerodha Kite',
  angel_one: 'Angel One',
  groww: 'Groww',
  robinhood: 'Robinhood',
  sqlite: 'SQLite',
  timescale: 'Timescale / Postgres',
};

function buildAlertRulePayload(symbol: string, condition: AlertCondition, value: string) {
  const threshold = Number(value);

  switch (condition) {
    case 'price_below':
      return {
        PriceBelow: {
          symbol,
          threshold,
        },
      };
    case 'price_above':
    default:
      return {
        PriceAbove: {
          symbol,
          threshold,
        },
      };
  }
}

function formatAlertRule(ruleJson: string) {
  try {
    const parsed = JSON.parse(ruleJson) as Record<string, any>;

    if (parsed.PriceAbove) {
      return `${parsed.PriceAbove.symbol} price above ${parsed.PriceAbove.threshold}`;
    }
    if (parsed.PriceBelow) {
      return `${parsed.PriceBelow.symbol} price below ${parsed.PriceBelow.threshold}`;
    }
    if (parsed.VwapCross) {
      return `${parsed.VwapCross.symbol} crossed VWAP`;
    }
    if (parsed.DailyPnl) {
      return `Daily P&L below ${parsed.DailyPnl.threshold}`;
    }
  } catch {
    // Fall back to raw JSON if the payload is malformed.
  }

  return ruleJson;
}

function toSettingsDraft(settings: AppSettingsDto): AppSettingsUpdateDto {
  return {
    general: {
      data_dir: settings.general.data_dir,
    },
    market_data: {
      adapter: settings.market_data.adapter,
    },
    execution: {
      adapter: settings.execution.adapter,
    },
    risk: {
      max_daily_loss: settings.risk.max_daily_loss,
      max_order_value: settings.risk.max_order_value,
    },
    storage: {
      backend: settings.storage.backend,
      sqlite_path: settings.storage.sqlite_path,
      postgres_url: settings.storage.postgres_url,
      wal_mode: settings.storage.wal_mode,
      pool_size: settings.storage.pool_size,
    },
    appearance: {
      theme: settings.appearance.theme,
      density: settings.appearance.density,
    },
    llm: {
      active: settings.llm.active,
      providers: settings.llm.providers.map((p) => ({
        id: p.id,
        name: p.name,
        base_url: p.base_url,
        model: p.model,
        api_kind: p.api_kind,
        api_key_env: p.api_key_env,
        max_tokens: p.max_tokens,
      })),
    },
    acp: {
      command: settings.acp.command,
      cwd: settings.acp.cwd,
    },
  };
}

const EMPTY_PROVIDER: LlmProviderWriteDto = {
  id: '',
  name: '',
  base_url: 'https://',
  model: '',
  api_kind: 'chat',
  api_key_env: '',
  max_tokens: 1024,
};

function formatSettingsLabel(value: string) {
  return SETTINGS_LABELS[value] ?? value.replace(/_/g, ' ').replace(/\b\w/g, (char) => char.toUpperCase());
}

export const Workspace: React.FC = () => {
  const watchlist = useMarketStore((s) => s.watchlist);
  const [selectedSymbol, setSelectedSymbol] = useState(watchlist[0] ?? 'RELIANCE.NS');
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [showLoadDialog, setShowLoadDialog] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>('accounts');
  const liveStatus = useLiveStore((s) => s.status);
  const [layoutName, setLayoutName] = useState('');
  const [saveConfirmation, setSaveConfirmation] = useState(false);
  const [loadConfirmation, setLoadConfirmation] = useState(false);
  const [centerTab, setCenterTab] = useState<CenterTab>('chart');
  const researchView = centerTab === 'strategy' || centerTab === 'backtest';
  const [strategyOpened, setStrategyOpened] = useState(false);
  useEffect(() => { if (centerTab === 'strategy') setStrategyOpened(true); }, [centerTab]);
  const [brokerTokens, setBrokerTokens] = useState<Record<string, string>>({});
  const [brokerActionError, setBrokerActionError] = useState<string | null>(null);
  const [brokerBusyId, setBrokerBusyId] = useState<string | null>(null);
  const [kiteToken, setKiteToken] = useState('');
  const [appSettings, setAppSettings] = useState<AppSettingsDto | null>(null);
  const [settingsDraft, setSettingsDraft] = useState<AppSettingsUpdateDto | null>(null);
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [settingsSuccess, setSettingsSuccess] = useState<string | null>(null);

  const viewMode = useWorkspaceStore((s) => s.viewMode);
  const setViewMode = useWorkspaceStore((s) => s.setViewMode);
  const ensureTile = useWorkspaceStore((s) => s.ensureTile);
  const setTiles = useWorkspaceStore((s) => s.setTiles);
  const saveLayout = useWorkspaceStore((s) => s.saveLayout);
  const loadLayout = useWorkspaceStore((s) => s.loadLayout);
  const layouts = useWorkspaceStore((s) => s.layouts);
  const brokerConnections = useBrokerStore((s) => s.connections);
  const setBrokerConnections = useBrokerStore((s) => s.setConnections);
  const activeBrokerId = useBrokerStore((s) => s.activeBrokerId);
  const setActiveBrokerId = useBrokerStore((s) => s.setActiveBrokerId);

  // In tiles mode a tab opens (or highlights) its panel instead of replacing the view.
  const selectTab = useCallback((tab: CenterTab) => {
    setCenterTab(tab);
    if (useWorkspaceStore.getState().viewMode === 'tiles') ensureTile(tab);
  }, [ensureTile]);

  // React to CommandBar dispatches
  const commandSymbol = useWorkspaceStore((s) => s.commandSymbol);
  const commandTab = useWorkspaceStore((s) => s.commandTab);
  const setCommandSymbol = useWorkspaceStore((s) => s.setCommandSymbol);
  const setCommandTab = useWorkspaceStore((s) => s.setCommandTab);

  useEffect(() => {
    if (commandSymbol) {
      setSelectedSymbol(commandSymbol);
      setCommandSymbol(null);
    }
  }, [commandSymbol, setCommandSymbol]);

  useEffect(() => {
    if (!commandTab) return;
    if ((VALID_TABS as readonly string[]).includes(commandTab)) {
      selectTab(commandTab as CenterTab);
    }
    setCommandTab(null);
  }, [commandTab, selectTab, setCommandTab]);

  const refreshBrokerConnections = useCallback(async () => {
    try {
      const nextConnections = await listBrokerConnections();
      setBrokerConnections(nextConnections);
      setBrokerActionError(null);
    } catch (err) {
      setBrokerActionError(err instanceof Error ? err.message : 'Unable to load broker connections');
    }
  }, [setBrokerConnections]);

  const refreshAppSettings = useCallback(async () => {
    try {
      const nextSettings = await getAppSettings();
      setAppSettings(nextSettings);
      setSettingsDraft(toSettingsDraft(nextSettings));
      setSettingsError(null);
    } catch (err) {
      setSettingsError(err instanceof Error ? err.message : 'Unable to load application settings');
    }
  }, []);

  // Refresh only the saved state (e.g. "key set" badges). Unlike refreshAppSettings this keeps
  // the draft, so saving an API key never throws away unsaved edits or a provider just added.
  const refreshSavedSettings = useCallback(async () => {
    try {
      setAppSettings(await getAppSettings());
    } catch {
      /* the badge simply stays as it was */
    }
  }, []);

  useEffect(() => {
    if (showSettings) {
      void refreshBrokerConnections();
      void refreshAppSettings();
      setSettingsSuccess(null);
    }
  }, [refreshAppSettings, refreshBrokerConnections, showSettings]);

  const handleBrokerTokenChange = useCallback((brokerId: string, token: string) => {
    setBrokerTokens((current) => ({
      ...current,
      [brokerId]: token,
    }));
  }, []);

  const handleConnectBroker = useCallback(async (brokerId: string) => {
    const token = brokerTokens[brokerId]?.trim();

    if (!token) {
      setBrokerActionError('Enter a live session token before connecting a broker.');
      return;
    }

    setBrokerBusyId(brokerId);
    try {
      await setBrokerSession(brokerId, token);
      await refreshBrokerConnections();
      await subscribeSymbols(watchlist);
      setBrokerTokens((current) => ({ ...current, [brokerId]: '' }));
      setBrokerActionError(null);
    } catch (err) {
      setBrokerActionError(err instanceof Error ? err.message : 'Unable to connect broker');
    } finally {
      setBrokerBusyId(null);
    }
  }, [brokerTokens, refreshBrokerConnections, watchlist]);

  const handleDisconnectBroker = useCallback(async (brokerId: string) => {
    setBrokerBusyId(brokerId);
    try {
      await clearBrokerSession(brokerId);
      await refreshBrokerConnections();
      setBrokerActionError(null);
    } catch (err) {
      setBrokerActionError(err instanceof Error ? err.message : 'Unable to disconnect broker');
    } finally {
      setBrokerBusyId(null);
    }
  }, [refreshBrokerConnections]);

  const handleSaveSettings = useCallback(async () => {
    if (!settingsDraft) {
      return;
    }

    setSettingsBusy(true);
    try {
      const nextSettings = await saveAppSettings(settingsDraft);
      setAppSettings(nextSettings);
      setSettingsDraft(toSettingsDraft(nextSettings));
      applyAppearance(nextSettings.appearance.theme, nextSettings.appearance.density);
      setSettingsError(null);
      setSettingsSuccess('Settings saved. Appearance applies now; storage and adapter changes apply on restart.');
    } catch (err) {
      setSettingsError(err instanceof Error ? err.message : 'Unable to save application settings');
      setSettingsSuccess(null);
    } finally {
      setSettingsBusy(false);
    }
  }, [settingsDraft]);

  const executionBrokers = brokerConnections.filter((broker) => broker.mode === 'paper' || broker.execution_available);

  const handleKiteLogin = useCallback(async () => {
    const token = kiteToken.trim();
    if (!token) {
      setBrokerActionError('Paste the request_token from the Kite login redirect URL.');
      return;
    }
    setBrokerBusyId('zerodha');
    try {
      await zerodhaLogin(token);
      await refreshBrokerConnections();
      await subscribeSymbols(watchlist);
      setKiteToken('');
      setBrokerActionError(null);
    } catch (err) {
      setBrokerActionError(err instanceof Error ? err.message : 'Kite login failed');
    } finally {
      setBrokerBusyId(null);
    }
  }, [kiteToken, refreshBrokerConnections, watchlist]);

  const updateLlmProvider = useCallback((id: string, patch: Partial<LlmProviderWriteDto>) => {
    setSettingsDraft((current) => current ? {
      ...current,
      llm: {
        ...current.llm,
        providers: current.llm.providers.map((p) => (p.id === id ? { ...p, ...patch } : p)),
      },
    } : current);
    setSettingsSuccess(null);
  }, []);

  const addLlmProvider = useCallback(() => {
    setSettingsDraft((current) => {
      if (!current) return current;
      const id = `provider-${current.llm.providers.length + 1}`;
      return {
        ...current,
        llm: {
          ...current.llm,
          providers: [...current.llm.providers, { ...EMPTY_PROVIDER, id, name: id }],
        },
      };
    });
    setSettingsSuccess(null);
  }, []);

  const removeLlmProvider = useCallback((id: string) => {
    setSettingsDraft((current) => {
      if (!current) return current;
      const providers = current.llm.providers.filter((p) => p.id !== id);
      return {
        ...current,
        llm: {
          active: current.llm.active === id ? '' : current.llm.active,
          providers,
        },
      };
    });
    setSettingsSuccess(null);
  }, []);

  const handleSaveLayout = () => {
    if (layoutName.trim()) {
      saveLayout(layoutName.trim(), { selectedSymbol, viewMode, tiles: useWorkspaceStore.getState().tiles, timestamp: Date.now() });
      setLayoutName('');
      setShowSaveDialog(false);
      setSaveConfirmation(true);
      setTimeout(() => setSaveConfirmation(false), 3000);
    }
  };

  const handleLoadLayout = (name: string) => {
    const layout = loadLayout(name);
    if (layout?.config?.selectedSymbol) {
      setSelectedSymbol(layout.config.selectedSymbol as string);
    }
    const savedTiles = layout?.config?.tiles;
    if (Array.isArray(savedTiles) && savedTiles.length > 0) {
      setTiles(savedTiles as TileItem[]);
    }
    if (layout?.config?.viewMode === 'tiles' || layout?.config?.viewMode === 'focus') {
      setViewMode(layout.config.viewMode);
    }
    setShowLoadDialog(false);
    setLoadConfirmation(true);
    setTimeout(() => setLoadConfirmation(false), 3000);
  };

  const renderPanel = (id: PanelId): React.ReactNode => {
    switch (id) {
      case 'watchlist': return <Watchlist onSelectSymbol={setSelectedSymbol} selectedSymbol={selectedSymbol} />;
      case 'chart': return <CandleChart symbol={selectedSymbol} />;
      case 'orders': return <OrderEntry defaultSymbol={selectedSymbol} />;
      case 'positions': return <PositionsPanel />;
      case 'alerts': return <div className="h-full overflow-y-auto p-2"><AlertConsole /></div>;
      case 'blotter': return <OrderBlotter />;
      case 'book': return <OrderBookPanel defaultSymbol={selectedSymbol} />;
      case 'news': return <NewsPanel />;
      case 'scanner': return <ScannerPanel />;
      case 'graph': return <GraphPanel />;
      case 'analytics': return <AnalyticsPanel defaultSymbol={selectedSymbol} />;
      case 'backtest': return <BacktestPanel defaultSymbol={selectedSymbol} />;
      case 'market': return <MarketOverview />;
      case 'automations': return <AutomationsPanel />;
      case 'data': return <DataBrowser defaultSymbol={selectedSymbol} />;
      case 'notebook': return <NotebookEditor />;
      case 'ml': return <MLWorkbench />;
      case 'health': return <HealthMonitor />;
      case 'copilot': return <CopilotTile />;
      case 'activity': return <ActivityPanel />;
      case 'strategy':
        return (
          <React.Suspense fallback={<div className="p-4 text-xs text-text-muted">Loading strategy workspace…</div>}>
            <StrategyIDE defaultSymbol={selectedSymbol} />
          </React.Suspense>
        );
      default: return null;
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-[var(--border-color)] bg-surface-1">
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowSaveDialog(true)}
            className="px-3 py-1 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
            data-testid="workspace-save-layout"
          >
            Save Layout
          </button>
          <button
            onClick={() => setShowLoadDialog(true)}
            className="px-3 py-1 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
            data-testid="workspace-load-layout"
          >
            Load Layout
          </button>
          <button
            onClick={() => setShowSettings(true)}
            className="px-3 py-1 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
            data-testid="open-settings"
          >
            Settings
          </button>
        </div>
        {saveConfirmation && (
          <span className="text-xs text-green-500" data-testid="layout-save-confirmation">
            Layout saved successfully
          </span>
        )}
        {loadConfirmation && (
          <span className="text-xs text-green-500" data-testid="layout-load-confirmation">
            Layout loaded successfully
          </span>
        )}
      </div>

      {/* Save Layout Dialog */}
      {showSaveDialog && (
        <div className="absolute inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-surface-1 border border-[var(--border-color)] rounded-lg p-4 w-96">
            <h3 className="text-sm font-medium mb-3">Save Workspace Layout</h3>
            <input
              type="text"
              value={layoutName}
              onChange={(e) => setLayoutName(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleSaveLayout()}
              placeholder="Layout name"
              className="w-full px-3 py-2 text-sm bg-surface-0 border border-[var(--border-color)] rounded mb-3"
              data-testid="layout-name-input"
              autoFocus
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowSaveDialog(false)}
                className="px-3 py-1 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveLayout}
                className="px-3 py-1 text-xs bg-primary-500 hover:bg-primary-600 text-white rounded"
                data-testid="confirm-save-layout"
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Load Layout Dialog */}
      {showLoadDialog && (
        <div className="absolute inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-surface-1 border border-[var(--border-color)] rounded-lg p-4 w-96">
            <h3 className="text-sm font-medium mb-3">Load Workspace Layout</h3>
            <div className="space-y-2 mb-3 max-h-64 overflow-auto">
              {layouts.length === 0 ? (
                <p className="text-sm text-text-muted">No saved layouts</p>
              ) : (
                layouts.map((layout) => (
                  <button
                    key={layout.name}
                    onClick={() => handleLoadLayout(layout.name)}
                    className="w-full px-3 py-2 text-sm text-left bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
                    data-testid={`layout-item-${layout.name}`}
                  >
                    {layout.name}
                  </button>
                ))
              )}
            </div>
            <div className="flex justify-end">
              <button
                onClick={() => setShowLoadDialog(false)}
                className="px-3 py-1 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
              >
                Cancel
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Settings Panel */}
      {showSettings && (
        <div className="absolute inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-surface-1 border border-[var(--border-color)] rounded-lg w-[920px] max-w-[96vw] h-[82vh] flex flex-col" data-testid="settings-panel">
            <div className="flex items-center justify-between border-b border-[var(--border-color)] px-4 py-3">
              <h3 className="text-sm font-medium">Settings</h3>
              <button type="button" onClick={() => setShowSettings(false)} aria-label="Close settings" data-testid="settings-close-x"
                className="px-2 text-text-muted hover:text-text-primary">✕</button>
            </div>
            <div className="flex min-h-0 flex-1">
              <nav aria-label="Settings sections" role="tablist" aria-orientation="vertical" className="w-44 shrink-0 space-y-1 border-r border-[var(--border-color)] p-2">
                {SETTINGS_TABS.map((tab) => (
                  <button
                    key={tab.id}
                    type="button"
                    role="tab"
                    aria-selected={settingsTab === tab.id}
                    onClick={() => setSettingsTab(tab.id)}
                    data-testid={`settings-tab-${tab.id}`}
                    className={`block w-full px-3 py-2 text-left text-xs ${settingsTab === tab.id ? 'bg-accent/15 text-accent shadow-[inset_2px_0_0_0_var(--color-accent)]' : 'text-text-secondary hover:bg-surface-2 hover:text-text-primary'}`}
                  >
                    {tab.label}
                    <span className="block text-[10px] font-normal text-text-muted">{tab.hint}</span>
                  </button>
                ))}
              </nav>
              <div className="min-w-0 flex-1 space-y-4 overflow-y-auto p-4" role="tabpanel" data-testid={`settings-pane-${settingsTab}`}>
                {settingsTab === 'accounts' && (
                  <>
                    <AdapterManager />
                    <details className="border border-[var(--border-color)] bg-surface-0 p-3" data-testid="session-tokens-details">
                      <summary className="cursor-pointer text-xs font-medium text-text-secondary">Session tokens (advanced)</summary>
                      <p className="mt-1 text-[11px] text-text-muted">For brokers that give you a token to paste (Angel One, Groww, Robinhood), or to pick the account new orders use.</p>
                      <div className="mt-3">
              <div data-testid="broker-settings-section">
                <div className="mb-2 flex items-center justify-between">
                  <h4 className="text-xs font-medium text-text-secondary">Session tokens</h4>
                  <button
                    type="button"
                    onClick={() => void refreshBrokerConnections()}
                    className="px-2 py-1 text-[11px] bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
                    data-testid="refresh-broker-connections"
                  >
                    Refresh
                  </button>
                </div>

                <label className="flex items-center justify-between gap-4 text-sm">
                  <span>Active Execution Broker</span>
                  <select
                    value={activeBrokerId}
                    onChange={(e) => setActiveBrokerId(e.target.value)}
                    className="min-w-[220px] px-2 py-1 text-sm bg-surface-0 border border-[var(--border-color)] rounded"
                    data-testid="active-broker-select"
                  >
                    {(executionBrokers.length > 0 ? executionBrokers : [{
                      broker_id: 'paper',
                      display_name: 'Paper Trading',
                      mode: 'paper',
                    } as Pick<BrokerConnectionDto, 'broker_id' | 'display_name' | 'mode'>]).map((broker) => (
                      <option key={broker.broker_id} value={broker.broker_id}>
                        {broker.mode === 'paper' ? 'Paper (Simulated)' : broker.display_name}
                      </option>
                    ))}
                  </select>
                </label>

                {brokerActionError && (
                  <p className="mt-2 text-xs text-bear" data-testid="broker-action-error">
                    {brokerActionError}
                  </p>
                )}

                <div className="mt-3 space-y-3">
                  {brokerConnections.map((broker) => {
                    const statusTone = broker.status === 'connected' || broker.status === 'ready'
                      ? 'border-bull/30 text-bull'
                      : broker.status === 'auth_required' || broker.status === 'not_configured'
                        ? 'border-warning/30 text-warning'
                        : 'border-bear/30 text-bear';

                    return (
                      <div
                        key={broker.broker_id}
                        className="rounded border border-[var(--border-color)] bg-surface-0 p-3"
                        data-testid={`broker-card-${broker.broker_id}`}
                      >
                        <div className="flex items-start justify-between gap-3">
                          <div>
                            <div className="flex items-center gap-2">
                              <span className="text-sm font-medium text-text-primary">{broker.display_name}</span>
                              <span className="rounded bg-surface-2 px-2 py-0.5 text-[10px] uppercase tracking-wide text-text-muted">
                                {broker.mode}
                              </span>
                              {broker.execution_available && (
                                <span className="rounded bg-surface-2 px-2 py-0.5 text-[10px] text-text-muted">Execution</span>
                              )}
                              {broker.market_data_available && (
                                <span className="rounded bg-surface-2 px-2 py-0.5 text-[10px] text-text-muted">Market Data</span>
                              )}
                            </div>
                            <p className="mt-2 text-xs text-text-muted">{broker.message}</p>
                          </div>

                          <span className={`rounded border px-2 py-1 text-[10px] uppercase tracking-wide ${statusTone}`}>
                            {broker.status.replace('_', ' ')}
                          </span>
                        </div>

                        {broker.mode === 'live' && broker.configured ? (
                          <div className="mt-3 flex items-center gap-2">
                            <input
                              type="password"
                              value={brokerTokens[broker.broker_id] ?? ''}
                              onChange={(e) => handleBrokerTokenChange(broker.broker_id, e.target.value)}
                              placeholder={broker.token_field_label || 'Session Token'}
                              className="min-w-0 flex-1 px-3 py-2 text-xs bg-surface-1 border border-[var(--border-color)] rounded"
                              data-testid={`broker-token-${broker.broker_id}`}
                            />
                            <button
                              type="button"
                              onClick={() => void handleConnectBroker(broker.broker_id)}
                              disabled={brokerBusyId === broker.broker_id || !(brokerTokens[broker.broker_id] ?? '').trim()}
                              className="px-3 py-2 text-xs bg-primary-500 hover:bg-primary-600 text-white rounded disabled:opacity-50"
                              data-testid={`broker-connect-${broker.broker_id}`}
                            >
                              {brokerBusyId === broker.broker_id ? 'Working…' : 'Connect'}
                            </button>
                            <button
                              type="button"
                              onClick={() => void handleDisconnectBroker(broker.broker_id)}
                              disabled={brokerBusyId === broker.broker_id || !broker.authenticated}
                              className="px-3 py-2 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded disabled:opacity-50"
                              data-testid={`broker-disconnect-${broker.broker_id}`}
                            >
                              Disconnect
                            </button>
                          </div>
                        ) : broker.mode === 'live' ? (
                          <p className="mt-3 text-xs text-text-muted">
                            Fill the required `.env` values and restart the desktop app to enable this broker.
                          </p>
                        ) : null}

                        {broker.broker_id === 'zerodha' && broker.configured ? (
                          <div className="mt-2 rounded border border-[var(--border-color)] bg-surface-1 px-3 py-2">
                            <p className="text-[11px] text-text-muted">
                              Kite Connect login: open
                              {' '}<code>https://kite.trade/connect/login?api_key=YOUR_KEY</code>{' '}
                              in a browser, sign in, then paste the <code>request_token</code> from the redirect URL here.
                            </p>
                            <div className="mt-2 flex items-center gap-2">
                              <input
                                type="text"
                                value={kiteToken}
                                onChange={(e) => setKiteToken(e.target.value)}
                                placeholder="request_token"
                                className="min-w-0 flex-1 px-3 py-2 text-xs bg-surface-0 border border-[var(--border-color)] rounded font-mono"
                                data-testid="kite-request-token"
                              />
                              <button
                                type="button"
                                onClick={() => void handleKiteLogin()}
                                disabled={brokerBusyId === 'zerodha' || !kiteToken.trim()}
                                className="px-3 py-2 text-xs bg-primary-500 hover:bg-primary-600 text-white rounded disabled:opacity-50"
                                data-testid="kite-login"
                              >
                                {brokerBusyId === 'zerodha' ? 'Working…' : 'Kite Login'}
                              </button>
                            </div>
                          </div>
                        ) : null}
                      </div>
                    );
                  })}
                </div>
              </div>
                      </div>
                    </details>
                  </>
                )}

                {settingsTab === 'data' && (
                  <>
                    <div data-testid="market-data-settings-section" className="space-y-3">
                      <div>
                        <h4 className="text-xs font-medium text-text-secondary">Default data provider</h4>
                        <p className="mt-1 text-[11px] text-text-muted">Used for tickers you haven&apos;t pinned. Pin a specific feed for any ticker from the SRC badge in the Watchlist.</p>
                      </div>
                      {settingsDraft ? (
                        <div className="grid grid-cols-2 gap-3">
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Preferred Market Data Adapter</span>
                          <select
                            value={settingsDraft.market_data.adapter}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                market_data: { adapter: e.target.value },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-market-data-adapter"
                          >
                            {(appSettings?.market_data.available_adapters ?? []).map((adapter) => (
                              <option key={adapter} value={adapter}>
                                {formatSettingsLabel(adapter)}
                              </option>
                            ))}
                          </select>
                        </label>
                        </div>
                      ) : <p className="text-sm text-text-muted">Loading…</p>}
                    </div>
                    <DataFeedsOverview />
                  </>
                )}

                {settingsTab === 'ai' && (
                  <>
                    <ChatGptCard />
              {settingsDraft && (
                <div data-testid="llm-settings-section">
                  <div className="mb-2 flex items-center justify-between">
                    <div>
                      <h4 className="text-xs font-medium text-text-secondary">AI Providers</h4>
                      <p className="mt-0.5 text-[11px] text-text-muted">
                        OpenAI-compatible endpoints for the Copilot. Each provider has its own API key field below; keys are stored encrypted in the credential vault.
                      </p>
                      <p className="mt-0.5 text-[11px] text-warning">
                        Copilot prompts and tool results (quotes, positions, orders) are sent to the active provider's endpoint. Only configure providers you trust with your trading data.
                      </p>
                    </div>
                    <button
                      type="button"
                      onClick={addLlmProvider}
                      className="px-2 py-1 text-[11px] bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
                      data-testid="llm-add-provider"
                    >
                      Add Provider
                    </button>
                  </div>

                  <label className="flex items-center justify-between gap-4 text-sm mb-2">
                    <span>Active Provider</span>
                    <select
                      value={settingsDraft.llm.active}
                      onChange={(e) => {
                        setSettingsDraft((current) => current ? {
                          ...current,
                          llm: { ...current.llm, active: e.target.value },
                        } : current);
                        setSettingsSuccess(null);
                      }}
                      className="min-w-[220px] px-2 py-1 text-sm bg-surface-0 border border-[var(--border-color)] rounded"
                      data-testid="llm-active-provider"
                    >
                      <option value="">Legacy [copilot] config</option>
                      {settingsDraft.llm.providers.map((p) => (
                        <option key={p.id} value={p.id}>{p.name || p.id}</option>
                      ))}
                    </select>
                  </label>

                  <div className="space-y-2">
                    {settingsDraft.llm.providers.map((p) => {
                      const persisted = appSettings?.llm.providers.find((x) => x.id === p.id);
                      return (
                        <div
                          key={p.id}
                          className="rounded border border-[var(--border-color)] bg-surface-0 p-3 space-y-2"
                          data-testid={`llm-provider-${p.id}`}
                        >
                          <div className="flex items-center gap-2">
                            <input
                              value={p.name}
                              onChange={(e) => updateLlmProvider(p.id, { name: e.target.value })}
                              placeholder="Display name"
                              className="flex-1 min-w-0 px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded"
                              data-testid={`llm-name-${p.id}`}
                            />
                            <select
                              value={p.api_kind}
                              onChange={(e) => updateLlmProvider(p.id, { api_kind: e.target.value })}
                              className="px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded"
                              data-testid={`llm-kind-${p.id}`}
                            >
                              <option value="chat">chat/completions</option>
                              <option value="responses">/responses API</option>
                              <option value="acp">ACP agent</option>
                            </select>
                            {persisted && (
                              <span className={`rounded border px-2 py-0.5 text-[10px] uppercase tracking-wide ${persisted.key_configured ? 'border-bull/30 text-bull' : 'border-warning/30 text-warning'}`}>
                                {persisted.key_configured ? 'key set' : 'key missing'}
                              </span>
                            )}
                            <button
                              type="button"
                              onClick={() => removeLlmProvider(p.id)}
                              className="px-2 py-1 text-[11px] text-bear bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded"
                              data-testid={`llm-remove-${p.id}`}
                            >
                              Remove
                            </button>
                          </div>
                          <div className="grid grid-cols-2 gap-2">
                            <input
                              value={p.base_url}
                              onChange={(e) => updateLlmProvider(p.id, { base_url: e.target.value })}
                              placeholder="https://openrouter.ai/api/v1"
                              className="px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded font-mono"
                              data-testid={`llm-base-url-${p.id}`}
                            />
                            <input
                              value={p.model}
                              onChange={(e) => updateLlmProvider(p.id, { model: e.target.value })}
                              placeholder="model slug"
                              className="px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded font-mono"
                              data-testid={`llm-model-${p.id}`}
                            />
                            <ProviderKeyField providerId={p.id} stored={Boolean(persisted?.key_configured)} onChanged={refreshSavedSettings} />
                            <label className="flex items-center gap-2 text-[11px] text-text-muted">Max tokens
                              <input
                              type="number"
                              value={p.max_tokens}
                              min={1}
                              onChange={(e) => updateLlmProvider(p.id, { max_tokens: Math.max(1, Number(e.target.value) || 1024) })}
                              placeholder="max tokens"
                              className="px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded"
                              data-testid={`llm-max-tokens-${p.id}`}
                            />
                            </label>
                          </div>
                        </div>
                      );
                    })}
                    {settingsDraft.llm.providers.length === 0 && (
                      <p className="text-xs text-text-muted" data-testid="llm-no-providers">
                        No providers configured. The copilot uses the legacy [copilot] section with the key saved under Accounts &gt; Copilot (OpenRouter).
                      </p>
                    )}
                  </div>

                  <div className="mt-3 rounded border border-[var(--border-color)] bg-surface-0 p-3" data-testid="acp-settings">
                    <h5 className="text-[11px] font-medium text-text-secondary mb-1">ACP Connector</h5>
                    <p className="text-[11px] text-text-muted mb-2">
                      Route an <code>api_kind: acp</code> provider to an external agent/IDE over stdio JSON-RPC
                      (e.g. <code>npx @zed-industries/claude-code-acp</code> or <code>gemini --acp</code>).
                    </p>
                    <div className="grid grid-cols-[2fr_1fr] gap-2">
                      <input
                        value={settingsDraft.acp.command}
                        onChange={(e) => {
                          setSettingsDraft((current) => current ? {
                            ...current,
                            acp: { ...current.acp, command: e.target.value },
                          } : current);
                          setSettingsSuccess(null);
                        }}
                        placeholder="agent command (empty = disabled)"
                        className="px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded font-mono"
                        data-testid="acp-command"
                      />
                      <input
                        value={settingsDraft.acp.cwd}
                        onChange={(e) => {
                          setSettingsDraft((current) => current ? {
                            ...current,
                            acp: { ...current.acp, cwd: e.target.value },
                          } : current);
                          setSettingsSuccess(null);
                        }}
                        placeholder="working dir (optional)"
                        className="px-2 py-1 text-xs bg-surface-1 border border-[var(--border-color)] rounded font-mono"
                        data-testid="acp-cwd"
                      />
                    </div>
                  </div>
                </div>
              )}
                  </>
                )}

                {settingsTab === 'trading' && (
                  <div data-testid="trading-settings-section" className="space-y-4">
                    <div className="border border-[var(--border-color)] bg-surface-0 p-3" data-testid="live-summary">
                      <h4 className="text-xs font-medium text-text-secondary">Live trading</h4>
                      <p className="mt-1 text-xs text-text-primary">
                        {liveStatus.armed ? 'ARMED: live orders can be sent.' : 'OFF: live orders are blocked.'}
                        {' '}<span className="text-text-muted">{liveStatus.orders_today} of {liveStatus.max_orders_per_day || '--'} orders today, max {liveStatus.max_qty_per_order || '--'} per order.</span>
                      </p>
                      <p className="mt-1 text-[11px] text-text-muted">It starts OFF at every launch. Use <strong>Live armed / Live off</strong> and <strong>Kill</strong> in the status bar. Caps are set under <code>[live]</code> in <code>{appSettings?.config_path ?? 'config/apex.toml'}</code>; every live order also needs your confirmation.</p>
                    </div>
                    {settingsDraft ? (
                      <>
                        <div>
                          <h4 className="mb-2 text-xs font-medium text-text-secondary">Risk limits</h4>
                          <div className="grid grid-cols-2 gap-3">
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Max Daily Loss</span>
                          <input
                            type="number"
                            min={1}
                            value={settingsDraft.risk.max_daily_loss}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                risk: { ...current.risk, max_daily_loss: Number(e.target.value) || 0 },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-max-daily-loss"
                          />
                        </label>
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Max Order Value</span>
                          <input
                            type="number"
                            min={1}
                            value={settingsDraft.risk.max_order_value}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                risk: { ...current.risk, max_order_value: Number(e.target.value) || 0 },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-max-order-value"
                          />
                        </label>
                          </div>
                          <p className="mt-1 text-[11px] text-text-muted">Orders that would breach these are rejected. Trading halts for the day when the daily loss is reached.</p>
                        </div>
                        <div>
                          <h4 className="mb-2 text-xs font-medium text-text-secondary">Default account</h4>
                          <div className="grid grid-cols-2 gap-3">
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Preferred Execution Adapter</span>
                          <select
                            value={settingsDraft.execution.adapter}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                execution: { adapter: e.target.value },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-execution-adapter"
                          >
                            {(appSettings?.execution.available_adapters ?? []).map((adapter) => (
                              <option key={adapter} value={adapter}>
                                {formatSettingsLabel(adapter)}
                              </option>
                            ))}
                          </select>
                        </label>
                          </div>
                        </div>
                      </>
                    ) : <p className="text-sm text-text-muted">Loading…</p>}
                  </div>
                )}

                {settingsTab === 'general' && (
                  <>
              {settingsDraft && (
                <div data-testid="appearance-settings-section">
                  <h4 className="text-xs font-medium text-text-secondary mb-2">Appearance</h4>
                  <div className="grid grid-cols-2 gap-3">
                    <label className="flex flex-col gap-1 text-sm">
                      <span>Theme</span>
                      <select
                        value={settingsDraft.appearance.theme}
                        onChange={(e) => {
                          const theme = e.target.value;
                          setSettingsDraft((current) => current ? {
                            ...current,
                            appearance: { ...current.appearance, theme },
                          } : current);
                          applyAppearance(theme, settingsDraft.appearance.density);
                          setSettingsSuccess(null);
                        }}
                        className="px-2 py-1.5 text-sm bg-surface-0 border border-[var(--border-color)] rounded"
                        data-testid="settings-theme"
                      >
                        <option value="terminal">Terminal (amber)</option>
                        <option value="dark">Dark</option>
                        <option value="light">Light</option>
                      </select>
                    </label>
                    <label className="flex flex-col gap-1 text-sm">
                      <span>Density</span>
                      <select
                        value={settingsDraft.appearance.density}
                        onChange={(e) => {
                          const density = e.target.value;
                          setSettingsDraft((current) => current ? {
                            ...current,
                            appearance: { ...current.appearance, density },
                          } : current);
                          applyAppearance(settingsDraft.appearance.theme, density);
                          setSettingsSuccess(null);
                        }}
                        className="px-2 py-1.5 text-sm bg-surface-0 border border-[var(--border-color)] rounded"
                        data-testid="settings-density"
                      >
                        <option value="comfortable">Comfortable</option>
                        <option value="compact">Compact</option>
                      </select>
                    </label>
                  </div>
                  <p className="mt-1 text-[11px] text-text-muted">Appearance applies immediately and is saved with the rest of your settings.</p>
                </div>
              )}
                    {appSettings?.runtime_storage_notice && (
                      <div role="alert" data-testid="storage-fallback-notice"
                        className="mb-2 border border-[var(--color-warning,#d9a400)] bg-surface-1 p-2 text-xs text-text-primary">
                        {appSettings.runtime_storage_notice}
                      </div>
                    )}
                    <details className="border border-[var(--border-color)] bg-surface-0 p-3" data-testid="runtime-settings-section">
                      <summary className="cursor-pointer text-xs font-medium text-text-secondary">Storage &amp; runtime (advanced)</summary>
                      <div className="mt-2 flex items-start justify-between gap-4">
                        <p className="text-xs text-text-muted">
                          Saved to <code>{appSettings?.config_path ?? 'config/apex.toml'}</code>; applies after restart. Running on{' '}
                          <span className="text-text-primary">{appSettings ? formatSettingsLabel(appSettings.runtime_storage_backend) : '--'}</span>
                          {' '}(<span className="font-mono">{appSettings?.runtime_storage_target ?? '--'}</span>).
                        </p>
                        <button type="button" onClick={() => void refreshAppSettings()} data-testid="refresh-app-settings"
                          className="shrink-0 px-2 py-1 text-[11px] bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded">Refresh</button>
                      </div>
                      {settingsDraft ? (
                        <div className="mt-3 space-y-3">
                          <div className="grid grid-cols-2 gap-3">
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Data Directory</span>
                          <input
                            type="text"
                            value={settingsDraft.general.data_dir}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                general: { ...current.general, data_dir: e.target.value },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-data-dir"
                          />
                        </label>
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Storage Backend</span>
                          <select
                            value={settingsDraft.storage.backend}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                storage: { ...current.storage, backend: e.target.value },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-storage-backend"
                          >
                            {(appSettings?.storage.available_backends ?? ['sqlite', 'timescale']).map((backend) => (
                              <option key={backend} value={backend}>
                                {formatSettingsLabel(backend)}
                              </option>
                            ))}
                          </select>
                        </label>
                          </div>
                          <div className="grid grid-cols-2 gap-3">
                        <label className="flex flex-col gap-1 text-sm">
                          <span>SQLite Path</span>
                          <input
                            type="text"
                            value={settingsDraft.storage.sqlite_path}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                storage: { ...current.storage, sqlite_path: e.target.value },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-sqlite-path"
                          />
                        </label>
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Postgres / Timescale URL</span>
                          <input
                            type="password"
                            value={settingsDraft.storage.postgres_url}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                storage: { ...current.storage, postgres_url: e.target.value },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            placeholder="postgresql://postgres:postgres@localhost:5432/apex_market"
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-postgres-url"
                          />
                        </label>
                          </div>
                          <div className="grid grid-cols-2 gap-3 items-end">
                        <label className="flex flex-col gap-1 text-sm">
                          <span>Storage Pool Size</span>
                          <input
                            type="number"
                            min={1}
                            value={settingsDraft.storage.pool_size}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                storage: { ...current.storage, pool_size: Math.max(1, Number(e.target.value) || 1) },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            className="px-3 py-2 text-sm bg-surface-1 border border-[var(--border-color)] rounded"
                            data-testid="settings-storage-pool-size"
                          />
                        </label>
                        <label className="flex items-center gap-2 text-sm pt-6">
                          <input
                            type="checkbox"
                            checked={settingsDraft.storage.wal_mode}
                            onChange={(e) => {
                              setSettingsDraft((current) => current ? {
                                ...current,
                                storage: { ...current.storage, wal_mode: e.target.checked },
                              } : current);
                              setSettingsSuccess(null);
                            }}
                            data-testid="settings-storage-wal-mode"
                          />
                          <span>Enable SQLite WAL mode</span>
                        </label>
                          </div>
                        </div>
                      ) : <p className="mt-2 text-sm text-text-muted">Loading application settings…</p>}
                    </details>
                  </>
                )}
              </div>
            </div>
            <div className="flex items-center justify-between gap-3 border-t border-[var(--border-color)] px-4 py-3">
              <div className="min-w-0 text-xs">
                {settingsError && <p className="text-bear" data-testid="settings-error">{settingsError}</p>}
                {settingsSuccess && <p className="text-bull" data-testid="settings-success">{settingsSuccess}</p>}
                {!settingsError && !settingsSuccess && (
                  <span className="text-text-muted">
                    Storage and adapter changes apply after restart (running on <strong className="text-text-primary">{appSettings ? formatSettingsLabel(appSettings.runtime_storage_backend) : 'current settings'}</strong>). Accounts and API keys apply immediately.
                  </span>
                )}
              </div>
              <div className="flex shrink-0 gap-2">
                <button
                  type="button"
                  onClick={() => void handleSaveSettings()}
                  disabled={settingsBusy || !settingsDraft}
                  className="px-3 py-1 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded disabled:opacity-50"
                  data-testid="save-app-settings"
                >
                  {settingsBusy ? 'Saving…' : 'Save settings'}
                </button>
                <button
                  onClick={() => setShowSettings(false)}
                  className="px-3 py-1 text-xs bg-primary-500 hover:bg-primary-600 text-white rounded"
                >
                  Close
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      <nav aria-label="Workspace sections" className="flex shrink-0 gap-1 overflow-x-auto whitespace-nowrap border-b border-[var(--border-color)] bg-surface-1 px-2 py-1">
        {CENTER_TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => selectTab(tab.id)}
            aria-current={centerTab === tab.id ? 'page' : undefined}
            data-testid={`tab-${tab.id}`}
            className={`shrink-0 rounded px-3 py-1.5 text-xs transition-colors ${
              centerTab === tab.id
                ? 'bg-accent/15 text-accent shadow-[inset_0_-2px_0_0_var(--color-accent)]'
                : 'text-text-muted hover:bg-surface-2 hover:text-text-primary'
            }`}
          >
            {tab.label}
          </button>
        ))}
        <button
          type="button"
          onClick={() => setCommandTab('copilot')}
          title="Open the Copilot as a tile (or focus it if it is already open)"
          data-testid="copilot-launch"
          className="ml-auto shrink-0 border border-accent/60 bg-accent/10 px-3 py-1.5 text-xs font-medium text-accent hover:bg-accent/20"
        >
          ✦ Copilot
        </button>
        <button
          type="button"
          onClick={() => setViewMode(viewMode === 'tiles' ? 'focus' : 'tiles')}
          aria-pressed={viewMode === 'tiles'}
          title={viewMode === 'tiles' ? 'Switch to single-panel focus view' : 'Switch to tiled multi-panel view'}
          data-testid="view-mode-toggle"
          className="shrink-0 border border-[var(--border-color)] px-3 py-1.5 text-xs text-text-secondary hover:border-accent hover:text-accent"
        >
          {viewMode === 'tiles' ? 'Tiles ▦' : 'Focus ▭'}
        </button>
      </nav>

      {viewMode === 'tiles' && <TiledWorkspace renderPanel={renderPanel} selectedSymbol={selectedSymbol} />}

      {/* Main Workspace Grid (focus mode) */}
      {viewMode === 'focus' && (
      <div className="flex-1 min-h-0 grid grid-cols-12 gap-1 p-1 bg-surface-0" data-testid="workspace-panel">
        {/* Left: Watchlist */}
        <div className={`${researchView ? 'hidden' : centerTab === 'chart' ? 'col-span-2' : 'col-span-3'} bg-surface-1 rounded-lg overflow-hidden border border-[var(--border-color)]`}>
          <Watchlist onSelectSymbol={setSelectedSymbol} selectedSymbol={selectedSymbol} />
        </div>

        {/* Center: chart, research, or other workspace sections */}
        <div className={`${researchView ? 'col-span-12' : centerTab === 'chart' ? 'col-span-7' : 'col-span-6'} flex min-h-0 min-w-0 flex-col gap-1`}>
          <div className={centerTab === 'strategy' ? 'flex flex-1 min-h-0 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden' : 'hidden'}>
            {(strategyOpened || centerTab === 'strategy') && <ErrorBoundary label="Strategy IDE"><React.Suspense fallback={<div className="p-4 text-xs text-text-muted">Loading strategy workspace…</div>}><StrategyIDE defaultSymbol={selectedSymbol} /></React.Suspense></ErrorBoundary>}
          </div>
          <ErrorBoundary key={centerTab} label={CENTER_TABS.find((t) => t.id === centerTab)?.label ?? 'Panel'}>
          {centerTab === 'chart' ? (
            <div className="flex-1 min-h-0 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <CandleChart symbol={selectedSymbol} />
            </div>
          ) : centerTab === 'strategy' ? (
            null
          ) : centerTab === 'ml' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <MLWorkbench />
            </div>
          ) : centerTab === 'blotter' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <OrderBlotter />
            </div>
          ) : centerTab === 'book' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <OrderBookPanel defaultSymbol={selectedSymbol} />
            </div>
          ) : centerTab === 'news' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <NewsPanel />
            </div>
          ) : centerTab === 'scanner' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <ScannerPanel />
            </div>
          ) : centerTab === 'graph' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <GraphPanel />
            </div>
          ) : centerTab === 'analytics' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <AnalyticsPanel defaultSymbol={selectedSymbol} />
            </div>
          ) : centerTab === 'backtest' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <BacktestPanel defaultSymbol={selectedSymbol} />
            </div>
          ) : centerTab === 'market' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <MarketOverview />
            </div>
          ) : centerTab === 'automations' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <AutomationsPanel />
            </div>
          ) : centerTab === 'data' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <DataBrowser defaultSymbol={selectedSymbol} />
            </div>
          ) : centerTab === 'notebook' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <NotebookEditor />
            </div>
          ) : centerTab === 'copilot' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <CopilotTile />
            </div>
          ) : centerTab === 'activity' ? (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <ActivityPanel />
            </div>
          ) : (
            <div className="flex-1 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <HealthMonitor />
            </div>
          )}
          </ErrorBoundary>
        </div>

        {/* Right: order ticket beside the chart, then positions and alerts */}
        <div className={`${researchView ? 'hidden' : 'col-span-3 flex'} min-h-0 flex-col gap-1`}>
          {centerTab === 'chart' && (
            <div className="h-[min(24rem,55%)] shrink-0 bg-surface-1 rounded-lg border border-[var(--border-color)] overflow-hidden">
              <OrderEntry defaultSymbol={selectedSymbol} />
            </div>
          )}
          <div className="flex-1 min-h-0 bg-surface-1 rounded-lg overflow-hidden border border-[var(--border-color)]">
            <PositionsPanel />
          </div>
          <div className={`${centerTab === 'chart' ? 'h-[min(10rem,20%)]' : 'h-52'} shrink-0 bg-surface-1 rounded-lg border border-[var(--border-color)] p-3 overflow-y-auto`}>
            <AlertConsole />
          </div>
        </div>
      </div>
      )}
    </div>
  );
};

const AlertConsole: React.FC = () => {
  const [rules, setRules] = useState<AlertRuleDto[]>([]);
  const [showCreateAlert, setShowCreateAlert] = useState(false);
  const [alertSymbol, setAlertSymbol] = useState('');
  const [alertCondition, setAlertCondition] = useState<AlertCondition>('price_above');
  const [alertValue, setAlertValue] = useState('');
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refreshRules = useCallback(async () => {
    setIsLoading(true);
    try {
      const nextRules = await getAlertRules();
      setRules(Array.isArray(nextRules) ? nextRules : []);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unable to load alerts');
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshRules();
  }, [refreshRules]);

  const handleCreateAlert = useCallback(async () => {
    const symbol = alertSymbol.trim().toUpperCase();

    if (symbol && alertValue) {
      const ruleId = `alert_${Date.now()}`;
      const payload = buildAlertRulePayload(symbol, alertCondition, alertValue);

      try {
        await addAlert(ruleId, JSON.stringify(payload));
        await refreshRules();
        setError(null);
      } catch (err) {
        setError(
          err instanceof Error ? err.message
          : typeof err === 'string' ? err
          : 'Unable to save alert',
        );
        return;
      }

      setAlertSymbol('');
      setAlertValue('');
      setShowCreateAlert(false);
    }
  }, [alertCondition, alertSymbol, alertValue, refreshRules]);

  const handleRemoveAlert = useCallback(async (ruleId: string) => {
    try {
      const removed = await removeAlert(ruleId);
      if (removed) {
        setRules((current) => current.filter((rule) => rule.id !== ruleId));
      }
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unable to remove alert');
    }
  }, []);

  return (
    <div className="h-full flex flex-col">
      <div className="flex items-center justify-between mb-2">
        <span className="text-sm font-medium text-text-secondary">Alerts</span>
        <button
          onClick={() => setShowCreateAlert(!showCreateAlert)}
          className="text-xs px-2 py-0.5 bg-primary-500 hover:bg-primary-600 text-white rounded"
          data-testid="create-alert-button"
        >
          +
        </button>
      </div>

      {showCreateAlert && (
        <div className="mb-2 p-2 border border-[var(--border-color)] rounded space-y-1">
          <SymbolField
            value={alertSymbol}
            onChange={setAlertSymbol}
            placeholder="Symbol"
            className="w-full px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] rounded"
            testId="alert-symbol-input"
          />
          <select
            value={alertCondition}
            onChange={(e) => setAlertCondition(e.target.value as AlertCondition)}
            className="w-full px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] rounded"
            data-testid="alert-condition-select"
          >
            <option value="price_above">Price Above</option>
            <option value="price_below">Price Below</option>
          </select>
          <input
            type="number"
            value={alertValue}
            onChange={(e) => setAlertValue(e.target.value)}
            placeholder="Value"
            className="w-full px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] rounded"
            data-testid="alert-value-input"
          />
          <button
            onClick={handleCreateAlert}
            className="w-full px-2 py-1 text-xs bg-primary-500 hover:bg-primary-600 text-white rounded"
            data-testid="alert-save-button"
          >
            Save
          </button>
        </div>
      )}

      <div className="flex-1 overflow-y-auto text-xs font-mono text-text-muted">
        {error ? (
          <p className="text-bear">{error}</p>
        ) : isLoading ? (
          <p>Loading alerts…</p>
        ) : rules.length === 0 ? (
          <p>No active alerts.</p>
        ) : (
          rules.map((rule) => (
            <div key={rule.id} className="mb-2 flex items-start justify-between gap-2" data-testid="alert-item">
              <div className="min-w-0">
                <div className="truncate">{formatAlertRule(rule.rule)}</div>
                <div className="text-[10px] text-text-muted/70">{rule.id}</div>
              </div>
              <button
                type="button"
                onClick={() => void handleRemoveAlert(rule.id)}
                className="text-[10px] text-bear hover:brightness-125"
                data-testid={`alert-remove-${rule.id}`}
              >
                Remove
              </button>
            </div>
          ))
        )}
      </div>
    </div>
  );
};
