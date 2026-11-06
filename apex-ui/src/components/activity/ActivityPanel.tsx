import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { listActivity } from '../../lib/tauri';
import { useArtifactStore } from '../../stores/artifactStore';
import { openItem, useNavStore } from '../../stores/navStore';
import type { NavKind } from '../../stores/navStore';
import type { ActivityDto } from '../../lib/types';
import { LedgerBadge, IntegrityView } from '../audit/LedgerBadge';
import { OrderTimeline } from '../audit/OrderTimeline';
import { FETCH_LIMIT, ACTOR_LABEL, type ActivityRowActor, ACTOR_TONE, CONTROL_ALL, CONTROL_GROUPS, actionLabel, activityToCsv, formatDetails, inControlGroup, maskSecrets } from './activityUtils';

const NAV_KINDS: Record<string, NavKind> = { strategy: 'strategy', model: 'model', automation: 'automation', dataset: 'dataset' };

const DATE_PRESETS = [
  { id: 'all', label: 'All time', ms: 0 },
  { id: 'hour', label: 'Last hour', ms: 3600_000 },
  { id: 'day', label: 'Last 24 hours', ms: 86_400_000 },
  { id: 'week', label: 'Last 7 days', ms: 7 * 86_400_000 },
];

type View = 'activity' | 'orders' | 'integrity';
const VIEWS: { id: View; label: string }[] = [
  { id: 'activity', label: 'Activity' },
  { id: 'orders', label: 'Orders' },
  { id: 'integrity', label: 'Integrity' },
];

const selectCls = 'border border-[var(--border-color)] bg-surface-0 px-1.5 py-1 text-[11px] text-text-primary focus:border-accent focus:outline-none';
const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-1 text-[11px] font-mono text-text-secondary hover:text-text-primary disabled:opacity-40';

export const ActivityPanel: React.FC = () => {
  const version = useArtifactStore((s) => s.activityVersion);
  const request = useNavStore((s) => s.request);
  const clearRequest = useNavStore((s) => s.clear);

  const [view, setView] = useState<View>('activity');
  const [entries, setEntries] = useState<ActivityDto[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [kind, setKind] = useState('');
  const [actor, setActor] = useState('');
  const [status, setStatus] = useState('');
  const [group, setGroup] = useState('');
  const [query, setQuery] = useState('');
  const [preset, setPreset] = useState('all');
  const [targetFilter, setTargetFilter] = useState('');
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const list = await listActivity({ limit: FETCH_LIMIT });
      setEntries(list);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => { void load(); }, [load, version]);

  // "Show only this model's activity" arrives as a navigation request.
  useEffect(() => {
    if (request?.kind === 'activity') {
      setTargetFilter(request.id);
      clearRequest(request.nonce);
    }
  }, [request, clearRequest]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const span = DATE_PRESETS.find((p) => p.id === preset)?.ms ?? 0;
    const cutoff = span ? Date.now() - span : 0;
    return entries.filter((a) =>
      (!kind || a.kind === kind)
      && (!actor || a.actor === actor)
      && (!status || (status === 'ok' ? a.ok : !a.ok))
      && inControlGroup(a.action, group)
      && (!targetFilter || a.target_id === targetFilter)
      && (!cutoff || Date.parse(a.ts) >= cutoff)
      && (!q || `${a.summary} ${a.action} ${a.target_id} ${a.details ?? ''}`.toLowerCase().includes(q)));
  }, [entries, kind, actor, status, group, query, preset, targetFilter]);

  const filtersActive = Boolean(kind || actor || status || group || query || targetFilter || preset !== 'all');
  const clearFilters = () => { setKind(''); setActor(''); setStatus(''); setGroup(''); setQuery(''); setPreset('all'); setTargetFilter(''); };
  const flash = (msg: string) => { setNotice(msg); window.setTimeout(() => setNotice(null), 4000); };

  const exportCsv = () => {
    const blob = new Blob([activityToCsv(filtered)], { type: 'text/csv;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `apex-activity-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-')}.csv`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    flash(`Exported ${filtered.length} entr${filtered.length === 1 ? 'y' : 'ies'} as CSV.`);
  };

  const copyJson = async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(filtered.map((a) => ({ ...a, summary: maskSecrets(a.summary), details: a.details === null ? null : maskSecrets(a.details) })), null, 2));
      flash(`Copied ${filtered.length} entr${filtered.length === 1 ? 'y' : 'ies'} as JSON.`);
    } catch {
      flash('Could not copy: the clipboard is not available.');
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col text-xs" data-testid="activity-panel">
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-color)] bg-surface-1 px-2 py-1" data-testid="activity-views">
        <div role="tablist" aria-label="Activity views" className="flex gap-1">
          {VIEWS.map((v) => (
            <button
              key={v.id} type="button" role="tab" aria-selected={view === v.id} onClick={() => setView(v.id)} data-testid={`activity-view-${v.id}`}
              className={`border px-2 py-0.5 font-mono text-[11px] ${view === v.id ? 'border-accent bg-accent/10 text-accent' : 'border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary'}`}
            >
              {v.label}
            </button>
          ))}
        </div>
        <span className="flex-1" />
        <LedgerBadge onOpen={() => setView('integrity')} />
      </div>
      {view === 'orders' && <OrderTimeline />}
      {view === 'integrity' && <div className="min-h-0 flex-1 overflow-auto"><IntegrityView /></div>}
      {view === 'activity' && (<>
      <div className="flex flex-wrap items-center gap-1.5 border-b border-[var(--border-color)] bg-surface-1 px-2 py-1.5">
        <input className={`${selectCls} min-w-[8rem] flex-1`} placeholder="Search summary, target, details" aria-label="Search activity" value={query} onChange={(e) => setQuery(e.target.value)} data-testid="activity-search" />
        <select className={selectCls} value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Filter by kind" data-testid="activity-filter-kind">
          <option value="">All kinds</option>
          {['strategy', 'model', 'dataset', 'automation', 'order', 'system'].map((k) => <option key={k} value={k}>{k}</option>)}
        </select>
        <select className={selectCls} value={group} onChange={(e) => setGroup(e.target.value)} aria-label="Filter by change type" title="Control: changes to limits, credentials, the vault, brokers and live trading" data-testid="activity-filter-group">
          <option value="">All changes</option>
          <option value={CONTROL_ALL}>Control (all)</option>
          {CONTROL_GROUPS.map((g) => <option key={g.id} value={g.id}>Control: {g.label}</option>)}
        </select>
        <select className={selectCls} value={actor} onChange={(e) => setActor(e.target.value)} aria-label="Filter by actor" data-testid="activity-filter-actor">
          <option value="">All actors</option>
          {(Object.keys(ACTOR_LABEL) as ActivityRowActor[]).map((a) => <option key={a} value={a}>{ACTOR_LABEL[a]}</option>)}
        </select>
        <select className={selectCls} value={status} onChange={(e) => setStatus(e.target.value)} aria-label="Filter by status" data-testid="activity-filter-status">
          <option value="">Any status</option>
          <option value="ok">OK</option>
          <option value="failed">Failed</option>
        </select>
        <select className={selectCls} value={preset} onChange={(e) => setPreset(e.target.value)} aria-label="Filter by date" data-testid="activity-filter-date">
          {DATE_PRESETS.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
        </select>
        {filtersActive && <button type="button" className={btnCls} onClick={clearFilters} data-testid="activity-clear-filters">Clear</button>}
      </div>

      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-color)] px-2 py-1">
        <span className="text-text-muted" data-testid="activity-count">{filtered.length === entries.length ? `${entries.length} entr${entries.length === 1 ? 'y' : 'ies'}` : `${filtered.length} of ${entries.length} entries`}</span>
        {targetFilter && (
          <span className="flex items-center gap-1 border border-accent/50 bg-accent/10 px-1.5 py-px font-mono text-[10px] text-accent" data-testid="activity-target-chip">
            only {targetFilter}
            <button type="button" aria-label="Show all activity" onClick={() => setTargetFilter('')} data-testid="activity-target-clear">×</button>
          </span>
        )}
        <span className="flex-1" />
        {notice && <span className="text-[10px] text-text-secondary" data-testid="activity-notice">{notice}</span>}
        <button type="button" className={btnCls} disabled={filtered.length === 0} onClick={exportCsv} data-testid="activity-export-csv">Export CSV</button>
        <button type="button" className={btnCls} disabled={filtered.length === 0} onClick={() => void copyJson()} data-testid="activity-copy-json">Copy JSON</button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {error && (
          <div className="m-2 border border-bear/40 bg-bear/10 px-2 py-1.5 text-[11px] text-bear" data-testid="activity-error">
            Could not load activity: {error} <button type="button" className="ml-2 underline" onClick={() => void load()} data-testid="activity-retry">Retry</button>
          </div>
        )}
        {!loaded && !error && <div className="p-4 text-text-muted" data-testid="activity-loading">Loading activity…</div>}
        {loaded && !error && entries.length === 0 && (
          <div className="p-6 text-center text-text-muted" data-testid="activity-empty">
            No activity yet. Anything you or the Copilot create, change or delete is recorded here.
          </div>
        )}
        {loaded && entries.length > 0 && filtered.length === 0 && (
          <div className="p-6 text-center text-text-muted" data-testid="activity-no-match">
            Nothing matches these filters. <button type="button" className="underline" onClick={clearFilters} data-testid="activity-no-match-clear">Clear filters</button>
          </div>
        )}
        <ul>
          {filtered.map((a) => {
            const open = Boolean(expanded[a.id]);
            const nav = NAV_KINDS[a.kind];
            return (
              <li
                key={a.id} data-testid="activity-row" data-actor={a.actor} data-ok={a.ok} data-kind={a.kind}
                className={`border-b border-[var(--border-color)] px-2 py-1.5 ${a.ok ? '' : 'border-l-2 border-l-bear bg-bear/10'}`}
              >
                <div className="flex items-start gap-2">
                  <button type="button" aria-expanded={open} aria-label={open ? 'Hide details' : 'Show details'} className="mt-px w-3 shrink-0 text-text-muted hover:text-text-primary" onClick={() => setExpanded((x) => ({ ...x, [a.id]: !open }))} data-testid="activity-row-toggle">{open ? '▾' : '▸'}</button>
                  <span className="w-20 shrink-0 font-mono text-[10px] text-text-muted" title={a.ts} data-testid="activity-time">
                    {new Date(a.ts).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}<br />{new Date(a.ts).toLocaleTimeString()}
                  </span>
                  <span className={`h-fit shrink-0 border px-1.5 py-px font-mono text-[10px] uppercase ${ACTOR_TONE[a.actor]}`} data-testid="activity-actor">{ACTOR_LABEL[a.actor]}</span>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-x-2">
                      <span className="font-mono text-[10px] uppercase text-text-secondary" data-testid="activity-action">{actionLabel(a.action)}</span>
                      <span className={`font-mono text-[10px] uppercase ${a.ok ? 'text-bull' : 'font-semibold text-bear'}`} data-testid="activity-status">{a.ok ? 'ok' : 'failed'}</span>
                      {!a.ok && <span className="border border-bear bg-bear/20 px-1.5 py-px font-mono text-[10px] font-semibold uppercase text-bear" title="This attempt did not succeed" data-testid="activity-failed-badge">failed attempt</span>}
                    </div>
                    <div className={`break-words ${a.ok ? 'text-text-primary' : 'text-bear'}`} data-testid="activity-summary">{maskSecrets(a.summary)}</div>
                    {!a.ok && a.details && <div className="mt-0.5 break-words font-mono text-[10px] text-bear" data-testid="activity-error-preview">{maskSecrets(a.details.split('\n')[0]).slice(0, 240)}</div>}
                    <div className="mt-0.5 flex flex-wrap items-center gap-2 text-[10px]">
                      {nav && a.target_id ? (
                        <button type="button" className="max-w-full truncate font-mono text-accent hover:underline" title={`Open ${a.kind}`} onClick={() => openItem(nav, a.target_id)} data-testid="activity-target">{a.kind}: {a.target_id}</button>
                      ) : (
                        <span className="font-mono text-text-muted" data-testid="activity-target-plain">{a.target_id ? `${a.kind}: ${a.target_id}` : a.kind}</span>
                      )}
                      {a.target_id && (
                        <button type="button" className="text-text-muted hover:text-text-primary" onClick={() => setTargetFilter(a.target_id)} data-testid="activity-only-target">
                          Show only this {a.kind === 'model' ? "model's" : a.kind === 'system' || a.kind === 'order' ? "item's" : `${a.kind}'s`} activity
                        </button>
                      )}
                    </div>
                    {open && (
                      <pre className="mt-1 max-h-60 overflow-auto whitespace-pre-wrap break-words border border-[var(--border-color)] bg-surface-0 p-1.5 font-mono text-[10px] text-text-secondary" data-testid="activity-details">
                        {`id: ${a.id}\ntime: ${a.ts}\naction: ${a.action}\ntarget: ${a.kind} ${a.target_id}\nstatus: ${a.ok ? 'ok' : 'failed'}\n\n${a.details ? formatDetails(a.details) : '(no further details)'}`}
                      </pre>
                    )}
                  </div>
                </div>
              </li>
            );
          })}
        </ul>
      </div>
      </>)}
    </div>
  );
};
