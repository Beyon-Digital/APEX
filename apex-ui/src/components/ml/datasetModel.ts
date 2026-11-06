/** Pure helpers for the dataset builder: name rules, row estimate, presets, class labels. */
import type { CustomFeatureDto, ExpressionCheckDto, FeatureDefDto, LabelSpecDto, SymbolDataDto } from '../../lib/types';

export const TIMEFRAMES = ['m1', 'm5', 'm15', 'h1', 'd1'] as const;
export const MAX_SYMBOLS = 30;
export const MAX_CONTEXT_SYMBOLS = 10;
export const MIN_BARS = 100;
export const MAX_BARS = 20000;
/** The most bars one history download returns; the backend clamps larger requests to this. */
export const MAX_FETCH_BARS = 5000;
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

/** Backend spellings of the timeframes (and the UI ids themselves) mapped to the UI ids in `TIMEFRAMES`. */
const TIMEFRAME_ALIASES: Record<string, string> = {
  '1m': 'm1', '5m': 'm5', '15m': 'm15', '1h': 'h1', '1d': 'd1',
  m1: 'm1', m5: 'm5', m15: 'm15', h1: 'h1', d1: 'd1',
};

/**
 * The UI id of a timeframe. The backend answers with canonical ids ("1d"), the UI uses "d1"; both
 * map to the same id here. Anything else (such as "4h") is returned lowercased and unchanged.
 */
export function uiTimeframe(tf: string): string {
  const t = tf.trim().toLowerCase();
  return TIMEFRAME_ALIASES[t] ?? t;
}

export function sameTimeframe(a: string, b: string): boolean {
  return uiTimeframe(a) === uiTimeframe(b);
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

/** Largest round-trip cost the form accepts, in percent. */
export const MAX_FEE_PCT = 10;

/** Separate up/down thresholds (percent), used when the label is asymmetric. */
export interface AsymmetricThresholds { up: number; down: number }

/** First problem with the label settings, or null. */
export function validateLabel(
  horizon: number, kind: LabelSpecDto['kind'], thresholdPct: number, asym: AsymmetricThresholds | null = null,
): string | null {
  if (!Number.isInteger(horizon) || horizon < 1 || horizon > MAX_HORIZON) return `Horizon must be a whole number from 1 to ${MAX_HORIZON} bars.`;
  if (kind === 'return_threshold') {
    const ok = (t: number) => t > 0 && t <= 50;
    if (asym) {
      if (!ok(asym.up)) return 'Up threshold must be above 0% and at most 50%.';
      if (!ok(asym.down)) return 'Down threshold must be above 0% and at most 50%.';
    } else if (!ok(thresholdPct)) return 'Threshold must be above 0% and at most 50%.';
  }
  return null;
}

export function validateFee(feePct: number): string | null {
  if (!Number.isFinite(feePct) || feePct < 0 || feePct > MAX_FEE_PCT) return `Fee must be a number from 0 to ${MAX_FEE_PCT} percent.`;
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

/* ------------------------------------------------------------------ stored data per symbol */

/** Fewer stored bars than this is flagged as thin. */
export const THIN_BARS = 300;

export type DataStatus = 'loading' | 'none' | 'thin' | 'ok';

/** `undefined` means the answer has not arrived yet. */
export function dataStatus(data: SymbolDataDto | undefined): DataStatus {
  if (!data) return 'loading';
  if (data.bars <= 0) return 'none';
  return data.bars < THIN_BARS ? 'thin' : 'ok';
}

const DAY_MS = 86_400_000;

/** Parse a YYYY-MM-DD date or ISO timestamp to a UTC day start (ms), or null. */
export function dayMs(value: string | null | undefined): number | null {
  if (!value) return null;
  const t = Date.parse(value.length === 10 ? `${value}T00:00:00Z` : value);
  return Number.isFinite(t) ? Math.floor(t / DAY_MS) * DAY_MS : null;
}

/** First problem with a from/to date range, or null. Both ends are required. */
export function validateRange(from: string, to: string): string | null {
  if (!from || !to) return 'Choose both a from and a to date.';
  if (dayMs(from) === null || dayMs(to) === null) return 'Enter valid dates.';
  if (!(from < to)) return 'The from date must be before the to date.';
  return null;
}

export type RangeCoverage = 'inside' | 'partial' | 'outside' | 'nodata' | 'unknown';

/** Gap that is not worth a warning (weekends and holidays at the end of the stored data). */
const END_TOLERANCE_DAYS = 4;

/** How a date range sits against what is stored for one symbol. */
export function rangeCoverage(data: SymbolDataDto | undefined, from: string, to: string): RangeCoverage {
  if (!data) return 'unknown';
  const first = dayMs(data.first_time);
  const last = dayMs(data.last_time);
  const f = dayMs(from);
  const t = dayMs(to);
  if (data.bars <= 0 || first === null || last === null) return 'nodata';
  if (f === null || t === null) return 'unknown';
  if (t < first || f > last) return 'outside';
  if (f < first || t > last + END_TOLERANCE_DAYS * DAY_MS) return 'partial';
  return 'inside';
}

/**
 * Stored bars of one symbol that fall inside the range. Stored bars are assumed to be spread
 * evenly between the first and last stored date, which is close enough for an estimate.
 */
export function barsInRange(data: SymbolDataDto | undefined, from: string, to: string): number | null {
  if (!data) return null;
  const first = dayMs(data.first_time);
  const last = dayMs(data.last_time);
  const f = dayMs(from);
  const t = dayMs(to);
  if (data.bars <= 0 || first === null || last === null) return 0;
  if (f === null || t === null) return null;
  const spanStart = first;
  const spanEnd = last + DAY_MS; // the last bar covers its whole day
  const lo = Math.max(f, spanStart);
  const hi = Math.min(t + DAY_MS, spanEnd); // `to` is inclusive
  if (hi <= lo) return 0;
  return Math.max(1, Math.round((data.bars * (hi - lo)) / (spanEnd - spanStart)));
}

/** Bars per calendar day, roughly, used to size a history download for a date range. */
const BARS_PER_DAY: Record<string, number> = { d1: 0.72, h1: 5, m15: 20, m5: 60, m1: 300 };

export interface FetchPlan {
  /** Bars requested from the backend: what is needed, at most `MAX_FETCH_BARS`. */
  bars: number;
  /** True when more bars are needed than one download can return. */
  capped: boolean;
}

/** How many bars a download asks for, given how many the dataset needs. */
export function planFetch(needed: number): FetchPlan {
  return { bars: Math.min(MAX_FETCH_BARS, needed), capped: needed > MAX_FETCH_BARS };
}

/** How many bars to download so that the stored history reaches back to `from`. */
export function barsToCoverRange(from: string, timeframe: string, warmup: number, now = Date.now()): number {
  const f = dayMs(from);
  const perDay = BARS_PER_DAY[timeframe] ?? 1;
  const days = f === null ? 0 : Math.max(0, Math.ceil((now - f) / DAY_MS));
  return Math.min(MAX_BARS, Math.max(MIN_BARS, Math.ceil(days * perDay) + warmup + 10));
}

/** Rows left after every symbol loses the warm-up and look-ahead bars. */
export function estimateRowsFromBars(perSymbolBars: number[], warmup: number, horizon: number): RowEstimate {
  const rows = perSymbolBars.reduce((sum, bars) => sum + Math.max(0, bars - warmup - horizon), 0);
  return { warmup, rows };
}

/** Longest warm-up of the chosen catalog features and the checked custom formulas. */
export function totalWarmup(selected: string[], catalog: FeatureDefDto[], customWarmups: number[]): number {
  const fromCatalog = selected.reduce((max, id) => Math.max(max, catalog.find((f) => f.id === id)?.warmup_bars ?? 0), 0);
  return customWarmups.reduce((max, w) => Math.max(max, w), fromCatalog);
}

/* ------------------------------------------------------------------ custom features */

export const CUSTOM_NAME_MAX = 41;
/** Columns of the dataset file that a custom feature must not shadow. */
const RESERVED_COLUMNS = ['time', 'symbol', 'fwd_ret', 'target'];

/** One editable row of the custom feature list. `key` is a stable id for React and the check cache. */
export interface CustomRow { key: string; name: string; expr: string }

/** What the live check knows about one row. */
export interface RowCheck {
  status: 'empty' | 'nosymbol' | 'checking' | 'ok' | 'error';
  result?: ExpressionCheckDto;
}

/** Same rule as the backend: [a-z][a-z0-9_]{0,40}, not a catalog id, unique. */
export function validateFeatureName(name: string, catalogIds: string[], otherNames: string[]): string | null {
  if (!name) return 'Give the feature a name.';
  if (!/^[a-z][a-z0-9_]{0,40}$/.test(name)) {
    return `Use a lowercase letter first, then lowercase letters, digits or "_" (at most ${CUSTOM_NAME_MAX} characters).`;
  }
  if (catalogIds.includes(name)) return `"${name}" is already a built-in feature. Pick another name.`;
  if (RESERVED_COLUMNS.includes(name)) return `"${name}" is a reserved dataset column. Pick another name.`;
  if (otherNames.includes(name)) return 'Another custom feature already uses this name.';
  return null;
}

/** First problem per row, in row order, as human sentences. Empty when every row is fine. */
export function customRowProblems(rows: CustomRow[], checks: Record<string, RowCheck>, catalogIds: string[]): string[] {
  const out: string[] = [];
  rows.forEach((row, i) => {
    const label = `Custom feature ${i + 1}`;
    const others = rows.filter((r) => r.key !== row.key).map((r) => r.name);
    const nameError = validateFeatureName(row.name, catalogIds, others);
    if (nameError) { out.push(`${label}: ${nameError}`); return; }
    const check = checks[row.key];
    if (!row.expr.trim()) { out.push(`${label} (${row.name}): enter a formula.`); return; }
    if (!check || check.status === 'checking') out.push(`${label} (${row.name}): the formula is still being checked.`);
    else if (check.status === 'error') out.push(`${label} (${row.name}): fix the formula. ${check.result?.error ?? ''}`.trim());
  });
  return out;
}

export function toCustomFeatureDtos(rows: CustomRow[]): CustomFeatureDto[] {
  return rows.map((r) => ({ name: r.name, expr: r.expr.trim() }));
}

let rowSeq = 0;
export function newRowKey(): string { rowSeq += 1; return `cf-${rowSeq}`; }

/** A name based on `base` that no row uses yet. */
export function uniqueFeatureName(base: string, taken: string[]): string {
  const clean = base.toLowerCase().replace(/[^a-z0-9_]+/g, '_').replace(/^[^a-z]+/, '').slice(0, 36) || 'feature';
  if (!taken.includes(clean)) return clean;
  for (let n = 2; ; n += 1) {
    const candidate = `${clean}_${n}`;
    if (!taken.includes(candidate)) return candidate;
  }
}

/** Context symbols named in `other("SYMBOL", ...)` calls of a formula. */
export function otherSymbols(expr: string): string[] {
  const out: string[] = [];
  for (const m of expr.matchAll(/other\(\s*["']([^"']+)["']/g)) {
    const sym = m[1].trim().toUpperCase();
    if (!out.includes(sym)) out.push(sym);
  }
  return out;
}

/** The expression with a caret line under character `pos` (for error messages). */
export function caretLine(expr: string, pos: number | null): string {
  const at = Math.max(0, Math.min(pos ?? 0, expr.length));
  return `${' '.repeat(at)}^`;
}

export interface LanguageEntry { name: string; signature: string; example: string; help: string; insert: string }

/** The formula language, one line per function (the editor's reference). */
export const LANGUAGE_REFERENCE: LanguageEntry[] = [
  { name: 'sma', signature: 'sma(x, n)', example: 'sma(close, 20)', help: 'Simple moving average of x over the last n bars.', insert: 'sma(close, 20)' },
  { name: 'ema', signature: 'ema(x, n)', example: 'ema(close, 12)', help: 'Exponential moving average with span n.', insert: 'ema(close, 12)' },
  { name: 'std', signature: 'std(x, n)', example: 'std(ret(close), 20)', help: 'Standard deviation of x over the last n bars.', insert: 'std(ret(close), 20)' },
  { name: 'sum', signature: 'sum(x, n)', example: 'sum(volume, 5)', help: 'Sum of x over the last n bars.', insert: 'sum(volume, 5)' },
  { name: 'max', signature: 'max(x, n)', example: 'close / max(high, 20)', help: 'Highest value of x over the last n bars.', insert: 'max(high, 20)' },
  { name: 'min', signature: 'min(x, n)', example: 'close / min(low, 20)', help: 'Lowest value of x over the last n bars.', insert: 'min(low, 20)' },
  { name: 'zscore', signature: 'zscore(x, n)', example: 'zscore(close, 30)', help: 'How many standard deviations x is from its n-bar average.', insert: 'zscore(close, 30)' },
  { name: 'rsi', signature: 'rsi(x, n=14)', example: 'rsi(close)', help: 'Relative strength index of x, 0 to 100 (n defaults to 14).', insert: 'rsi(close, 14)' },
  { name: 'ret', signature: 'ret(x, n=1)', example: 'ret(close, 5)', help: 'Change over n bars: x / shift(x, n) - 1 (n defaults to 1).', insert: 'ret(close, 5)' },
  { name: 'shift', signature: 'shift(x, n)', example: 'shift(close, 3)', help: 'The value n bars ago (n of 0 or more, never the future).', insert: 'shift(close, 3)' },
  { name: 'abs', signature: 'abs(x)', example: 'abs(ret(close))', help: 'Absolute value.', insert: 'abs(ret(close))' },
  { name: 'log', signature: 'log(x)', example: 'log(close)', help: 'Natural logarithm. Gives no value (NaN) for x of 0 or less.', insert: 'log(close)' },
  { name: 'sqrt', signature: 'sqrt(x)', example: 'sqrt(volume)', help: 'Square root. Gives no value (NaN) for negative x.', insert: 'sqrt(volume)' },
  { name: 'atr', signature: 'atr(n=14)', example: 'atr(14) / close', help: 'Average true range over n bars (n defaults to 14).', insert: 'atr(14)' },
  { name: 'other', signature: 'other("SYMBOL", "field")', example: 'other("SPY", "close")', help: 'A field (open, high, low, close, volume) of a context symbol, matched by time using only values already known.', insert: 'other("SPY", "close")' },
];

export const LANGUAGE_NOTES = [
  'Names: open, high, low, close, volume. Operators: + - * / and comparisons > < >= <= (result 1 or 0). Use parentheses to group.',
  'Windows (n) are whole numbers from 1 to 500. A formula is at most 300 characters.',
  'Every function only looks backwards, so a formula can never see the future. Dividing by zero gives no value instead of an error.',
];

export interface ExampleFormula { id: string; name: string; expr: string; help: string }

export const EXAMPLE_FORMULAS: ExampleFormula[] = [
  { id: 'dist_sma20', name: 'dist_sma20', expr: 'close / sma(close, 20) - 1', help: 'How far the close is from its 20-bar average' },
  { id: 'vol_spike', name: 'vol_spike', expr: 'volume / sma(volume, 20)', help: 'Volume relative to its 20-bar average' },
  { id: 'range_atr', name: 'range_atr', expr: '(high - low) / atr(14)', help: 'Bar range in units of ATR' },
  { id: 'mom_z', name: 'mom_z', expr: 'zscore(ret(close, 5), 60)', help: 'Unusually strong 5-bar move' },
  { id: 'up_day', name: 'up_day', expr: 'close > shift(close, 1)', help: '1 if the close is above the previous close' },
  { id: 'rel_spy', name: 'rel_spy', expr: 'ret(close, 5) - ret(other("SPY", "close"), 5)', help: 'Return relative to a context symbol (add SPY as context)' },
];
