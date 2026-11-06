/**
 * Web-mode mock of the ML workbench and activity-log commands.
 *
 * The real implementations live in the Rust backend; this keeps the UI buildable and testable in
 * a plain browser. It also owns the in-memory activity log and the helpers that announce changes
 * (`apex:artifacts-changed`), so the panels refresh exactly as they do on the desktop.
 *
 * Test hook: `window.__apexMock` lets a spec act "as the Copilot" (create a strategy, train a
 * model, add an automation) and see the open panels update.
 */
import type {
  ActivityActor, ActivityDto, ArtifactEventDto, ArtifactKind, BuildDatasetRequestDto, CvFoldDto,
  DatasetInfoDto, DatasetPreviewDto, FeatureDefDto, MLModelDto, MLTrainingRequestDto, ModelDetailDto,
  ModelSignalHistoryDto,
} from './types';

export const ARTIFACT_EVENT = 'apex:artifacts-changed';
export const ACTIVITY_EVENT = 'apex:activity-added';

const activity: ActivityDto[] = [];
let seq = 0;

export function mockActivity(): ActivityDto[] {
  return [...activity];
}

/** Record an activity and announce it, like `activity::record` in the backend. */
export function recordMockActivity(
  actor: ActivityActor, action: string, kind: ActivityDto['kind'], targetId: string,
  summary: string, ok = true, details: string | null = null,
): ActivityDto {
  const entry: ActivityDto = {
    id: `act-${++seq}`, ts: new Date().toISOString(), actor, action, kind,
    target_id: targetId, summary, ok, details,
  };
  activity.unshift(entry);
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent(ACTIVITY_EVENT, { detail: entry }));
    if (ok && (kind === 'strategy' || kind === 'model' || kind === 'automation' || kind === 'dataset')) {
      const event: ArtifactEventDto = { kind: kind as ArtifactKind, id: targetId, action, actor };
      window.dispatchEvent(new CustomEvent(ARTIFACT_EVENT, { detail: event }));
    }
  }
  return entry;
}

export const MOCK_FEATURES: FeatureDefDto[] = [
  { id: 'ret_1', label: '1-bar return', group: 'Price', description: 'Close-to-close return over 1 bar.', warmup_bars: 1 },
  { id: 'ret_5', label: '5-bar return', group: 'Price', description: 'Return over 5 bars.', warmup_bars: 5 },
  { id: 'ret_10', label: '10-bar return', group: 'Price', description: 'Return over 10 bars.', warmup_bars: 10 },
  { id: 'range_pct', label: 'Bar range %', group: 'Price', description: '(high - low) / close.', warmup_bars: 1 },
  { id: 'sma_ratio_20', label: 'Close / SMA 20', group: 'Trend', description: 'Close divided by its 20-bar average.', warmup_bars: 20 },
  { id: 'sma_ratio_50', label: 'Close / SMA 50', group: 'Trend', description: 'Close divided by its 50-bar average.', warmup_bars: 50 },
  { id: 'ema_ratio_12', label: 'Close / EMA 12', group: 'Trend', description: 'Close divided by its 12-bar EMA.', warmup_bars: 12 },
  { id: 'macd_hist', label: 'MACD histogram', group: 'Momentum', description: 'MACD line minus signal line, scaled by close.', warmup_bars: 35 },
  { id: 'rsi_14', label: 'RSI 14', group: 'Momentum', description: 'Relative strength index, 0-100.', warmup_bars: 14 },
  { id: 'roc_10', label: 'Rate of change 10', group: 'Momentum', description: 'Percent change over 10 bars.', warmup_bars: 10 },
  { id: 'vol_10', label: 'Volatility 10', group: 'Volatility', description: 'Std-dev of 1-bar returns over 10 bars.', warmup_bars: 11 },
  { id: 'vol_20', label: 'Volatility 20', group: 'Volatility', description: 'Std-dev of 1-bar returns over 20 bars.', warmup_bars: 21 },
  { id: 'atr_pct_14', label: 'ATR 14 %', group: 'Volatility', description: 'Average true range as a share of close.', warmup_bars: 15 },
  { id: 'bb_pos_20', label: 'Bollinger position', group: 'Volatility', description: 'Where close sits between the 20-bar bands (0-1).', warmup_bars: 20 },
  { id: 'vol_ratio_20', label: 'Volume / avg 20', group: 'Volume', description: 'Volume divided by its 20-bar average.', warmup_bars: 20 },
];

/** Test hook: make the next call of `cmd` fail once (e.g. to exercise optimistic-update rollback). */
let failNext: { cmd: string; message: string } | null = null;
/** Test hook: delay the answer of the next call of `cmd` by `ms` (to exercise out-of-order replies). */
let delayNext: { cmd: string; ms: number } | null = null;

const datasets: DatasetInfoDto[] = [];
const details = new Map<string, ModelDetailDto>();
let trainCounter = 0;

function folds(n: number, base: number): CvFoldDto[] {
  return Array.from({ length: n }, (_, i) => {
    const acc = Math.max(0.35, Math.min(0.8, base + Math.sin(i * 1.7) * 0.05));
    return {
      fold: i + 1, train_rows: 120 + i * 60, test_rows: 60, accuracy: acc, f1: acc - 0.03,
      precision: acc + 0.01, recall: acc - 0.01, baseline_accuracy: 0.5,
      trade_return_pct: Number((Math.sin(i * 2.1) * 3 + 1.2).toFixed(2)),
      hit_rate: Number((acc - 0.02).toFixed(3)), trades: 14 + i,
    };
  });
}

function toSummary(d: ModelDetailDto): MLModelDto {
  return {
    model_id: d.model_id, algorithm: d.algorithm, status: d.status, metrics: d.metrics,
    feature_names: d.feature_names, created_at: d.created_at, data_path: d.data_path, target_column: d.target_column,
  };
}

/** Create a model in the mock registry, as the trainer would. */
export function createMockModel(req: Partial<MLTrainingRequestDto>, actor: ActivityActor = 'user'): ModelDetailDto {
  trainCounter += 1;
  const id = `${req.algorithm ?? 'random_forest'}_20260101T0000${String(trainCounter).padStart(2, '0')}Z`;
  const features = req.feature_columns?.length ? req.feature_columns : ['ret_1', 'rsi_14', 'vol_10'];
  const ds = req.dataset ? datasets.find((d) => d.name === req.dataset) : undefined;
  const acc = 0.5 + ((trainCounter * 7) % 13) / 100;
  const cv = folds(req.n_splits ?? 5, acc);
  const detail: ModelDetailDto = {
    model_id: id, algorithm: req.algorithm ?? 'random_forest', status: 'completed',
    metrics: { accuracy: acc, f1: acc - 0.03, precision: acc + 0.01, recall: acc - 0.01 },
    feature_names: features, created_at: new Date().toISOString(),
    data_path: ds?.path ?? req.data_path ?? 'data/sample.csv', target_column: req.target_column ?? 'target',
    display_name: '', notes: '', tags: [], source: actor, hyperparams: { ...(req.hyperparams ?? {}) },
    n_splits: req.n_splits ?? 5, lag_periods: req.lag_periods ?? [], dataset: ds?.name ?? req.dataset ?? null,
    symbols: ds?.symbols ?? [], timeframe: ds?.timeframe ?? null, label: ds?.label ?? null, rows: ds?.rows ?? 480,
    duration_secs: 1.8, artifact_bytes: 482_113, library_versions: { scikit_learn: '1.9.1', python: '3.13' },
    cv_folds: cv,
    feature_importance: features.map((feature, i) => ({ feature, importance: Number((1 / (i + 2)).toFixed(3)) })),
    trading: { strategy_return_pct: 8.4, buy_hold_return_pct: 5.1, hit_rate: acc - 0.02, trades: 63, max_drawdown_pct: -4.2, sharpe: 1.1 },
    log_tail: `INFO Starting training run: algorithm=${req.algorithm ?? 'random_forest'}\nINFO Fold 1 - accuracy=${acc.toFixed(4)}\nINFO Model saved`,
    used_by: [],
  };
  details.set(id, detail);
  recordMockActivity(actor, 'model.train', 'model', id, `Trained ${detail.algorithm} on ${detail.dataset ?? detail.data_path}`);
  return detail;
}

export function mockUsage(modelId: string, automations: { id: string; name: string; enabled: boolean; symbol: string; model_id: string }[]) {
  return automations.filter((a) => a.model_id === modelId).map((a) => ({ kind: 'automation' as const, id: a.id, name: a.name, enabled: a.enabled, symbol: a.symbol }));
}

export type AutomationLookup = () => { id: string; name: string; enabled: boolean; symbol: string; model_id: string }[];

/** Handle ML-workbench and activity commands. Returns `undefined` when `cmd` is not one of them. */
export function handleMlMock(cmd: string, args: Record<string, unknown> | undefined, automations: AutomationLookup): unknown {
  const result = handleMlMockNow(cmd, args, automations);
  if (result !== undefined && delayNext && delayNext.cmd === cmd) {
    const { ms } = delayNext;
    delayNext = null;
    return new Promise((resolve) => setTimeout(() => resolve(result), ms));
  }
  return result;
}

function handleMlMockNow(cmd: string, args: Record<string, unknown> | undefined, automations: AutomationLookup): unknown {
  if (failNext && failNext.cmd === cmd) {
    const message = failNext.message;
    failNext = null;
    throw new Error(message);
  }
  switch (cmd) {
    case 'predict_model_signal': {
      const id = String(args?.modelId);
      if (!details.has(id)) throw new Error(`Unknown model ${id}`);
      const symbol = String(args?.symbol ?? '');
      if (!symbol.trim()) throw new Error('Symbol is required');
      const p = 0.55 + (symbol.length % 4) * 0.08;
      return { model_id: id, symbol, signal: symbol.length % 2 === 0 ? 1 : -1, probability: Number(p.toFixed(3)), features_used: details.get(id)?.feature_names ?? [] };
    }
    case 'list_activity': {
      const kind = args?.kind as string | undefined;
      const target = args?.targetId as string | undefined;
      const limit = (args?.limit as number | undefined) ?? 200;
      return activity.filter((a) => (!kind || a.kind === kind) && (!target || a.target_id === target)).slice(0, limit);
    }
    case 'list_feature_catalog': return MOCK_FEATURES;
    case 'list_datasets': return [...datasets];
    case 'build_dataset': {
      const r = args?.request as BuildDatasetRequestDto;
      if (!r.features.length) throw new Error('Choose at least one feature');
      if (!r.symbols.length) throw new Error('Choose at least one symbol');
      const rows = Math.max(0, r.bars * r.symbols.length - 60 - r.label.horizon_bars);
      const info: DatasetInfoDto = {
        name: r.name, path: `datasets/${r.name}.csv`, rows, symbols: r.symbols, timeframe: r.timeframe,
        features: r.features, label: r.label,
        class_counts: r.label.kind === 'direction' ? { 0: Math.round(rows * 0.48), 1: Math.round(rows * 0.52) }
          : { '-1': Math.round(rows * 0.2), 0: Math.round(rows * 0.6), 1: Math.round(rows * 0.2) },
        created_at: new Date().toISOString(), first_time: '2024-01-02T00:00:00Z', last_time: '2026-01-02T00:00:00Z', source: 'user',
      };
      mockBuildHook(r, info); // test hook, see the end of this file
      const i = datasets.findIndex((d) => d.name === r.name);
      if (i >= 0) datasets[i] = info; else datasets.push(info);
      recordMockActivity('user', 'dataset.build', 'dataset', r.name, `Built dataset ${r.name} (${rows} rows)`);
      return info;
    }
    case 'preview_dataset': {
      const d = datasets.find((x) => x.name === args?.name);
      if (!d) throw new Error(`Unknown dataset ${String(args?.name)}`);
      const columns = ['time', 'symbol', ...d.features, 'fwd_ret', 'target'];
      const sample = Array.from({ length: 12 }, (_, i) => ['2025-01-0' + ((i % 9) + 1), d.symbols[0], ...d.features.map((_f, j) => Number((Math.sin(i + j) * 0.02).toFixed(4))), 0.004, i % 2]);
      return { ...d, columns, sample } as DatasetPreviewDto;
    }
    case 'delete_dataset': {
      const i = datasets.findIndex((d) => d.name === args?.name);
      if (i >= 0) { datasets.splice(i, 1); recordMockActivity('user', 'dataset.delete', 'dataset', String(args?.name), `Deleted dataset ${String(args?.name)}`); }
      return i >= 0;
    }
    case 'list_ml_models': return [...details.values()].map(toSummary).sort((a, b) => b.created_at.localeCompare(a.created_at));
    case 'train_ml_model': {
      const req = args?.request as MLTrainingRequestDto;
      const run = () => {
        const d = createMockModel(req, 'user');
        return { model_id: d.model_id, metrics: d.metrics, feature_names: d.feature_names, status: 'completed', duration_secs: d.duration_secs };
      };
      const gate = mockTrainGate(req); // test hook, see the end of this file
      return gate ? gate.then(run) : run();
    }
    case 'delete_ml_model': {
      const id = String(args?.modelId);
      const had = details.delete(id);
      if (had) recordMockActivity('user', 'model.delete', 'model', id, `Deleted model ${id}`);
      return had;
    }
    case 'get_ml_model': {
      const d = details.get(String(args?.modelId));
      if (!d) throw new Error(`Unknown model ${String(args?.modelId)}`);
      return { ...d, used_by: mockUsage(d.model_id, automations()) };
    }
    case 'update_ml_model': {
      const d = details.get(String(args?.modelId));
      if (!d) throw new Error(`Unknown model ${String(args?.modelId)}`);
      if (typeof args?.displayName === 'string') d.display_name = args.displayName.trim().slice(0, 80);
      if (typeof args?.notes === 'string') d.notes = args.notes.slice(0, 4000);
      if (Array.isArray(args?.tags)) d.tags = (args.tags as string[]).map((t) => t.trim()).filter(Boolean).slice(0, 12);
      recordMockActivity('user', 'model.update', 'model', d.model_id, `Edited details of ${d.display_name || d.model_id}`);
      return { ...d, used_by: mockUsage(d.model_id, automations()) };
    }
    case 'model_signal_history': {
      const id = String(args?.modelId);
      if (!details.has(id)) throw new Error(`Unknown model ${id}`);
      const n = Math.min(Number(args?.bars ?? 60), 300);
      const points = Array.from({ length: n }, (_, i) => ({
        time: new Date(Date.UTC(2025, 0, 1 + i)).toISOString(),
        close: Number((100 + Math.sin(i / 6) * 5 + i * 0.05).toFixed(2)),
        signal: Math.sin(i / 5) > 0.2 ? 1 : Math.sin(i / 5) < -0.2 ? -1 : 0,
        probability: Number((0.5 + Math.abs(Math.sin(i / 5)) * 0.3).toFixed(3)),
      }));
      return { model_id: id, symbol: String(args?.symbol), points } as ModelSignalHistoryDto;
    }
    default: return undefined;
  }
}

if (typeof window !== 'undefined') {
  (window as unknown as { __apexMock?: unknown }).__apexMock = {
    /** Act as the Copilot training a model. */
    trainModelAsCopilot: (algorithm = 'random_forest') => createMockModel({ algorithm }, 'copilot').model_id,
    /** Train a model, then overwrite fields of its stored detail (metrics, cv_folds, trading, rows ...). */
    trainModelCustom: (patch: Partial<ModelDetailDto> = {}, actor: ActivityActor = 'user') => {
      const d = createMockModel({ algorithm: patch.algorithm }, actor);
      Object.assign(d, patch, { model_id: d.model_id });
      return d.model_id;
    },
    failNext: (cmd: string, message = 'Simulated failure') => { failNext = { cmd, message }; },
    delayNext: (cmd: string, ms = 500) => { delayNext = { cmd, ms }; },
    /** Act as the Copilot editing a model's name, notes or tags. */
    editModelAsCopilot: (modelId: string, patch: Partial<Pick<ModelDetailDto, 'display_name' | 'notes' | 'tags'>>) => {
      const d = details.get(modelId);
      if (!d) throw new Error(`Unknown model ${modelId}`);
      Object.assign(d, patch);
      recordMockActivity('copilot', 'model.update', 'model', modelId, `Edited details of ${d.display_name || modelId}`);
    },
    recordActivity: recordMockActivity,
    activity: mockActivity,
  };
}

// --- ML workbench test hook (agent U2): magic dataset names make training slow or fail --------------
/**
 * A dataset whose name contains "slow" takes about 1.2 s to train; one containing "failxgb" makes
 * xgboost fail (as when the package is missing) while the other algorithms succeed. Returns a
 * promise to wait for, or `undefined` to train immediately.
 */
export function mockTrainGate(req: Partial<MLTrainingRequestDto>): Promise<void> | undefined {
  const name = req.dataset ?? '';
  const failing = name.includes('failxgb') && req.algorithm === 'xgboost';
  const slow = name.includes('slow');
  if (!failing && !slow) return undefined;
  return new Promise<void>((resolve, reject) => {
    setTimeout(() => (failing ? reject(new Error('xgboost is not installed in the Python environment')) : resolve()), slow ? 1200 : 100);
  });
}

/**
 * Dataset names containing "boom" make the build fail (to show backend errors inline); names
 * containing "skew" get a 5% / 90% / 5% class split (to show the class-balance warning).
 */
export function mockBuildHook(r: Pick<BuildDatasetRequestDto, 'name' | 'symbols'>, info: DatasetInfoDto): void {
  if (r.name.includes('boom')) throw new Error(`No market data available for ${r.symbols.join(', ')}`);
  if (r.name.includes('skew')) {
    const n = info.rows;
    info.class_counts = { '-1': Math.round(n * 0.05), '0': n - 2 * Math.round(n * 0.05), '1': Math.round(n * 0.05) };
  }
}
