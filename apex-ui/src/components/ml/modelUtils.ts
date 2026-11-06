import type { ActivityActor, CvFoldDto, LabelSpecDto, MLModelDto, ModelDetailDto } from '../../lib/types';

/** Pure helpers shared by the model registry, detail and compare views. */

export type SourceKind = ActivityActor | 'unknown';

export const SOURCE_LABEL: Record<SourceKind, string> = {
  user: 'You',
  copilot: 'Copilot',
  automation: 'Automation',
  system: 'System',
  unknown: 'Unknown',
};

export const ALGORITHM_LABEL: Record<string, string> = {
  random_forest: 'Random Forest',
  gradient_boosting: 'Gradient Boosting',
  logistic_regression: 'Logistic Regression',
  xgboost: 'XGBoost',
};

export const algorithmLabel = (algorithm: string): string => ALGORITHM_LABEL[algorithm] ?? algorithm;

/** Library defaults, used to show which hyperparameters a run changed. */
export const DEFAULT_HYPERPARAMS: Record<string, Record<string, unknown>> = {
  random_forest: { n_estimators: 100, max_depth: null, min_samples_split: 2, min_samples_leaf: 1, max_features: 'sqrt' },
  gradient_boosting: { n_estimators: 100, learning_rate: 0.1, max_depth: 3, subsample: 1, min_samples_leaf: 1 },
  logistic_regression: { C: 1, max_iter: 100, penalty: 'l2', solver: 'lbfgs' },
  xgboost: { n_estimators: 100, max_depth: 6, learning_rate: 0.3, subsample: 1, colsample_bytree: 1 },
};

export const displayName = (m: { display_name?: string; model_id: string }): string =>
  m.display_name && m.display_name.trim() ? m.display_name.trim() : m.model_id;

export function fmtValue(v: unknown): string {
  if (v === null || v === undefined) return 'none';
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}

export function pct(v: number | null | undefined, digits = 1): string {
  return v === null || v === undefined || Number.isNaN(v) ? 'n/a' : `${(v * 100).toFixed(digits)}%`;
}

export function signedPct(v: number | null | undefined, digits = 1): string {
  if (v === null || v === undefined || Number.isNaN(v)) return 'n/a';
  return `${v >= 0 ? '+' : ''}${v.toFixed(digits)}%`;
}

export function num(v: number | null | undefined, digits = 2): string {
  return v === null || v === undefined || Number.isNaN(v) ? 'n/a' : v.toFixed(digits);
}

export function relativeTime(iso: string, now: number = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const secs = Math.round((now - t) / 1000);
  if (secs < 45) return 'just now';
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  if (days < 60) return `${days} d ago`;
  return new Date(t).toLocaleDateString();
}

export function exactTime(iso: string): string {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? iso : new Date(t).toLocaleString();
}

export function bytesLabel(n: number | null | undefined): string {
  if (n === null || n === undefined) return 'n/a';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function labelText(label: LabelSpecDto | null): string {
  if (!label) return 'n/a';
  return label.kind === 'direction'
    ? `Up or down after ${label.horizon_bars} bar${label.horizon_bars === 1 ? '' : 's'}`
    : `Move beyond ±${label.threshold_pct}% after ${label.horizon_bars} bar${label.horizon_bars === 1 ? '' : 's'}`;
}

/** Mean majority-class accuracy over the folds that recorded one. */
export function baselineAccuracy(folds: CvFoldDto[]): number | null {
  const values = folds.map((f) => f.baseline_accuracy).filter((v): v is number => typeof v === 'number');
  if (values.length === 0) return null;
  return values.reduce((a, b) => a + b, 0) / values.length;
}

export type VerdictTone = 'good' | 'warn' | 'bad' | 'info';

export interface Verdict {
  tone: VerdictTone;
  text: string;
  /** Percentage points above (+) or below (-) the majority-class baseline, when known. */
  edgePts: number | null;
  warnings: { id: 'trades' | 'rows' | 'legacy'; text: string }[];
}

export const MIN_TRADES = 30;
export const MIN_ROWS = 300;

type VerdictInput = Pick<ModelDetailDto, 'metrics' | 'cv_folds' | 'trading' | 'rows'>;

/** Plain-language read of a run: did it beat the naive baseline, and is there enough data to trust it. */
export function modelVerdict(m: VerdictInput): Verdict {
  const accuracy = m.metrics.accuracy;
  const baseline = baselineAccuracy(m.cv_folds);
  const warnings: Verdict['warnings'] = [];
  if (m.trading && m.trading.trades < MIN_TRADES) {
    warnings.push({ id: 'trades', text: `Only ${m.trading.trades} trades in the test windows (fewer than ${MIN_TRADES}). Trading results are too thin to trust.` });
  }
  if (m.rows !== null && m.rows !== undefined && m.rows < MIN_ROWS) {
    warnings.push({ id: 'rows', text: `Only ${m.rows} rows of data (fewer than ${MIN_ROWS}). Metrics may not hold on new data.` });
  }

  let tone: VerdictTone;
  let text: string;
  let edgePts: number | null = null;
  if (typeof accuracy !== 'number') {
    tone = 'info';
    text = 'No accuracy was recorded for this run.';
  } else if (baseline === null) {
    tone = 'info';
    text = `Accuracy ${pct(accuracy)}. There is no majority-class baseline for this run, so it cannot be judged against a naive guess.`;
  } else {
    edgePts = (accuracy - baseline) * 100;
    const pts = Math.abs(edgePts).toFixed(1);
    if (edgePts >= 1) {
      tone = 'good';
      text = `Beats always predicting the majority class by ${pts} pts (${pct(accuracy)} vs ${pct(baseline)}).`;
    } else if (edgePts > -1) {
      tone = 'warn';
      text = `No better than always predicting the majority class (${pct(accuracy)} vs ${pct(baseline)}). It has not found a usable edge.`;
    } else {
      tone = 'bad';
      text = `Worse than always predicting the majority class by ${pts} pts (${pct(accuracy)} vs ${pct(baseline)}).`;
    }
  }
  if (m.trading) {
    const beat = m.trading.strategy_return_pct - m.trading.buy_hold_return_pct;
    text += beat >= 0
      ? ` Trading its signals returned ${signedPct(m.trading.strategy_return_pct)}, ahead of buy and hold (${signedPct(m.trading.buy_hold_return_pct)}).`
      : ` Trading its signals returned ${signedPct(m.trading.strategy_return_pct)}, behind buy and hold (${signedPct(m.trading.buy_hold_return_pct)}).`;
    if (beat < 0 && tone === 'good') tone = 'warn';
  }
  return { tone, text, edgePts, warnings };
}

/** True when the run predates per-fold results and run data (nothing to chart). */
export const isLegacyModel = (m: Pick<ModelDetailDto, 'cv_folds' | 'trading' | 'feature_importance'>): boolean =>
  m.cv_folds.length === 0 && m.trading === null && m.feature_importance.length === 0;

/** Indexes holding the best value. Empty when fewer than two values exist or they are all equal. */
export function bestIndexes(values: (number | null | undefined)[], higherIsBetter = true): number[] {
  const present = values
    .map((v, i) => ({ v, i }))
    .filter((x): x is { v: number; i: number } => typeof x.v === 'number' && !Number.isNaN(x.v));
  if (present.length < 2) return [];
  const best = higherIsBetter ? Math.max(...present.map((x) => x.v)) : Math.min(...present.map((x) => x.v));
  const worst = higherIsBetter ? Math.min(...present.map((x) => x.v)) : Math.max(...present.map((x) => x.v));
  if (best === worst) return [];
  return present.filter((x) => x.v === best).map((x) => x.i);
}

export interface ParamRow {
  key: string;
  value: unknown;
  def: unknown;
  hasDefault: boolean;
  changed: boolean;
}

/** Hyperparameters of a run next to the library defaults. */
export function paramRows(algorithm: string, hyperparams: Record<string, unknown>): ParamRow[] {
  const defaults = DEFAULT_HYPERPARAMS[algorithm] ?? {};
  const keys = [...new Set([...Object.keys(hyperparams), ...Object.keys(defaults)])];
  return keys.map((key) => {
    const hasDefault = key in defaults;
    const set = key in hyperparams;
    const value = set ? hyperparams[key] : defaults[key];
    const def = defaults[key];
    return { key, value, def, hasDefault, changed: set && (!hasDefault || fmtValue(value) !== fmtValue(def)) };
  }).sort((a, b) => Number(b.changed) - Number(a.changed) || a.key.localeCompare(b.key));
}

/** Human label for the CV table / charts: mean of a numeric field across folds. */
export function meanOf(folds: CvFoldDto[], pick: (f: CvFoldDto) => number | null): number | null {
  const values = folds.map(pick).filter((v): v is number => typeof v === 'number');
  return values.length ? values.reduce((a, b) => a + b, 0) / values.length : null;
}

export interface RegistryRow {
  summary: MLModelDto;
  detail: ModelDetailDto | null;
}

export type SortKey = 'newest' | 'accuracy' | 'trading';

export function sortRows(rows: RegistryRow[], key: SortKey): RegistryRow[] {
  const copy = [...rows];
  const acc = (r: RegistryRow) => r.summary.metrics.accuracy ?? -Infinity;
  const trade = (r: RegistryRow) => r.detail?.trading?.strategy_return_pct ?? -Infinity;
  if (key === 'accuracy') copy.sort((a, b) => acc(b) - acc(a));
  else if (key === 'trading') copy.sort((a, b) => trade(b) - trade(a));
  else copy.sort((a, b) => b.summary.created_at.localeCompare(a.summary.created_at));
  return copy;
}

export function matchesQuery(row: RegistryRow, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const d = row.detail;
  const hay = [
    row.summary.model_id, row.summary.algorithm, algorithmLabel(row.summary.algorithm),
    d?.display_name ?? '', d?.dataset ?? '', ...(d?.tags ?? []), ...(d?.symbols ?? []),
  ].join(' ').toLowerCase();
  return hay.includes(q);
}

/** Series colours for comparing up to three models. */
export const SERIES_COLORS = ['var(--color-accent)', 'var(--color-warning)', '#b388ff'];
