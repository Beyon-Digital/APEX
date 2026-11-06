import React, { useEffect, useMemo, useState } from 'react';
import { openItem } from '../../stores/navStore';
import {
  DATASET_RETURN_COLUMN, datasetFeatures, useMLStore, validateSettings, type RunResult, type TrainSettings,
} from '../../stores/mlStore';
import {
  ALGORITHMS, PRESETS, algorithmInfo, changedCount, displayValue, isChanged, presetValues, specsFor, validateHyperparams,
  type AlgorithmId,
} from './hyperparams';

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

const algoLabel = (id: string) => algorithmInfo(id)?.label ?? id;

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

export const TrainingForm: React.FC<TrainingFormProps> = ({ onOpenData }) => {
  const settings = useMLStore((s) => s.settings);
  const datasets = useMLStore((s) => s.datasets);
  const patch = useMLStore((s) => s.patchSettings);
  const setHyperparams = useMLStore((s) => s.setHyperparams);
  const isTraining = useMLStore((s) => s.isTraining);
  const startedAt = useMLStore((s) => s.trainingStartedAt);
  const trainingLabel = useMLStore((s) => s.trainingLabel);
  const trainingError = useMLStore((s) => s.trainingError);
  const lastResult = useMLStore((s) => s.lastResult);
  const leaderboard = useMLStore((s) => s.leaderboard);
  const train = useMLStore((s) => s.train);
  const compare = useMLStore((s) => s.compare);

  const elapsed = useElapsed(startedAt);
  const algorithm = settings.algorithm;
  const specs = specsFor(algorithm);
  const values = settings.hyperparams[algorithm] ?? {};
  const hpErrors = validateHyperparams(algorithm, values);
  const problems = useMemo(() => validateSettings(settings, datasets), [settings, datasets]);
  const { info, features } = datasetFeatures(settings, datasets);
  const comparing = leaderboard.length > 0;

  const compareProblems = useMemo(
    () => settings.compareAlgos.flatMap((a) => validateSettings(settings, datasets, a)),
    [settings, datasets],
  );

  const set = <K extends keyof TrainSettings>(key: K, value: TrainSettings[K]) => patch({ [key]: value } as Partial<TrainSettings>);

  const setValue = (key: string, text: string) => setHyperparams(algorithm, { ...values, [key]: text });
  const resetKey = (key: string) => {
    const next = { ...values };
    delete next[key];
    setHyperparams(algorithm, next);
  };

  const applyPreset = (preset: (typeof PRESETS)[number]['id']) => {
    const targets = new Set<AlgorithmId>([algorithm, ...settings.compareAlgos]);
    const byAlgo = { ...settings.hyperparams };
    targets.forEach((a) => { byAlgo[a] = presetValues(a, preset); });
    patch({ hyperparams: byAlgo });
  };

  const toggleFeature = (id: string) => {
    const base = info?.features ?? [];
    const current = settings.selectedFeatures ?? base;
    patch({ selectedFeatures: current.includes(id) ? current.filter((f) => f !== id) : base.filter((f) => f === id || current.includes(f)) });
  };

  const toggleCompare = (id: AlgorithmId) =>
    set('compareAlgos', settings.compareAlgos.includes(id) ? settings.compareAlgos.filter((a) => a !== id) : [...settings.compareAlgos, id]);

  const lagInput = (
    <label className="text-[11px] text-text-secondary">Lag periods (comma-separated, optional)
      <input className={`${inputCls} mt-0.5 block w-full font-mono`} value={settings.lagPeriods} onChange={(e) => set('lagPeriods', e.target.value)} placeholder="1,5,10" data-testid="ml-lag-periods" />
    </label>
  );

  return (
    <div className="space-y-3 max-w-4xl" data-testid="ml-training-form">
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
                    <span className="text-[11px] text-text-secondary" data-testid="ml-train-feature-count">Features ({features.length} of {info.features.length} on)</span>
                    <span className="flex gap-2 text-[10px] font-mono text-accent">
                      <button type="button" className="hover:underline" onClick={() => set('selectedFeatures', null)} data-testid="ml-train-features-all">all</button>
                      <button type="button" className="hover:underline" onClick={() => set('selectedFeatures', [])} data-testid="ml-train-features-none">none</button>
                    </span>
                  </div>
                  <div className="flex flex-wrap gap-x-3 gap-y-1 border border-[var(--border-color)] bg-surface-0 p-2" data-testid="ml-feature-list">
                    {info.features.map((f) => (
                      <label key={f} className="flex cursor-pointer items-center gap-1 text-xs font-mono text-text-primary">
                        <input type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]" checked={features.includes(f)} onChange={() => toggleFeature(f)} data-testid={`ml-train-feature-${f}`} />
                        {f}
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

      {/* Algorithm and hyperparameters */}
      <section className={sectionCls} data-testid="ml-train-algorithm">
        <div className={headCls}>2. Algorithm</div>
        <select className={`${inputCls} block w-64`} value={algorithm} onChange={(e) => set('algorithm', e.target.value as AlgorithmId)} data-testid="ml-algorithm-select" aria-label="Algorithm">
          {ALGORITHMS.map((a) => <option key={a.id} value={a.id}>{a.label}</option>)}
        </select>
        <div className="text-[11px] text-text-muted" data-testid="ml-algorithm-desc">{algorithmInfo(algorithm)?.description}</div>

        <div className="flex flex-wrap items-center gap-1.5 pt-1">
          <span className="text-[11px] text-text-secondary">Hyperparameters</span>
          <span className="text-[11px] text-text-muted" data-testid="ml-hp-changed-count">{changedCount(algorithm, values)} changed from default</span>
          <span className="ml-auto flex items-center gap-1.5">
            <span className="text-[11px] text-text-muted">Presets</span>
            {PRESETS.map((p) => (
              <button key={p.id} type="button" className={btnCls} title={p.hint} onClick={() => applyPreset(p.id)} data-testid={`ml-preset-${p.id}`}>{p.label}</button>
            ))}
            <button type="button" className={btnCls} onClick={() => setHyperparams(algorithm, {})} disabled={changedCount(algorithm, values) === 0 && Object.keys(values).length === 0} data-testid="ml-hp-reset-all">Reset all</button>
          </span>
        </div>
        <div className="grid grid-cols-1 gap-2 md:grid-cols-2" data-testid="ml-hp-editor">
          {specs.map((spec) => {
            const changed = isChanged(spec, values);
            const err = hpErrors[spec.key];
            return (
              <div key={spec.key} className={`border p-2 ${err ? 'border-bear/50' : changed ? 'border-accent/50' : 'border-[var(--border-color)]'} bg-surface-0`} data-testid={`ml-hp-field-${spec.key}`}>
                <div className="flex items-center justify-between gap-2">
                  <label className="text-[11px] text-text-secondary" htmlFor={`ml-hp-${spec.key}`}>
                    {spec.label} <span className="font-mono text-text-muted">{spec.key}</span>
                  </label>
                  {changed && <button type="button" className="text-[10px] font-mono text-accent hover:underline" onClick={() => resetKey(spec.key)} data-testid={`ml-hp-reset-${spec.key}`}>reset</button>}
                </div>
                {spec.type === 'select' ? (
                  <select id={`ml-hp-${spec.key}`} className={`${inputCls} mt-0.5 block w-full`} value={displayValue(spec, values)} onChange={(e) => setValue(spec.key, e.target.value)} data-testid={`ml-hp-${spec.key}`}>
                    {spec.options?.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
                  </select>
                ) : (
                  <input
                    id={`ml-hp-${spec.key}`}
                    className={`${inputCls} mt-0.5 block w-full font-mono`}
                    type="number" min={spec.min} max={spec.max} step={spec.step}
                    placeholder={spec.default === null ? 'no limit' : String(spec.default)}
                    value={displayValue(spec, values)}
                    onChange={(e) => setValue(spec.key, e.target.value)}
                    aria-invalid={err !== undefined}
                    data-testid={`ml-hp-${spec.key}`}
                  />
                )}
                <div className="mt-0.5 text-[10px] text-text-muted">{spec.help}</div>
                {err && <div className="text-[11px] text-bear" data-testid={`ml-hp-error-${spec.key}`}>{err}</div>}
              </div>
            );
          })}
        </div>
      </section>

      {/* Validation */}
      <section className={sectionCls} data-testid="ml-train-validation-section">
        <div className={headCls}>3. Validation</div>
        <label className="block text-[11px] text-text-secondary">
          Cross-validation splits (2-10)
          <input className={`${inputCls} mt-0.5 block w-24`} type="number" min={2} max={10} value={settings.nSplits} onChange={(e) => set('nSplits', e.target.value)} data-testid="ml-cv-splits" />
        </label>
        <div className="text-[11px] text-text-muted" data-testid="ml-cv-help">
          Time-ordered cross-validation: each split trains on the past and tests on the next block, never on the future. A gap (embargo) is left between the training and test rows so a training row&apos;s label, which looks ahead, cannot overlap the test period.
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
        <div className="text-[11px] text-text-muted">
          Trains each ticked algorithm one after another, in the order ticked, on the same dataset, CV splits and features, using each algorithm&apos;s own hyperparameters. A failure does not stop the rest.
        </div>
        <div className="flex flex-wrap gap-3">
          {ALGORITHMS.map((a) => (
            <label key={a.id} className="flex cursor-pointer items-center gap-1 text-xs text-text-primary">
              <input type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]" checked={settings.compareAlgos.includes(a.id)} onChange={() => toggleCompare(a.id)} data-testid={`ml-compare-algo-${a.id}`} />
              {a.label}
            </label>
          ))}
          <button type="button" className={primaryBtnCls} disabled={isTraining || settings.compareAlgos.length === 0 || compareProblems.length > 0} onClick={() => void compare()} data-testid="ml-compare-run">
            {isTraining && comparing ? 'Comparing...' : 'Compare'}
          </button>
        </div>
        {comparing && (
          <table className="w-full text-xs" data-testid="ml-leaderboard">
            <thead className="text-left text-[11px] text-text-secondary">
              <tr><th className="py-1">Algorithm</th><th>Status</th><th>Accuracy</th><th>Trading return</th><th /></tr>
            </thead>
            <tbody>
              {leaderboard.map((row) => (
                <tr key={row.algorithm} className="border-t border-[var(--border-color)]" data-testid={`ml-leaderboard-row-${row.algorithm}`}>
                  <td className="py-1 text-text-primary">{algoLabel(row.algorithm)}</td>
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
