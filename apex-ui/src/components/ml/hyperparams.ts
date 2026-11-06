/**
 * Algorithms and their tunable parameters for the training form.
 *
 * The backend reports the algorithm list and every parameter (`AlgorithmDto` / `ParamDefDto`); the
 * tables below are only the fallback used when that call fails, plus the optional quick-fill values.
 * Values are kept as the raw text the user typed (`HyperparamValues`) and are only converted to
 * numbers when a training request is built, so a half-typed "0." never gets lost while editing.
 * An empty or missing value means "use the default", which is why only changed values are sent.
 */
import type { AlgorithmDto, ParamDefDto } from '../../lib/types';

export type AlgorithmId = string;
export type BuiltinAlgorithmId = 'random_forest' | 'gradient_boosting' | 'logistic_regression' | 'xgboost';

interface HyperparamSpec {
  key: string;
  label: string;
  type: 'number' | 'int' | 'select';
  /** Value the trainer uses when the key is not sent. `null` means "library default / no limit". */
  default: number | string | null;
  min?: number;
  max?: number;
  step?: number;
  options?: { value: string; label: string }[];
  help: string;
}

interface AlgorithmInfo {
  id: BuiltinAlgorithmId;
  label: string;
  description: string;
}

const ALGORITHMS: AlgorithmInfo[] = [
  { id: 'random_forest', label: 'Random Forest', description: 'Many decision trees voting. Hard to overfit badly, a good first baseline.' },
  { id: 'gradient_boosting', label: 'Gradient Boosting', description: 'Trees built one after another, each fixing the last one\'s mistakes. Often more accurate, easier to overfit.' },
  { id: 'logistic_regression', label: 'Logistic Regression', description: 'A linear model. Fast and hard to overfit; misses non-linear patterns.' },
  { id: 'xgboost', label: 'XGBoost', description: 'Fast, regularised gradient boosting. Needs the xgboost package installed.' },
];

const CLASS_WEIGHT: HyperparamSpec = {
  key: 'class_weight', label: 'Class weight', type: 'select', default: '',
  options: [{ value: '', label: 'None (default)' }, { value: 'balanced', label: 'Balanced' }],
  help: 'Balanced gives rare classes more weight, which helps when up and down bars are unevenly split.',
};

const HYPERPARAMS: Record<BuiltinAlgorithmId, HyperparamSpec[]> = {
  random_forest: [
    { key: 'n_estimators', label: 'Trees', type: 'int', default: 100, min: 10, max: 2000, step: 10, help: 'Number of trees. More is steadier but slower.' },
    { key: 'max_depth', label: 'Max depth', type: 'int', default: null, min: 1, max: 50, step: 1, help: 'Deepest a tree may grow. Empty means no limit. Lower values overfit less.' },
    { key: 'min_samples_leaf', label: 'Min samples per leaf', type: 'int', default: 1, min: 1, max: 500, step: 1, help: 'Smallest group of rows a leaf may hold. Raise it to smooth out noisy patterns.' },
    {
      key: 'max_features', label: 'Max features per split', type: 'select', default: 'sqrt',
      options: [{ value: 'sqrt', label: 'sqrt (default)' }, { value: 'log2', label: 'log2' }],
      help: 'How many features each split considers. Fewer makes the trees more different from each other.',
    },
    CLASS_WEIGHT,
  ],
  gradient_boosting: [
    { key: 'n_estimators', label: 'Boosting rounds', type: 'int', default: 100, min: 10, max: 2000, step: 10, help: 'Number of trees added one after another.' },
    { key: 'learning_rate', label: 'Learning rate', type: 'number', default: 0.1, min: 0.001, max: 1, step: 0.01, help: 'How far each tree moves the prediction. Lower is safer but needs more rounds.' },
    { key: 'max_depth', label: 'Max depth', type: 'int', default: 3, min: 1, max: 20, step: 1, help: 'Depth of each tree. Shallow trees (2-5) usually work best for boosting.' },
    { key: 'subsample', label: 'Row subsample', type: 'number', default: 1, min: 0.1, max: 1, step: 0.05, help: 'Share of rows each tree sees. Below 1 adds randomness and reduces overfitting.' },
  ],
  logistic_regression: [
    { key: 'C', label: 'C (inverse regularisation)', type: 'number', default: 1, min: 0.0001, max: 1000, step: 0.1, help: 'Smaller means stronger regularisation and a simpler model.' },
    { key: 'max_iter', label: 'Max iterations', type: 'int', default: 100, min: 50, max: 10000, step: 50, help: 'Solver iteration limit. Raise it if training warns about convergence.' },
    CLASS_WEIGHT,
  ],
  xgboost: [
    { key: 'n_estimators', label: 'Boosting rounds', type: 'int', default: 100, min: 10, max: 2000, step: 10, help: 'Number of trees added one after another.' },
    { key: 'max_depth', label: 'Max depth', type: 'int', default: 6, min: 1, max: 20, step: 1, help: 'Depth of each tree. Lower values overfit less.' },
    { key: 'learning_rate', label: 'Learning rate', type: 'number', default: 0.3, min: 0.001, max: 1, step: 0.01, help: 'Step size per round. 0.05-0.1 with more rounds is a common robust choice.' },
    { key: 'subsample', label: 'Row subsample', type: 'number', default: 1, min: 0.1, max: 1, step: 0.05, help: 'Share of rows each tree sees.' },
    { key: 'colsample_bytree', label: 'Column subsample', type: 'number', default: 1, min: 0.1, max: 1, step: 0.05, help: 'Share of features each tree sees.' },
  ],
};

/** Raw field text per key. A missing key (or an empty string) means "use the default". */
export type HyperparamValues = Record<string, string>;
export type HyperparamsByAlgorithm = Record<string, HyperparamValues>;

export type ParamValue = number | string | boolean;

/* ---------- Built-in fallback list (used only when the backend call fails) ---------- */

function toParamDef(spec: HyperparamSpec): ParamDefDto {
  if (spec.type === 'select') {
    const choices = (spec.options ?? []).map((o) => o.value).filter((v) => v !== '');
    return { key: spec.key, label: spec.label, type: 'choice', default: spec.default === '' ? null : spec.default, choices, help: spec.help };
  }
  return {
    key: spec.key, label: spec.label, type: spec.type === 'int' ? 'int' : 'float', default: spec.default,
    min: spec.min ?? null, max: spec.max ?? null, help: spec.help,
  };
}

export const CUSTOM_ALGORITHM_ID = 'custom';

/** The algorithm list shown until (or unless) the backend reports its own. */
export const BUILTIN_ALGORITHMS: AlgorithmDto[] = [
  ...ALGORITHMS.map((a): AlgorithmDto => ({
    id: a.id, label: a.label, description: a.description, params: HYPERPARAMS[a.id].map(toParamDef), accepts_extra_params: true,
  })),
  {
    id: CUSTOM_ALGORITHM_ID, label: 'Custom estimator (Python file)',
    description: 'Your own scikit-learn style classifier from estimators/<name>.py.', params: [], accepts_extra_params: true,
  },
];

export function findAlgorithm(algorithms: AlgorithmDto[], id: string): AlgorithmDto | undefined {
  return algorithms.find((a) => a.id === id);
}

export function paramDefsFor(algorithms: AlgorithmDto[], id: string): ParamDefDto[] {
  return findAlgorithm(algorithms, id)?.params ?? [];
}

/* ---------- Parsing and validation of one field ---------- */

type Parsed = { ok: true; unset: true } | { ok: true; unset: false; value: ParamValue } | { ok: false; error: string };

const INT_RE = /^[+-]?\d+$/;
const FLOAT_RE = /^[+-]?(\d+\.?\d*|\.\d+)(e[+-]?\d+)?$/i;

function rangeText(def: ParamDefDto): string {
  const hasMin = def.min !== null && def.min !== undefined;
  const hasMax = def.max !== null && def.max !== undefined;
  if (hasMin && hasMax) return ` between ${def.min} and ${def.max}`;
  if (hasMin) return ` of at least ${def.min}`;
  if (hasMax) return ` of at most ${def.max}`;
  return '';
}

export function parseParam(def: ParamDefDto, raw: string | undefined): Parsed {
  const text = (raw ?? '').trim();
  if (text === '') return { ok: true, unset: true };
  switch (def.type) {
    case 'int':
    case 'float': {
      const isInt = def.type === 'int';
      const n = Number(text);
      if (!(isInt ? INT_RE : FLOAT_RE).test(text) || !Number.isFinite(n) || (isInt && !Number.isSafeInteger(n))) {
        return { ok: false, error: `${def.label} must be ${isInt ? 'a whole number' : 'a number'}${rangeText(def)}.` };
      }
      if ((def.min !== null && def.min !== undefined && n < def.min) || (def.max !== null && def.max !== undefined && n > def.max)) {
        return { ok: false, error: `${def.label} must be ${isInt ? 'a whole number' : 'a number'}${rangeText(def)}.` };
      }
      return { ok: true, unset: false, value: n };
    }
    case 'bool':
      if (text === 'true' || text === 'false') return { ok: true, unset: false, value: text === 'true' };
      return { ok: false, error: `${def.label} must be on or off.` };
    case 'choice':
      if ((def.choices ?? []).includes(text)) return { ok: true, unset: false, value: text };
      return { ok: false, error: `${def.label}: pick one of the listed options.` };
    default:
      return { ok: true, unset: false, value: text };
  }
}

function sameAsDefault(def: ParamDefDto, value: ParamValue): boolean {
  const d = def.default;
  if (d === null || d === undefined) return false;
  if (def.type === 'int' || def.type === 'float') return Number(d) === value;
  if (def.type === 'bool') return (d === true || d === 'true') === value;
  return String(d) === String(value);
}

/** True when this field holds something other than its default. */
export function isChanged(def: ParamDefDto, values: HyperparamValues): boolean {
  const parsed = parseParam(def, values[def.key]);
  if (!parsed.ok) return true; // being edited into something invalid still counts as touched
  return !parsed.unset && !sameAsDefault(def, parsed.value);
}

/** Per-key error messages for the current text; empty when everything is valid. */
export function validateParams(defs: ParamDefDto[], values: HyperparamValues): Record<string, string> {
  const errors: Record<string, string> = {};
  for (const def of defs) {
    const parsed = parseParam(def, values[def.key]);
    if (!parsed.ok) errors[def.key] = parsed.error;
  }
  return errors;
}

/** The generated parameters to send: only valid values that differ from the default. */
export function changedParams(defs: ParamDefDto[], values: HyperparamValues): Record<string, ParamValue> {
  const out: Record<string, ParamValue> = {};
  for (const def of defs) {
    const parsed = parseParam(def, values[def.key]);
    if (!parsed.ok || parsed.unset || sameAsDefault(def, parsed.value)) continue;
    out[def.key] = parsed.value;
  }
  return out;
}

export function changedCount(defs: ParamDefDto[], values: HyperparamValues): number {
  return defs.filter((d) => isChanged(d, values)).length;
}

/** What an empty field shows as its placeholder. */
export function placeholderFor(def: ParamDefDto): string {
  if (def.default === null || def.default === undefined) return 'not set';
  return String(def.default);
}

/* ---------- The free-form JSON box ---------- */

export type AdvancedParse = { ok: true; value: Record<string, ParamValue | null> } | { ok: false; error: string };

/** Parse the "Advanced parameters" text: empty is fine, otherwise a flat JSON object. */
export function parseAdvanced(text: string | undefined): AdvancedParse {
  const raw = (text ?? '').trim();
  if (raw === '') return { ok: true, value: {} };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    return { ok: false, error: `Advanced parameters are not valid JSON: ${err instanceof Error ? err.message : 'parse error'}` };
  }
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return { ok: false, error: 'Advanced parameters must be a JSON object, for example {"n_jobs": 2}.' };
  }
  const out: Record<string, ParamValue | null> = {};
  for (const [key, v] of Object.entries(parsed as Record<string, unknown>)) {
    if (v !== null && typeof v !== 'number' && typeof v !== 'string' && typeof v !== 'boolean') {
      return { ok: false, error: `"${key}" must be a number, text, true/false or null. Nested objects and lists are not supported.` };
    }
    out[key] = v;
  }
  return { ok: true, value: out };
}

export interface MergedParams {
  /** Generated changed values plus the JSON keys. Meaningful only when `errors` is empty. */
  params: Record<string, ParamValue | null>;
  /** Keys set both in a changed field and in the JSON. */
  collisions: string[];
  /** JSON problems and collisions, in plain words. */
  errors: string[];
  /** Number of keys the JSON box contributes. */
  advancedKeys: number;
}

export function mergeParams(defs: ParamDefDto[], values: HyperparamValues, advancedText: string | undefined): MergedParams {
  const generated = changedParams(defs, values);
  const advanced = parseAdvanced(advancedText);
  if (!advanced.ok) return { params: generated, collisions: [], errors: [advanced.error], advancedKeys: 0 };
  const collisions = Object.keys(advanced.value).filter((k) => k in generated);
  const errors = collisions.map((k) => `"${k}" is set both in its field above and in the Advanced JSON. Clear one of them.`);
  return { params: { ...generated, ...advanced.value }, collisions, errors, advancedKeys: Object.keys(advanced.value).length };
}

/* ---------- Quick-fills (the old fixed presets, now just ordinary values) ---------- */

export type PresetId = 'fast' | 'balanced' | 'thorough';

export const PRESETS: { id: PresetId; label: string; hint: string }[] = [
  { id: 'fast', label: 'Fast', hint: 'Small and quick. Good for trying out a dataset.' },
  { id: 'balanced', label: 'Balanced', hint: 'Moderate size with regularisation. A sensible default for trading data.' },
  { id: 'thorough', label: 'Thorough', hint: 'Larger and slower, with gentler learning steps.' },
];

const PRESET_VALUES: Record<BuiltinAlgorithmId, Record<PresetId, Record<string, string>>> = {
  random_forest: {
    fast: { n_estimators: '50', max_depth: '6', min_samples_leaf: '5' },
    balanced: { n_estimators: '200', max_depth: '8', min_samples_leaf: '10', class_weight: 'balanced' },
    thorough: { n_estimators: '600', max_depth: '12', min_samples_leaf: '5', class_weight: 'balanced' },
  },
  gradient_boosting: {
    fast: { n_estimators: '60', learning_rate: '0.15', max_depth: '2', subsample: '0.8' },
    balanced: { n_estimators: '200', learning_rate: '0.05', max_depth: '3', subsample: '0.8' },
    thorough: { n_estimators: '600', learning_rate: '0.02', max_depth: '4', subsample: '0.7' },
  },
  logistic_regression: {
    fast: { C: '1', max_iter: '200' },
    balanced: { C: '0.5', max_iter: '500', class_weight: 'balanced' },
    thorough: { C: '0.1', max_iter: '2000', class_weight: 'balanced' },
  },
  xgboost: {
    fast: { n_estimators: '60', max_depth: '3', learning_rate: '0.2', subsample: '0.8', colsample_bytree: '0.8' },
    balanced: { n_estimators: '250', max_depth: '4', learning_rate: '0.05', subsample: '0.8', colsample_bytree: '0.8' },
    thorough: { n_estimators: '800', max_depth: '5', learning_rate: '0.02', subsample: '0.7', colsample_bytree: '0.7' },
  },
};

/** True when the quick-fills know this algorithm. */
export function hasQuickFill(algorithm: string): boolean {
  return algorithm in PRESET_VALUES;
}

/**
 * Field text a quick-fill sets for an algorithm (every key it names, even when equal to the default).
 * Keys the algorithm does not report are skipped so a quick-fill never invents a parameter.
 */
export function presetValues(algorithm: string, preset: PresetId, defs: ParamDefDto[]): HyperparamValues {
  const wanted = PRESET_VALUES[algorithm as BuiltinAlgorithmId]?.[preset] ?? {};
  const known = new Set(defs.map((d) => d.key));
  return Object.fromEntries(Object.entries(wanted).filter(([k]) => known.has(k)));
}

/**
 * Turn a stored model's hyperparameters back into form state: values for the generated fields, and a
 * JSON object text for everything else (except `random_state`, which has its own field).
 */
export function valuesFromStored(defs: ParamDefDto[], stored: Record<string, unknown> | undefined): { values: HyperparamValues; advanced: string; randomState: string } {
  const values: HyperparamValues = {};
  const extra: Record<string, unknown> = {};
  let randomState = '';
  const known = new Set(defs.map((d) => d.key));
  for (const [key, v] of Object.entries(stored ?? {})) {
    if (v === undefined || v === null) continue;
    if (key === 'random_state' && !known.has(key)) {
      if (typeof v === 'number' && Number.isInteger(v)) randomState = String(v);
    } else if (known.has(key)) {
      values[key] = String(v);
    } else if (typeof v === 'number' || typeof v === 'string' || typeof v === 'boolean') {
      extra[key] = v;
    }
  }
  return { values, advanced: Object.keys(extra).length > 0 ? JSON.stringify(extra, null, 2) : '', randomState };
}
