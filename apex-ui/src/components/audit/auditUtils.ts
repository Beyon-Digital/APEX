import type { OrderEventDto, OrderSourceDto, VersionDto, VersionedKind } from '../../lib/types';

/* ----- labels ----- */

export const KIND_LABEL: Record<VersionedKind, string> = {
  strategy: 'strategy', estimator: 'estimator', recipe: 'algorithm recipe', dataset: 'dataset', model: 'model', automation: 'automation', notebook: 'notebook',
};

export const ACTION_TONE: Record<VersionDto['action'], string> = {
  create: 'border-bull/40 bg-bull/10 text-bull',
  update: 'border-accent/50 bg-accent/10 text-accent',
  delete: 'border-bear/50 bg-bear/10 text-bear',
  restore: 'border-warning/40 bg-warning/10 text-warning',
  external: 'border-warning/60 bg-warning/15 text-warning',
};

export const ACTOR_LABELS: Record<string, string> = { user: 'You', copilot: 'Copilot', automation: 'Automation', system: 'System', external: 'External' };
export const ACTOR_TONES: Record<string, string> = {
  user: 'border-bull/40 bg-bull/10 text-bull',
  copilot: 'border-accent/50 bg-accent/10 text-accent',
  automation: 'border-warning/40 bg-warning/10 text-warning',
  system: 'border-[var(--border-color)] bg-surface-2 text-text-muted',
  external: 'border-bear/50 bg-bear/10 text-bear',
};

export const shortHash = (hash: string | null | undefined): string => (hash ? hash.slice(0, 8) : '—');

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** The newest version that still has content that can be restored (used for "Restore latest version"). */
export function latestRestorable(versions: VersionDto[]): VersionDto | null {
  return [...versions].sort((a, b) => b.version - a.version).find((v) => v.action !== 'delete' && v.retained && v.content_hash !== null) ?? null;
}

/** Why Restore is unavailable for this version, or null when it can be restored. */
export function restoreBlockedReason(v: VersionDto, head: VersionDto | null): string | null {
  if (v.action === 'delete') return 'This is a delete marker. It has no content to restore.';
  if (!v.retained) return 'The ledger keeps only the hash of this version (the original file is write-once), so it cannot be restored from here.';
  if (head && head.version === v.version && head.action !== 'delete') return 'This is already the current version.';
  return null;
}

/* ----- line diff ----- */

export type DiffOp = { type: 'same' | 'add' | 'del'; text: string; aLine: number | null; bLine: number | null };

export interface DiffResult {
  ops: DiffOp[];
  added: number;
  removed: number;
  /** The inputs were cut to `cap` lines before diffing. */
  capped: boolean;
}

export const DIFF_LINE_CAP = 2000;

/** Unified line diff using the longest common subsequence. No dependency; large inputs are capped. */
export function diffLines(a: string, b: string, cap = DIFF_LINE_CAP): DiffResult {
  let x = a === '' ? [] : a.split('\n');
  let y = b === '' ? [] : b.split('\n');
  const capped = x.length > cap || y.length > cap;
  if (capped) { x = x.slice(0, cap); y = y.slice(0, cap); }

  // Common prefix and suffix cost nothing and shrink the table.
  let start = 0;
  while (start < x.length && start < y.length && x[start] === y[start]) start += 1;
  let endX = x.length;
  let endY = y.length;
  while (endX > start && endY > start && x[endX - 1] === y[endY - 1]) { endX -= 1; endY -= 1; }

  const n = endX - start;
  const m = endY - start;
  // lcs[i][j] = length of the LCS of x[start+i..] and y[start+j..]
  const width = m + 1;
  const lcs = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      lcs[i * width + j] = x[start + i] === y[start + j]
        ? lcs[(i + 1) * width + j + 1] + 1
        : Math.max(lcs[(i + 1) * width + j], lcs[i * width + j + 1]);
    }
  }

  const ops: DiffOp[] = [];
  let aLine = 1;
  let bLine = 1;
  const push = (type: DiffOp['type'], text: string) => {
    ops.push({ type, text, aLine: type === 'add' ? null : aLine, bLine: type === 'del' ? null : bLine });
    if (type !== 'add') aLine += 1;
    if (type !== 'del') bLine += 1;
  };
  for (let k = 0; k < start; k += 1) push('same', x[k]);
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && x[start + i] === y[start + j]) { push('same', x[start + i]); i += 1; j += 1; }
    else if (i < n && (j >= m || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])) { push('del', x[start + i]); i += 1; }
    else { push('add', y[start + j]); j += 1; }
  }
  for (let k = endX; k < x.length; k += 1) push('same', x[k]);

  return { ops, added: ops.filter((o) => o.type === 'add').length, removed: ops.filter((o) => o.type === 'del').length, capped };
}

export type DiffRow = DiffOp | { type: 'fold'; count: number };

/** Keep changed lines with `context` lines around them; fold long runs of unchanged lines. */
export function foldContext(ops: DiffOp[], context = 3): DiffRow[] {
  const keep = new Array<boolean>(ops.length).fill(false);
  ops.forEach((op, idx) => {
    if (op.type === 'same') return;
    for (let k = Math.max(0, idx - context); k <= Math.min(ops.length - 1, idx + context); k += 1) keep[k] = true;
  });
  const rows: DiffRow[] = [];
  let run = 0;
  ops.forEach((op, idx) => {
    if (keep[idx]) {
      if (run > 0) { rows.push({ type: 'fold', count: run }); run = 0; }
      rows.push(op);
    } else run += 1;
  });
  if (run > 0) rows.push({ type: 'fold', count: run });
  return rows;
}

/* ----- order events ----- */

export const EVENT_LABEL: Record<string, string> = {
  submitted: 'Submitted', risk_accepted: 'Risk check passed', risk_rejected: 'Risk check rejected', broker_ack: 'Broker acknowledged',
  status: 'Status', fill: 'Fill', cancel_requested: 'Cancel requested', cancelled: 'Cancelled', modified: 'Modified', rejected: 'Rejected',
  kill_switch: 'Kill switch', live_armed: 'Live trading armed', live_disarmed: 'Live trading disarmed',
};

export const eventLabel = (event: string): string => EVENT_LABEL[event] ?? event.replace(/_/g, ' ');

/** "manual", "Copilot", "automation@v3", "strategy@v7", "kill switch". */
export function sourceLabel(src: OrderSourceDto | null | undefined): string {
  if (!src) return 'unknown';
  const base = src.kind === 'copilot' ? 'Copilot' : src.kind === 'kill_switch' ? 'kill switch' : src.kind;
  return src.version !== null && src.version !== undefined ? `${base}@v${src.version}` : base;
}

/** The versioned object a source points at, when it has one. */
export function sourceTarget(src: OrderSourceDto | null | undefined): { kind: VersionedKind; id: string; version: number } | null {
  if (!src || src.version === null || src.version === undefined || !src.id) return null;
  if (src.kind === 'strategy') return { kind: 'strategy', id: src.id, version: src.version };
  if (src.kind === 'automation') return { kind: 'automation', id: src.id, version: src.version };
  return null;
}

export type OrderState = 'submitted' | 'open' | 'partially_filled' | 'filled' | 'cancelled' | 'rejected' | 'system';

export interface OrderGroup {
  key: string;
  orderId: string;
  brokerId: string;
  events: OrderEventDto[];
  source: OrderSourceDto | null;
  state: OrderState;
  symbol: string;
  side: string;
  quantity: number | null;
  lastTs: string;
}

const str = (v: unknown): string => (typeof v === 'string' ? v : v === undefined || v === null ? '' : String(v));

/** Current state of an order from its events (events are in the order they happened). */
export function stateOf(events: OrderEventDto[]): OrderState {
  let state: OrderState = 'submitted';
  for (const e of events) {
    if (e.event === 'risk_rejected' || e.event === 'rejected') state = 'rejected';
    else if (e.event === 'cancelled') state = 'cancelled';
    else if (e.event === 'status' || e.event === 'fill') {
      if (state === 'rejected' || state === 'cancelled') continue;
      const s = str(e.data.status).toLowerCase();
      if (s.includes('partial')) state = 'partially_filled';
      else if (s === 'filled' || s === 'complete') state = 'filled';
      else if (s === 'open' || s === 'pending') state = 'open';
      else if (e.event === 'fill' && state !== 'filled') state = 'partially_filled';
    } else if (e.event === 'broker_ack' && state === 'submitted') state = 'open';
  }
  return state;
}

/**
 * Group events per order (oldest event first inside a group, most recent order first).
 *
 * The backend writes `submitted`, `risk_accepted` and `risk_rejected` before the broker has given the
 * order an id, so those events have an empty `order_id` and are tied to the order by `data.request_id`
 * (the broker_ack carries both). They are grouped under the order that request became. A request that
 * never got an order id (risk rejected, refused by the live guard) is its own group. Only events with
 * neither (kill switch, arming) are "System events".
 */
export function groupOrderEvents(events: OrderEventDto[]): OrderGroup[] {
  const sorted = [...events].sort((a, b) => a.seq - b.seq);
  const orderKey = (e: OrderEventDto) => `${e.broker_id}\u0000${e.order_id}`;
  const requestId = (e: OrderEventDto) => (typeof e.data.request_id === 'string' && e.data.request_id ? e.data.request_id : '');
  const owner = new Map<string, string>();
  for (const e of sorted) {
    if (e.order_id && requestId(e)) owner.set(requestId(e), orderKey(e));
  }
  const map = new Map<string, OrderEventDto[]>();
  for (const e of sorted) {
    const key = e.order_id ? orderKey(e) : requestId(e) ? (owner.get(requestId(e)) ?? `request\u0000${e.broker_id}\u0000${requestId(e)}`) : 'system';
    const list = map.get(key) ?? [];
    list.push(e);
    map.set(key, list);
  }
  const groups: OrderGroup[] = [];
  map.forEach((list, key) => {
    const system = key === 'system';
    const submitted = list.find((e) => e.event === 'submitted');
    const req = (submitted?.data.request ?? {}) as Record<string, unknown>;
    groups.push({
      key, orderId: system ? '' : (list.find((e) => e.order_id)?.order_id ?? ''), brokerId: system ? '' : list[0].broker_id, events: list,
      source: submitted?.source ?? list.find((e) => e.source)?.source ?? null,
      state: system ? 'system' : stateOf(list),
      symbol: str(req.symbol), side: str(req.side), quantity: typeof req.quantity === 'number' ? req.quantity : null,
      lastTs: list[list.length - 1].ts,
    });
  });
  return groups.sort((a, b) => b.events[b.events.length - 1].seq - a.events[a.events.length - 1].seq);
}

export const STATE_LABEL: Record<OrderState, string> = {
  submitted: 'submitted', open: 'open', partially_filled: 'partially filled', filled: 'filled', cancelled: 'cancelled', rejected: 'rejected', system: 'system',
};
export const STATE_TONE: Record<OrderState, string> = {
  submitted: 'text-text-secondary', open: 'text-warning', partially_filled: 'text-warning', filled: 'text-bull', cancelled: 'text-text-muted', rejected: 'text-bear', system: 'text-text-muted',
};

/** One short line of the facts that matter for an event. */
export function eventFacts(e: OrderEventDto): string {
  const d = e.data;
  switch (e.event) {
    case 'submitted': {
      const r = (d.request ?? {}) as Record<string, unknown>;
      const parts = [str(r.side), str(r.quantity), str(r.symbol), str(r.order_type)].filter(Boolean);
      return `${parts.join(' ')}${d.live ? ' (LIVE)' : ' (paper)'}${d.confirmed === true || d.confirmation_present ? ', confirmed' : ''}`;
    }
    case 'risk_rejected': case 'rejected': return [str(d.reason), d.stage ? `(${str(d.stage)})` : ''].filter(Boolean).join(' ');
    case 'broker_ack': return str(d.broker_order_id) ? `broker order ${str(d.broker_order_id)}` : '';
    // delta_qty and avg_price are what this fill added and the average so far; quantity and price on a
    // fill event are the order's own (total quantity, limit price, null for a market order).
    case 'fill': return `${str(d.delta_qty ?? d.quantity)} @ ${str(d.avg_price ?? d.price)}`;
    case 'status': return `${str(d.status)}, filled ${str(d.filled_qty)} @ avg ${str(d.avg_price)}`;
    case 'cancelled': return d.filled_qty !== undefined ? `filled ${str(d.filled_qty)} before cancel` : '';
    default: return '';
  }
}
