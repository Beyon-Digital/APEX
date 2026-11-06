/**
 * Web-mode mock of the immutable audit ledger (the Rust side is apex-tauri/src/ledger.rs).
 *
 * It follows the same rules as the real one so the UI and its specs exercise the real behaviour:
 *  - every change appends a new version under the same object; nothing is ever overwritten,
 *  - saving content identical to the current head is a no-op (no duplicate version),
 *  - a delete is a tombstone version; re-creating the same id continues the SAME object,
 *  - a restore appends a new version whose content equals an older one,
 *  - records are hash-chained (prev_hash + hash) and the event streams ("activity", "orders") too.
 *
 * The hash here is a simple deterministic 256-bit mix, NOT sha256: it only has to be repeatable so
 * `verify()` can notice an edit. HONEST LIMIT (same as the real ledger): a hash chain detects edits
 * and deletions in the middle of a log, but someone who can rewrite the whole thing can recompute
 * every hash, which is why the head hashes are exposed to be copied somewhere else.
 *
 * Test hook: `window.__apexMock.ledger` (see `ledgerHooks`).
 */
import type {
  LedgerProblemDto, LedgerStatusDto, LedgerVerifyDto, OrderEventDto, OrderSourceDto, VersionContentDto, VersionDto,
  VersionedKind, VersionedObjectDto, ActivityActor,
} from './types';

const ZERO_HASH = '0'.repeat(64);
/** Display cap for content shown in the viewer; the stored copy is always complete. */
const CONTENT_DISPLAY_CAP = 200_000;

/** Deterministic 64-hex-character hash (eight 32-bit FNV-1a lanes with different seeds). */
export function mockHash(input: string): string {
  let out = '';
  for (let lane = 0; lane < 8; lane += 1) {
    let h = (0x811c9dc5 ^ Math.imul(lane + 1, 0x9e3779b1)) >>> 0;
    for (let i = 0; i < input.length; i += 1) {
      h ^= input.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
      h ^= h >>> 15;
    }
    h = Math.imul(h ^ input.length, 0x85ebca6b) >>> 0;
    out += h.toString(16).padStart(8, '0');
  }
  return out;
}

type Action = VersionDto['action'];
type Actor = VersionDto['actor'];

const canonical = (r: Omit<VersionDto, 'hash'>): string =>
  JSON.stringify([r.seq, r.kind, r.object_id, r.version, r.action, r.actor, r.ts, r.content_hash, r.size, r.retained,
    r.parent_version, r.restored_from, r.summary, r.prev_hash]);

const records: VersionDto[] = [];
const blobs = new Map<string, string>();
const objectKey = (kind: VersionedKind, id: string) => `${kind}\u0000${id}`;
const byObject = new Map<string, VersionDto[]>();

interface StreamEvent { seq: number; ts: string; value: Record<string, unknown>; prev_hash: string; hash: string }
type StreamName = 'activity' | 'orders';
const streams: Record<StreamName, StreamEvent[]> = { activity: [], orders: [] };
const eventCanonical = (e: Omit<StreamEvent, 'hash'>) => JSON.stringify([e.seq, e.ts, e.value, e.prev_hash]);

let lastVerify: LedgerVerifyDto | null = null;

const copy = (r: VersionDto): VersionDto => ({ ...r });

export function ledgerHead(kind: VersionedKind, id: string): VersionDto | null {
  const list = byObject.get(objectKey(kind, id));
  return list && list.length > 0 ? copy(list[list.length - 1]) : null;
}

export function ledgerHistoryOf(kind: VersionedKind, id: string): VersionDto[] {
  return (byObject.get(objectKey(kind, id)) ?? []).map(copy);
}

function append(kind: VersionedKind, id: string, fields: Pick<VersionDto, 'action' | 'actor' | 'content_hash' | 'size' | 'retained' | 'restored_from' | 'summary'>): VersionDto {
  const list = byObject.get(objectKey(kind, id)) ?? [];
  const head = list[list.length - 1];
  const base: Omit<VersionDto, 'hash'> = {
    seq: records.length + 1, kind, object_id: id, version: (head?.version ?? 0) + 1, ts: new Date().toISOString(),
    parent_version: head?.version ?? null, prev_hash: records.length > 0 ? records[records.length - 1].hash : ZERO_HASH,
    ...fields,
  };
  const record: VersionDto = { ...base, hash: mockHash(canonical(base)) };
  records.push(record);
  list.push(record);
  byObject.set(objectKey(kind, id), list);
  return copy(record);
}

export interface PutResult { record: VersionDto; created: boolean }

/**
 * Save new content as a new version. When the content equals the current head (and the head is not a
 * delete marker) nothing is written and `created` is false.
 * `reference: true` keeps only the hash (very large write-once files): retained=false.
 */
export function ledgerPut(
  kind: VersionedKind, id: string, text: string, actor: Actor, summary: string,
  opts: { action?: Action; reference?: boolean } = {},
): PutResult {
  const hash = mockHash(text);
  const head = ledgerHead(kind, id);
  if (head && head.action !== 'delete' && head.content_hash === hash) return { record: head, created: false };
  const reference = Boolean(opts.reference);
  if (!reference && !blobs.has(hash)) blobs.set(hash, text);
  const action: Action = opts.action ?? (!head || head.action === 'delete' ? 'create' : 'update');
  const record = append(kind, id, {
    action, actor, content_hash: hash, size: text.length, retained: !reference, restored_from: null, summary,
  });
  return { record, created: true };
}

/** Append a delete marker. Returns null when there is nothing to delete (unknown or already deleted). */
export function ledgerTombstone(kind: VersionedKind, id: string, actor: Actor, summary: string): VersionDto | null {
  const head = ledgerHead(kind, id);
  if (!head || head.action === 'delete') return null;
  return append(kind, id, { action: 'delete', actor, content_hash: null, size: 0, retained: true, restored_from: null, summary });
}

/** Bring an older version back as a NEW version. Returns the new record and the restored text. */
export function ledgerRestoreVersion(kind: VersionedKind, id: string, version: number, actor: Actor): { record: VersionDto; text: string } {
  const old = (byObject.get(objectKey(kind, id)) ?? []).find((r) => r.version === version);
  if (!old) throw new Error(`No version ${version} of ${kind} ${id}`);
  if (old.action === 'delete' || old.content_hash === null) throw new Error('A delete marker has no content to restore.');
  if (!old.retained) throw new Error('The ledger holds only the hash of this version (the original file is write-once), so it cannot be restored from here.');
  const text = blobs.get(old.content_hash);
  if (text === undefined || mockHash(text) !== old.content_hash) throw new Error('The stored content of this version does not match its hash.');
  const record = append(kind, id, {
    action: 'restore', actor, content_hash: old.content_hash, size: old.size, retained: true, restored_from: version,
    summary: `Restored version ${version}`,
  });
  return { record, text };
}

export function ledgerContent(kind: VersionedKind, id: string, version: number): VersionContentDto {
  const rec = (byObject.get(objectKey(kind, id)) ?? []).find((r) => r.version === version);
  if (!rec) throw new Error(`No version ${version} of ${kind} ${id}`);
  const stored = rec.content_hash !== null && rec.retained ? blobs.get(rec.content_hash) : undefined;
  if (rec.content_hash !== null && rec.retained && (stored === undefined || mockHash(stored) !== rec.content_hash)) {
    throw new Error('The stored content of this version does not match its hash.');
  }
  const truncated = stored !== undefined && stored.length > CONTENT_DISPLAY_CAP;
  return { record: copy(rec), text: stored === undefined ? null : truncated ? stored.slice(0, CONTENT_DISPLAY_CAP) : stored, truncated, binary: false };
}

/** The text of the newest version that has content (used to restore a deleted object). */
export function ledgerLatestContentVersion(kind: VersionedKind, id: string): number | null {
  const list = byObject.get(objectKey(kind, id)) ?? [];
  for (let i = list.length - 1; i >= 0; i -= 1) {
    if (list[i].action !== 'delete' && list[i].retained && list[i].content_hash !== null) return list[i].version;
  }
  return null;
}

export function ledgerObjectList(kind: VersionedKind, includeDeleted: boolean): VersionedObjectDto[] {
  const out: VersionedObjectDto[] = [];
  byObject.forEach((list) => {
    const head = list[list.length - 1];
    if (!head || head.kind !== kind) return;
    const deleted = head.action === 'delete';
    if (deleted && !includeDeleted) return;
    out.push({
      kind, object_id: head.object_id, head_version: head.version, deleted, versions: list.length,
      created_at: list[0].ts, updated_at: head.ts,
    });
  });
  return out.sort((a, b) => a.object_id.localeCompare(b.object_id));
}

/* ----- settings redaction (the Rust side is redact_config in apex-tauri) ----- */

const SECRET_KEY = /password|secret|token|api_key|key_material/i;
export const REDACTED = '<redacted>';
export const isSecretKey = (key: string): boolean => SECRET_KEY.test(key);
/** scheme://user:password@host becomes scheme://user:<redacted>@host. */
export const redactUserinfo = (text: string): string => text.replace(/(\b[a-z][a-z0-9+.-]*:\/\/[^\s:/@"']+:)[^\s@/"']*(@)/gi, `$1${REDACTED}$2`);

/**
 * Settings text as kept in the ledger: the password of any URL and the value of any key whose name
 * contains password, secret, token, api_key or key_material is replaced by "<redacted>". Everything
 * else, including comments, spacing and line order, is kept byte for byte.
 */
export function redactConfig(text: string): string {
  const lines = text.split('\n').map((line) => {
    const m = /^(\s*"?([^\s="]+)"?\s*=\s*)("(?:[^"\\]|\\.)*"|'[^']*'|[^\s#]+)(.*)$/.exec(line);
    return m && isSecretKey(m[2]) ? `${m[1]}"${REDACTED}"${m[4]}` : line;
  });
  return redactUserinfo(lines.join('\n'));
}

/* ----- event streams ----- */

export function ledgerAppendEvent(stream: StreamName, value: Record<string, unknown>): StreamEvent {
  const list = streams[stream];
  const base = { seq: list.length + 1, ts: new Date().toISOString(), value, prev_hash: list.length > 0 ? list[list.length - 1].hash : ZERO_HASH };
  const event: StreamEvent = { ...base, hash: mockHash(eventCanonical(base)) };
  list.push(event);
  return event;
}

/** Record one order event. `orderId`/`brokerId` may be empty for system-wide events (kill switch, arming). */
export function ledgerOrderEvent(
  orderId: string, brokerId: string, event: string, actor: string, source: OrderSourceDto | null, data: Record<string, unknown> = {},
): OrderEventDto {
  const e = ledgerAppendEvent('orders', { order_id: orderId, broker_id: brokerId, event, actor, source, data });
  return toOrderEvent(e);
}

function toOrderEvent(e: StreamEvent): OrderEventDto {
  const v = e.value as Record<string, unknown>;
  return {
    seq: e.seq, ts: e.ts, order_id: String(v.order_id ?? ''), broker_id: String(v.broker_id ?? ''), event: String(v.event ?? ''),
    actor: String(v.actor ?? ''), source: (v.source as OrderSourceDto | null) ?? null, data: (v.data as Record<string, unknown>) ?? {},
    hash: e.hash, prev_hash: e.prev_hash,
  };
}

/**
 * Newest first. Like the backend, asking for one order also returns the events from before the broker
 * gave it an id (submitted, risk_accepted, risk_rejected), which are tied to it by data.request_id.
 */
export function ledgerListOrderEvents(filter: { orderId?: string; brokerId?: string; limit?: number } = {}): OrderEventDto[] {
  const all = streams.orders.map(toOrderEvent);
  const requests = new Set<string>();
  if (filter.orderId) {
    for (const e of all) {
      const r = e.data.request_id;
      if (e.order_id === filter.orderId && (!filter.brokerId || e.broker_id === filter.brokerId) && typeof r === 'string') requests.add(r);
    }
  }
  const out: OrderEventDto[] = [];
  const limit = filter.limit && filter.limit > 0 ? filter.limit : 500;
  for (let i = all.length - 1; i >= 0 && out.length < limit; i -= 1) {
    const e = all[i];
    if (filter.brokerId && e.broker_id !== filter.brokerId) continue;
    if (filter.orderId) {
      const r = e.data.request_id;
      if (e.order_id !== filter.orderId && !(typeof r === 'string' && requests.has(r))) continue;
    }
    out.push(e);
  }
  return out;
}

/* ----- verify and status ----- */

export function ledgerVerifyNow(): LedgerVerifyDto {
  const problems: LedgerProblemDto[] = [];
  const checkedBlobs = new Set<string>();
  let prev = ZERO_HASH;
  for (const r of records) {
    const where = `${r.kind} ${r.object_id} v${r.version} (record ${r.seq})`;
    const { hash: stored, ...rest } = r;
    if (mockHash(canonical(rest)) !== stored) problems.push({ kind: 'record_hash', where, detail: 'The record was changed after it was written: its hash no longer matches its fields.' });
    if (r.prev_hash !== prev) problems.push({ kind: 'chain_link', where, detail: 'This record does not point at the hash of the record before it: something was edited or removed.' });
    prev = stored;
    if (r.retained && r.content_hash !== null) {
      checkedBlobs.add(r.content_hash);
      const blob = blobs.get(r.content_hash);
      if (blob === undefined) problems.push({ kind: 'blob_missing', where, detail: 'The stored content is missing.' });
      else if (mockHash(blob) !== r.content_hash) problems.push({ kind: 'blob_hash', where, detail: 'The stored content no longer matches its hash.' });
    }
  }
  let checkedEvents = 0;
  (Object.keys(streams) as StreamName[]).forEach((name) => {
    let previous = ZERO_HASH;
    for (const e of streams[name]) {
      checkedEvents += 1;
      const where = `${name} event ${e.seq}`;
      const { hash: stored, ...rest } = e;
      if (mockHash(eventCanonical(rest)) !== stored) problems.push({ kind: 'stream_hash', where, detail: 'The event was changed after it was written.' });
      if (e.prev_hash !== previous) problems.push({ kind: 'stream_link', where, detail: 'This event does not point at the one before it: something was edited or removed.' });
      previous = stored;
    }
  });
  lastVerify = {
    ok: problems.length === 0, checked_records: records.length, checked_blobs: checkedBlobs.size, checked_events: checkedEvents,
    problems, head_hash: records.length > 0 ? records[records.length - 1].hash : null, verified_at: new Date().toISOString(),
  };
  return lastVerify;
}

export function ledgerStatusNow(): LedgerStatusDto {
  const heads: Record<string, string | null> = {};
  (Object.keys(streams) as StreamName[]).forEach((name) => {
    const list = streams[name];
    heads[name] = list.length > 0 ? list[list.length - 1].hash : null;
  });
  return {
    ok: lastVerify ? lastVerify.ok : null, records: records.length, objects: byObject.size,
    head_hash: records.length > 0 ? records[records.length - 1].hash : null,
    last_verified_at: lastVerify?.verified_at ?? null, stream_heads: heads,
  };
}

/* ----- working-copy appliers: how a restored version reaches the mock's own stores ----- */

type Applier = (id: string, text: string) => void;
const appliers = new Map<VersionedKind, Applier>();
export function registerLedgerApplier(kind: VersionedKind, apply: Applier): void { appliers.set(kind, apply); }
export function applyToWorkingCopy(kind: VersionedKind, id: string, text: string): void { appliers.get(kind)?.(id, text); }

/* ----- test hook ----- */

/**
 * Simulate tampering (specs only). Nothing in the app calls this.
 *  - "record" (default): edit a record's summary without re-hashing it,
 *  - "blob": change the stored content of that version,
 *  - "chain": point the record at a wrong previous hash.
 */
function tamper(kind: VersionedKind, id: string, version: number, mode: 'record' | 'blob' | 'chain' = 'record'): void {
  const rec = (byObject.get(objectKey(kind, id)) ?? []).find((r) => r.version === version);
  if (!rec) throw new Error(`No version ${version} of ${kind} ${id}`);
  if (mode === 'record') rec.summary = `${rec.summary} (edited)`;
  else if (mode === 'chain') rec.prev_hash = mockHash('wrong');
  else if (rec.content_hash) blobs.set(rec.content_hash, `${blobs.get(rec.content_hash) ?? ''}\n# tampered`);
}

function tamperEvent(stream: StreamName, seq: number): void {
  const e = streams[stream].find((x) => x.seq === seq);
  if (!e) throw new Error(`No ${stream} event ${seq}`);
  e.value = { ...e.value, tampered: true };
}

export const ledgerHooks = {
  tamper,
  tamperEvent,
  /** Act as someone editing the file outside APEX: the working copy changes and the ledger records an "external" version. */
  externalEdit: (kind: VersionedKind, id: string, text: string): VersionDto => {
    applyToWorkingCopy(kind, id, text);
    return ledgerPut(kind, id, text, 'external', 'Changed outside APEX', { action: 'external' }).record;
  },
  /** Seed a version directly (any actor) without touching a working copy. */
  put: (kind: VersionedKind, id: string, text: string, actor: ActivityActor | 'external' = 'user', summary = 'Saved') => ledgerPut(kind, id, text, actor, summary).record,
  putReference: (kind: VersionedKind, id: string, text: string, actor: ActivityActor | 'external' = 'user', summary = 'Large file kept write-once') =>
    ledgerPut(kind, id, text, actor, summary, { reference: true }).record,
  history: ledgerHistoryOf,
  head: ledgerHead,
  verify: ledgerVerifyNow,
  status: ledgerStatusNow,
  orderEvents: ledgerListOrderEvents,
};
