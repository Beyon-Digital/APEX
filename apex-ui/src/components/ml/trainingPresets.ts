import type { AlgorithmDto, TradeMode } from '../../lib/types';
import type { HyperparamValues } from './hyperparams';
import type { TrainSettings } from '../../stores/mlStore';

/**
 * Your own saved training configurations. Stored in localStorage and exportable as JSON.
 * A preset holds the choices that make a training run repeatable, not the dataset itself, so it can
 * be loaded onto another dataset: features the new dataset lacks are dropped with a note.
 */

export interface TrainingPreset {
  id: string;
  name: string;
  savedAt: string;
  algorithm: string;
  /** Raw field text for the generated parameters (only keys the user set). */
  params: HyperparamValues;
  /** The Advanced JSON box, verbatim. */
  advanced: string;
  nSplits: string;
  tradeMode: TradeMode;
  holdoutPct: string;
  randomState: string;
  /** Feature names; `null` means every feature of whatever dataset is loaded. */
  features: string[] | null;
  lagPeriods: string;
  estimatorFile: string;
}

export type PresetDraft = Omit<TrainingPreset, 'id' | 'savedAt'>;

export const PRESETS_KEY = 'apex-ml-user-presets';
export const EXPORT_FORMAT = 'apex-training-presets';
const MAX_NAME = 60;

export function loadPresets(): TrainingPreset[] {
  try {
    const raw = typeof localStorage !== 'undefined' ? localStorage.getItem(PRESETS_KEY) : null;
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    const out: TrainingPreset[] = [];
    for (const item of parsed) {
      const draft = sanitize(item);
      if (typeof draft === 'string') continue;
      const o = item as { id?: unknown; savedAt?: unknown };
      out.push({ ...draft, id: typeof o.id === 'string' ? o.id : newPresetId(), savedAt: typeof o.savedAt === 'string' ? o.savedAt : new Date().toISOString() });
    }
    return out;
  } catch {
    return [];
  }
}

export function persistPresets(presets: TrainingPreset[]): void {
  try {
    localStorage.setItem(PRESETS_KEY, JSON.stringify(presets));
  } catch { /* storage may be unavailable */ }
}

let counter = 0;
export function newPresetId(): string {
  counter += 1;
  return `up_${Date.now().toString(36)}_${counter}`;
}

/** Stable text for test ids and keys: "My RF (fast)" -> "my-rf-fast". */
export function presetSlug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}

/** Why this name cannot be used, or `null` when it is fine. Names are unique ignoring case. */
export function nameProblem(name: string, existing: TrainingPreset[], exceptId?: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return 'Give the preset a name.';
  if (trimmed.length > MAX_NAME) return `Names are limited to ${MAX_NAME} characters.`;
  if (existing.some((p) => p.id !== exceptId && p.name.toLowerCase() === trimmed.toLowerCase())) return `A preset called "${trimmed}" already exists.`;
  return null;
}

/** `name`, or "name (2)", "name (3)" ... when taken. */
export function uniqueName(name: string, existing: TrainingPreset[]): string {
  const base = name.trim().slice(0, MAX_NAME - 4) || 'Preset';
  const taken = new Set(existing.map((p) => p.name.toLowerCase()));
  if (!taken.has(base.toLowerCase())) return base;
  for (let n = 2; ; n += 1) {
    const candidate = `${base} (${n})`;
    if (!taken.has(candidate.toLowerCase())) return candidate;
  }
}

/* ---------- Capture and apply ---------- */

export function snapshotPreset(name: string, settings: TrainSettings, features: string[] | null): PresetDraft {
  const params: HyperparamValues = {};
  for (const [k, v] of Object.entries(settings.hyperparams[settings.algorithm] ?? {})) {
    if (v.trim() !== '') params[k] = v;
  }
  return {
    name: name.trim(),
    algorithm: settings.algorithm,
    params,
    advanced: settings.advanced[settings.algorithm] ?? '',
    nSplits: settings.nSplits,
    tradeMode: settings.tradeMode,
    holdoutPct: settings.holdoutPct,
    randomState: settings.randomState,
    features,
    lagPeriods: settings.lagPeriods,
    estimatorFile: settings.estimatorFile,
  };
}

export interface ApplyContext {
  settings: TrainSettings;
  /** Every feature name of the selected dataset, or `null` when no dataset is selected. */
  availableFeatures: string[] | null;
  algorithms: AlgorithmDto[];
  /** Saved estimator file names, or `null` while they are not loaded. */
  estimatorNames: string[] | null;
}

export interface ApplyResult {
  patch: Partial<TrainSettings>;
  /** Things that did not carry over, in plain words. */
  notes: string[];
}

export function applyPreset(preset: TrainingPreset, ctx: ApplyContext): ApplyResult {
  const { settings } = ctx;
  const notes: string[] = [];
  const algo = ctx.algorithms.find((a) => a.id === preset.algorithm);
  const algorithm = algo ? preset.algorithm : settings.algorithm;
  let params = preset.params;
  let advanced = preset.advanced;
  if (!algo) {
    notes.push(`Algorithm "${preset.algorithm}" is not available here, so the algorithm and its parameters were left as they are.`);
    params = settings.hyperparams[algorithm] ?? {};
    advanced = settings.advanced[algorithm] ?? '';
  } else {
    const known = new Set(algo.params.map((p) => p.key));
    const unknown = Object.keys(preset.params).filter((k) => !known.has(k));
    if (unknown.length > 0) {
      params = Object.fromEntries(Object.entries(preset.params).filter(([k]) => known.has(k)));
      notes.push(`${unknown.length} parameter${unknown.length === 1 ? '' : 's'} this algorithm does not offer ${unknown.length === 1 ? 'was' : 'were'} dropped: ${unknown.join(', ')}.`);
    }
  }

  const patch: Partial<TrainSettings> = {
    algorithm,
    hyperparams: { ...settings.hyperparams, [algorithm]: { ...params } },
    advanced: { ...settings.advanced, [algorithm]: advanced },
    nSplits: preset.nSplits,
    tradeMode: preset.tradeMode,
    holdoutPct: preset.holdoutPct,
    randomState: preset.randomState,
    lagPeriods: preset.lagPeriods,
  };

  if (algorithm === 'custom' && preset.estimatorFile) {
    patch.estimatorFile = preset.estimatorFile;
    if (ctx.estimatorNames && !ctx.estimatorNames.includes(preset.estimatorFile)) {
      notes.push(`Estimator file ${preset.estimatorFile} does not exist here. Save it again or pick another file.`);
    }
  }

  if (preset.features !== null) {
    if (settings.mode === 'csv') {
      patch.featuresText = preset.features.join(', ');
    } else if (ctx.availableFeatures === null) {
      patch.selectedFeatures = null;
    } else {
      const have = new Set(ctx.availableFeatures);
      const kept = preset.features.filter((f) => have.has(f));
      const dropped = preset.features.filter((f) => !have.has(f));
      if (dropped.length > 0) {
        notes.push(kept.length === 0
          ? `None of the preset's ${dropped.length} feature${dropped.length === 1 ? '' : 's'} exist in this dataset (${dropped.join(', ')}), so all of its features are selected.`
          : `${dropped.length} feature${dropped.length === 1 ? '' : 's'} from the preset ${dropped.length === 1 ? 'is' : 'are'} not in this dataset and ${dropped.length === 1 ? 'was' : 'were'} dropped: ${dropped.join(', ')}.`);
      }
      patch.selectedFeatures = kept.length === 0 ? null : kept;
    }
  } else if (settings.mode === 'dataset') {
    patch.selectedFeatures = null;
  }
  return { patch, notes };
}

/* ---------- Import and export ---------- */

const TRADE_MODES: TradeMode[] = ['long_flat', 'long_short'];

function asText(v: unknown): string | null {
  if (typeof v === 'string') return v;
  if (typeof v === 'number' && Number.isFinite(v)) return String(v);
  return null;
}

/** Check one preset object. Returns the cleaned preset, or a message saying what is wrong. */
function sanitize(item: unknown): PresetDraft | string {
  if (item === null || typeof item !== 'object' || Array.isArray(item)) return 'is not an object';
  const o = item as Record<string, unknown>;
  if (typeof o.name !== 'string' || !o.name.trim()) return 'has no name';
  if (o.name.trim().length > MAX_NAME) return `has a name longer than ${MAX_NAME} characters`;
  if (typeof o.algorithm !== 'string' || !o.algorithm.trim()) return 'has no algorithm';

  const params: HyperparamValues = {};
  if (o.params !== undefined) {
    if (o.params === null || typeof o.params !== 'object' || Array.isArray(o.params)) return '"params" must be an object';
    for (const [k, v] of Object.entries(o.params as Record<string, unknown>)) {
      const text = typeof v === 'boolean' ? String(v) : asText(v);
      if (text === null) return `"params.${k}" must be a number, text or true/false`;
      params[k] = text;
    }
  }
  const advanced = o.advanced === undefined ? '' : typeof o.advanced === 'string' ? o.advanced : null;
  if (advanced === null) return '"advanced" must be text';

  const nSplits = o.nSplits === undefined ? '5' : asText(o.nSplits);
  if (nSplits === null) return '"nSplits" must be a number';
  const holdoutPct = o.holdoutPct === undefined ? '0' : asText(o.holdoutPct);
  if (holdoutPct === null) return '"holdoutPct" must be a number';
  const randomState = o.randomState === undefined || o.randomState === null ? '' : asText(o.randomState);
  if (randomState === null) return '"randomState" must be a number or empty';
  const tradeMode = o.tradeMode === undefined ? 'long_flat' : o.tradeMode;
  if (!TRADE_MODES.includes(tradeMode as TradeMode)) return '"tradeMode" must be long_flat or long_short';

  let features: string[] | null = null;
  if (o.features !== undefined && o.features !== null) {
    if (!Array.isArray(o.features) || o.features.some((f) => typeof f !== 'string')) return '"features" must be a list of names or null';
    features = [...new Set(o.features as string[])];
  }
  const lagPeriods = o.lagPeriods === undefined ? '' : asText(o.lagPeriods);
  if (lagPeriods === null) return '"lagPeriods" must be text like "1,5"';
  const estimatorFile = o.estimatorFile === undefined || o.estimatorFile === null ? '' : typeof o.estimatorFile === 'string' ? o.estimatorFile : null;
  if (estimatorFile === null) return '"estimatorFile" must be a file name';

  return {
    name: o.name.trim(), algorithm: o.algorithm.trim(), params, advanced, nSplits, tradeMode: tradeMode as TradeMode,
    holdoutPct, randomState, features, lagPeriods, estimatorFile,
  };
}

export function exportPresetsJson(presets: TrainingPreset[]): string {
  const clean = presets.map(({ name, algorithm, params, advanced, nSplits, tradeMode, holdoutPct, randomState, features, lagPeriods, estimatorFile }) => (
    { name, algorithm, params, advanced, nSplits, tradeMode, holdoutPct, randomState, features, lagPeriods, estimatorFile }
  ));
  return JSON.stringify({ format: EXPORT_FORMAT, version: 1, presets: clean }, null, 2);
}

export type ImportParse = { ok: true; drafts: PresetDraft[] } | { ok: false; error: string };

/** Accepts an export (`{format, presets: [...]}`), a bare list, or one preset object. */
export function parsePresetImport(text: string): ImportParse {
  if (!text.trim()) return { ok: false, error: 'Paste the exported JSON first.' };
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (err) {
    return { ok: false, error: `That is not valid JSON: ${err instanceof Error ? err.message : 'parse error'}` };
  }
  let list: unknown[];
  if (Array.isArray(parsed)) list = parsed;
  else if (parsed && typeof parsed === 'object' && Array.isArray((parsed as { presets?: unknown }).presets)) list = (parsed as { presets: unknown[] }).presets;
  else if (parsed && typeof parsed === 'object' && 'name' in (parsed as object)) list = [parsed];
  else return { ok: false, error: 'No presets found. Expected the JSON produced by Export.' };
  if (list.length === 0) return { ok: false, error: 'The file contains no presets.' };
  if (list.length > 100) return { ok: false, error: 'Too many presets in one import (limit 100).' };
  const drafts: PresetDraft[] = [];
  for (let i = 0; i < list.length; i += 1) {
    const draft = sanitize(list[i]);
    if (typeof draft === 'string') return { ok: false, error: `Preset ${i + 1} ${draft}.` };
    drafts.push(draft);
  }
  return { ok: true, drafts };
}
