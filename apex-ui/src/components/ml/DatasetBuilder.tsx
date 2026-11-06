import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { SymbolField } from '../common/SymbolField';
import { buildDataset, deleteDataset, previewDataset } from '../../lib/tauri';
import { useMLStore } from '../../stores/mlStore';
import type { DatasetInfoDto, DatasetPreviewDto, LabelSpecDto } from '../../lib/types';
import {
  FEATURE_PRESETS, MAX_BARS, MAX_SYMBOLS, MIN_BARS, MIN_CLASS_SHARE, MIN_ROWS_WARN, ROWS_PER_FEATURE, TIMEFRAMES,
  classShares, estimateRows, fmtShare, groupFeatures, presetFeatureIds, suggestDatasetName, validateDatasetName, validateLabel,
} from './datasetModel';

const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';
const primaryBtnCls =
  'px-3 py-1.5 text-xs font-mono font-semibold bg-accent text-on-accent hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed';
const sectionCls = 'border border-[var(--border-color)] bg-surface-1 p-3 space-y-2';
const headCls = 'text-[11px] font-semibold uppercase tracking-wider text-text-secondary';

export interface DatasetBuilderProps {
  /** Show this dataset (set when something navigated to it). */
  focusDataset?: { name: string; nonce: number } | null;
  /** "Use for training": the shell switches to the Train tab with this dataset chosen. */
  onUseForTraining: (name: string) => void;
}

function cell(value: string | number): string {
  if (typeof value === 'number') return Number.isInteger(value) ? String(value) : value.toFixed(4);
  return value;
}

function fmtDate(iso: string | null): string {
  return iso ? iso.slice(0, 10) : '?';
}

export const DatasetBuilder: React.FC<DatasetBuilderProps> = ({ focusDataset, onUseForTraining }) => {
  const catalog = useMLStore((s) => s.catalog);
  const datasets = useMLStore((s) => s.datasets);
  const loadCatalog = useMLStore((s) => s.loadCatalog);
  const refreshDatasets = useMLStore((s) => s.refreshDatasets);

  const [symbols, setSymbols] = useState<string[]>([]);
  const [symbolInput, setSymbolInput] = useState('');
  const [timeframe, setTimeframe] = useState<string>('d1');
  const [bars, setBars] = useState('1000');
  const [selected, setSelected] = useState<string[] | null>(null); // null = every feature
  const [search, setSearch] = useState('');
  const [horizon, setHorizon] = useState('5');
  const [kind, setKind] = useState<LabelSpecDto['kind']>('direction');
  const [threshold, setThreshold] = useState('0.5');
  const [name, setName] = useState('');
  const [nameTouched, setNameTouched] = useState(false);

  const [building, setBuilding] = useState(false);
  const [buildError, setBuildError] = useState<string | null>(null);
  const [card, setCard] = useState<DatasetInfoDto | null>(null);
  const [preview, setPreview] = useState<DatasetPreviewDto | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [listError, setListError] = useState<string | null>(null);

  useEffect(() => { void loadCatalog(); }, [loadCatalog]);

  const selectedIds = useMemo(() => selected ?? catalog.map((f) => f.id), [selected, catalog]);
  const horizonN = Number(horizon);
  const thresholdN = Number(threshold);
  const barsN = Number(bars);

  const suggested = useMemo(
    () => suggestDatasetName(symbols, timeframe, Number.isFinite(horizonN) ? horizonN : 0),
    [symbols, timeframe, horizonN],
  );
  const effectiveName = nameTouched ? name : suggested;

  const estimate = useMemo(
    () => estimateRows(Number.isFinite(barsN) ? barsN : 0, symbols.length, selectedIds, catalog, Number.isFinite(horizonN) ? horizonN : 0),
    [barsN, symbols.length, selectedIds, catalog, horizonN],
  );

  const nameError = validateDatasetName(effectiveName);
  const labelError = validateLabel(horizonN, kind, thresholdN);
  const barsError = !Number.isInteger(barsN) || barsN < MIN_BARS || barsN > MAX_BARS
    ? `Bars must be a whole number from ${MIN_BARS} to ${MAX_BARS}.` : null;
  const problems = [
    symbols.length === 0 ? 'Add at least one symbol.' : null,
    selectedIds.length === 0 ? 'Select at least one feature.' : null,
    barsError,
    labelError,
    nameError,
  ].filter((p): p is string => p !== null);

  const lowRows = symbols.length > 0 && selectedIds.length > 0 && !barsError && estimate.rows < MIN_ROWS_WARN;
  const tooManyFeatures = selectedIds.length > 0 && estimate.rows > 0 && estimate.rows / selectedIds.length < ROWS_PER_FEATURE;
  const nameClash = datasets.some((d) => d.name === effectiveName);

  /* ----- symbols ----- */
  const addSymbol = useCallback((raw: string) => {
    const symbol = raw.trim().toUpperCase();
    if (!symbol) return;
    setSymbols((prev) => (prev.includes(symbol) || prev.length >= MAX_SYMBOLS ? prev : [...prev, symbol]));
    setSymbolInput('');
  }, []);

  /* ----- features ----- */
  const toggleFeature = (id: string) =>
    setSelected(selectedIds.includes(id) ? selectedIds.filter((f) => f !== id) : [...selectedIds, id]);
  const q = search.trim().toLowerCase();
  const visibleGroups = useMemo(
    () => groupFeatures(catalog)
      .map((g) => ({
        ...g,
        features: g.features.filter((f) => !q || f.id.includes(q) || f.label.toLowerCase().includes(q) || f.description.toLowerCase().includes(q)),
      }))
      .filter((g) => g.features.length > 0),
    [catalog, q],
  );
  const toggleGroup = (ids: string[]) => {
    const all = ids.every((id) => selectedIds.includes(id));
    setSelected(all ? selectedIds.filter((id) => !ids.includes(id)) : [...new Set([...selectedIds, ...ids])]);
  };

  /* ----- datasets ----- */
  // Only the newest preview request may write: an older one can resolve last.
  const previewSeq = useRef(0);
  const showDataset = useCallback(async (datasetName: string) => {
    const seq = ++previewSeq.current;
    setPreviewError(null);
    setPreview(null);
    setCard(datasets.find((d) => d.name === datasetName) ?? null);
    try {
      const p = await previewDataset(datasetName);
      if (seq !== previewSeq.current) return;
      setCard(p);
      setPreview(p);
    } catch (e) {
      if (seq !== previewSeq.current) return;
      setPreviewError(e instanceof Error ? e.message : String(e));
    }
  }, [datasets]);

  useEffect(() => {
    if (focusDataset) void showDataset(focusDataset.name);
    // Only a new request should trigger this, not a refreshed dataset list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusDataset?.nonce]);

  const build = async () => {
    if (problems.length > 0 || building) return;
    setBuilding(true);
    setBuildError(null);
    try {
      const info = await buildDataset({
        name: effectiveName,
        symbols,
        timeframe,
        bars: barsN,
        features: catalog.filter((f) => selectedIds.includes(f.id)).map((f) => f.id),
        label: { horizon_bars: horizonN, kind, threshold_pct: kind === 'direction' ? 0 : thresholdN },
      });
      await refreshDatasets();
      const seq = ++previewSeq.current;
      setCard(info);
      setPreview(null);
      setPreviewError(null);
      try {
        const p = await previewDataset(info.name);
        if (seq === previewSeq.current) { setCard(p); setPreview(p); }
      } catch (e) {
        if (seq === previewSeq.current) setPreviewError(e instanceof Error ? e.message : String(e));
      }
    } catch (e) {
      setBuildError(e instanceof Error ? e.message : String(e));
    } finally {
      setBuilding(false);
    }
  };

  const remove = async (datasetName: string) => {
    setConfirmDelete(null);
    setListError(null);
    try {
      await deleteDataset(datasetName);
      if (card?.name === datasetName) { setCard(null); setPreview(null); }
      await refreshDatasets();
    } catch (e) {
      setListError(e instanceof Error ? e.message : String(e));
    }
  };

  const shares = card ? classShares(card.label.kind, card.class_counts) : [];
  const smallest = shares.length > 0 ? Math.min(...shares.map((s) => s.share)) : 1;

  return (
    <div className="space-y-3 max-w-4xl" data-testid="ml-dataset-builder">
      <div className="text-[11px] text-text-muted">
        Build a training dataset from the app&apos;s own market data: pick symbols and indicators, then say what the model should predict.
      </div>

      {/* Market data */}
      <section className={sectionCls} data-testid="ml-ds-market">
        <div className={headCls}>1. Market data</div>
        <div className="flex flex-wrap items-center gap-1.5" data-testid="ml-ds-symbols">
          {symbols.map((s) => (
            <span key={s} className="inline-flex items-center gap-1 border border-accent/40 bg-accent/10 px-1.5 py-0.5 text-xs font-mono text-accent" data-testid={`ml-ds-symbol-chip-${s}`}>
              {s}
              <button type="button" onClick={() => setSymbols(symbols.filter((x) => x !== s))} aria-label={`Remove ${s}`} className="text-text-muted hover:text-bear" data-testid={`ml-ds-symbol-remove-${s}`}>×</button>
            </span>
          ))}
          {symbols.length === 0 && <span className="text-[11px] text-text-muted">No symbols yet.</span>}
        </div>
        <div
          className="flex items-center gap-1.5"
          onKeyDown={(e) => {
            // Enter on a typed symbol adds it; Enter that picked a suggestion was already handled (defaultPrevented).
            if (e.key === 'Enter' && !e.defaultPrevented) { e.preventDefault(); addSymbol(symbolInput); }
          }}
        >
          <SymbolField
            value={symbolInput}
            onChange={setSymbolInput}
            onPick={addSymbol}
            placeholder={symbols.length >= MAX_SYMBOLS ? `Up to ${MAX_SYMBOLS} symbols` : 'Add symbol (e.g. AAPL)'}
            disabled={symbols.length >= MAX_SYMBOLS}
            className={`${inputCls} w-48`}
            testId="ml-ds-symbol-input"
            ariaLabel="Add symbol"
          />
          <button type="button" className={btnCls} onClick={() => addSymbol(symbolInput)} disabled={!symbolInput.trim()} data-testid="ml-ds-symbol-add">Add</button>
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <label className="text-[11px] text-text-secondary">
            Timeframe
            <select className={`${inputCls} block mt-0.5`} value={timeframe} onChange={(e) => setTimeframe(e.target.value)} data-testid="ml-ds-timeframe">
              {TIMEFRAMES.map((t) => <option key={t} value={t}>{t}</option>)}
            </select>
          </label>
          <label className="text-[11px] text-text-secondary">
            Bars per symbol
            <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={MIN_BARS} max={MAX_BARS} step={100} value={bars} onChange={(e) => setBars(e.target.value)} data-testid="ml-ds-bars" />
          </label>
        </div>
      </section>

      {/* Features */}
      <section className={sectionCls} data-testid="ml-ds-features">
        <div className="flex items-center justify-between gap-2">
          <div className={headCls}>2. Features</div>
          <span className="text-[11px] font-mono text-text-secondary" data-testid="ml-ds-feature-count">{selectedIds.length} of {catalog.length} selected</span>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[11px] text-text-muted">Presets</span>
          {FEATURE_PRESETS.map((p) => (
            <button key={p.id} type="button" className={btnCls} onClick={() => setSelected(presetFeatureIds(p, catalog))} data-testid={`ml-ds-preset-${p.id}`}>{p.label}</button>
          ))}
          <button type="button" className={btnCls} onClick={() => setSelected([])} data-testid="ml-ds-preset-none">None</button>
          <input className={`${inputCls} ml-auto w-40`} placeholder="Search features" value={search} onChange={(e) => setSearch(e.target.value)} data-testid="ml-ds-feature-search" aria-label="Search features" />
        </div>
        {catalog.length === 0 && <div className="text-[11px] text-text-muted" data-testid="ml-ds-feature-empty">Loading the feature catalog...</div>}
        {catalog.length > 0 && visibleGroups.length === 0 && <div className="text-[11px] text-text-muted" data-testid="ml-ds-feature-nomatch">No feature matches &quot;{search}&quot;.</div>}
        <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
          {visibleGroups.map(({ group, features }) => {
            const ids = features.map((f) => f.id);
            const allOn = ids.every((id) => selectedIds.includes(id));
            return (
              <div key={group} className="border border-[var(--border-color)] bg-surface-0 p-2" data-testid={`ml-ds-group-${group}`}>
                <div className="mb-1 flex items-center justify-between">
                  <span className="text-[11px] font-semibold text-text-secondary">{group}</span>
                  <button type="button" className="text-[10px] font-mono text-accent hover:underline" onClick={() => toggleGroup(ids)} data-testid={`ml-ds-group-all-${group}`}>
                    {allOn ? 'clear' : 'select all'}
                  </button>
                </div>
                <div className="space-y-0.5">
                  {features.map((f) => (
                    <label key={f.id} className="flex cursor-pointer items-start gap-1.5 text-xs text-text-primary" title={f.description}>
                      <input type="checkbox" className="mt-0.5 accent-[rgb(var(--color-accent-rgb))]" checked={selectedIds.includes(f.id)} onChange={() => toggleFeature(f.id)} data-testid={`ml-ds-feature-${f.id}`} />
                      <span className="min-w-0">
                        <span className="font-mono">{f.id}</span>
                        <span className="text-text-muted"> {f.label}, warm-up {f.warmup_bars}</span>
                      </span>
                    </label>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      </section>

      {/* Label */}
      <section className={sectionCls} data-testid="ml-ds-label">
        <div className={headCls}>3. What to predict</div>
        <div className="flex flex-wrap items-end gap-3">
          <label className="text-[11px] text-text-secondary">
            Horizon (bars ahead)
            <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={1} max={100} value={horizon} onChange={(e) => setHorizon(e.target.value)} data-testid="ml-ds-horizon" />
          </label>
          <label className="text-[11px] text-text-secondary">
            Label
            <select className={`${inputCls} block mt-0.5`} value={kind} onChange={(e) => setKind(e.target.value as LabelSpecDto['kind'])} data-testid="ml-ds-label-kind">
              <option value="direction">Direction (up / down)</option>
              <option value="return_threshold">Return threshold (up / flat / down)</option>
            </select>
          </label>
          <label className="text-[11px] text-text-secondary">
            Threshold %
            <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={0} step={0.1} value={threshold} onChange={(e) => setThreshold(e.target.value)} disabled={kind === 'direction'} data-testid="ml-ds-threshold" />
          </label>
        </div>
        <div className="text-[11px] text-text-muted" data-testid="ml-ds-label-help">
          {kind === 'direction'
            ? `Each row is labelled 1 if the price is higher ${Number.isFinite(horizonN) ? horizonN : '?'} bars later, otherwise 0.`
            : `Labelled 1 if the return ${Number.isFinite(horizonN) ? horizonN : '?'} bars later is above +${threshold || '?'}%, -1 if below -${threshold || '?'}%, otherwise 0 (flat).`}
        </div>
      </section>

      {/* Summary and build */}
      <section className={sectionCls} data-testid="ml-ds-summary">
        <div className={headCls}>4. Review and build</div>
        <div className="text-xs text-text-secondary">
          About <span className="font-mono font-semibold text-text-primary" data-testid="ml-ds-estimated-rows">{estimate.rows}</span> rows
          <span className="text-text-muted"> ({symbols.length} symbol{symbols.length === 1 ? '' : 's'} x ({Number.isFinite(barsN) ? barsN : 0} bars - {estimate.warmup} warm-up - {Number.isFinite(horizonN) ? horizonN : 0} horizon))</span>
        </div>
        {lowRows && (
          <div className="text-[11px] text-warning" data-testid="ml-ds-warn-rows">
            Only about {estimate.rows} rows. Under {MIN_ROWS_WARN} rows is too little to train and cross-validate reliably. Add bars or symbols.
          </div>
        )}
        {tooManyFeatures && (
          <div className="text-[11px] text-warning" data-testid="ml-ds-warn-features">
            {selectedIds.length} features for about {estimate.rows} rows is under {ROWS_PER_FEATURE} rows per feature, which invites overfitting. Drop features or add data.
          </div>
        )}
        <div className="flex flex-wrap items-end gap-2">
          <label className="text-[11px] text-text-secondary">
            Dataset name
            <input
              className={`${inputCls} block mt-0.5 w-72 font-mono`}
              value={effectiveName}
              onChange={(e) => { setName(e.target.value); setNameTouched(true); }}
              aria-invalid={nameError !== null}
              data-testid="ml-ds-name"
            />
          </label>
          {nameTouched && (
            <button type="button" className={btnCls} onClick={() => { setNameTouched(false); setName(''); }} data-testid="ml-ds-name-suggest">Use suggested</button>
          )}
          <button type="button" className={primaryBtnCls} onClick={() => void build()} disabled={problems.length > 0 || building} data-testid="ml-ds-build">
            {building ? 'Building...' : nameClash ? 'Rebuild dataset' : 'Build dataset'}
          </button>
        </div>
        {nameError && <div className="text-[11px] text-bear" data-testid="ml-ds-name-error">{nameError}</div>}
        {nameClash && !nameError && <div className="text-[11px] text-text-muted" data-testid="ml-ds-name-exists">A dataset with this name exists and will be replaced.</div>}
        {problems.length > 0 && (
          <ul className="list-disc pl-4 text-[11px] text-text-muted" data-testid="ml-ds-validation">
            {problems.map((p) => <li key={p}>{p}</li>)}
          </ul>
        )}
        {buildError && <div className="border border-bear/30 bg-bear/10 px-2 py-1 text-xs text-bear" data-testid="ml-ds-error">{buildError}</div>}
      </section>

      {/* Result / selected dataset */}
      {card && (
        <section className={sectionCls} data-testid="ml-ds-result">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="text-xs font-mono font-semibold text-text-primary" data-testid="ml-ds-result-name">{card.name}</div>
            <button type="button" className={primaryBtnCls} onClick={() => onUseForTraining(card.name)} data-testid="ml-ds-result-train">Train on this dataset</button>
          </div>
          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs md:grid-cols-4">
            <div><span className="text-text-muted">Rows </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-rows">{card.rows}</span></div>
            <div><span className="text-text-muted">Range </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-range">{fmtDate(card.first_time)} to {fmtDate(card.last_time)}</span></div>
            <div><span className="text-text-muted">Symbols </span><span className="font-mono text-text-primary">{card.symbols.join(', ')} ({card.timeframe})</span></div>
            <div><span className="text-text-muted">Features </span><span className="font-mono text-text-primary">{card.features.length}</span></div>
          </div>
          <div className="space-y-1" data-testid="ml-ds-balance">
            <div className="text-[11px] text-text-muted">Class balance</div>
            {shares.map((s) => (
              <div key={s.cls} className="flex items-center gap-2 text-xs" data-testid={`ml-ds-class-${s.cls}`}>
                <span className="w-12 text-text-secondary">{s.label}</span>
                <div className="h-2 flex-1 bg-surface-0 border border-[var(--border-color)]">
                  <div className={`h-full ${s.share < MIN_CLASS_SHARE ? 'bg-warning' : 'bg-accent'}`} style={{ width: `${Math.round(s.share * 100)}%` }} />
                </div>
                <span className="w-24 text-right font-mono text-text-primary">{fmtShare(s.share)} ({s.count})</span>
              </div>
            ))}
            {shares.length > 0 && smallest < MIN_CLASS_SHARE && (
              <div className="text-[11px] text-warning" data-testid="ml-ds-balance-warn">
                One class is under {Math.round(MIN_CLASS_SHARE * 100)}% of the rows. The model can score well by ignoring it. Try a smaller threshold or a longer horizon.
              </div>
            )}
          </div>
          {previewError && <div className="text-xs text-bear" data-testid="ml-ds-preview-error">{previewError}</div>}
          {preview && preview.name === card.name && (
            <div className="overflow-auto border border-[var(--border-color)]" data-testid="ml-ds-preview">
              <table className="w-full text-[11px] font-mono" data-testid="ml-ds-preview-table">
                <thead className="bg-surface-2 text-text-secondary">
                  <tr>{preview.columns.map((c) => <th key={c} className="whitespace-nowrap px-2 py-1 text-left font-semibold">{c}</th>)}</tr>
                </thead>
                <tbody>
                  {preview.sample.map((row, i) => (
                    <tr key={i} className="border-t border-[var(--border-color)]" data-testid="ml-ds-preview-row">
                      {row.map((v, j) => <td key={j} className="whitespace-nowrap px-2 py-0.5 text-text-primary">{cell(v)}</td>)}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}

      {/* Existing datasets */}
      <section className={sectionCls} data-testid="ml-ds-list">
        <div className={headCls}>Your datasets ({datasets.length})</div>
        {listError && <div className="text-xs text-bear" data-testid="ml-ds-list-error">{listError}</div>}
        {datasets.length === 0 && <div className="text-[11px] text-text-muted" data-testid="ml-ds-list-empty">No datasets yet. Build one above.</div>}
        {datasets.map((d) => (
          <div key={d.name} className={`flex flex-wrap items-center gap-2 border px-2 py-1 text-xs ${card?.name === d.name ? 'border-accent bg-accent/5' : 'border-[var(--border-color)] bg-surface-0'}`} data-testid={`ml-dataset-${d.name}`}>
            <button type="button" className="min-w-0 flex-1 truncate text-left font-mono text-text-primary hover:text-accent" onClick={() => void showDataset(d.name)} data-testid={`ml-ds-select-${d.name}`}>
              {d.name}
              <span className="text-text-muted"> {d.rows} rows, {d.symbols.join('+')}, {d.timeframe}{d.source !== 'user' ? `, by ${d.source}` : ''}</span>
            </button>
            <button type="button" className={btnCls} onClick={() => void showDataset(d.name)} data-testid={`ml-ds-preview-${d.name}`}>Preview</button>
            <button type="button" className={btnCls} onClick={() => onUseForTraining(d.name)} data-testid={`ml-ds-train-${d.name}`}>Train</button>
            {confirmDelete === d.name ? (
              <>
                <span className="text-bear">Delete {d.name}?</span>
                <button type="button" className={`${btnCls} text-bear`} onClick={() => void remove(d.name)} data-testid={`ml-ds-delete-confirm-${d.name}`}>Delete</button>
                <button type="button" className={btnCls} onClick={() => setConfirmDelete(null)} data-testid={`ml-ds-delete-cancel-${d.name}`}>Cancel</button>
              </>
            ) : (
              <button type="button" className={`${btnCls} hover:text-bear`} onClick={() => setConfirmDelete(d.name)} data-testid={`ml-ds-delete-${d.name}`}>Delete</button>
            )}
          </div>
        ))}
      </section>
    </div>
  );
};
