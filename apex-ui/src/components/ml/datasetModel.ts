/** Pure helpers for the dataset builder: name rules, row estimate, presets, class labels. */
import type { FeatureDefDto, LabelSpecDto } from '../../lib/types';

export const TIMEFRAMES = ['m1', 'm5', 'm15', 'h1', 'd1'] as const;
export const MAX_SYMBOLS = 10;
export const MIN_BARS = 100;
export const MAX_BARS = 5000;
export const MAX_HORIZON = 100;
export const MIN_ROWS_WARN = 300;
export const MIN_CLASS_SHARE = 0.15;
/** Rule of thumb: fewer than this many rows per feature invites overfitting. */
export const ROWS_PER_FEATURE = 20;
export const DATASET_NAME_MAX = 64;

/** Same rule as the backend: lowercase letters, digits, underscore and hyphen, 1-64 characters. */
export function validateDatasetName(name: string): string | null {
  if (!name) return 'Give the dataset a name.';
  if (name.length > DATASET_NAME_MAX) return `Name is too long (max ${DATASET_NAME_MAX} characters).`;
  if (!/^[a-z0-9_-]+$/.test(name)) return 'Use only lowercase letters, digits, "_" and "-".';
  return null;
}

export function suggestDatasetName(symbols: string[], timeframe: string, horizon: number): string {
  const sym = symbols.slice(0, 3).join('_').toLowerCase().replace(/[^a-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '');
  const raw = `${sym || 'dataset'}_${timeframe}_h${horizon}`;
  return raw.slice(0, DATASET_NAME_MAX);
}

export interface RowEstimate {
  warmup: number;
  /** Rows left after each symbol loses its warm-up and look-ahead bars. */
  rows: number;
}

/**
 * Estimated training rows. Every symbol loses the longest warm-up (the first bars cannot have an
 * indicator value) and `horizon` bars at the end (no future to label them with).
 */
export function estimateRows(
  bars: number, symbolCount: number, selected: string[], catalog: FeatureDefDto[], horizon: number,
): RowEstimate {
  const warmup = selected.reduce((max, id) => Math.max(max, catalog.find((f) => f.id === id)?.warmup_bars ?? 0), 0);
  const perSymbol = Math.max(0, bars - warmup - horizon);
  return { warmup, rows: perSymbol * symbolCount };
}

export interface FeaturePreset { id: string; label: string; ids: string[] | 'all' }

export const FEATURE_PRESETS: FeaturePreset[] = [
  { id: 'trend', label: 'Trend', ids: ['ret_5', 'ret_10', 'sma_ratio_20', 'sma_ratio_50', 'ema_ratio_12'] },
  { id: 'momentum', label: 'Momentum', ids: ['ret_1', 'ret_5', 'macd_hist', 'rsi_14', 'roc_10'] },
  { id: 'volatility', label: 'Volatility', ids: ['range_pct', 'vol_10', 'vol_20', 'atr_pct_14', 'bb_pos_20'] },
  { id: 'everything', label: 'Everything', ids: 'all' },
];

export function presetFeatureIds(preset: FeaturePreset, catalog: FeatureDefDto[]): string[] {
  const known = new Set(catalog.map((f) => f.id));
  return preset.ids === 'all' ? catalog.map((f) => f.id) : preset.ids.filter((id) => known.has(id));
}

export function groupFeatures(catalog: FeatureDefDto[]): { group: string; features: FeatureDefDto[] }[] {
  const order: string[] = [];
  const map = new Map<string, FeatureDefDto[]>();
  for (const f of catalog) {
    if (!map.has(f.group)) { map.set(f.group, []); order.push(f.group); }
    map.get(f.group)!.push(f);
  }
  return order.map((group) => ({ group, features: map.get(group)! }));
}

/** First problem with the label settings, or null. */
export function validateLabel(horizon: number, kind: LabelSpecDto['kind'], thresholdPct: number): string | null {
  if (!Number.isInteger(horizon) || horizon < 1 || horizon > MAX_HORIZON) return `Horizon must be a whole number from 1 to ${MAX_HORIZON} bars.`;
  if (kind === 'return_threshold' && !(thresholdPct > 0 && thresholdPct <= 50)) return 'Threshold must be above 0% and at most 50%.';
  return null;
}

export function classLabel(kind: LabelSpecDto['kind'], cls: string): string {
  if (kind === 'direction') return cls === '1' ? 'Up' : 'Down';
  return cls === '1' ? 'Up' : cls === '-1' ? 'Down' : 'Flat';
}

export interface ClassShare { cls: string; label: string; count: number; share: number }

export function classShares(kind: LabelSpecDto['kind'], counts: Record<string, number>): ClassShare[] {
  const total = Object.values(counts).reduce((a, b) => a + b, 0);
  return Object.entries(counts)
    .sort((a, b) => Number(a[0]) - Number(b[0]))
    .map(([cls, count]) => ({ cls, label: classLabel(kind, cls), count, share: total > 0 ? count / total : 0 }));
}

export function fmtShare(share: number): string {
  return `${(share * 100).toFixed(1)}%`;
}
