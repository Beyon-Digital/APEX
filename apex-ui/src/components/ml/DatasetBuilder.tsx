import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { buildDataset, deleteDataset, previewDataset } from '../../lib/tauri';
import { DeletedObjects } from '../audit/DeletedObjects';
import { VersionHistory } from '../audit/VersionHistory';
import { useMLStore } from '../../stores/mlStore';
import type { BuildDatasetRequestDto, DatasetInfoDto, DatasetPreviewDto, LabelSpecDto } from '../../lib/types';
import { CustomFeatureEditor } from './CustomFeatureEditor';
import { useExpressionChecks, useSymbolData } from './dataHooks';
import { SymbolDataPicker } from './SymbolDataPicker';
import {
  FEATURE_PRESETS, MAX_BARS, MAX_CONTEXT_SYMBOLS, MAX_SYMBOLS, MIN_BARS, MIN_CLASS_SHARE, MIN_ROWS_WARN, ROWS_PER_FEATURE, TIMEFRAMES, planFetch, uiTimeframe,
  barsInRange, barsToCoverRange, classShares, customRowProblems, estimateRowsFromBars, fmtShare, groupFeatures, newRowKey,
  presetFeatureIds, rangeCoverage, suggestDatasetName, toCustomFeatureDtos, totalWarmup, validateDatasetName, validateFee,
  validateLabel, validateRange, DATASET_NAME_MAX, type CustomRow,
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

/** Label settings in plain words, for the dataset card. */
function describeLabel(label: LabelSpecDto): string {
  if (label.kind === 'direction') return `${label.horizon_bars} bars ahead, up or down`;
  const up = label.up_threshold_pct ?? label.threshold_pct;
  const down = label.down_threshold_pct ?? label.threshold_pct;
  return `${label.horizon_bars} bars ahead, up above +${up}%, down below -${down}%`;
}

/** First free "<base>_copy" name for a dataset whose settings are being reused. */
function copyName(base: string, taken: string[]): string {
  const stem = base.replace(/_copy\d*$/, '');
  for (let n = 1; ; n += 1) {
    const suffix = n === 1 ? '_copy' : `_copy${n}`;
    const candidate = `${stem.slice(0, DATASET_NAME_MAX - suffix.length)}${suffix}`;
    if (!taken.includes(candidate)) return candidate;
  }
}

export const DatasetBuilder: React.FC<DatasetBuilderProps> = ({ focusDataset, onUseForTraining }) => {
  const catalog = useMLStore((s) => s.catalog);
  const datasets = useMLStore((s) => s.datasets);
  const loadCatalog = useMLStore((s) => s.loadCatalog);
  const refreshDatasets = useMLStore((s) => s.refreshDatasets);

  const [symbols, setSymbols] = useState<string[]>([]);
  const [contextSymbols, setContextSymbols] = useState<string[]>([]);
  const [timeframe, setTimeframe] = useState<string>('d1');
  const [period, setPeriod] = useState<'bars' | 'range'>('bars');
  const [bars, setBars] = useState('1000');
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const [selected, setSelected] = useState<string[] | null>(null); // null = every feature
  const [customRows, setCustomRows] = useState<CustomRow[]>([]);
  const [search, setSearch] = useState('');
  const [horizon, setHorizon] = useState('5');
  const [kind, setKind] = useState<LabelSpecDto['kind']>('direction');
  const [threshold, setThreshold] = useState('0.5');
  const [asym, setAsym] = useState(false);
  const [upThreshold, setUpThreshold] = useState('0.5');
  const [downThreshold, setDownThreshold] = useState('0.5');
  const [fee, setFee] = useState('0');
  const [name, setName] = useState('');
  const [nameTouched, setNameTouched] = useState(false);
  const [dupNote, setDupNote] = useState<string | null>(null);

  const [building, setBuilding] = useState(false);
  const [buildError, setBuildError] = useState<string | null>(null);
  const [card, setCard] = useState<DatasetInfoDto | null>(null);
  const [preview, setPreview] = useState<DatasetPreviewDto | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const formTop = useRef<HTMLElement>(null);

  useEffect(() => { void loadCatalog(); }, [loadCatalog]);

  const selectedIds = useMemo(() => selected ?? catalog.map((f) => f.id), [selected, catalog]);
  const catalogIds = useMemo(() => catalog.map((f) => f.id), [catalog]);
  const horizonN = Number(horizon);
  const thresholdN = Number(threshold);
  const upN = Number(upThreshold);
  const downN = Number(downThreshold);
  const feeN = Number(fee);
  const barsN = Number(bars);
  const horizonOk = Number.isFinite(horizonN) ? horizonN : 0;
  const rangeMode = period === 'range';

  const symbolData = useSymbolData(symbols, timeframe);
  const contextData = useSymbolData(contextSymbols, timeframe);
  const checks = useExpressionChecks(customRows, symbols[0] ?? null, timeframe, contextSymbols, symbolData.revision + contextData.revision);

  const suggested = useMemo(() => suggestDatasetName(symbols, timeframe, horizonOk), [symbols, timeframe, horizonOk]);
  const effectiveName = nameTouched ? name : suggested;

  const customWarmups = customRows.map((r) => (checks[r.key]?.status === 'ok' ? checks[r.key].result?.warmup_bars ?? 0 : 0));
  const warmup = totalWarmup(selectedIds, catalog, customWarmups);

  const barsError = !Number.isInteger(barsN) || barsN < MIN_BARS || barsN > MAX_BARS
    ? `Bars must be a whole number from ${MIN_BARS} to ${MAX_BARS}.` : null;
  const rangeError = rangeMode ? validateRange(from, to) : null;

  const perSymbolBars = symbols.map((s) => {
    const row = symbolData.data[s];
    if (rangeMode) return rangeError ? 0 : barsInRange(row, from, to) ?? 0;
    const wanted = Number.isFinite(barsN) ? barsN : 0;
    return row ? Math.min(wanted, row.bars) : wanted; // never more bars than are stored
  });
  const estimate = estimateRowsFromBars(perSymbolBars, warmup, horizonOk);

  const nameError = validateDatasetName(effectiveName);
  const asymmetric = kind === 'return_threshold' && asym;
  const labelError = validateLabel(horizonN, kind, thresholdN, asymmetric ? { up: upN, down: downN } : null);
  const feeError = validateFee(feeN);
  const customProblems = customRowProblems(customRows, checks, catalogIds);
  const featureTotal = selectedIds.length + customRows.length;
  const problems = [
    symbols.length === 0 ? 'Add at least one symbol.' : null,
    featureTotal === 0 ? 'Select at least one feature or add a custom feature.' : null,
    rangeMode ? rangeError : barsError,
    labelError,
    feeError,
    ...customProblems,
    nameError,
  ].filter((p): p is string => p !== null);

  const rangeChecked = rangeMode && !rangeError;
  const coverage = rangeChecked ? symbols.map((s) => ({ symbol: s, kind: rangeCoverage(symbolData.data[s], from, to), data: symbolData.data[s] })) : [];
  const coverageWarnings = coverage.filter((c) => c.kind === 'partial' || c.kind === 'outside');

  const lowRows = symbols.length > 0 && featureTotal > 0 && !(rangeMode ? rangeError : barsError) && estimate.rows < MIN_ROWS_WARN;
  const tooManyFeatures = featureTotal > 0 && estimate.rows > 0 && estimate.rows / featureTotal < ROWS_PER_FEATURE;
  const nameClash = datasets.some((d) => d.name === effectiveName);

  /** Bars the dataset needs, and how many "Fetch history" can download (at most MAX_FETCH_BARS). */
  const neededBars = rangeMode
    ? (from ? barsToCoverRange(from, timeframe, warmup) : 1000)
    : (Number.isInteger(barsN) && barsN >= MIN_BARS && barsN <= MAX_BARS ? barsN : 1000);
  const fetchPlan = planFetch(neededBars);
  const fetchBars = fetchPlan.bars;
  /** `bars` sent with the build: the typed value, or in date-range mode one derived from the range (always valid). */
  const requestBars = rangeMode ? neededBars : barsN;

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
  const addContext = useCallback((symbol: string) => {
    setContextSymbols((prev) => (prev.includes(symbol) || prev.length >= MAX_CONTEXT_SYMBOLS ? prev : [...prev, symbol]));
  }, []);

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

  /** Load an existing dataset's settings into the form so they can be tweaked and rebuilt under a new name. */
  const duplicateSettings = (d: DatasetInfoDto) => {
    const known = new Set(catalogIds);
    const features = catalogIds.length > 0 ? d.features.filter((id) => known.has(id)) : d.features;
    const custom = d.custom_features ?? [];
    const label = d.label;
    const horizonBars = label.horizon_bars;
    const catalogWarmup = totalWarmup(features, catalog, []);
    const perSymbol = Math.ceil(d.rows / Math.max(1, d.symbols.length)) + catalogWarmup + horizonBars;
    setSymbols(d.symbols);
    setContextSymbols(d.context_symbols ?? []);
    const tf = uiTimeframe(d.timeframe);
    if ((TIMEFRAMES as readonly string[]).includes(tf)) setTimeframe(tf);
    setPeriod('bars');
    setBars(String(Math.min(MAX_BARS, Math.max(MIN_BARS, Math.ceil(perSymbol / 10) * 10))));
    setFrom('');
    setTo('');
    setSelected(features);
    setCustomRows(custom.map((c) => ({ key: newRowKey(), name: c.name, expr: c.expr })));
    setHorizon(String(horizonBars));
    setKind(label.kind);
    const up = label.up_threshold_pct ?? label.threshold_pct;
    const down = label.down_threshold_pct ?? label.threshold_pct;
    setThreshold(String(label.kind === 'direction' ? 0.5 : up));
    setAsym(label.kind === 'return_threshold' && up !== down);
    setUpThreshold(String(up || 0.5));
    setDownThreshold(String(down || 0.5));
    setFee(String(d.fee_pct ?? 0));
    setName(copyName(d.name, datasets.map((x) => x.name)));
    setNameTouched(true);
    setBuildError(null);
    setDupNote(`Settings loaded from ${d.name}. The number of bars is estimated from its rows, so check it before building.`);
    formTop.current?.scrollIntoView?.({ block: 'start' });
  };

  const build = async () => {
    if (problems.length > 0 || building) return;
    setBuilding(true);
    setBuildError(null);
    try {
      const label: LabelSpecDto = kind === 'direction'
        ? { horizon_bars: horizonN, kind, threshold_pct: 0 }
        : asymmetric
          ? { horizon_bars: horizonN, kind, threshold_pct: upN, up_threshold_pct: upN, down_threshold_pct: downN }
          : { horizon_bars: horizonN, kind, threshold_pct: thresholdN };
      const request: BuildDatasetRequestDto = {
        name: effectiveName,
        symbols,
        timeframe,
        bars: requestBars,
        features: catalog.filter((f) => selectedIds.includes(f.id)).map((f) => f.id),
        label,
        custom_features: toCustomFeatureDtos(customRows),
        context_symbols: contextSymbols,
        fee_pct: feeN,
        ...(rangeMode ? { from, to } : {}),
      };
      const info = await buildDataset(request);
      await refreshDatasets();
      setDupNote(null);
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
        Build a training dataset from the app&apos;s own market data: pick any symbols and period, choose built-in indicators and/or write your own formulas, then say what the model should predict.
      </div>
      {dupNote && (
        <div className="flex items-center justify-between gap-2 border border-accent/40 bg-accent/10 px-2 py-1 text-[11px] text-text-primary" role="status" data-testid="ml-ds-dup-note">
          <span>{dupNote}</span>
          <button type="button" className="font-mono text-text-muted hover:text-text-primary" onClick={() => setDupNote(null)} aria-label="Dismiss note" data-testid="ml-ds-dup-dismiss">×</button>
        </div>
      )}

      {/* Market data */}
      <section ref={formTop} className={sectionCls} data-testid="ml-ds-market">
        <div className={headCls}>1. Market data</div>
        <div className="text-[11px] text-text-muted">
          Type any ticker the app can resolve. Each symbol shows what is already stored for the timeframe; download more with Fetch history. Every symbol becomes rows of the dataset.
        </div>
        <SymbolDataPicker
          symbols={symbols}
          onChange={setSymbols}
          timeframe={timeframe}
          max={MAX_SYMBOLS}
          state={symbolData}
          fetchBars={fetchBars}
          fetchCapped={fetchPlan.capped}
          testPrefix="ml-ds-symbol"
          listTestId="ml-ds-symbols"
          emptyText="No symbols yet."
          placeholder="Add symbol (e.g. AAPL)"
          ariaLabel="Add symbol"
        />
        <div className="flex flex-wrap items-end gap-3">
          <label className="text-[11px] text-text-secondary">
            Timeframe
            <select className={`${inputCls} block mt-0.5`} value={timeframe} onChange={(e) => setTimeframe(e.target.value)} data-testid="ml-ds-timeframe">
              {TIMEFRAMES.map((t) => <option key={t} value={t}>{t}</option>)}
            </select>
          </label>
          <fieldset className="text-[11px] text-text-secondary" data-testid="ml-ds-period">
            <legend className="mb-0.5">Period</legend>
            <div className="flex items-center gap-3 py-1">
              <label className="flex items-center gap-1">
                <input type="radio" name="ml-ds-period" className="accent-[rgb(var(--color-accent-rgb))]" checked={period === 'bars'} onChange={() => setPeriod('bars')} data-testid="ml-ds-period-bars" />
                Latest bars
              </label>
              <label className="flex items-center gap-1">
                <input type="radio" name="ml-ds-period" className="accent-[rgb(var(--color-accent-rgb))]" checked={period === 'range'} onChange={() => setPeriod('range')} data-testid="ml-ds-period-range" />
                Date range
              </label>
            </div>
          </fieldset>
          {!rangeMode && (
            <label className="text-[11px] text-text-secondary">
              Bars per symbol
              <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={MIN_BARS} max={MAX_BARS} step={100} value={bars} onChange={(e) => setBars(e.target.value)} data-testid="ml-ds-bars" />
            </label>
          )}
          {rangeMode && (
            <>
              <label className="text-[11px] text-text-secondary">
                From
                <input className={`${inputCls} block mt-0.5`} type="date" value={from} onChange={(e) => setFrom(e.target.value)} aria-invalid={rangeError !== null && from !== '' && to !== ''} data-testid="ml-ds-from" />
              </label>
              <label className="text-[11px] text-text-secondary">
                To (inclusive)
                <input className={`${inputCls} block mt-0.5`} type="date" value={to} onChange={(e) => setTo(e.target.value)} aria-invalid={rangeError !== null && from !== '' && to !== ''} data-testid="ml-ds-to" />
              </label>
            </>
          )}
        </div>
        <div className="text-[11px] text-text-muted" data-testid="ml-ds-period-help">
          {rangeMode
            ? 'Uses only the bars between the two dates (both included), however many that is.'
            : 'Uses the most recent bars stored for each symbol, up to this number.'}
        </div>
        {rangeMode && rangeError && (from !== '' || to !== '') && <div className="text-[11px] text-bear" data-testid="ml-ds-range-error">{rangeError}</div>}
        {coverageWarnings.map((c) => (
          <div key={c.symbol} className="text-[11px] text-warning" data-testid={`ml-ds-range-warn-${c.symbol}`} data-coverage={c.kind}>
            {c.kind === 'outside'
              ? `${c.symbol}: no stored bars fall in this range (stored data covers ${c.data?.first_time?.slice(0, 10)} to ${c.data?.last_time?.slice(0, 10)}).`
              : `${c.symbol}: the range reaches outside the stored data (${c.data?.first_time?.slice(0, 10)} to ${c.data?.last_time?.slice(0, 10)}), so part of it has no bars.`}
            {' '}Fetch more history or narrow the range.
          </div>
        ))}
        {rangeChecked && coverage.some((c) => c.kind === 'nodata') && (
          <div className="text-[11px] text-warning" data-testid="ml-ds-range-nodata">
            {coverage.filter((c) => c.kind === 'nodata').map((c) => c.symbol).join(', ')}: nothing stored for this timeframe yet. Use Fetch history first.
          </div>
        )}

        <div className="space-y-1 border-t border-[var(--border-color)] pt-2" data-testid="ml-ds-context">
          <div className="text-[11px] font-semibold text-text-secondary">Context symbols (optional)</div>
          <div className="text-[11px] text-text-muted">
            Extra symbols, such as an index or a related asset, that your own formulas can read with <span className="font-mono">other(&quot;SPY&quot;, &quot;close&quot;)</span>. They are not rows of the dataset.
          </div>
          <SymbolDataPicker
            symbols={contextSymbols}
            onChange={setContextSymbols}
            timeframe={timeframe}
            max={MAX_CONTEXT_SYMBOLS}
            state={contextData}
            fetchBars={fetchBars}
          fetchCapped={fetchPlan.capped}
            testPrefix="ml-ds-ctx"
            listTestId="ml-ds-ctx-list"
            emptyText="No context symbols."
            placeholder="Add context symbol (e.g. SPY)"
            ariaLabel="Add context symbol"
          />
        </div>
      </section>

      {/* Features */}
      <section className={sectionCls} data-testid="ml-ds-features">
        <div className="flex items-center justify-between gap-2">
          <div className={headCls}>2. Features</div>
          <span className="text-[11px] font-mono text-text-secondary" data-testid="ml-ds-feature-total">
            {featureTotal} feature{featureTotal === 1 ? '' : 's'} ({selectedIds.length} built-in + {customRows.length} custom)
          </span>
        </div>
        <div className="text-[11px] text-text-muted">
          Nothing here is required: use built-in indicators, your own formulas, or only one of the two.
        </div>

        <div className="flex items-center justify-between gap-2">
          <div className="text-[11px] font-semibold text-text-secondary">Built-in indicators</div>
          <span className="text-[11px] font-mono text-text-secondary" data-testid="ml-ds-feature-count">{selectedIds.length} of {catalog.length} selected</span>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[11px] text-text-muted">Optional shortcuts</span>
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

        <div className="space-y-1 border-t border-[var(--border-color)] pt-2" data-testid="ml-ds-custom">
          <div className="text-[11px] font-semibold text-text-secondary">Custom features</div>
          <CustomFeatureEditor
            rows={customRows}
            onChange={setCustomRows}
            checks={checks}
            catalogIds={catalogIds}
            checkSymbol={symbols[0] ?? null}
            contextSymbols={contextSymbols}
            onAddContext={addContext}
          />
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
          {!asymmetric && (
            <label className="text-[11px] text-text-secondary">
              Threshold %
              <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={0} step={0.1} value={threshold} onChange={(e) => setThreshold(e.target.value)} disabled={kind === 'direction'} data-testid="ml-ds-threshold" />
            </label>
          )}
          {asymmetric && (
            <>
              <label className="text-[11px] text-text-secondary">
                Up threshold %
                <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={0} step={0.1} value={upThreshold} onChange={(e) => setUpThreshold(e.target.value)} data-testid="ml-ds-up-threshold" />
              </label>
              <label className="text-[11px] text-text-secondary">
                Down threshold %
                <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={0} step={0.1} value={downThreshold} onChange={(e) => setDownThreshold(e.target.value)} data-testid="ml-ds-down-threshold" />
              </label>
            </>
          )}
          {kind === 'return_threshold' && (
            <label className="flex items-center gap-1 pb-1 text-[11px] text-text-secondary">
              <input
                type="checkbox"
                className="accent-[rgb(var(--color-accent-rgb))]"
                checked={asym}
                onChange={(e) => {
                  setAsym(e.target.checked);
                  if (e.target.checked) { setUpThreshold(threshold); setDownThreshold(threshold); } else setThreshold(upThreshold);
                }}
                data-testid="ml-ds-asym"
              />
              Separate up and down thresholds
            </label>
          )}
          <label className="text-[11px] text-text-secondary">
            Fee % (round trip)
            <input className={`${inputCls} block mt-0.5 w-24`} type="number" min={0} max={10} step={0.01} value={fee} onChange={(e) => setFee(e.target.value)} aria-invalid={feeError !== null} data-testid="ml-ds-fee" />
          </label>
        </div>
        <div className="space-y-0.5 text-[11px] text-text-muted">
          <div data-testid="ml-ds-horizon-help">Horizon: how many bars into the future the outcome is measured. Rows at the end of the data have no future yet and are dropped.</div>
          <div data-testid="ml-ds-label-help">
            {kind === 'direction'
              ? `Direction: each row is labelled 1 (up) if the return ${Number.isFinite(horizonN) ? horizonN : '?'} bars later is above zero after the fee, otherwise 0. Automations treat 1 as long and 0 as flat, or short if shorting is allowed.`
              : asymmetric
                ? `Return threshold: labelled 1 if the return ${Number.isFinite(horizonN) ? horizonN : '?'} bars later is above +${upThreshold || '?'}% after the fee, -1 if a short would still earn more than ${downThreshold || '?'}% after paying the fee (the fall must beat the threshold plus the fee), otherwise 0 (flat).`
                : `Return threshold: labelled 1 if the return ${Number.isFinite(horizonN) ? horizonN : '?'} bars later is above +${threshold || '?'}% after the fee, -1 if a short would still earn more than ${threshold || '?'}% after paying the fee (the fall must beat the threshold plus the fee), otherwise 0 (flat).`}
          </div>
          {kind === 'return_threshold' && (
            <div data-testid="ml-ds-asym-help">Use separate thresholds when a move up and a move down are not equally worth trading, for example a higher bar for shorts.</div>
          )}
          <div data-testid="ml-ds-fee-help">
            Fee: the cost of buying and selling again, in percent of the price (commission, spread and slippage together). It is subtracted from every forward return before labelling, so labels and trading results are net of it.
          </div>
        </div>
      </section>

      {/* Summary and build */}
      <section className={sectionCls} data-testid="ml-ds-summary">
        <div className={headCls}>4. Review and build</div>
        <div className="text-xs text-text-secondary">
          About <span className="font-mono font-semibold text-text-primary" data-testid="ml-ds-estimated-rows">{estimate.rows}</span> rows
          <span className="text-text-muted" data-testid="ml-ds-estimate-detail">
            {' '}({symbols.length} symbol{symbols.length === 1 ? '' : 's'}, {rangeMode ? 'bars in the date range' : `up to ${Number.isFinite(barsN) ? barsN : 0} bars each`}, minus <span data-testid="ml-ds-warmup">{estimate.warmup}</span> warm-up and {horizonOk} horizon per symbol)
          </span>
        </div>
        {lowRows && (
          <div className="text-[11px] text-warning" data-testid="ml-ds-warn-rows">
            Only about {estimate.rows} rows. Under {MIN_ROWS_WARN} rows is too little to train and cross-validate reliably. Add bars or symbols.
          </div>
        )}
        {tooManyFeatures && (
          <div className="text-[11px] text-warning" data-testid="ml-ds-warn-features">
            {featureTotal} features for about {estimate.rows} rows is under {ROWS_PER_FEATURE} rows per feature, which invites overfitting. Drop features or add data.
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
            <div className="flex items-center gap-1.5">
              <button type="button" className={btnCls} onClick={() => duplicateSettings(card)} data-testid="ml-ds-result-duplicate">Duplicate settings</button>
              <button type="button" className={primaryBtnCls} onClick={() => onUseForTraining(card.name)} data-testid="ml-ds-result-train">Train on this dataset</button>
            </div>
          </div>
          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs md:grid-cols-4">
            <div><span className="text-text-muted">Rows </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-rows">{card.rows}</span></div>
            <div><span className="text-text-muted">Range </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-range">{fmtDate(card.first_time)} to {fmtDate(card.last_time)}</span></div>
            <div><span className="text-text-muted">Symbols </span><span className="font-mono text-text-primary">{card.symbols.join(', ')} ({card.timeframe})</span></div>
            <div><span className="text-text-muted">Features </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-features">{card.features.length}{(card.custom_features ?? []).length > 0 ? ` + ${card.custom_features.length} custom` : ''}</span></div>
            <div><span className="text-text-muted">Label </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-label">{describeLabel(card.label)}</span></div>
            <div><span className="text-text-muted">Fee </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-fee">{card.fee_pct ?? 0}% round trip</span></div>
            {(card.context_symbols ?? []).length > 0 && (
              <div><span className="text-text-muted">Context </span><span className="font-mono text-text-primary" data-testid="ml-ds-result-context">{card.context_symbols.join(', ')}</span></div>
            )}
          </div>
          {(card.custom_features ?? []).length > 0 && (
            <ul className="space-y-0.5 text-[11px] font-mono" data-testid="ml-ds-result-custom">
              {card.custom_features.map((c) => (
                <li key={c.name} data-testid={`ml-ds-result-custom-${c.name}`}><span className="text-accent">{c.name}</span> <span className="text-text-muted">=</span> <span className="text-text-primary">{c.expr}</span></li>
              ))}
            </ul>
          )}
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
        {datasets.map((d) => {
          const custom = d.custom_features ?? [];
          const ctx = d.context_symbols ?? [];
          const meta = [
            `${d.rows} rows`, d.symbols.join('+'), d.timeframe,
            `${fmtDate(d.first_time)} to ${fmtDate(d.last_time)}`,
            custom.length > 0 ? `custom: ${custom.map((c) => c.name).join(', ')}` : null,
            ctx.length > 0 ? `context: ${ctx.join(', ')}` : null,
            (d.fee_pct ?? 0) > 0 ? `fee ${d.fee_pct}%` : null,
            d.source !== 'user' ? `by ${d.source}` : null,
          ].filter((x): x is string => x !== null).join(', ');
          return (
            <div key={d.name} className={`flex flex-wrap items-center gap-2 border px-2 py-1 text-xs ${card?.name === d.name ? 'border-accent bg-accent/5' : 'border-[var(--border-color)] bg-surface-0'}`} data-testid={`ml-dataset-${d.name}`}>
              <button type="button" className="min-w-0 flex-1 truncate text-left font-mono text-text-primary hover:text-accent" onClick={() => void showDataset(d.name)} data-testid={`ml-ds-select-${d.name}`}>
                {d.name}
                <span className="text-text-muted" data-testid={`ml-ds-meta-${d.name}`}> {meta}</span>
              </button>
              <button type="button" className={btnCls} onClick={() => void showDataset(d.name)} data-testid={`ml-ds-preview-${d.name}`}>Preview</button>
              <button type="button" className={btnCls} onClick={() => duplicateSettings(d)} data-testid={`ml-ds-duplicate-${d.name}`}>Duplicate settings</button>
              <button type="button" className={btnCls} onClick={() => onUseForTraining(d.name)} data-testid={`ml-ds-train-${d.name}`}>Train</button>
              <VersionHistory kind="dataset" id={d.name} testId={`audit-history-open-dataset-${d.name}`} onRestored={() => void refreshDatasets()} className={btnCls} />
              {confirmDelete === d.name ? (
                <>
                  <span className="text-bear">Delete {d.name}? Its history is kept and it can be restored.</span>
                  <button type="button" className={`${btnCls} text-bear`} onClick={() => void remove(d.name)} data-testid={`ml-ds-delete-confirm-${d.name}`}>Delete (history is kept)</button>
                  <button type="button" className={btnCls} onClick={() => setConfirmDelete(null)} data-testid={`ml-ds-delete-cancel-${d.name}`}>Cancel</button>
                </>
              ) : (
                <button type="button" className={`${btnCls} hover:text-bear`} onClick={() => setConfirmDelete(d.name)} data-testid={`ml-ds-delete-${d.name}`} title="The dataset leaves the list; its history is kept and it can be restored">Delete (history is kept)</button>
              )}
            </div>
          );
        })}
        <DeletedObjects kind="dataset" onRestored={() => void refreshDatasets()} />
      </section>
    </div>
  );
};
