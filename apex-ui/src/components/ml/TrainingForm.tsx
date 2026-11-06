import React, { useEffect, useMemo, useState } from 'react';
import { openItem } from '../../stores/navStore';
import {
  DATASET_RETURN_COLUMN, MAX_HOLDOUT_PCT, MAX_SPLITS, RECIPE_ALGORITHM_ID, allDatasetFeatures, datasetFeatures, isRecipeQueueId, isTrainable,
  recipeNameFor, recipeQueueId, trainContext, useMLStore, validateSettings,
  type RunResult, type TrainSettings,
} from '../../stores/mlStore';
import type { TradeMode } from '../../lib/types';
import { AlgorithmBuilder } from './AlgorithmBuilder';
import { EstimatorEditor } from './EstimatorEditor';
import { ParamEditor } from './ParamEditor';
import {
  CUSTOM_ALGORITHM_ID, PRESETS, changedCount, findAlgorithm, hasQuickFill, mergeParams, paramDefsFor, presetValues, type PresetId,
} from './hyperparams';
import { RecipeDiagram } from './RecipeDiagram';
import { TEMPLATES, nodesFromRecipe, summarizeRecipe } from './recipeModel';
import { presetSlug } from './trainingPresets';

const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';
const primaryBtnCls =
  'px-3 py-1.5 text-xs font-mono font-semibold bg-accent text-on-accent hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed';
const sectionCls = 'border border-[var(--border-color)] bg-surface-1 p-3 space-y-2';
const headCls = 'text-[11px] font-semibold uppercase tracking-wider text-text-secondary';

const pct = (v: number | undefined) => (v === undefined || !Number.isFinite(v) ? 'n/a' : `${(v * 100).toFixed(1)}%`);
const signed = (v: number) => `${v >= 0 ? '+' : ''}${v.toFixed(2)}%`;

export interface TrainingFormProps {
  /** Called when the user wants to see the datasets (e.g. none exist yet). */
  onOpenData: () => void;
}

function useElapsed(startedAt: number | null): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (startedAt === null) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(id);
  }, [startedAt]);
  return startedAt === null ? 0 : Math.max(0, Math.floor((now - startedAt) / 1000));
}

const slug = presetSlug;

/** Value of the "Build an algorithm..." entry in the algorithm select. */
const BUILD_OPTION = '__build__';

/** Your saved training configurations: save, load, rename, delete, export, import. */
const PresetPanel: React.FC = () => {
  const presets = useMLStore((s) => s.presets);
  const savePreset = useMLStore((s) => s.savePreset);
  const loadPreset = useMLStore((s) => s.loadPreset);
  const renamePreset = useMLStore((s) => s.renamePreset);
  const deletePreset = useMLStore((s) => s.deletePreset);
  const exportPresets = useMLStore((s) => s.exportPresets);
  const importPresets = useMLStore((s) => s.importPresets);

  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [notes, setNotes] = useState<string[]>([]);
  const [info, setInfo] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; text: string } | null>(null);
  const [renameError, setRenameError] = useState<string | null>(null);
  const [io, setIo] = useState('');
  const [ioMessage, setIoMessage] = useState<{ ok: boolean; text: string } | null>(null);

  const save = () => {
    const result = savePreset(name);
    if (!result.ok) { setError(result.error ?? 'Could not save'); return; }
    setInfo(`Saved "${name.trim()}".`);
    setError(null); setNotes([]); setName('');
  };

  const load = (id: string, presetName: string) => {
    const result = loadPreset(id);
    setError(null);
    setNotes(result.notes);
    setInfo(result.ok ? `Loaded "${presetName}".` : null);
  };

  const commitRename = () => {
    if (!renaming) return;
    const result = renamePreset(renaming.id, renaming.text);
    if (!result.ok) { setRenameError(result.error ?? 'Could not rename'); return; }
    setRenaming(null); setRenameError(null);
  };

  const doExport = async () => {
    const text = exportPresets();
    setIo(text);
    try {
      await navigator.clipboard.writeText(text);
      setIoMessage({ ok: true, text: 'Copied to the clipboard. The JSON is also shown below.' });
    } catch {
      setIoMessage({ ok: true, text: 'Could not use the clipboard. Copy the JSON below instead.' });
    }
  };

  const doImport = () => {
    const result = importPresets(io);
    setIoMessage(result.ok
      ? { ok: true, text: `Imported ${result.added} preset${result.added === 1 ? '' : 's'}.` }
      : { ok: false, text: result.error ?? 'Import failed' });
  };

  return (
    <section className={sectionCls} data-testid="ml-user-presets">
      <div className={headCls}>Saved configurations</div>
      <div className="text-[11px] text-text-muted">
        Save everything on this tab except the dataset (algorithm, parameters, evaluation settings, features by name, lags) and load it later, also onto another dataset.
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <input className={`${inputCls} w-56`} value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') save(); }} placeholder="Name this configuration" aria-label="Preset name" data-testid="ml-upreset-name" />
        <button type="button" className={btnCls} onClick={save} disabled={!name.trim()} data-testid="ml-upreset-save">Save current</button>
        {error && <span className="text-[11px] text-bear" data-testid="ml-upreset-error">{error}</span>}
        {!error && info && <span className="text-[11px] text-bull" data-testid="ml-upreset-info">{info}</span>}
      </div>
      {notes.length > 0 && (
        <ul className="list-disc border border-warning/40 bg-warning/10 py-1 pl-6 pr-2 text-[11px] text-warning" data-testid="ml-upreset-notes">
          {notes.map((n) => <li key={n}>{n}</li>)}
        </ul>
      )}
      {presets.length === 0 ? (
        <div className="text-[11px] text-text-muted" data-testid="ml-upreset-empty">No saved configurations yet.</div>
      ) : (
        <ul className="space-y-1" data-testid="ml-upreset-list">
          {presets.map((p) => (
            <li key={p.id} className="flex flex-wrap items-center gap-2 border border-[var(--border-color)] bg-surface-0 px-2 py-1" data-testid={`ml-upreset-row-${slug(p.name)}`}>
              {renaming?.id === p.id ? (
                <>
                  <input className={`${inputCls} w-48`} value={renaming.text} onChange={(e) => setRenaming({ id: p.id, text: e.target.value })} onKeyDown={(e) => { if (e.key === 'Enter') commitRename(); if (e.key === 'Escape') { setRenaming(null); setRenameError(null); } }} aria-label="New preset name" data-testid="ml-upreset-rename-input" autoFocus />
                  <button type="button" className={btnCls} onClick={commitRename} data-testid="ml-upreset-rename-ok">OK</button>
                  <button type="button" className={btnCls} onClick={() => { setRenaming(null); setRenameError(null); }} data-testid="ml-upreset-rename-cancel">Cancel</button>
                  {renameError && <span className="text-[11px] text-bear" data-testid="ml-upreset-rename-error">{renameError}</span>}
                </>
              ) : (
                <>
                  <span className="text-xs text-text-primary" data-testid={`ml-upreset-name-${slug(p.name)}`}>{p.name}</span>
                  <span className="font-mono text-[10px] text-text-muted">{p.algorithm}, {p.nSplits} splits, {p.tradeMode === 'long_short' ? 'long/short' : 'long/flat'}</span>
                  <span className="ml-auto flex gap-2 text-[11px] font-mono text-accent">
                    <button type="button" className="hover:underline" onClick={() => load(p.id, p.name)} data-testid={`ml-upreset-load-${slug(p.name)}`}>load</button>
                    <button type="button" className="hover:underline" onClick={() => { setRenaming({ id: p.id, text: p.name }); setRenameError(null); }} data-testid={`ml-upreset-rename-${slug(p.name)}`}>rename</button>
                    <button type="button" className="text-bear hover:underline" onClick={() => deletePreset(p.id)} data-testid={`ml-upreset-delete-${slug(p.name)}`}>delete</button>
                  </span>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
      <div className="space-y-1 border-t border-[var(--border-color)] pt-2">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[11px] text-text-secondary">Share</span>
          <button type="button" className={btnCls} onClick={() => void doExport()} disabled={presets.length === 0} data-testid="ml-upreset-export">Export all (copy)</button>
          <button type="button" className={btnCls} onClick={doImport} disabled={!io.trim()} data-testid="ml-upreset-import">Import pasted JSON</button>
          {ioMessage && <span className={`text-[11px] ${ioMessage.ok ? 'text-bull' : 'text-bear'}`} data-testid="ml-upreset-io-message">{ioMessage.text}</span>}
        </div>
        <textarea className={`${inputCls} block h-20 w-full font-mono`} value={io} onChange={(e) => { setIo(e.target.value); setIoMessage(null); }} placeholder="Export fills this box. To import, paste exported JSON here." spellCheck={false} aria-label="Preset JSON" data-testid="ml-upreset-io" />
      </div>
    </section>
  );
};

export const TrainingForm: React.FC<TrainingFormProps> = ({ onOpenData }) => {
  const settings = useMLStore((s) => s.settings);
  const datasets = useMLStore((s) => s.datasets);
  const algorithms = useMLStore((s) => s.algorithms);
  const algorithmsNotice = useMLStore((s) => s.algorithmsNotice);
  const estimatorFiles = useMLStore((s) => s.estimatorFiles);
  const patch = useMLStore((s) => s.patchSettings);
  const setHyperparams = useMLStore((s) => s.setHyperparams);
  const loadAlgorithms = useMLStore((s) => s.loadAlgorithms);
  const refreshEstimators = useMLStore((s) => s.refreshEstimators);
  const isTraining = useMLStore((s) => s.isTraining);
  const startedAt = useMLStore((s) => s.trainingStartedAt);
  const trainingLabel = useMLStore((s) => s.trainingLabel);
  const trainingError = useMLStore((s) => s.trainingError);
  const lastResult = useMLStore((s) => s.lastResult);
  const leaderboard = useMLStore((s) => s.leaderboard);
  const train = useMLStore((s) => s.train);
  const compare = useMLStore((s) => s.compare);
  const recipes = useMLStore((s) => s.recipes);
  const recipesLoaded = useMLStore((s) => s.recipesLoaded);
  const blocks = useMLStore((s) => s.blocks);
  const builder = useMLStore((s) => s.builder);
  const refreshRecipes = useMLStore((s) => s.refreshRecipes);
  const loadBlocks = useMLStore((s) => s.loadBlocks);
  const openBuilder = useMLStore((s) => s.openBuilder);

  useEffect(() => { void loadAlgorithms(); void refreshEstimators(); void refreshRecipes(); void loadBlocks(); }, [loadAlgorithms, refreshEstimators, refreshRecipes, loadBlocks]);

  const elapsed = useElapsed(startedAt);
  const algorithm = settings.algorithm;
  const isRecipe = algorithm === RECIPE_ALGORITHM_ID;
  const selectedRecipe = isRecipe ? recipes.find((r) => r.name === settings.recipe) : undefined;
  const algo = findAlgorithm(algorithms, algorithm);
  const defs = paramDefsFor(algorithms, algorithm);
  const values = settings.hyperparams[algorithm] ?? {};
  const advanced = settings.advanced[algorithm] ?? '';
  const ctx = useMemo(() => trainContext({ algorithms, estimatorFiles, recipes, recipesLoaded }), [algorithms, estimatorFiles, recipes, recipesLoaded]);
  const problems = useMemo(() => validateSettings(settings, datasets, settings.algorithm, ctx), [settings, datasets, ctx]);
  const { info, features } = datasetFeatures(settings, datasets);
  const allFeatures = info ? allDatasetFeatures(info) : [];
  const customNames = new Set((info?.custom_features ?? []).map((c) => c.name));
  const comparing = leaderboard.length > 0;
  const labelOf = (id: string) => {
    const recipeName = isRecipeQueueId(id) ? recipeNameFor(settings, id) : null;
    if (recipeName !== null) return recipes.find((r) => r.name === recipeName)?.label || recipeName;
    return findAlgorithm(algorithms, id)?.label ?? id;
  };

  const queue = useMemo(() => settings.compareAlgos.filter((a) => isTrainable(a, settings, ctx)), [settings, ctx]);
  const compareProblems = useMemo(
    () => queue.flatMap((a) => validateSettings(settings, datasets, a, ctx)),
    [queue, settings, datasets, ctx],
  );

  const set = <K extends keyof TrainSettings>(key: K, value: TrainSettings[K]) => patch({ [key]: value } as Partial<TrainSettings>);

  const quickFill = (preset: PresetId) => {
    const targets = new Set<string>([algorithm, ...queue]);
    const byAlgo = { ...settings.hyperparams };
    targets.forEach((a) => {
      if (hasQuickFill(a)) byAlgo[a] = presetValues(a, preset, paramDefsFor(algorithms, a));
    });
    patch({ hyperparams: byAlgo });
  };

  const toggleFeature = (id: string) => {
    const current = settings.selectedFeatures ?? allFeatures;
    patch({ selectedFeatures: current.includes(id) ? current.filter((f) => f !== id) : allFeatures.filter((f) => f === id || current.includes(f)) });
  };

  const toggleCompare = (id: string) =>
    set('compareAlgos', settings.compareAlgos.includes(id) ? settings.compareAlgos.filter((a) => a !== id) : [...settings.compareAlgos, id]);

  const selectValue = isRecipe ? recipeQueueId(settings.recipe) : algorithm;
  const chooseAlgorithm = (value: string) => {
    if (value === BUILD_OPTION) { openBuilder(null, null); return; }
    if (isRecipeQueueId(value)) patch({ algorithm: RECIPE_ALGORITHM_ID, recipe: recipeNameFor(settings, value) ?? '' });
    else patch({ algorithm: value });
  };

  const holdoutNumber = Number(settings.holdoutPct);
  const holdoutShown = Number.isFinite(holdoutNumber) && settings.holdoutPct.trim() !== '' ? holdoutNumber : 0;

  const lagInput = (
    <label className="text-[11px] text-text-secondary">Lag periods (comma-separated, optional)
      <input className={`${inputCls} mt-0.5 block w-full font-mono`} value={settings.lagPeriods} onChange={(e) => set('lagPeriods', e.target.value)} placeholder="1,5,10" data-testid="ml-lag-periods" />
    </label>
  );

  const tradeModes: { id: TradeMode; label: string; text: string }[] = [
    { id: 'long_flat', label: 'Long / flat', text: 'A buy signal holds the asset; any other signal sits in cash. Never bets on a fall.' },
    { id: 'long_short', label: 'Long / short', text: 'A buy signal holds the asset and a sell signal bets on a fall (short). Scores what a rule that may short could earn.' },
  ];

  return (
    <div className="space-y-3 max-w-4xl" data-testid="ml-training-form">
      {builder && <AlgorithmBuilder />}
      <div className={`space-y-3 ${builder ? 'hidden' : ''}`} data-testid="ml-training-sections">
      <PresetPanel />

      {/* Data */}
      <section className={sectionCls} data-testid="ml-train-data">
        <div className="flex items-center justify-between">
          <div className={headCls}>1. Data</div>
          <div className="flex gap-1">
            <button type="button" className={`${btnCls} ${settings.mode === 'dataset' ? 'border-accent text-accent' : ''}`} onClick={() => set('mode', 'dataset')} data-testid="ml-train-mode-dataset">Built dataset</button>
            <button type="button" className={`${btnCls} ${settings.mode === 'csv' ? 'border-accent text-accent' : ''}`} onClick={() => set('mode', 'csv')} data-testid="ml-train-mode-csv">Custom CSV (advanced)</button>
          </div>
        </div>

        {settings.mode === 'dataset' ? (
          <>
            {datasets.length === 0 ? (
              <div className="text-xs text-text-muted" data-testid="ml-train-no-datasets">
                No datasets yet.{' '}
                <button type="button" className="text-accent hover:underline" onClick={onOpenData} data-testid="ml-train-go-data">Build one in the Data tab</button>.
              </div>
            ) : (
              <label className="block text-[11px] text-text-secondary">
                Dataset
                <select
                  className={`${inputCls} mt-0.5 block w-72 font-mono`}
                  value={info ? settings.dataset : ''}
                  onChange={(e) => patch({ dataset: e.target.value, selectedFeatures: null })}
                  data-testid="ml-train-dataset"
                >
                  <option value="">Choose a dataset...</option>
                  {datasets.map((d) => <option key={d.name} value={d.name}>{d.name} ({d.rows} rows)</option>)}
                </select>
              </label>
            )}
            {info && (
              <>
                <div className="text-[11px] text-text-muted" data-testid="ml-train-dataset-info">
                  {info.symbols.join(', ')} {info.timeframe}, {info.rows} rows, label: {info.label.kind === 'direction' ? 'direction' : `return beyond ${info.label.threshold_pct}%`} over {info.label.horizon_bars} bars. Trading return is scored with the {DATASET_RETURN_COLUMN} column.
                </div>
                <div>
                  <div className="mb-1 flex items-center justify-between">
                    <span className="text-[11px] text-text-secondary" data-testid="ml-train-feature-count">Features ({features.length} of {allFeatures.length} on)</span>
                    <span className="flex gap-2 text-[10px] font-mono text-accent">
                      <button type="button" className="hover:underline" onClick={() => set('selectedFeatures', null)} data-testid="ml-train-features-all">all</button>
                      <button type="button" className="hover:underline" onClick={() => set('selectedFeatures', [])} data-testid="ml-train-features-none">none</button>
                    </span>
                  </div>
                  <div className="flex flex-wrap gap-x-3 gap-y-1 border border-[var(--border-color)] bg-surface-0 p-2" data-testid="ml-feature-list">
                    {allFeatures.map((f) => (
                      <label key={f} className="flex cursor-pointer items-center gap-1 text-xs font-mono text-text-primary">
                        <input type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]" checked={features.includes(f)} onChange={() => toggleFeature(f)} data-testid={`ml-train-feature-${f}`} />
                        {f}
                        {customNames.has(f) && <span className="border border-accent/50 px-0.5 text-[9px] uppercase text-accent">custom</span>}
                      </label>
                    ))}
                  </div>
                </div>
                <div className="md:w-1/2">{lagInput}</div>
              </>
            )}
          </>
        ) : (
          <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
            <label className="text-[11px] text-text-secondary">CSV path
              <input className={`${inputCls} mt-0.5 block w-full font-mono`} value={settings.dataPath} onChange={(e) => set('dataPath', e.target.value)} data-testid="ml-data-path" />
            </label>
            <label className="text-[11px] text-text-secondary">Target column
              <input className={`${inputCls} mt-0.5 block w-full font-mono`} value={settings.targetColumn} onChange={(e) => set('targetColumn', e.target.value)} data-testid="ml-target-column" />
            </label>
            <label className="text-[11px] text-text-secondary md:col-span-2">Feature columns (comma-separated)
              <input className={`${inputCls} mt-0.5 block w-full font-mono`} value={settings.featuresText} onChange={(e) => set('featuresText', e.target.value)} data-testid="ml-features-text" />
            </label>
            {lagInput}
          </div>
        )}
      </section>

      {/* Algorithm and parameters */}
      <section className={sectionCls} data-testid="ml-train-algorithm">
        <div className={headCls}>2. Algorithm</div>
        {algorithmsNotice && <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="ml-algorithms-notice">{algorithmsNotice}</div>}
        <select className={`${inputCls} block w-72`} value={selectValue} onChange={(e) => chooseAlgorithm(e.target.value)} data-testid="ml-algorithm-select" aria-label="Algorithm">
          {!isRecipe && !algo && <option value={algorithm}>{algorithm}</option>}
          <optgroup label="Built-in algorithms" data-testid="ml-algorithm-group-builtin">
            {algorithms.filter((a) => a.id !== RECIPE_ALGORITHM_ID).map((a) => <option key={a.id} value={a.id} title={a.description}>{a.label}</option>)}
          </optgroup>
          <optgroup label="My algorithms" data-testid="ml-algorithm-group-recipes">
            {isRecipe && !selectedRecipe && <option value={selectValue}>{settings.recipe || 'No algorithm chosen'} (missing)</option>}
            {recipes.map((r) => <option key={r.name} value={recipeQueueId(r.name)} title={r.description}>{r.label || r.name}</option>)}
            <option value={BUILD_OPTION}>Build an algorithm...</option>
          </optgroup>
        </select>
        <div className="text-[11px] text-text-muted" data-testid="ml-algorithm-desc">{isRecipe ? selectedRecipe?.description : algo?.description}</div>

        {recipesLoaded && recipes.length === 0 && (
          <div className="space-y-1 border border-dashed border-[var(--border-color)] p-2" data-testid="ml-recipe-empty">
            <div className="text-[11px] text-text-muted">No saved algorithms yet. Build one from parts, no code needed. Start from an example:</div>
            <div className="flex flex-wrap gap-1">
              {TEMPLATES.map((t) => (
                <button key={t.id} type="button" className={btnCls} title={t.blurb} onClick={() => openBuilder(t.recipe, null)} data-testid={`ml-recipe-template-${t.id}`}>{t.title}</button>
              ))}
            </div>
          </div>
        )}

        {algorithm === CUSTOM_ALGORITHM_ID && <EstimatorEditor />}

        {isRecipe ? (
          selectedRecipe ? (
            <div className="space-y-2 border border-[var(--border-color)] bg-surface-0 p-2" data-testid="ml-recipe-summary">
              <div className="flex flex-wrap items-center gap-2">
                <span className="min-w-0 truncate text-xs font-semibold text-text-primary" title={selectedRecipe.label} data-testid="ml-recipe-label">{selectedRecipe.label || selectedRecipe.name}</span>
                <span className="font-mono text-[10px] text-text-muted" data-testid="ml-recipe-name">{selectedRecipe.name}</span>
                <button type="button" className={`${btnCls} ml-auto`} onClick={() => openBuilder(selectedRecipe, selectedRecipe.name)} data-testid="ml-recipe-edit">Edit recipe</button>
              </div>
              <RecipeDiagram nodes={nodesFromRecipe(selectedRecipe, blocks, algorithms)} />
              <RecipeLines recipe={selectedRecipe} />
              <div className="text-[11px] text-text-muted">The pipeline is fitted on training rows only. Its settings live in the recipe, so there are no parameters to set here.</div>
            </div>
          ) : (
            <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="ml-recipe-missing">
              {settings.recipe ? `The saved algorithm "${settings.recipe}" no longer exists.` : 'No saved algorithm is chosen.'} Pick another from the list, or build one.
            </div>
          )
        ) : (
        <ParamEditor
          defs={defs}
          values={values}
          advanced={advanced}
          onValues={(next) => setHyperparams(algorithm, next)}
          onAdvanced={(text) => patch({ advanced: { ...settings.advanced, [algorithm]: text } })}
          toolbarExtra={hasQuickFill(algorithm) ? (
            <>
              <span className="text-[11px] text-text-muted" title="Quick-fills just set the ordinary fields below, which you can edit afterwards.">Quick fill</span>
              {PRESETS.map((p) => (
                <button key={p.id} type="button" className={btnCls} title={p.hint} onClick={() => quickFill(p.id)} data-testid={`ml-preset-${p.id}`}>{p.label}</button>
              ))}
            </>
          ) : null}
        />
        )}
      </section>

      {/* Evaluation */}
      <section className={sectionCls} data-testid="ml-train-validation-section">
        <div className={headCls}>3. Evaluation</div>
        <label className="block text-[11px] text-text-secondary">
          Cross-validation splits (2-{MAX_SPLITS})
          <input className={`${inputCls} mt-0.5 block w-24`} type="number" min={2} max={MAX_SPLITS} value={settings.nSplits} onChange={(e) => set('nSplits', e.target.value)} data-testid="ml-cv-splits" />
        </label>
        <div className="text-[11px] text-text-muted" data-testid="ml-cv-help">
          Time-ordered cross-validation: each split trains on the past and tests on the next block, never on the future. A gap (embargo) is left between the training and test rows so a training row&apos;s label, which looks ahead, cannot overlap the test period.
        </div>

        <fieldset className="space-y-1" data-testid="ml-trade-mode">
          <legend className="text-[11px] text-text-secondary">Trading simulation</legend>
          {tradeModes.map((m) => (
            <label key={m.id} className="flex cursor-pointer items-start gap-1.5 text-xs text-text-primary">
              <input type="radio" name="ml-trade-mode" className="mt-0.5 accent-[rgb(var(--color-accent-rgb))]" checked={settings.tradeMode === m.id} onChange={() => set('tradeMode', m.id)} data-testid={`ml-trade-mode-${m.id}`} />
              <span>{m.label} <span className="text-text-muted">{m.text}</span></span>
            </label>
          ))}
          {settings.tradeMode === 'long_short' && (
            <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="ml-trade-mode-warning">
              This only changes how the model is scored. An automation shorts only if its rule allows shorting and its broker can short; otherwise it stays flat on a sell signal, and its real result will be closer to the long / flat score.
            </div>
          )}
        </fieldset>

        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          <div className="space-y-1">
            <label className="block text-[11px] text-text-secondary" htmlFor="ml-holdout-pct">Hold-out (0-{MAX_HOLDOUT_PCT}%)</label>
            <div className="flex items-center gap-2">
              <input type="range" min={0} max={MAX_HOLDOUT_PCT} step={1} value={Math.min(MAX_HOLDOUT_PCT, Math.max(0, holdoutShown))} onChange={(e) => set('holdoutPct', e.target.value)} className="w-40 accent-[rgb(var(--color-accent-rgb))]" aria-label="Hold-out percent" data-testid="ml-holdout-slider" />
              <input id="ml-holdout-pct" className={`${inputCls} w-20`} type="number" min={0} max={MAX_HOLDOUT_PCT} step={1} value={settings.holdoutPct} onChange={(e) => set('holdoutPct', e.target.value)} data-testid="ml-holdout-pct" />
            </div>
            <div className="text-[11px] text-text-muted" data-testid="ml-holdout-help">
              {holdoutShown > 0
                ? `Scored once on the last ${holdoutShown}% of time, never used for training or tuning.`
                : 'Off. Set a percentage to score the model once on the last part of time, never used for training or tuning.'}
            </div>
          </div>
          <div className="space-y-1">
            <label className="block text-[11px] text-text-secondary" htmlFor="ml-random-state">Random state (optional)</label>
            <input id="ml-random-state" className={`${inputCls} w-32 font-mono`} type="number" min={0} step={1} placeholder="auto" value={settings.randomState} onChange={(e) => set('randomState', e.target.value)} data-testid="ml-random-state" />
            <div className="text-[11px] text-text-muted">A fixed number makes a run repeatable. Leave empty to let the trainer choose.</div>
          </div>
        </div>
      </section>

      {/* Train */}
      <section className={sectionCls} data-testid="ml-train-run">
        <div className={headCls}>4. Train</div>
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" className={primaryBtnCls} disabled={isTraining || problems.length > 0} onClick={() => void train()} data-testid="ml-train-button">
            {isTraining && !comparing ? 'Training...' : 'Train model'}
          </button>
          {isTraining && (
            <span className="text-xs font-mono text-text-secondary" data-testid="ml-training-status">
              <span className="animate-pulse text-accent">●</span> {trainingLabel ? `${trainingLabel} ` : ''}
              <span data-testid="ml-train-timer">{elapsed}s</span>
            </span>
          )}
        </div>
        {problems.length > 0 && (
          <ul className="list-disc pl-4 text-[11px] text-bear" data-testid="ml-train-validation">
            {problems.map((p) => <li key={p}>{p}</li>)}
          </ul>
        )}
        {trainingError && <div className="border border-bear/30 bg-bear/10 px-2 py-1 text-xs text-bear" data-testid="ml-training-error">Error: {trainingError}</div>}
        {lastResult?.ok && <ResultCard result={lastResult} />}
      </section>

      {/* Compare */}
      <section className={sectionCls} data-testid="ml-compare">
        <div className={headCls}>Compare algorithms</div>
        <div className="text-[11px] text-text-muted" data-testid="ml-compare-note">
          Trains each ticked algorithm one after another, in the order ticked, on the same dataset, features, CV splits, trading simulation, hold-out and random state. Parameters apply only to the algorithm they were set for; an algorithm you have not edited runs with its defaults. A failure does not stop the rest.
        </div>
        <div className="flex flex-wrap gap-3">
          {algorithms.map((a) => {
            const aDefs = paramDefsFor(algorithms, a.id);
            const merged = mergeParams(aDefs, settings.hyperparams[a.id] ?? {}, settings.advanced[a.id]);
            const n = changedCount(aDefs, settings.hyperparams[a.id] ?? {}) + merged.advancedKeys;
            return (
              <label key={a.id} className="flex cursor-pointer items-center gap-1 text-xs text-text-primary">
                <input type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]" checked={settings.compareAlgos.includes(a.id)} onChange={() => toggleCompare(a.id)} data-testid={`ml-compare-algo-${a.id}`} />
                {a.label}
                <span className="text-[10px] text-text-muted" data-testid={`ml-compare-params-${a.id}`}>{n > 0 ? `${n} custom` : 'defaults'}</span>
              </label>
            );
          })}
          {recipes.map((r) => {
            const id = recipeQueueId(r.name);
            return (
              <label key={id} className="flex cursor-pointer items-center gap-1 text-xs text-text-primary">
                <input type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]" checked={settings.compareAlgos.includes(id)} onChange={() => toggleCompare(id)} data-testid={`ml-compare-algo-${id}`} />
                <span className="max-w-[12rem] truncate" title={r.label || r.name}>{r.label || r.name}</span>
                <span className="text-[10px] text-text-muted" data-testid={`ml-compare-params-${id}`}>my algorithm</span>
              </label>
            );
          })}
          <button type="button" className={primaryBtnCls} disabled={isTraining || queue.length === 0 || compareProblems.length > 0} onClick={() => void compare()} data-testid="ml-compare-run">
            {isTraining && comparing ? 'Comparing...' : 'Compare'}
          </button>
        </div>
        {queue.length > 0 && compareProblems.length > 0 && (
          <ul className="list-disc pl-4 text-[11px] text-bear" data-testid="ml-compare-validation">
            {[...new Set(compareProblems)].map((p) => <li key={p}>{p}</li>)}
          </ul>
        )}
        {comparing && (
          <table className="w-full text-xs" data-testid="ml-leaderboard">
            <thead className="text-left text-[11px] text-text-secondary">
              <tr><th className="py-1">Algorithm</th><th>Status</th><th>Accuracy</th><th>Trading return</th><th /></tr>
            </thead>
            <tbody>
              {leaderboard.map((row) => (
                <tr key={row.algorithm} className="border-t border-[var(--border-color)]" data-testid={`ml-leaderboard-row-${row.algorithm}`}>
                  <td className="py-1 text-text-primary">{labelOf(row.algorithm)}</td>
                  <td className={row.status === 'failed' ? 'text-bear' : row.status === 'done' ? 'text-bull' : 'text-text-muted'} data-testid={`ml-leaderboard-status-${row.algorithm}`}>{row.status}</td>
                  <td className="font-mono" data-testid={`ml-leaderboard-acc-${row.algorithm}`}>{row.result?.ok ? pct(row.result.metrics?.accuracy) : ''}</td>
                  <td className="font-mono" data-testid={`ml-leaderboard-ret-${row.algorithm}`}>
                    {row.result?.ok ? (row.result.trading ? signed(row.result.trading.strategy_return_pct) : 'n/a') : ''}
                  </td>
                  <td className="text-right">
                    {row.result?.ok && row.result.modelId && (
                      <button type="button" className="text-accent hover:underline" onClick={() => openItem('model', row.result!.modelId)} data-testid={`ml-leaderboard-open-${row.algorithm}`}>Open</button>
                    )}
                    {row.result && !row.result.ok && <span className="text-[11px] text-bear" data-testid={`ml-leaderboard-error-${row.algorithm}`}>{row.result.error}</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
      </div>
    </div>
  );
};

/** Plain-words lines under the diagram: steps, model, calibration. */
const RecipeLines: React.FC<{ recipe: import('../../lib/types').RecipeDto }> = ({ recipe }) => {
  const blocks = useMLStore((s) => s.blocks);
  const algorithms = useMLStore((s) => s.algorithms);
  const text = summarizeRecipe(recipe, blocks, algorithms);
  return (
    <ol className="list-decimal space-y-0.5 pl-5 text-[11px] text-text-secondary" data-testid="ml-recipe-lines">
      {text.steps.map((line, i) => <li key={`${line}-${i}`} data-testid={`ml-recipe-step-${i}`}>{line}</li>)}
      <li data-testid="ml-recipe-model">
        {text.model}
        {text.members.length > 0 && (
          <ul className="list-disc pl-5 text-text-muted">{text.members.map((m, i) => <li key={`${m}-${i}`}>{m}</li>)}</ul>
        )}
      </li>
      <li data-testid="ml-recipe-calibration">{text.calibration}</li>
    </ol>
  );
};

const ResultCard: React.FC<{ result: RunResult }> = ({ result }) => (
  <div className="space-y-1 border border-bull/40 bg-bull/5 p-2" data-testid="ml-train-result">
    <div className="flex items-center justify-between gap-2">
      <span className="text-xs text-text-primary">
        Trained <span className="font-mono" data-testid="ml-result-model-id">{result.modelId}</span>
        {result.durationSecs !== undefined && <span className="text-text-muted"> in {result.durationSecs.toFixed(1)}s</span>}
      </span>
      {result.modelId && <button type="button" className={primaryBtnCls} onClick={() => openItem('model', result.modelId)} data-testid="ml-open-model">Open model</button>}
    </div>
    <div className="grid grid-cols-2 gap-x-4 text-xs md:grid-cols-4">
      {(['accuracy', 'f1', 'precision', 'recall'] as const).map((k) => (
        <div key={k}><span className="text-text-muted">{k} </span><span className="font-mono text-text-primary" data-testid={`ml-result-${k}`}>{pct(result.metrics?.[k])}</span></div>
      ))}
    </div>
    {result.trading && (
      <div className="text-xs" data-testid="ml-result-trading">
        <span className="text-text-muted">Trading: </span>
        strategy <span className="font-mono">{signed(result.trading.strategy_return_pct)}</span>, buy and hold <span className="font-mono">{signed(result.trading.buy_hold_return_pct)}</span>, hit rate <span className="font-mono">{pct(result.trading.hit_rate)}</span>, {result.trading.trades} trades, max drawdown <span className="font-mono">{signed(result.trading.max_drawdown_pct)}</span>, Sharpe <span className="font-mono">{result.trading.sharpe.toFixed(2)}</span>
      </div>
    )}
  </div>
);
