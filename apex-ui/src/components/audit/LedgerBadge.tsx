import React, { useState } from 'react';
import { exactTime, relativeTime } from '../ml/modelUtils';
import { useLedgerStatus } from './useLedger';

const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-0.5 font-mono text-[10px] text-text-secondary hover:text-text-primary hover:border-accent disabled:cursor-not-allowed disabled:opacity-40';

type BadgeState = 'ok' | 'problem' | 'unverified';

const STATE_TONE: Record<BadgeState, string> = {
  ok: 'border-bull/40 bg-bull/10 text-bull',
  problem: 'border-bear/60 bg-bear/15 text-bear',
  unverified: 'border-[var(--border-color)] bg-surface-2 text-text-muted',
};

const clock = (iso: string | null): string => {
  if (!iso) return '';
  const t = Date.parse(iso);
  return Number.isNaN(t) ? '' : new Date(t).toLocaleTimeString();
};

/** The ledger's integrity at a glance, with a Verify now button. `onOpen` jumps to the full view. */
export const LedgerBadge: React.FC<{ onOpen?: () => void }> = ({ onOpen }) => {
  const { status, verifying, verify, error } = useLedgerStatus();
  const state: BadgeState = status?.ok === true ? 'ok' : status?.ok === false ? 'problem' : 'unverified';
  const text = state === 'ok' ? `Verified OK ${clock(status?.last_verified_at ?? null)}`.trim() : state === 'problem' ? 'Integrity problem' : 'Not verified';
  return (
    <span className="flex items-center gap-1.5" data-testid="ledger-badge" data-state={state}>
      <button
        type="button" onClick={onOpen} disabled={!onOpen} title={status?.last_verified_at ? `Last verified ${exactTime(status.last_verified_at)}` : 'The audit ledger has not been verified since the app started.'}
        className={`border px-1.5 py-px font-mono text-[10px] ${STATE_TONE[state]} ${onOpen ? 'hover:brightness-125' : 'cursor-default'}`} data-testid="ledger-badge-text"
      >
        {text}
      </button>
      <button type="button" className={btnCls} disabled={verifying} onClick={() => void verify()} data-testid="ledger-badge-verify">{verifying ? 'Verifying…' : 'Verify now'}</button>
      {error && <span className="text-[10px] text-bear" data-testid="ledger-badge-error">{error}</span>}
    </span>
  );
};

const HeadRow: React.FC<{ name: string; label: string; hash: string | null | undefined; onCopied: (msg: string) => void }> = ({ name, label, hash, onCopied }) => {
  const copy = async () => {
    if (!hash) return;
    try { await navigator.clipboard.writeText(hash); onCopied(`Copied the ${label} head hash.`); } catch { onCopied('Could not copy: the clipboard is not available.'); }
  };
  return (
    <div className="flex flex-wrap items-center gap-2" data-testid={`ledger-head-row-${name}`}>
      <span className="w-28 shrink-0 text-text-secondary">{label}</span>
      <span className="min-w-0 flex-1 break-all font-mono text-[10px] text-text-primary" data-testid={`ledger-head-${name}`}>{hash ?? '(nothing recorded yet)'}</span>
      <button type="button" className={btnCls} disabled={!hash} onClick={() => void copy()} data-testid={`ledger-copy-${name}`}>Copy</button>
    </div>
  );
};

/** Integrity view: verify, see problems, copy the head hashes. */
export const IntegrityView: React.FC = () => {
  const { status, result, verifying, verify, error } = useLedgerStatus();
  const [notice, setNotice] = useState<string | null>(null);
  const flash = (msg: string) => { setNotice(msg); window.setTimeout(() => setNotice(null), 4000); };
  return (
    <div className="space-y-3 overflow-auto p-3 text-xs" data-testid="ledger-integrity">
      <div className="flex flex-wrap items-center gap-2">
        <LedgerBadge />
        {status && <span className="text-text-muted" data-testid="ledger-counts">{status.records} versions of {status.objects} objects</span>}
      </div>

      <div className="space-y-1.5 border border-[var(--border-color)] bg-surface-1 p-2">
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" className={btnCls} disabled={verifying} onClick={() => void verify()} data-testid="ledger-verify">{verifying ? 'Verifying…' : 'Verify now'}</button>
          {verifying && <span className="text-text-muted" role="status" data-testid="ledger-verify-progress">Checking every version, stored file and event in the logs…</span>}
          {error && <span className="text-bear" role="alert" data-testid="ledger-verify-error">{error}</span>}
        </div>
        {result && !verifying && (
          <div data-testid="ledger-verify-result" data-ok={result.ok}>
            <div className={result.ok ? 'text-bull' : 'font-semibold text-bear'} data-testid="ledger-verify-summary">
              {result.ok ? 'Everything checks out.' : `${result.problems.length} problem${result.problems.length === 1 ? '' : 's'} found.`}
              <span className="ml-2 font-normal text-text-muted">
                Checked {result.checked_records} versions, {result.checked_blobs} stored files and {result.checked_events} events, {relativeTime(result.verified_at)}.
              </span>
            </div>
            {result.problems.length > 0 && (
              <ul className="mt-1 space-y-1" data-testid="ledger-problems">
                {result.problems.map((p, i) => (
                  <li key={`${p.kind}-${p.where}-${i}`} className="border border-bear/40 bg-bear/10 px-2 py-1" data-testid="ledger-problem">
                    <span className="font-mono text-[10px] uppercase text-bear" data-testid="ledger-problem-kind">{p.kind}</span>
                    <span className="ml-2 font-mono text-[11px] text-text-primary" data-testid="ledger-problem-where">{p.where}</span>
                    <div className="text-[11px] text-text-secondary" data-testid="ledger-problem-detail">{p.detail}</div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>

      <div className="space-y-1.5 border border-[var(--border-color)] bg-surface-1 p-2">
        <div className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">Head hashes</div>
        <HeadRow name="ledger" label="Versions (ledger)" hash={status?.head_hash} onCopied={flash} />
        <HeadRow name="activity" label="Activity log" hash={status?.stream_heads.activity} onCopied={flash} />
        <HeadRow name="orders" label="Order events" hash={status?.stream_heads.orders} onCopied={flash} />
        {notice && <div className="text-[10px] text-bull" role="status" data-testid="ledger-copy-notice">{notice}</div>}
        <p className="text-[11px] text-text-muted" data-testid="ledger-limit-note">
          Each record carries the hash of the one before it, so an edit or deletion in the middle of a log breaks the chain and shows up as a problem. Someone with full write access to the data folder could rewrite the whole log and recompute every hash. Copy these somewhere safe (a note, an email, another machine) to detect a later rewrite of the whole log: if the chain no longer contains a hash you saved, it was rewritten.
        </p>
      </div>
    </div>
  );
};
