import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { deleteMLModel, getMlModel, listMLModels, updateMlModel } from '../../lib/tauri';
import { touchedKey, useArtifactStore } from '../../stores/artifactStore';
import type { MLModelDto, ModelDetailDto } from '../../lib/types';
import { ModelCompare } from './ModelCompare';
import { ModelDetail, SourceBadge } from './ModelDetail';
import type { ModelPatch } from './ModelDetail';
import {
  SOURCE_LABEL, algorithmLabel, displayName, exactTime, matchesQuery, pct, relativeTime, signedPct, sortRows,
} from './modelUtils';
import type { RegistryRow, SortKey } from './modelUtils';

export interface ModelsTabProps {
  /** Select this model when the tab opens (set when something navigated to a model). */
  initialModelId?: string;
  /** Train again from this model's settings: the workbench switches to the Train tab pre-filled. */
  onRetrain: (detail: ModelDetailDto) => void;
  /** Go to the Train tab with an empty form. */
  onOpenTrain: () => void;
}

const NARROW_PX = 600;

const selectCls = 'border border-[var(--border-color)] bg-surface-0 px-1.5 py-1 text-[11px] text-text-primary focus:border-accent focus:outline-none';
const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-1 text-[11px] font-mono text-text-secondary hover:text-text-primary disabled:opacity-40';
const accentBtn = 'border border-accent/50 bg-accent/15 px-2 py-1 text-[11px] font-mono text-accent hover:bg-accent/25 disabled:opacity-40';

function applyPatch(d: ModelDetailDto, patch: ModelPatch): ModelDetailDto {
  return {
    ...d,
    ...(patch.displayName !== undefined ? { display_name: patch.displayName } : {}),
    ...(patch.notes !== undefined ? { notes: patch.notes } : {}),
    ...(patch.tags !== undefined ? { tags: patch.tags } : {}),
  };
}

/** Undo only the fields a failed patch touched, so a concurrent edit to another field survives. */
function revertPatch(current: ModelDetailDto, before: ModelDetailDto, patch: ModelPatch): ModelDetailDto {
  return {
    ...current,
    ...(patch.displayName !== undefined ? { display_name: before.display_name } : {}),
    ...(patch.notes !== undefined ? { notes: before.notes } : {}),
    ...(patch.tags !== undefined ? { tags: before.tags } : {}),
  };
}

export const ModelsTab: React.FC<ModelsTabProps> = ({ initialModelId, onRetrain, onOpenTrain }) => {
  const modelVersion = useArtifactStore((s) => s.versions.model);
  const automationVersion = useArtifactStore((s) => s.versions.automation);
  const touched = useArtifactStore((s) => s.copilotTouched);
  const dismissTouched = useArtifactStore((s) => s.dismissTouched);

  const [summaries, setSummaries] = useState<MLModelDto[]>([]);
  const [details, setDetails] = useState<Record<string, ModelDetailDto>>({});
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [selectedId, setSelectedId] = useState<string | null>(initialModelId ?? null);
  const [checked, setChecked] = useState<string[]>([]);
  const [comparing, setComparing] = useState(false);
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState<SortKey>('newest');
  const [algoFilter, setAlgoFilter] = useState('');
  const [sourceFilter, setSourceFilter] = useState('');
  const [tagFilter, setTagFilter] = useState('');
  const [deleteTargets, setDeleteTargets] = useState<ModelDetailDto[] | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const detailsRef = useRef(details);
  detailsRef.current = details;
  const loadSeq = useRef(0);
  const deleteSeq = useRef(0);
  const listRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const [narrow, setNarrow] = useState(false);

  const load = useCallback(async () => {
    const seq = ++loadSeq.current;
    try {
      const list = await listMLModels();
      const results = await Promise.allSettled(list.map((m) => getMlModel(m.model_id)));
      if (seq !== loadSeq.current) return;
      const next: Record<string, ModelDetailDto> = {};
      results.forEach((r) => { if (r.status === 'fulfilled') next[r.value.model_id] = r.value; });
      setSummaries(list);
      setDetails(next);
      setError(null);
    } catch (e) {
      if (seq !== loadSeq.current) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (seq === loadSeq.current) setLoaded(true);
    }
  }, []);

  useEffect(() => { void load(); }, [load, modelVersion, automationVersion]);

  // In a narrow tile there is no room for list and detail side by side: show one at a time.
  useEffect(() => {
    const el = rootRef.current;
    if (!el || typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(([entry]) => setNarrow(entry.contentRect.width < NARROW_PX));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Something navigated to a model.
  useEffect(() => {
    if (initialModelId) { setSelectedId(initialModelId); setComparing(false); }
  }, [initialModelId]);

  // Opening a model clears its "new" badge, including one the Copilot adds while it is open.
  useEffect(() => {
    if (selectedId && touched[touchedKey('model', selectedId)]) dismissTouched('model', selectedId);
  }, [selectedId, touched, dismissTouched]);

  // Drop selections and checks for models that no longer exist.
  useEffect(() => {
    if (!loaded) return;
    const ids = new Set(summaries.map((m) => m.model_id));
    setChecked((c) => (c.every((id) => ids.has(id)) ? c : c.filter((id) => ids.has(id))));
    setSelectedId((s) => (s && !ids.has(s) ? null : s));
  }, [summaries, loaded]);

  const rows: RegistryRow[] = useMemo(() => summaries.map((s) => ({ summary: s, detail: details[s.model_id] ?? null })), [summaries, details]);
  const algorithms = useMemo(() => [...new Set(summaries.map((s) => s.algorithm))].sort(), [summaries]);
  const allTags = useMemo(() => [...new Set(Object.values(details).flatMap((d) => d.tags))].sort(), [details]);
  const sources = useMemo(() => [...new Set(Object.values(details).map((d) => d.source))].sort(), [details]);

  const visible = useMemo(() => {
    const filtered = rows.filter((r) =>
      matchesQuery(r, query)
      && (!algoFilter || r.summary.algorithm === algoFilter)
      && (!sourceFilter || r.detail?.source === sourceFilter || (sourceFilter === 'unknown' && !r.detail))
      && (!tagFilter || r.detail?.tags.includes(tagFilter)));
    return sortRows(filtered, sort);
  }, [rows, query, algoFilter, sourceFilter, tagFilter, sort]);

  const filtersActive = Boolean(query || algoFilter || sourceFilter || tagFilter);
  const clearFilters = () => { setQuery(''); setAlgoFilter(''); setSourceFilter(''); setTagFilter(''); };

  const open = useCallback((id: string) => {
    setSelectedId(id);
    setComparing(false);
    dismissTouched('model', id);
  }, [dismissTouched]);

  const toggleCheck = (id: string) => setChecked((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id]));

  const save = useCallback(async (id: string, patch: ModelPatch) => {
    const before = detailsRef.current[id];
    if (!before) throw new Error('Model is not loaded');
    setDetails((d) => (d[id] ? { ...d, [id]: applyPatch(d[id], patch) } : d));
    try {
      const next = await updateMlModel(id, patch);
      setDetails((d) => ({ ...d, [id]: next }));
    } catch (e) {
      setDetails((d) => (d[id] ? { ...d, [id]: revertPatch(d[id], before, patch) } : d));
      throw e;
    }
  }, []);

  const askDelete = useCallback(async (targets: ModelDetailDto[]) => {
    setDeleteError(null);
    const seq = ++deleteSeq.current;
    setDeleteTargets(targets);
    // Re-read so the list of dependent automations is current, not the cached copy.
    const fresh = await Promise.all(targets.map((t) => getMlModel(t.model_id).catch(() => t)));
    // Apply only to the dialog this call opened: it may have been cancelled or replaced meanwhile.
    setDeleteTargets((cur) => (cur && seq === deleteSeq.current ? fresh : cur));
  }, []);

  const confirmDelete = async () => {
    if (!deleteTargets) return;
    setDeleteBusy(true);
    setDeleteError(null);
    const failed: string[] = [];
    for (const t of deleteTargets) {
      try { await deleteMLModel(t.model_id); } catch (e) { failed.push(`${displayName(t)}: ${e instanceof Error ? e.message : String(e)}`); }
    }
    setDeleteBusy(false);
    if (failed.length) setDeleteError(failed.join('; '));
    else setDeleteTargets(null);
    setChecked((c) => c.filter((id) => !deleteTargets.some((t) => t.model_id === id)));
    if (selectedId && deleteTargets.some((t) => t.model_id === selectedId)) setSelectedId(null);
    setComparing(false);
    await load();
  };

  const onListKey = (e: React.KeyboardEvent) => {
    if (e.target instanceof HTMLInputElement && e.target.type !== 'checkbox') return;
    if (e.key === 'Escape') { setSelectedId(null); setComparing(false); return; }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    if (visible.length === 0) return;
    e.preventDefault();
    const focusedId = (document.activeElement as HTMLElement | null)?.closest('[data-row-id]')?.getAttribute('data-row-id') ?? selectedId;
    const at = visible.findIndex((r) => r.summary.model_id === focusedId);
    const nextIndex = e.key === 'ArrowDown' ? Math.min(visible.length - 1, at + 1) : Math.max(0, at <= 0 ? 0 : at - 1);
    const nextId = visible[nextIndex].summary.model_id;
    open(nextId);
    requestAnimationFrame(() => listRef.current?.querySelector<HTMLElement>(`[data-row-id="${CSS.escape(nextId)}"]`)?.focus());
  };

  const selected = selectedId ? details[selectedId] ?? null : null;
  const compareModels = checked.map((id) => details[id]).filter((d): d is ModelDetailDto => Boolean(d));
  const canCompare = checked.length >= 2 && checked.length <= 3 && compareModels.length === checked.length;
  const showCompare = comparing && canCompare;
  const split = showCompare || selectedId !== null;
  const deleteAutomations = deleteTargets?.flatMap((t) => t.used_by.map((u) => ({ ...u, model: displayName(t) }))) ?? [];

  const registry = (
    <div className={`flex min-h-0 flex-col ${split && !narrow ? 'w-[44%] min-w-[300px] border-r border-[var(--border-color)]' : 'w-full'} ${split && narrow ? 'hidden' : ''}`} data-testid="ml-models-registry">
      <div className="flex flex-wrap items-center gap-1.5 border-b border-[var(--border-color)] bg-surface-1 px-2 py-1.5">
        <input
          className={`${selectCls} min-w-[8rem] flex-1`} placeholder="Search name, algorithm, tag, dataset" aria-label="Search models"
          value={query} onChange={(e) => setQuery(e.target.value)} data-testid="ml-models-search"
        />
        <select className={selectCls} value={sort} onChange={(e) => setSort(e.target.value as SortKey)} aria-label="Sort models" data-testid="ml-models-sort">
          <option value="newest">Newest</option>
          <option value="accuracy">Accuracy</option>
          <option value="trading">Trading return</option>
        </select>
        <select className={selectCls} value={algoFilter} onChange={(e) => setAlgoFilter(e.target.value)} aria-label="Filter by algorithm" data-testid="ml-models-filter-algorithm">
          <option value="">All algorithms</option>
          {algorithms.map((a) => <option key={a} value={a}>{algorithmLabel(a)}</option>)}
        </select>
        <select className={selectCls} value={sourceFilter} onChange={(e) => setSourceFilter(e.target.value)} aria-label="Filter by source" data-testid="ml-models-filter-source">
          <option value="">All sources</option>
          {sources.map((s) => <option key={s} value={s}>{SOURCE_LABEL[s]}</option>)}
        </select>
        <select className={selectCls} value={tagFilter} onChange={(e) => setTagFilter(e.target.value)} aria-label="Filter by tag" data-testid="ml-models-filter-tag">
          <option value="">All tags</option>
          {allTags.map((t) => <option key={t} value={t}>{t}</option>)}
        </select>
        {filtersActive && <button type="button" className={btnCls} onClick={clearFilters} data-testid="ml-models-clear-filters">Clear</button>}
      </div>

      <div className="flex items-center gap-2 border-b border-[var(--border-color)] px-2 py-1 text-[11px]">
        <span className="text-text-muted" data-testid="ml-models-count">{visible.length === summaries.length ? `${summaries.length} model${summaries.length === 1 ? '' : 's'}` : `${visible.length} of ${summaries.length} models`}</span>
        <span className="flex-1" />
        {checked.length > 0 && <span className="text-text-secondary" data-testid="ml-models-selected-count">{checked.length} selected</span>}
        <button type="button" className={accentBtn} disabled={!canCompare} onClick={() => { setComparing(true); }} title={canCompare ? 'Compare side by side' : 'Tick 2 or 3 models to compare'} data-testid="ml-models-compare">Compare{checked.length ? ` (${checked.length})` : ''}</button>
        <button type="button" className={`${btnCls} hover:text-bear`} disabled={checked.length === 0} onClick={() => void askDelete(checked.map((id) => details[id]).filter((d): d is ModelDetailDto => Boolean(d)))} data-testid="ml-models-bulk-delete">Delete selected</button>
      </div>
      {checked.length === 1 && <div className="px-2 py-1 text-[10px] text-text-muted" data-testid="ml-models-compare-hint">Tick one more model to compare (up to 3).</div>}
      {checked.length > 3 && <div className="px-2 py-1 text-[10px] text-text-muted" data-testid="ml-models-compare-hint">Comparison needs 2 or 3 models. Untick some to compare.</div>}

      <div className="min-h-0 flex-1 overflow-auto" ref={listRef} onKeyDown={onListKey} data-testid="ml-models-list">
        {error && (
          <div className="m-2 border border-bear/40 bg-bear/10 px-2 py-1.5 text-[11px] text-bear" data-testid="ml-models-error">
            Could not load models: {error} <button type="button" className="ml-2 underline" onClick={() => void load()} data-testid="ml-models-retry">Retry</button>
          </div>
        )}
        {!loaded && !error && <div className="p-4 text-xs text-text-muted" data-testid="ml-models-loading">Loading models…</div>}
        {loaded && !error && summaries.length === 0 && (
          <div className="p-6 text-center" data-testid="ml-models-empty">
            <p className="text-xs text-text-secondary">No trained models yet.</p>
            <p className="mt-1 text-[11px] text-text-muted">Build a dataset and train one on the Train tab. Models the Copilot trains show up here too.</p>
            <button type="button" className={`${accentBtn} mt-3`} onClick={onOpenTrain} data-testid="ml-models-empty-train">Go to Train</button>
          </div>
        )}
        {loaded && summaries.length > 0 && visible.length === 0 && (
          <div className="p-6 text-center" data-testid="ml-models-no-match">
            <p className="text-xs text-text-secondary">No models match these filters.</p>
            <div className="mt-3 flex justify-center gap-2">
              <button type="button" className={btnCls} onClick={clearFilters} data-testid="ml-models-no-match-clear">Clear filters</button>
              <button type="button" className={accentBtn} onClick={onOpenTrain} data-testid="ml-models-no-match-train">Train a new model</button>
            </div>
          </div>
        )}
        {visible.length > 0 && (
          <table className="w-full text-[11px]" data-testid="ml-models-table">
            <thead className="sticky top-0 bg-surface-1">
              <tr className="text-left text-[10px] uppercase text-text-muted">
                <th className="w-6 px-2 py-1 font-normal" />
                <th className="px-1.5 py-1 font-normal">Model</th>
                {!split && <th className="px-1.5 py-1 font-normal">Source</th>}
                {!split && <th className="px-1.5 py-1 font-normal">Created</th>}
                {!split && <th className="px-1.5 py-1 font-normal">Rows</th>}
                <th className="px-1.5 py-1 font-normal">Accuracy</th>
                <th className="px-1.5 py-1 font-normal">Trading</th>
                {!split && <th className="px-1.5 py-1 font-normal">Used</th>}
                {!split && <th className="px-1.5 py-1 font-normal">Tags</th>}
              </tr>
            </thead>
            <tbody>
              {visible.map(({ summary: s, detail: d }) => {
                const isNew = Boolean(touched[touchedKey('model', s.model_id)]);
                const isSel = selectedId === s.model_id;
                const t = d?.trading ?? null;
                return (
                  <tr
                    key={s.model_id} tabIndex={0} data-row-id={s.model_id} data-testid="ml-model-row" data-model-id={s.model_id} data-selected={isSel}
                    aria-selected={isSel}
                    className={`cursor-pointer border-t border-[var(--border-color)] outline-none focus:bg-surface-2 ${isSel ? 'bg-accent/10' : 'hover:bg-surface-1'}`}
                    onClick={() => open(s.model_id)}
                    onKeyDown={(e) => {
                      if (e.target !== e.currentTarget) return;
                      if (e.key === 'Enter') { e.preventDefault(); open(s.model_id); }
                      if (e.key === ' ') { e.preventDefault(); toggleCheck(s.model_id); }
                    }}
                  >
                    <td className="px-2 py-1" onClick={(e) => e.stopPropagation()}>
                      <input type="checkbox" checked={checked.includes(s.model_id)} onChange={() => toggleCheck(s.model_id)} aria-label={`Select ${d ? displayName(d) : s.model_id}`} data-testid="ml-model-check" />
                    </td>
                    <td className="max-w-[14rem] px-1.5 py-1">
                      <div className="flex items-center gap-1.5">
                        <span className="truncate font-mono text-text-primary" title={s.model_id} data-testid="ml-model-name">{d ? displayName(d) : s.model_id}</span>
                        {isNew && <span className="shrink-0 border border-accent/60 bg-accent/20 px-1 font-mono text-[9px] uppercase text-accent" data-testid="ml-model-new-badge">new</span>}
                      </div>
                      <div className="flex items-center gap-1.5">
                        <span className="truncate text-[10px] text-text-muted" data-testid="ml-model-algorithm">{algorithmLabel(s.algorithm)}</span>
                        {split && d && <SourceBadge source={d.source} testId="ml-model-source" />}
                      </div>
                    </td>
                    {!split && <td className="px-1.5 py-1">{d ? <SourceBadge source={d.source} testId="ml-model-source" /> : <span className="text-text-muted">...</span>}</td>}
                    {!split && <td className="px-1.5 py-1 text-text-secondary" title={exactTime(s.created_at)} data-testid="ml-model-created">{relativeTime(s.created_at)}</td>}
                    {!split && <td className="px-1.5 py-1 font-mono text-text-secondary" data-testid="ml-model-rows">{d?.rows ?? 'n/a'}</td>}
                    <td className="px-1.5 py-1 font-mono" data-testid="ml-model-accuracy">{pct(s.metrics.accuracy)}</td>
                    <td className="px-1.5 py-1 font-mono" data-testid="ml-model-trading">
                      {t ? (
                        <span className={t.strategy_return_pct >= t.buy_hold_return_pct ? 'text-bull' : 'text-bear'}>
                          {signedPct(t.strategy_return_pct)}<span className="text-text-muted"> vs {signedPct(t.buy_hold_return_pct)}</span>
                        </span>
                      ) : <span className="text-text-muted">n/a</span>}
                    </td>
                    {!split && <td className="px-1.5 py-1 font-mono text-text-secondary" data-testid="ml-model-used">{d?.used_by.length ?? 0}</td>}
                    {!split && (
                      <td className="px-1.5 py-1" data-testid="ml-model-tags">
                        <div className="flex flex-wrap gap-1">{(d?.tags ?? []).map((tag) => <span key={tag} className="border border-[var(--border-color)] bg-surface-2 px-1 font-mono text-[10px] text-text-secondary">{tag}</span>)}</div>
                      </td>
                    )}
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );

  let pane: React.ReactNode = null;
  if (showCompare) {
    pane = <ModelCompare models={compareModels} onClose={() => setComparing(false)} onOpen={open} />;
  } else if (selectedId) {
    pane = selected ? (
      <ModelDetail
        detail={selected} onSave={(patch) => save(selected.model_id, patch)} onRetrain={onRetrain}
        onDelete={(d) => void askDelete([d])} onClose={() => setSelectedId(null)} onChanged={() => void load()}
      />
    ) : (
      <div className="p-4 text-xs text-text-muted" data-testid="ml-model-loading">{loaded ? 'This model could not be loaded.' : 'Loading model…'}</div>
    );
  }

  return (
    <div ref={rootRef} className="relative flex h-full min-h-[32rem]" data-testid="ml-models-tab">
      {registry}
      {split && <div className="min-w-0 flex-1 bg-surface-0" data-testid="ml-models-pane">{pane}</div>}

      {deleteTargets && (
        <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/60" role="dialog" aria-modal="true" aria-label="Confirm delete" data-testid="ml-delete-dialog">
          <div className="max-h-[90%] w-[28rem] max-w-[95%] overflow-auto border border-bear/50 bg-surface-1 p-4">
            <h3 className="font-mono text-sm text-text-primary">Delete {deleteTargets.length === 1 ? 'this model' : `${deleteTargets.length} models`}?</h3>
            <ul className="mt-2 space-y-0.5 text-[11px] text-text-secondary" data-testid="ml-delete-models">
              {deleteTargets.map((t) => <li key={t.model_id} className="font-mono">{displayName(t)}</li>)}
            </ul>
            {deleteAutomations.length > 0 ? (
              <div className="mt-3 border border-warning/40 bg-warning/10 p-2" data-testid="ml-delete-warning">
                <p className="text-[11px] text-warning">These automations use {deleteTargets.length === 1 ? 'it' : 'them'} and will stop working:</p>
                <ul className="mt-1 space-y-0.5 text-[11px]">
                  {deleteAutomations.map((a) => (
                    <li key={a.id} className="font-mono text-text-primary" data-testid="ml-delete-automation">{a.name} <span className="text-text-muted">({a.symbol}, {a.enabled ? 'enabled' : 'paused'})</span></li>
                  ))}
                </ul>
              </div>
            ) : (
              <p className="mt-3 text-[11px] text-text-muted" data-testid="ml-delete-no-usage">No automation uses {deleteTargets.length === 1 ? 'this model' : 'these models'}.</p>
            )}
            <p className="mt-2 text-[10px] text-text-muted">The saved model file is removed. This cannot be undone.</p>
            {deleteError && <p className="mt-2 text-[11px] text-bear" data-testid="ml-delete-error">{deleteError}</p>}
            <div className="mt-3 flex justify-end gap-2">
              <button type="button" className={btnCls} onClick={() => setDeleteTargets(null)} disabled={deleteBusy} data-testid="ml-delete-cancel">Cancel</button>
              <button type="button" className="border border-bear/60 bg-bear/20 px-2 py-1 font-mono text-[11px] text-bear hover:bg-bear/30 disabled:opacity-50" onClick={() => void confirmDelete()} disabled={deleteBusy} data-testid="ml-delete-confirm">{deleteBusy ? 'Deleting…' : 'Delete'}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
