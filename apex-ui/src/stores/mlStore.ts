import { create } from 'zustand';
import {
  changedHyperparams, specsFor, validateHyperparams,
  type AlgorithmId, type HyperparamValues, type HyperparamsByAlgorithm,
} from '../components/ml/hyperparams';
import { getMlModel, listDatasets, listFeatureCatalog, listMLModels, trainMLModel } from '../lib/tauri';
import type {
  DatasetInfoDto, FeatureDefDto, MLModelDto, MLTrainingRequestDto, ModelDetailDto, TradingSummaryDto,
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
}

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

export function datasetFeatures(settings: TrainSettings, datasets: DatasetInfoDto[]): { info: DatasetInfoDto | undefined; features: string[] } {
  const info = datasets.find((d) => d.name === settings.dataset);
  if (!info) return { info, features: [] };
  const chosen = settings.selectedFeatures;
  return { info, features: chosen ? info.features.filter((f) => chosen.includes(f)) : info.features };
}

/** Everything wrong with the current settings, in the order the form shows it. Empty when ready. */
export function validateSettings(
  settings: TrainSettings, datasets: DatasetInfoDto[], algorithm: string = settings.algorithm,
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
  if (lags.some((l) => !/^\d+$/.test(l) || Number(l) < 1)) problems.push('Lag periods must be whole numbers of 1 or more, separated by commas.');
  const splits = Number(settings.nSplits);
  if (settings.nSplits.trim() === '' || !Number.isInteger(splits) || splits < 2 || splits > 10) {
    problems.push('CV splits must be a whole number from 2 to 10.');
  }
  problems.push(...Object.values(validateHyperparams(algorithm, settings.hyperparams[algorithm as AlgorithmId] ?? {})));
  return problems;
}

/** The request sent to the trainer. Only hyperparameters that differ from the defaults are included. */
export function buildRequest(settings: TrainSettings, datasets: DatasetInfoDto[], algorithm: string = settings.algorithm): MLTrainingRequestDto {
  const hyperparams = changedHyperparams(algorithm, settings.hyperparams[algorithm as AlgorithmId] ?? {});
  const base = { algorithm, hyperparams, n_splits: Number(settings.nSplits) };
  if (settings.mode === 'dataset') {
    const { info, features } = datasetFeatures(settings, datasets);
    return {
      ...base,
      data_path: info?.path ?? '',
      target_column: DATASET_TARGET_COLUMN,
      feature_columns: features,
      lag_periods: parseList(settings.lagPeriods).map(Number),
      dataset: settings.dataset,
      return_column: DATASET_RETURN_COLUMN,
    };
  }
  return {
    ...base,
    data_path: settings.dataPath.trim(),
    target_column: settings.targetColumn.trim(),
    feature_columns: parseList(settings.featuresText),
    lag_periods: parseList(settings.lagPeriods).map(Number),
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

  patchSettings: (patch: Partial<TrainSettings>) => void;
  setHyperparams: (algorithm: AlgorithmId, values: HyperparamValues) => void;
  resetSettings: () => void;
  /** Fill the form from a trained model so it can be trained again with tweaks. */
  prefillFromModel: (detail: ModelDetailDto) => void;

  train: () => Promise<void>;
  compare: () => Promise<void>;
}

async function trainOne(settings: TrainSettings, datasets: DatasetInfoDto[], algorithm: string): Promise<RunResult> {
  try {
    const result = await trainMLModel(buildRequest(settings, datasets, algorithm));
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

export const useMLStore = create<MLState>((set, get) => ({
  models: [],
  datasets: [],
  catalog: [],
  settings: loadSettings(),

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
    const algorithm = (specsFor(detail.algorithm).length > 0 ? detail.algorithm : 'random_forest') as AlgorithmId;
    const values: HyperparamValues = {};
    for (const spec of specsFor(algorithm)) {
      const v = detail.hyperparams?.[spec.key];
      if (v !== undefined && v !== null) values[spec.key] = String(v);
    }
    const fromDataset = Boolean(detail.dataset);
    const baseFeatures = baseFeatureNames(detail.feature_names, detail.lag_periods);
    get().patchSettings({
      mode: fromDataset ? 'dataset' : 'csv',
      dataset: detail.dataset ?? '',
      dataPath: fromDataset ? get().settings.dataPath : detail.data_path,
      targetColumn: fromDataset ? get().settings.targetColumn : detail.target_column,
      featuresText: fromDataset ? get().settings.featuresText : baseFeatures.join(', '),
      lagPeriods: detail.lag_periods?.join(', ') ?? '',
      algorithm,
      hyperparams: { ...get().settings.hyperparams, [algorithm]: values },
      nSplits: String(detail.n_splits),
      selectedFeatures: fromDataset ? baseFeatures : null,
    });
  },

  train: async () => {
    if (get().isTraining) return;
    const { settings, datasets } = get();
    if (validateSettings(settings, datasets).length > 0) return;
    set({
      isTraining: true, trainingStartedAt: Date.now(), trainingLabel: settings.algorithm,
      trainingError: null, lastResult: null, leaderboard: [],
    });
    const result = await trainOne(settings, datasets, settings.algorithm);
    set({
      isTraining: false, trainingStartedAt: null, trainingLabel: '',
      lastResult: result, trainingError: result.ok ? null : (result.error ?? 'Training failed'),
    });
    void get().refreshModels();
  },

  compare: async () => {
    if (get().isTraining) return;
    const { settings, datasets } = get();
    const algos = settings.compareAlgos;
    if (algos.length === 0) return;
    if (algos.some((a) => validateSettings(settings, datasets, a).length > 0)) return;
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
      const result = await trainOne(settings, datasets, algorithm);
      update(algorithm, { status: result.ok ? 'done' : 'failed', result });
    }
    set({ isTraining: false, trainingStartedAt: null, trainingLabel: '' });
    void get().refreshModels();
  },
}));
