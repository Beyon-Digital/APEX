import type {
  ActivityActor, AutomationDto, BrokerConnectionDto, BuildingBlocksDto, CvFoldDto, LabelSpecDto, MLModelDto, ModelDetailDto, RecipeDto,
  RecipeMemberDto, RecipeStepDto, TradeMode,
} from '../../lib/types';

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
  extra_trees: 'Extra Trees',
  hist_gradient_boosting: 'Histogram Gradient Boosting',
  custom: 'Custom estimator',
  recipe: 'Recipe',
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
  const bars = `${label.horizon_bars} bar${label.horizon_bars === 1 ? '' : 's'}`;
  if (label.kind === 'direction') return `Up or down after ${bars}`;
  const { up, down } = labelThresholds(label);
  return up === down
    ? `Move beyond ±${up}% after ${bars}`
    : `Up beyond +${up}% / down beyond -${down}% after ${bars}`;
}

/** Effective up and down thresholds (percent) of a label; both fall back to `threshold_pct`. */
export function labelThresholds(label: LabelSpecDto): { up: number; down: number } {
  return {
    up: label.up_threshold_pct ?? label.threshold_pct,
    down: label.down_threshold_pct ?? label.threshold_pct,
  };
}

// ── Trading rule, hold-out and shorting ─────────────────────────────

export const TRADE_MODE_LABEL: Record<TradeMode, string> = { long_flat: 'Long / flat', long_short: 'Long / short' };

export const tradeModeLabel = (mode: TradeMode | undefined): string => TRADE_MODE_LABEL[mode ?? 'long_flat'];

export const TRADE_MODE_HINT: Record<TradeMode, string> = {
  long_flat: 'The trading return assumes the rule: buy when the model signals up, otherwise sit in cash. No short positions.',
  long_short: 'The trading return assumes the rule: buy when the model signals up and sell short when it signals down. It only applies to an automation with shorting allowed on a broker that can short.',
};

/** Hold-out accuracy this many percentage points below cross-validation accuracy counts as overfit. */
export const HOLDOUT_GAP_PTS = 5;

export type HoldoutVerdictId = 'none' | 'held' | 'overfit' | 'no-edge' | 'unknown';

export interface HoldoutVerdict {
  id: HoldoutVerdictId;
  tone: VerdictTone;
  text: string;
  /** Cross-validation accuracy minus hold-out accuracy in points (positive = hold-out is worse). */
  gapPts: number | null;
}

/**
 * Did the model hold up on the final period that cross-validation never saw?
 * Overfit when hold-out accuracy is at least HOLDOUT_GAP_PTS points below cross-validation accuracy.
 * Otherwise it held up, unless it did not beat the hold-out's majority-class baseline.
 */
export function holdoutVerdict(m: Pick<ModelDetailDto, 'holdout' | 'metrics'>): HoldoutVerdict {
  const h = m.holdout;
  if (!h) {
    return { id: 'none', tone: 'info', gapPts: null, text: 'No hold-out was set. Set a hold-out share in the Train tab to test the model on a final period that cross-validation never sees.' };
  }
  const cv = m.metrics.accuracy;
  if (typeof cv !== 'number' || typeof h.accuracy !== 'number') {
    return { id: 'unknown', tone: 'info', gapPts: null, text: 'Hold-out result recorded, but there is no cross-validation accuracy to compare it with.' };
  }
  const gap = Math.round((cv - h.accuracy) * 100 * 1e6) / 1e6; // rounded so exactly 5.0 pts is not lost to float error
  if (gap >= HOLDOUT_GAP_PTS) {
    return { id: 'overfit', tone: 'bad', gapPts: gap, text: `Much worse than cross-validation: likely overfit. Hold-out ${pct(h.accuracy)} vs ${pct(cv)} in cross-validation (${gap.toFixed(1)} pts lower, limit ${HOLDOUT_GAP_PTS}).` };
  }
  if (typeof h.baseline_accuracy === 'number' && h.accuracy < h.baseline_accuracy) {
    return { id: 'no-edge', tone: 'warn', gapPts: gap, text: `Close to cross-validation, but below always predicting the majority class on the hold-out (${pct(h.accuracy)} vs ${pct(h.baseline_accuracy)}).` };
  }
  return { id: 'held', tone: 'good', gapPts: gap, text: `Held up on the hold-out: ${pct(h.accuracy)} vs ${pct(cv)} in cross-validation (within ${HOLDOUT_GAP_PTS} pts).` };
}

export interface ShortingContext {
  /** True when the model was evaluated long/short but an automation using it cannot actually short. */
  limited: boolean;
  reasons: string[];
}

/**
 * A long/short trading return is only reachable by automations with shorting allowed on a broker that
 * can short. List the automations using the model that fall short of that.
 */
export function shortingContext(
  model: Pick<ModelDetailDto, 'trade_mode' | 'model_id'>,
  automations: Pick<AutomationDto, 'name' | 'model_id' | 'allow_short' | 'broker_id'>[],
  brokers: Pick<BrokerConnectionDto, 'broker_id' | 'can_short'>[],
): ShortingContext {
  if (model.trade_mode !== 'long_short') return { limited: false, reasons: [] };
  const reasons: string[] = [];
  for (const a of automations.filter((x) => x.model_id === model.model_id)) {
    if (!a.allow_short) reasons.push(`${a.name} is long only (shorting not allowed)`);
    else if (!brokers.find((b) => b.broker_id === a.broker_id)?.can_short) reasons.push(`${a.name} uses ${a.broker_id}, which cannot short`);
  }
  return { limited: reasons.length > 0, reasons };
}

/** What a class means for this model's label when an automation trades it. */
export function classMeaning(label: LabelSpecDto | null): string {
  if (label?.kind === 'direction') return 'Class 1 buys. Class 0 sells short when shorting is allowed, otherwise it exits to flat.';
  if (label?.kind === 'return_threshold') return 'Class 1 buys. Class -1 sells short when shorting is allowed, otherwise it exits to flat. Class 0 is flat.';
  return 'Class 1 buys. Class 0 exits to flat. Class -1 sells short when shorting is allowed.';
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

type VerdictInput = Pick<ModelDetailDto, 'metrics' | 'cv_folds' | 'trading' | 'rows'> & {
  trade_mode?: TradeMode;
};

/** Plain-language read of a run: did it beat the naive baseline, and is there enough data to trust it. */
export function modelVerdict(m: VerdictInput, shorting?: ShortingContext): Verdict {
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
  if (m.trading && m.trade_mode === 'long_short' && shorting?.limited) {
    // The figure needs shorts that the automations cannot make, so it must not count as a success.
    text += ` Trading long and short returned ${signedPct(m.trading.strategy_return_pct)}, but that needs shorting and not every automation using this model can short, so do not rely on it.`;
    if (tone === 'good') tone = 'warn';
  } else if (m.trading) {
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
    ...(d?.recipe ? [d.recipe.name, d.recipe.label, ...recipeMembers(d.recipe).map((m) => algorithmLabel(m.algorithm))] : []),
    d?.display_name ?? '', d?.dataset ?? '', ...(d?.tags ?? []), ...(d?.symbols ?? []),
  ].join(' ').toLowerCase();
  return hay.includes(q);
}

/** Series colours for comparing up to three models. */
export const SERIES_COLORS = ['var(--color-accent)', 'var(--color-warning)', '#b388ff'];

// ── Algorithm recipes ───────────────────────────────────────────────

/** True when the model was built from a recipe (its summary says algorithm "recipe"). */
export const isRecipeModel = (m: { algorithm: string }): boolean => m.algorithm === 'recipe';

/** What to call a model's algorithm: the recipe's own label for recipe models, else the built-in name. */
export function modelAlgorithmLabel(m: { algorithm: string; recipe?: RecipeDto | null }): string {
  if (!isRecipeModel(m)) return algorithmLabel(m.algorithm);
  return m.recipe?.label?.trim() || m.recipe?.name || 'Recipe';
}

/**
 * Value of the algorithm filter for a model. Built-ins use their id; recipe models use
 * "recipe:<name>" (or plain "recipe" while their recipe is not known yet).
 */
export function algorithmFilterKey(m: { algorithm: string }, detail: { recipe: RecipeDto | null } | null): string {
  if (!isRecipeModel(m)) return m.algorithm;
  return detail?.recipe?.name ? `recipe:${detail.recipe.name}` : 'recipe';
}

/** True when a model passes the algorithm filter ("recipe" matches every recipe model). */
export function matchesAlgorithmFilter(filter: string, m: { algorithm: string }, detail: { recipe: RecipeDto | null } | null): boolean {
  if (!filter) return true;
  if (filter === 'recipe') return isRecipeModel(m);
  return algorithmFilterKey(m, detail) === filter;
}

export interface AlgorithmOption { value: string; label: string; recipe: boolean }

/** Options of the algorithm filter: built-ins first, then "all recipes" and each recipe by label. */
export function algorithmFilterOptions(rows: RegistryRow[]): AlgorithmOption[] {
  const builtIn = new Set<string>();
  const recipes = new Map<string, string>();
  let anyRecipe = false;
  for (const { summary, detail } of rows) {
    if (!isRecipeModel(summary)) { builtIn.add(summary.algorithm); continue; }
    anyRecipe = true;
    if (detail?.recipe?.name) recipes.set(detail.recipe.name, detail.recipe.label?.trim() || detail.recipe.name);
  }
  const out: AlgorithmOption[] = [...builtIn].sort().map((a) => ({ value: a, label: algorithmLabel(a), recipe: false }));
  if (anyRecipe) out.push({ value: 'recipe', label: 'All recipes', recipe: true });
  [...recipes.entries()].sort((a, b) => a[1].localeCompare(b[1]))
    .forEach(([name, label]) => out.push({ value: `recipe:${name}`, label: `${label} (recipe)`, recipe: true }));
  return out;
}

export const STEP_LABEL: Record<string, string> = {
  standard_scaler: 'Standard scaler',
  robust_scaler: 'Robust scaler',
  minmax_scaler: 'Min-max scaler',
  quantile_transformer: 'Quantile transformer',
  power_transformer: 'Power transformer',
  clip_outliers: 'Clip outliers',
  variance_threshold: 'Drop flat features',
  select_k_best: 'Keep the best k features',
  pca: 'PCA',
};

/** Defaults of the step parameters, as in the building blocks, to tell which values a recipe changed. */
export const STEP_DEFAULTS: Record<string, Record<string, unknown>> = {
  standard_scaler: { with_mean: true, with_std: true },
  quantile_transformer: { output_distribution: 'uniform', n_quantiles: 200 },
  clip_outliers: { lower_pct: 1, upper_pct: 99 },
  variance_threshold: { threshold: 0 },
  select_k_best: { k: 10, score: 'f_classif' },
  pca: { n_components: 0.95, whiten: false },
};

export const stepLabel = (kind: string): string => STEP_LABEL[kind] ?? kind;

export interface RecipeParamView { key: string; text: string; changed: boolean }

/** Parameters a step or member sets, each marked as changed when it differs from the default (or has none). */
export function paramViews(params: Record<string, unknown> | undefined, defaults: Record<string, unknown> = {}): RecipeParamView[] {
  return Object.entries(params ?? {}).map(([key, value]) => ({
    key,
    text: `${key} = ${fmtValue(value)}`,
    changed: !(key in defaults) || fmtValue(defaults[key]) !== fmtValue(value),
  }));
}

export const stepParamViews = (s: RecipeStepDto): RecipeParamView[] => paramViews(s.params, STEP_DEFAULTS[s.kind]);

const paramText = (views: RecipeParamView[]): string => (views.length ? ` (${views.map((v) => v.text).join(', ')})` : '');

/** One line per step, e.g. "PCA (n_components = 0.9)". */
export const describeStep = (s: RecipeStepDto): string => `${stepLabel(s.kind)}${paramText(stepParamViews(s).filter((v) => v.changed))}`;

/** All models a recipe fits: its members, plus the final estimator for stacking. */
export function recipeMembers(r: RecipeDto): RecipeMemberDto[] {
  const m = r.model;
  if (!m) return [];
  if (m.kind === 'single') return [{ algorithm: m.algorithm, params: m.params }];
  return m.kind === 'stacking' ? [...m.members, m.final] : m.members;
}

export const KIND_LABEL: Record<string, string> = { single: 'Single model', voting: 'Voting ensemble', stacking: 'Stacking ensemble' };

export function describeMember(m: RecipeMemberDto, withWeight = false): string {
  const parts = paramViews(m.params).map((v) => v.text);
  if (withWeight && (m.weight ?? 1) !== 1) parts.push(`weight ${m.weight}`);
  return `${algorithmLabel(m.algorithm)}${parts.length ? ` (${parts.join(', ')})` : ''}`;
}

/** The model part of a recipe as a headline, e.g. "Voting ensemble, soft (3 models)". */
export function describeModelKind(r: RecipeDto): string {
  const m = r.model;
  if (!m) return 'n/a';
  if (m.kind === 'single') return KIND_LABEL.single;
  if (m.kind === 'voting') return `${KIND_LABEL.voting}, ${m.voting} (${m.members.length} models)`;
  return `${KIND_LABEL.stacking}, ${m.blocks} blocks (${m.members.length} models plus a final estimator)`;
}

export const calibrationText = (c: RecipeDto['calibrate'] | undefined): string => (c && c !== 'none' ? c : 'none');

/** Why a recipe model may have no feature importances. Null when nothing in the recipe explains it. */
export function importanceReason(r: RecipeDto | null): string | null {
  if (!r) return null;
  const reasons: string[] = [];
  if (r.steps.some((s) => s.kind === 'pca')) reasons.push('PCA replaces the original features with combinations of them, so a score per original feature is not available.');
  if (r.model?.kind === 'voting' && r.model.voting === 'hard') reasons.push('Hard voting only combines votes, and no importances could be averaged from its members.');
  return reasons.length ? reasons.join(' ') : null;
}

const REGEX_NAME = /^[a-z][a-z0-9_]{0,40}$/;
export const isValidRecipeName = (name: string): boolean => REGEX_NAME.test(name);

/** A free name for a copy of a recipe: "<name>_copy", then "<name>_copy2", ... within the 41 character limit. */
export function suggestRecipeName(name: string, taken: string[]): string {
  const used = new Set(taken);
  for (let i = 1; i < 1000; i += 1) {
    const suffix = i === 1 ? '_copy' : `_copy${i}`;
    const candidate = `${name.slice(0, 41 - suffix.length)}${suffix}`;
    if (!used.has(candidate)) return candidate;
  }
  return `${name.slice(0, 30)}_copy_${Date.now() % 100000}`;
}

/**
 * Stable text of the parts of a recipe that decide how it trains (name, label, text and timestamps are ignored).
 *
 * The trainer stores the recipe with every step's default parameters written out, while a saved recipe keeps only
 * what the user typed. Pass the building blocks so both forms are filled with the same step defaults first;
 * without them a step that has parameters never matches its own trained copy.
 */
export function recipeFingerprint(r: RecipeDto, blocks?: BuildingBlocksDto | null): string {
  const norm = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(norm);
    if (v && typeof v === 'object') {
      return Object.fromEntries(Object.entries(v as Record<string, unknown>).filter(([, x]) => x !== undefined).sort(([a], [b]) => a.localeCompare(b)).map(([k, x]) => [k, norm(x)]));
    }
    return v;
  };
  const withDefaults = (step: RecipeDto['steps'][number]) => {
    const defaults = Object.fromEntries(
      (blocks?.steps.find((k) => k.kind === step.kind)?.params ?? []).filter((p) => p.default !== null && p.default !== undefined).map((p) => [p.key, p.default]),
    );
    return { ...step, params: { ...defaults, ...(step.params ?? {}) } };
  };
  const model = r.model && 'members' in r.model
    ? { ...r.model, members: r.model.members.map((m) => ({ ...m, weight: m.weight ?? 1 })) }
    : r.model;
  return JSON.stringify(norm({ steps: (r.steps ?? []).map(withDefaults), model, calibrate: r.calibrate ?? 'none' }));
}

export type RecipeStatus = 'same' | 'edited' | 'missing';

export type RecipeRelation =
  | { status: 'same' | 'edited'; current: RecipeDto }
  | { status: 'missing'; current: null };

/** How a model's recipe copy relates to the recipe saved under the same name now. */
export function recipeStatus(copy: RecipeDto, saved: RecipeDto[], blocks?: BuildingBlocksDto | null): RecipeRelation {
  const current = saved.find((r) => r.name === copy.name);
  if (!current) return { status: 'missing', current: null };
  return { status: recipeFingerprint(current, blocks) === recipeFingerprint(copy, blocks) ? 'same' : 'edited', current };
}

/** What the Train tab is handed to retrain a recipe model: the model's detail with the recipe to select. */
export const retrainDetailFor = (detail: ModelDetailDto, recipe: RecipeDto): ModelDetailDto => ({ ...detail, algorithm: 'recipe', recipe });
