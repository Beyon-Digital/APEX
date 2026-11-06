import { create } from 'zustand';
import {
  BUILTIN_ALGORITHMS, CUSTOM_ALGORITHM_ID, findAlgorithm, mergeParams, paramDefsFor, valuesFromStored, validateParams,
  type AlgorithmId, type HyperparamValues, type HyperparamsByAlgorithm,
} from '../components/ml/hyperparams';
import {
  applyPreset, exportPresetsJson, loadPresets, nameProblem, newPresetId, parsePresetImport, persistPresets, snapshotPreset, uniqueName,
  type TrainingPreset,
} from '../components/ml/trainingPresets';
import {
  deleteEstimatorFile, getMlModel, listAlgorithms, listDatasets, listEstimatorFiles, listFeatureCatalog, listMLModels, saveEstimatorFile,
  trainMLModel,
} from '../lib/tauri';
import type {
  AlgorithmDto, DatasetInfoDto, EstimatorFileDto, FeatureDefDto, MLModelDto, MLTrainingRequestDto, ModelDetailDto, TradeMode,
  TradingSummaryDto,
} from '../lib/types';

/* ---------- Training settings (remembered between sessions) ---------- */

/** Column names every built dataset has (see the dataset preview). */
export const DATASET_TARGET_COLUMN = 'target';
export const DATASET_RETURN_COLUMN = 'fwd_ret';

export interface TrainSettings {
  mode: 'dataset' | 'csv';
  dataset: string;
  dataPath: string;
  targetColumn: string;
  featuresText: string;
  lagPeriods: string;
  algorithm: AlgorithmId;
  hyperparams: HyperparamsByAlgorithm;
  /** Kept as text so a half-typed value is not lost; validated on Train. */
  nSplits: string;
  /** Dataset features to train on; `null` means all of them. */
  selectedFeatures: string[] | null;
  compareAlgos: AlgorithmId[];
  /** Free-form "Advanced parameters" JSON text, per algorithm. */
  advanced: Record<string, string>;
  /** How the evaluation trades the predictions. */
  tradeMode: TradeMode;
  /** Share of the final rows kept out of cross-validation, 0-40. Text so a half-typed value survives. */
  holdoutPct: string;
  /** Empty means "let the trainer choose". */
  randomState: string;
  /** Custom estimator file used when the algorithm is "custom". */
  estimatorFile: string;
}

export const MAX_SPLITS = 20;
export const MAX_HOLDOUT_PCT = 40;
/** Same bounds as the backend (commands/ml.rs): random_state 0..=2147483647, lag periods 1..=250. */
export const MAX_RANDOM_STATE = 2147483647;
export const MAX_LAG_PERIOD = 250;

export const DEFAULT_SETTINGS: TrainSettings = {
  mode: 'dataset',
  dataset: '',
  dataPath: 'data/sample.csv',
  targetColumn: 'signal',
  featuresText: 'sma_20, sma_50, ema_12, rsi_14, macd_signal',
  lagPeriods: '',
  algorithm: 'random_forest',
  hyperparams: {},
  nSplits: '5',
  selectedFeatures: null,
  compareAlgos: ['random_forest', 'gradient_boosting'],
  advanced: {},
  tradeMode: 'long_flat',
  holdoutPct: '0',
  randomState: '',
  estimatorFile: '',
};

const SETTINGS_KEY = 'apex-ml-train-settings';

function loadSettings(): TrainSettings {
  try {
    const raw = typeof localStorage !== 'undefined' ? localStorage.getItem(SETTINGS_KEY) : null;
    if (!raw) return { ...DEFAULT_SETTINGS };
    const parsed = JSON.parse(raw) as Partial<TrainSettings>;
    return { ...DEFAULT_SETTINGS, ...parsed };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

function saveSettings(settings: TrainSettings) {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  } catch { /* storage may be unavailable */ }
}

export function parseList(text: string): string[] {
  return text.split(',').map((s) => s.trim()).filter(Boolean);
}

/**
 * The trainer stores generated lag columns (`<feature>_lag<n>`) in a model's feature names. Retraining
 * regenerates them from `lag_periods`, so only the base features may go back into the form.
 */
export function baseFeatureNames(names: string[], lags: number[] | undefined): string[] {
  const all = new Set(names);
  return names.filter((name) => {
    const m = /^(.+)_lag(\d+)$/.exec(name);
    return !(m && (lags ?? []).includes(Number(m[2])) && all.has(m[1]));
  });
}

/** Every feature column of a dataset: the catalog ones plus the user's own formulas. */
export function allDatasetFeatures(info: DatasetInfoDto): string[] {
  const names = [...info.features];
  for (const c of info.custom_features ?? []) if (!names.includes(c.name)) names.push(c.name);
  return names;
}

export function datasetFeatures(settings: TrainSettings, datasets: DatasetInfoDto[]): { info: DatasetInfoDto | undefined; features: string[] } {
  const info = datasets.find((d) => d.name === settings.dataset);
  if (!info) return { info, features: [] };
  const all = allDatasetFeatures(info);
  const chosen = settings.selectedFeatures;
  return { info, features: chosen ? all.filter((f) => chosen.includes(f)) : all };
}

/** What validation and requests need besides the settings themselves. */
export interface TrainContext {
  algorithms: AlgorithmDto[];
  /** Saved estimator file names; `undefined` while not loaded (the existence check is skipped). */
  estimatorNames?: string[];
}

const DEFAULT_CONTEXT: TrainContext = { algorithms: BUILTIN_ALGORITHMS };

function isBlank(text: string) {
  return text.trim() === '';
}

/** Everything wrong with the current settings, in the order the form shows it. Empty when ready. */
export function validateSettings(
  settings: TrainSettings, datasets: DatasetInfoDto[], algorithm: string = settings.algorithm, ctx: TrainContext = DEFAULT_CONTEXT,
): string[] {
  const problems: string[] = [];
  if (settings.mode === 'dataset') {
    const { info, features } = datasetFeatures(settings, datasets);
    if (!info) problems.push('Pick a dataset to train on (build one in the Data tab).');
    else if (features.length === 0) problems.push('Select at least one feature.');
  } else {
    if (!settings.dataPath.trim()) problems.push('Enter the CSV path.');
    if (!settings.targetColumn.trim()) problems.push('Enter the target column.');
    if (parseList(settings.featuresText).length === 0) problems.push('Enter at least one feature column.');
  }
  const lags = parseList(settings.lagPeriods);
  if (lags.some((l) => !/^\d+$/.test(l))) problems.push('Lag periods must be whole numbers, separated by commas.');
  else if (lags.some((l) => Number(l) < 1 || Number(l) > MAX_LAG_PERIOD)) problems.push(`lag_periods values must be between 1 and ${MAX_LAG_PERIOD}`);
  const splits = Number(settings.nSplits);
  if (isBlank(settings.nSplits) || !Number.isInteger(splits) || splits < 2 || splits > MAX_SPLITS) {
    problems.push(`CV splits must be a whole number from 2 to ${MAX_SPLITS}.`);
  }
  const holdout = Number(settings.holdoutPct);
  if (isBlank(settings.holdoutPct) || !Number.isFinite(holdout) || holdout < 0 || holdout > MAX_HOLDOUT_PCT) {
    problems.push(`Hold-out must be a number from 0 to ${MAX_HOLDOUT_PCT} percent.`);
  }
  if (!isBlank(settings.randomState)) {
    const rs = Number(settings.randomState);
    if (!/^\d+$/.test(settings.randomState.trim()) || rs > MAX_RANDOM_STATE) problems.push(`random_state must be an integer from 0 to ${MAX_RANDOM_STATE}`);
  }
  if (!findAlgorithm(ctx.algorithms, algorithm)) problems.push(`Algorithm "${algorithm}" is not available.`);
  const defs = paramDefsFor(ctx.algorithms, algorithm);
  problems.push(...Object.values(validateParams(defs, settings.hyperparams[algorithm] ?? {})));
  problems.push(...mergeParams(defs, settings.hyperparams[algorithm] ?? {}, settings.advanced[algorithm]).errors);
  if (algorithm === CUSTOM_ALGORITHM_ID) {
    if (isBlank(settings.estimatorFile)) problems.push('Choose or create an estimator file for the custom algorithm.');
    else if (ctx.estimatorNames && !ctx.estimatorNames.includes(settings.estimatorFile)) problems.push(`Estimator file ${settings.estimatorFile} does not exist. Save it or choose another.`);
  }
  if (!isBlank(settings.randomState)) {
    const merged = mergeParams(defs, settings.hyperparams[algorithm] ?? {}, settings.advanced[algorithm]);
    if ('random_state' in merged.params) problems.push('Random state is set both in its field and in the parameters. Use one of them.');
  }
  return problems;
}

/** The request sent to the trainer. Only parameters that differ from the defaults are included. */
export function buildRequest(
  settings: TrainSettings, datasets: DatasetInfoDto[], algorithm: string = settings.algorithm, ctx: TrainContext = DEFAULT_CONTEXT,
): MLTrainingRequestDto {
  const defs = paramDefsFor(ctx.algorithms, algorithm);
  const hyperparams = mergeParams(defs, settings.hyperparams[algorithm] ?? {}, settings.advanced[algorithm]).params;
  const base: MLTrainingRequestDto = {
    algorithm, hyperparams, n_splits: Number(settings.nSplits),
    trade_mode: settings.tradeMode, holdout_pct: Number(settings.holdoutPct),
    data_path: '', target_column: '', feature_columns: [], lag_periods: parseList(settings.lagPeriods).map(Number),
  };
  if (!isBlank(settings.randomState)) base.random_state = Number(settings.randomState);
  if (algorithm === CUSTOM_ALGORITHM_ID) base.estimator_file = settings.estimatorFile.trim();
  if (settings.mode === 'dataset') {
    const { info, features } = datasetFeatures(settings, datasets);
    return {
      ...base,
      data_path: info?.path ?? '',
      target_column: DATASET_TARGET_COLUMN,
      feature_columns: features,
      dataset: settings.dataset,
      return_column: DATASET_RETURN_COLUMN,
    };
  }
  return {
    ...base,
    data_path: settings.dataPath.trim(),
    target_column: settings.targetColumn.trim(),
    feature_columns: parseList(settings.featuresText),
  };
}

/* ---------- Training runs ---------- */

export interface RunResult {
  algorithm: string;
  ok: boolean;
  modelId?: string;
  metrics?: Record<string, number>;
  durationSecs?: number;
  trading?: TradingSummaryDto | null;
  error?: string;
}

export interface LeaderRow {
  algorithm: string;
  status: 'queued' | 'running' | 'done' | 'failed';
  result?: RunResult;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : typeof err === 'string' ? err : 'Training failed';
}

interface MLState {
  models: MLModelDto[];
  datasets: DatasetInfoDto[];
  catalog: FeatureDefDto[];
  settings: TrainSettings;
  /** Algorithms the trainer offers (the built-in list until the backend answers). */
  algorithms: AlgorithmDto[];
  /** Set when the backend list could not be loaded and the built-in one is shown instead. */
  algorithmsNotice: string | null;
  estimatorFiles: EstimatorFileDto[];
  /** Your saved training configurations. */
  presets: TrainingPreset[];

  isTraining: boolean;
  /** Epoch ms when the running job (or comparison) started. */
  trainingStartedAt: number | null;
  trainingLabel: string;
  trainingError: string | null;
  lastResult: RunResult | null;
  leaderboard: LeaderRow[];

  setModels: (models: MLModelDto[]) => void;
  addModel: (model: MLModelDto) => void;
  removeModel: (modelId: string) => void;
  setTraining: (training: boolean) => void;
  setTrainingError: (error: string | null) => void;
  updateModelStatus: (modelId: string, status: MLModelDto['status']) => void;

  refreshModels: () => Promise<void>;
  refreshDatasets: () => Promise<void>;
  loadCatalog: () => Promise<void>;
  loadAlgorithms: () => Promise<void>;
  fetchAlgorithms: () => Promise<void>;
  refreshEstimators: () => Promise<void>;
  /** Save a file; resolves to an error message, or `null` on success. */
  saveEstimator: (name: string, content: string) => Promise<{ file: EstimatorFileDto | null; error: string | null }>;
  removeEstimator: (name: string) => Promise<string | null>;

  savePreset: (name: string) => { ok: boolean; error?: string };
  /** Apply a preset to the form. `notes` lists what did not carry over. */
  loadPreset: (id: string) => { ok: boolean; notes: string[] };
  renamePreset: (id: string, name: string) => { ok: boolean; error?: string };
  deletePreset: (id: string) => void;
  exportPresets: () => string;
  importPresets: (text: string) => { ok: boolean; added: number; error?: string };

  patchSettings: (patch: Partial<TrainSettings>) => void;
  setHyperparams: (algorithm: AlgorithmId, values: HyperparamValues) => void;
  resetSettings: () => void;
  /** Fill the form from a trained model so it can be trained again with tweaks. */
  prefillFromModel: (detail: ModelDetailDto) => void;

  train: () => Promise<void>;
  compare: () => Promise<void>;
}

async function trainOne(settings: TrainSettings, datasets: DatasetInfoDto[], algorithm: string, ctx: TrainContext): Promise<RunResult> {
  try {
    const result = await trainMLModel(buildRequest(settings, datasets, algorithm, ctx));
    let trading: TradingSummaryDto | null = null;
    try {
      trading = (await getMlModel(result.model_id)).trading;
    } catch { /* the model trained; the trading summary is optional */ }
    return {
      algorithm, ok: true, modelId: result.model_id, metrics: result.metrics,
      durationSecs: result.duration_secs, trading,
    };
  } catch (err) {
    return { algorithm, ok: false, error: errorText(err) };
  }
}

/** The algorithm list and estimator names to validate against. */
export function trainContext(state: Pick<MLState, 'algorithms' | 'estimatorFiles'>): TrainContext {
  return { algorithms: state.algorithms, estimatorNames: state.estimatorFiles.length > 0 ? state.estimatorFiles.map((f) => f.name) : undefined };
}

let algorithmsLoading: Promise<void> | null = null;

export const useMLStore = create<MLState>((set, get) => ({
  models: [],
  datasets: [],
  catalog: [],
  settings: loadSettings(),
  algorithms: BUILTIN_ALGORITHMS,
  algorithmsNotice: null,
  estimatorFiles: [],
  presets: loadPresets(),

  isTraining: false,
  trainingStartedAt: null,
  trainingLabel: '',
  trainingError: null,
  lastResult: null,
  leaderboard: [],

  setModels: (models) => set({ models }),
  addModel: (model) => set((state) => ({ models: [...state.models, model] })),
  removeModel: (modelId) =>
    set((state) => ({ models: state.models.filter((m) => m.model_id !== modelId) })),
  setTraining: (training) => set({ isTraining: training }),
  setTrainingError: (error) => set({ trainingError: error }),
  updateModelStatus: (modelId, status) =>
    set((state) => ({
      models: state.models.map((m) => (m.model_id === modelId ? { ...m, status } : m)),
    })),

  refreshModels: async () => {
    try {
      const models = await listMLModels();
      if (Array.isArray(models)) set({ models });
    } catch { /* backend may not be available */ }
  },
  refreshDatasets: async () => {
    try {
      const datasets = await listDatasets();
      if (Array.isArray(datasets)) set({ datasets });
    } catch { /* backend may not be available */ }
  },
  loadCatalog: async () => {
    if (get().catalog.length > 0) return;
    try {
      const catalog = await listFeatureCatalog();
      if (Array.isArray(catalog)) set({ catalog });
    } catch { /* backend may not be available */ }
  },

  loadAlgorithms: async () => {
    // Several mounts (React strict mode, re-opened tiles) may ask at once: share one request.
    if (algorithmsLoading) return algorithmsLoading;
    algorithmsLoading = get().fetchAlgorithms().finally(() => { algorithmsLoading = null; });
    return algorithmsLoading;
  },
  fetchAlgorithms: async () => {
    try {
      const algorithms = await listAlgorithms();
      if (!Array.isArray(algorithms) || algorithms.length === 0) throw new Error('empty');
      set({ algorithms, algorithmsNotice: null });
      const { settings } = get();
      if (!algorithms.some((a) => a.id === settings.algorithm)) {
        get().patchSettings({ algorithm: algorithms.some((a) => a.id === 'random_forest') ? 'random_forest' : algorithms[0].id });
      }
    } catch {
      set({ algorithms: BUILTIN_ALGORITHMS, algorithmsNotice: 'Could not load the algorithm list from the backend, so the built-in list is shown. Other algorithms may be available once it responds.' });
    }
  },
  refreshEstimators: async () => {
    try {
      const files = await listEstimatorFiles();
      if (Array.isArray(files)) set({ estimatorFiles: files });
    } catch { /* backend may not be available */ }
  },
  saveEstimator: async (name, content) => {
    try {
      const file = await saveEstimatorFile(name, content);
      set((state) => ({ estimatorFiles: [...state.estimatorFiles.filter((f) => f.name !== file.name), file].sort((a, b) => a.name.localeCompare(b.name)) }));
      return { file, error: null };
    } catch (err) {
      return { file: null, error: errorText(err) };
    }
  },
  removeEstimator: async (name) => {
    try {
      await deleteEstimatorFile(name);
      set((state) => ({ estimatorFiles: state.estimatorFiles.filter((f) => f.name !== name) }));
      if (get().settings.estimatorFile === name) get().patchSettings({ estimatorFile: '' });
      return null;
    } catch (err) {
      return errorText(err);
    }
  },

  savePreset: (name) => {
    const { settings, presets } = get();
    const problem = nameProblem(name, presets);
    if (problem) return { ok: false, error: problem };
    const features = settings.mode === 'dataset' ? settings.selectedFeatures : parseList(settings.featuresText);
    const now = new Date().toISOString();
    const next = [...presets, { ...snapshotPreset(name, settings, features ? features : null), id: newPresetId(), savedAt: now }];
    persistPresets(next);
    set({ presets: next });
    return { ok: true };
  },
  loadPreset: (id) => {
    const { presets, settings, datasets, algorithms, estimatorFiles } = get();
    const preset = presets.find((p) => p.id === id);
    if (!preset) return { ok: false, notes: ['That preset no longer exists.'] };
    const info = datasets.find((d) => d.name === settings.dataset);
    const { patch, notes } = applyPreset(preset, {
      settings, availableFeatures: info ? allDatasetFeatures(info) : null, algorithms,
      estimatorNames: estimatorFiles.length > 0 ? estimatorFiles.map((f) => f.name) : null,
    });
    get().patchSettings(patch);
    return { ok: true, notes };
  },
  renamePreset: (id, name) => {
    const { presets } = get();
    const problem = nameProblem(name, presets, id);
    if (problem) return { ok: false, error: problem };
    const next = presets.map((p) => (p.id === id ? { ...p, name: name.trim() } : p));
    persistPresets(next);
    set({ presets: next });
    return { ok: true };
  },
  deletePreset: (id) => {
    const next = get().presets.filter((p) => p.id !== id);
    persistPresets(next);
    set({ presets: next });
  },
  exportPresets: () => exportPresetsJson(get().presets),
  importPresets: (text) => {
    const parsed = parsePresetImport(text);
    if (!parsed.ok) return { ok: false, added: 0, error: parsed.error };
    let next = [...get().presets];
    const now = new Date().toISOString();
    for (const draft of parsed.drafts) {
      next = [...next, { ...draft, name: uniqueName(draft.name, next), id: newPresetId(), savedAt: now }];
    }
    persistPresets(next);
    set({ presets: next });
    return { ok: true, added: parsed.drafts.length };
  },

  patchSettings: (patch) => {
    const settings = { ...get().settings, ...patch };
    saveSettings(settings);
    set({ settings });
  },
  setHyperparams: (algorithm, values) => {
    get().patchSettings({ hyperparams: { ...get().settings.hyperparams, [algorithm]: values } });
  },
  resetSettings: () => {
    saveSettings(DEFAULT_SETTINGS);
    set({ settings: { ...DEFAULT_SETTINGS } });
  },
  prefillFromModel: (detail) => {
    const { algorithms } = get();
    const algorithm = findAlgorithm(algorithms, detail.algorithm) ? detail.algorithm : 'random_forest';
    const stored = valuesFromStored(paramDefsFor(algorithms, algorithm), detail.hyperparams);
    const fromDataset = Boolean(detail.dataset);
    const baseFeatures = baseFeatureNames(detail.feature_names, detail.lag_periods);
    const holdoutPct = detail.holdout && detail.rows
      ? Math.min(MAX_HOLDOUT_PCT, Math.max(1, Math.round((100 * detail.holdout.rows) / detail.rows)))
      : 0;
    get().patchSettings({
      mode: fromDataset ? 'dataset' : 'csv',
      dataset: detail.dataset ?? '',
      dataPath: fromDataset ? get().settings.dataPath : detail.data_path,
      targetColumn: fromDataset ? get().settings.targetColumn : detail.target_column,
      featuresText: fromDataset ? get().settings.featuresText : baseFeatures.join(', '),
      lagPeriods: detail.lag_periods?.join(', ') ?? '',
      algorithm,
      hyperparams: { ...get().settings.hyperparams, [algorithm]: stored.values },
      advanced: { ...get().settings.advanced, [algorithm]: stored.advanced },
      nSplits: String(detail.n_splits),
      selectedFeatures: fromDataset ? baseFeatures : null,
      tradeMode: detail.trade_mode === 'long_short' ? 'long_short' : 'long_flat',
      holdoutPct: String(holdoutPct),
      randomState: stored.randomState,
      estimatorFile: detail.estimator_file ?? '',
    });
  },

  train: async () => {
    if (get().isTraining) return;
    const { settings, datasets } = get();
    const ctx = trainContext(get());
    if (validateSettings(settings, datasets, settings.algorithm, ctx).length > 0) return;
    set({
      isTraining: true, trainingStartedAt: Date.now(), trainingLabel: settings.algorithm,
      trainingError: null, lastResult: null, leaderboard: [],
    });
    const result = await trainOne(settings, datasets, settings.algorithm, ctx);
    set({
      isTraining: false, trainingStartedAt: null, trainingLabel: '',
      lastResult: result, trainingError: result.ok ? null : (result.error ?? 'Training failed'),
    });
    void get().refreshModels();
  },

  compare: async () => {
    if (get().isTraining) return;
    const { settings, datasets } = get();
    const ctx = trainContext(get());
    const algos = settings.compareAlgos.filter((a) => findAlgorithm(ctx.algorithms, a));
    if (algos.length === 0) return;
    if (algos.some((a) => validateSettings(settings, datasets, a, ctx).length > 0)) return;
    set({
      isTraining: true, trainingStartedAt: Date.now(), trainingError: null, lastResult: null,
      trainingLabel: `Comparing ${algos.length} algorithms`,
      leaderboard: algos.map((algorithm) => ({ algorithm, status: 'queued' as const })),
    });
    const update = (algorithm: string, row: Partial<LeaderRow>) =>
      set((state) => ({ leaderboard: state.leaderboard.map((r) => (r.algorithm === algorithm ? { ...r, ...row } : r)) }));
    for (const algorithm of algos) {
      update(algorithm, { status: 'running' });
      // One algorithm failing (for example xgboost missing) must not stop the rest of the queue.
      const result = await trainOne(settings, datasets, algorithm, ctx);
      update(algorithm, { status: result.ok ? 'done' : 'failed', result });
    }
    set({ isTraining: false, trainingStartedAt: null, trainingLabel: '' });
    void get().refreshModels();
  },
}));
