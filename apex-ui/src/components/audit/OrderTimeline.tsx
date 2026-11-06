import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { createPortal } from 'react-dom';
import { listOrderEvents } from '../../lib/tauri';
import type { OrderEventDto } from '../../lib/types';
import { exactTime } from '../ml/modelUtils';
import {
  STATE_LABEL, STATE_TONE, eventFacts, eventLabel, groupOrderEvents, shortHash, sourceLabel, sourceTarget,
  type OrderGroup, type OrderState,
} from './auditUtils';
import { VersionHistory } from './VersionHistory';

const POLL_MS = 3000;
const selectCls = 'border border-[var(--border-color)] bg-surface-0 px-1.5 py-1 text-[11px] text-text-primary focus:border-accent focus:outline-none';
const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-0.5 font-mono text-[10px] text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-40';

const SOURCE_TONE: Record<string, string> = {
  manual: 'border-[var(--border-color)] bg-surface-2 text-text-secondary',
  copilot: 'border-accent/50 bg-accent/10 text-accent',
  automation: 'border-warning/40 bg-warning/10 text-warning',
  strategy: 'border-bull/40 bg-bull/10 text-bull',
  kill_switch: 'border-bear/50 bg-bear/10 text-bear',
};

const GroupCard: React.FC<{ group: OrderGroup }> = ({ group }) => {
  // The strategy / automation version that produced the order, opened in the history viewer.
  const [viewing, setViewing] = useState(false);
  const target = sourceTarget(group.source);
  const system = group.state === 'system';
  return (
    <section className="border border-[var(--border-color)] bg-surface-1" data-testid="order-timeline-group" data-order-id={group.orderId} data-state={group.state} data-broker={group.brokerId}>
      <header className="flex flex-wrap items-center gap-2 border-b border-[var(--border-color)] px-2 py-1.5">
        {system ? (
          <span className="font-semibold text-text-primary" data-testid="order-group-title">System events</span>
        ) : (
          <>
            <span className="font-mono text-[11px] text-text-primary" data-testid="order-group-id" title={group.orderId || 'This request never became a broker order'}>
              {group.orderId ? `order ${group.orderId.length > 14 ? `${group.orderId.slice(0, 14)}…` : group.orderId}` : 'not sent to a broker'}
            </span>
            {group.symbol && <span className="font-mono text-[11px] text-text-secondary">{group.side} {group.quantity ?? ''} {group.symbol}</span>}
            <span className="font-mono text-[10px] text-text-muted" data-testid="order-group-broker">{group.brokerId}</span>
            <span className={`font-mono text-[10px] uppercase ${STATE_TONE[group.state]}`} data-testid="order-final-status">{STATE_LABEL[group.state]}</span>
            <span className="flex-1" />
            <span className={`border px-1.5 py-px font-mono text-[10px] ${SOURCE_TONE[group.source?.kind ?? ''] ?? SOURCE_TONE.manual}`} data-testid="order-source-badge" title={group.source?.hash ? `content ${group.source.hash}` : undefined}>
              {sourceLabel(group.source)}
            </span>
            {target && (
              <>
                <button type="button" className={btnCls} onClick={() => setViewing(true)} title={`Open version ${target.version} of ${target.id} in the history viewer`} data-testid="order-source-link">
                  View {target.kind} v{target.version}
                </button>
                <VersionHistory kind={target.kind} id={target.id} open={viewing} onClose={() => setViewing(false)} initialVersion={target.version} />
              </>
            )}
          </>
        )}
      </header>
      <ol className="px-2 py-1">
        {group.events.map((e) => (
          <li key={e.seq} className="flex flex-wrap items-baseline gap-x-2 border-b border-[var(--border-color)]/50 py-1 last:border-b-0" data-testid="order-event" data-event={e.event} data-seq={e.seq}>
            <span className="w-16 shrink-0 font-mono text-[10px] text-text-muted" title={exactTime(e.ts)}>{new Date(e.ts).toLocaleTimeString()}</span>
            <span className={`w-36 shrink-0 font-mono text-[11px] ${e.event.includes('reject') ? 'text-bear' : 'text-text-primary'}`} data-testid="order-event-name">{eventLabel(e.event)}</span>
            <span className="min-w-0 flex-1 break-words text-[11px] text-text-secondary" data-testid="order-event-facts">{eventFacts(e)}</span>
            <span className="font-mono text-[10px] text-text-muted" title={`event hash ${e.hash}`}>{e.actor} {shortHash(e.hash)}</span>
          </li>
        ))}
      </ol>
    </section>
  );
};

interface Props {
  /** Show only this order (no filters). Used by the blotter's Audit trail. */
  orderId?: string;
  brokerId?: string;
}

/** Order events grouped into one timeline per order: submitted, risk, ack, fills, final state, and what produced it. */
export const OrderTimeline: React.FC<Props> = ({ orderId, brokerId }) => {
  const single = Boolean(orderId);
  const [events, setEvents] = useState<OrderEventDto[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [broker, setBroker] = useState('');
  const [idQuery, setIdQuery] = useState('');
  const [sourceKind, setSourceKind] = useState('');
  const [state, setState] = useState<'' | OrderState>('');

  const load = useCallback(async () => {
    try {
      setEvents(await listOrderEvents({ ...(orderId ? { orderId } : {}), ...(brokerId ? { brokerId } : {}), limit: 1000 }));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  }, [orderId, brokerId]);

  useEffect(() => {
    void load();
    const t = setInterval(() => void load(), POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  const groups = useMemo(() => groupOrderEvents(events), [events]);
  const brokers = useMemo(() => Array.from(new Set(groups.map((g) => g.brokerId).filter(Boolean))).sort(), [groups]);
  const sources = useMemo(() => Array.from(new Set(groups.map((g) => g.source?.kind).filter((k): k is string => Boolean(k)))).sort(), [groups]);
  const shown = useMemo(() => {
    const q = idQuery.trim().toLowerCase();
    return groups.filter((g) =>
      (!broker || g.brokerId === broker)
      && (!q || g.orderId.toLowerCase().includes(q))
      && (!sourceKind || g.source?.kind === sourceKind)
      && (!state || g.state === state));
  }, [groups, broker, idQuery, sourceKind, state]);
  const filtersActive = Boolean(broker || idQuery || sourceKind || state);

  return (
    <div className="flex min-h-0 flex-1 flex-col text-xs" data-testid={single ? 'order-timeline-single' : 'order-timeline'}>
      {!single && (
        <div className="flex flex-wrap items-center gap-1.5 border-b border-[var(--border-color)] bg-surface-1 px-2 py-1.5">
          <input className={`${selectCls} min-w-[7rem] flex-1`} placeholder="Order id" aria-label="Filter by order id" value={idQuery} onChange={(e) => setIdQuery(e.target.value)} data-testid="orders-filter-id" />
          <select className={selectCls} value={broker} onChange={(e) => setBroker(e.target.value)} aria-label="Filter by broker" data-testid="orders-filter-broker">
            <option value="">All brokers</option>
            {brokers.map((b) => <option key={b} value={b}>{b}</option>)}
          </select>
          <select className={selectCls} value={sourceKind} onChange={(e) => setSourceKind(e.target.value)} aria-label="Filter by source" data-testid="orders-filter-source">
            <option value="">All sources</option>
            {sources.map((s) => <option key={s} value={s}>{s === 'copilot' ? 'Copilot' : s}</option>)}
          </select>
          <select className={selectCls} value={state} onChange={(e) => setState(e.target.value as '' | OrderState)} aria-label="Filter by status" data-testid="orders-filter-status">
            <option value="">Any status</option>
            {(['submitted', 'open', 'partially_filled', 'filled', 'cancelled', 'rejected'] as OrderState[]).map((s) => <option key={s} value={s}>{STATE_LABEL[s]}</option>)}
          </select>
          {filtersActive && <button type="button" className={btnCls} onClick={() => { setBroker(''); setIdQuery(''); setSourceKind(''); setState(''); }} data-testid="orders-filter-clear">Clear</button>}
          <span className="flex-1" />
          <span className="text-text-muted" data-testid="orders-count">{shown.length === groups.length ? `${groups.length} order${groups.length === 1 ? '' : 's'}` : `${shown.length} of ${groups.length} orders`}</span>
        </div>
      )}
      <div className="min-h-0 flex-1 space-y-2 overflow-auto p-2">
        {error && <div className="border border-bear/40 bg-bear/10 px-2 py-1.5 text-bear" role="alert" data-testid="orders-error">Could not load order events: {error}</div>}
        {!loaded && !error && <div className="p-4 text-text-muted">Loading order events…</div>}
        {loaded && !error && groups.length === 0 && (
          <div className="p-6 text-center text-text-muted" data-testid="orders-empty">
            {single ? 'No audit events were recorded for this order.' : 'No orders recorded yet. Every order, and every state it goes through, is recorded here as a separate event that is never changed.'}
          </div>
        )}
        {groups.length > 0 && shown.length === 0 && <div className="p-6 text-center text-text-muted" data-testid="orders-no-match">Nothing matches these filters.</div>}
        {shown.map((g) => <GroupCard key={g.key} group={g} />)}
      </div>
    </div>
  );
};

/** A drawer with the audit trail of one order (opened from the blotter). */
export const OrderAuditDrawer: React.FC<{ orderId: string; brokerId: string; onClose: () => void }> = ({ orderId, brokerId, onClose }) => {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
  return createPortal(
    <div className="fixed inset-0 z-[80] flex justify-end bg-black/50" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div role="dialog" aria-modal="true" aria-label={`Audit trail of order ${orderId}`} className="flex h-full w-full max-w-[36rem] flex-col border-l border-[var(--border-color)] bg-surface-0 shadow-xl" data-testid="order-audit-drawer">
        <div className="flex items-center gap-2 border-b border-[var(--border-color)] bg-surface-1 px-3 py-2">
          <div className="min-w-0 flex-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">Audit trail</div>
            <div className="truncate font-mono text-sm text-text-primary" title={orderId}>order {orderId}</div>
          </div>
          <button type="button" className={btnCls} onClick={onClose} aria-label="Close audit trail" data-testid="order-audit-close">×</button>
        </div>
        <OrderTimeline orderId={orderId} brokerId={brokerId} />
      </div>
    </div>,
    document.body,
  );
};
