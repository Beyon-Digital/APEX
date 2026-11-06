/**
 * Algorithms and their tunable hyperparameters for the training form.
 *
 * The keys mirror the backend allow-list, so every key listed here is accepted by the trainer.
 * Values are kept as the raw text the user typed (`HyperparamValues`) and are only converted to
 * numbers when a training request is built, so a half-typed "0." never gets lost while editing.
 */

export type AlgorithmId = 'random_forest' | 'gradient_boosting' | 'logistic_regression' | 'xgboost';

export interface HyperparamSpec {
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

export interface AlgorithmInfo {
  id: AlgorithmId;
  label: string;
  description: string;
}

export const ALGORITHMS: AlgorithmInfo[] = [
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

export const HYPERPARAMS: Record<AlgorithmId, HyperparamSpec[]> = {
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
export type HyperparamsByAlgorithm = Partial<Record<AlgorithmId, HyperparamValues>>;

export function specsFor(algorithm: string): HyperparamSpec[] {
  return HYPERPARAMS[algorithm as AlgorithmId] ?? [];
}

export function algorithmInfo(algorithm: string): AlgorithmInfo | undefined {
  return ALGORITHMS.find((a) => a.id === algorithm);
}

function defaultText(spec: HyperparamSpec): string {
  return spec.default === null ? '' : String(spec.default);
}

/** The text a field shows: what the user set, or the default. */
export function displayValue(spec: HyperparamSpec, values: HyperparamValues): string {
  return values[spec.key] ?? defaultText(spec);
}

const INVALID = Symbol('invalid');

function parseValue(spec: HyperparamSpec, text: string): number | string | null | typeof INVALID {
  const trimmed = text.trim();
  if (spec.type === 'select') {
    return spec.options?.some((o) => o.value === trimmed) ? trimmed : INVALID;
  }
  if (trimmed === '') return spec.default === null ? null : INVALID;
  const n = Number(trimmed);
  if (!Number.isFinite(n)) return INVALID;
  if (spec.type === 'int' && !Number.isInteger(n)) return INVALID;
  if (spec.min !== undefined && n < spec.min) return INVALID;
  if (spec.max !== undefined && n > spec.max) return INVALID;
  return n;
}

/** Per-key error messages for the current text; empty when everything is valid. */
export function validateHyperparams(algorithm: string, values: HyperparamValues): Record<string, string> {
  const errors: Record<string, string> = {};
  for (const spec of specsFor(algorithm)) {
    const text = values[spec.key];
    if (text === undefined) continue;
    if (parseValue(spec, text) !== INVALID) continue;
    if (spec.type === 'select') {
      errors[spec.key] = `${spec.label}: pick one of the listed options.`;
    } else {
      const kind = spec.type === 'int' ? 'a whole number' : 'a number';
      const range = spec.min !== undefined && spec.max !== undefined ? ` between ${spec.min} and ${spec.max}` : '';
      errors[spec.key] = `${spec.label} must be ${kind}${range}.`;
    }
  }
  return errors;
}

/** True when this field holds something other than its default. */
export function isChanged(spec: HyperparamSpec, values: HyperparamValues): boolean {
  const text = values[spec.key];
  if (text === undefined) return false;
  const parsed = parseValue(spec, text);
  if (parsed === INVALID) return true; // being edited into something invalid still counts as touched
  if (spec.type === 'select') return text !== defaultText(spec);
  return parsed !== spec.default;
}

/**
 * The hyperparameters to send: only valid values that differ from the default.
 * Everything else is left out so the trainer keeps its own defaults.
 */
export function changedHyperparams(algorithm: string, values: HyperparamValues): Record<string, number | string> {
  const out: Record<string, number | string> = {};
  for (const spec of specsFor(algorithm)) {
    const text = values[spec.key];
    if (text === undefined) continue;
    const parsed = parseValue(spec, text);
    if (parsed === INVALID || parsed === null) continue;
    if (spec.type === 'select') {
      if (parsed !== '' && text !== defaultText(spec)) out[spec.key] = parsed as string;
    } else if (parsed !== spec.default) {
      out[spec.key] = parsed as number;
    }
  }
  return out;
}

export function changedCount(algorithm: string, values: HyperparamValues): number {
  return specsFor(algorithm).filter((s) => isChanged(s, values)).length;
}

export type PresetId = 'fast' | 'balanced' | 'thorough';

export const PRESETS: { id: PresetId; label: string; hint: string }[] = [
  { id: 'fast', label: 'Fast', hint: 'Small and quick. Good for trying out a dataset.' },
  { id: 'balanced', label: 'Balanced', hint: 'Moderate size with regularisation. A sensible default for trading data.' },
  { id: 'thorough', label: 'Thorough', hint: 'Larger and slower, with gentler learning steps.' },
];

const PRESET_VALUES: Record<AlgorithmId, Record<PresetId, Record<string, string>>> = {
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

/** Field text a preset sets for an algorithm (every key the preset names, even when equal to the default). */
export function presetValues(algorithm: string, preset: PresetId): HyperparamValues {
  return { ...(PRESET_VALUES[algorithm as AlgorithmId]?.[preset] ?? {}) };
}

/** Turn a stored model's hyperparameters back into field text, keeping only known keys. */
export function valuesFromModel(algorithm: string, stored: Record<string, unknown>): HyperparamValues {
  const out: HyperparamValues = {};
  for (const spec of specsFor(algorithm)) {
    const v = stored[spec.key];
    if (v === undefined || v === null) continue;
    out[spec.key] = String(v);
  }
  return out;
}
