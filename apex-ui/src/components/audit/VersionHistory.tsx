import React, { useEffect, useMemo, useState } from 'react';
import { createPortal } from 'react-dom';
import { ledgerRestore, ledgerVersion } from '../../lib/tauri';
import type { ArtifactKind, VersionContentDto, VersionDto, VersionedKind } from '../../lib/types';
import { useArtifactStore } from '../../stores/artifactStore';
import { exactTime, relativeTime } from '../ml/modelUtils';
import {
  ACTION_TONE, ACTOR_LABELS, ACTOR_TONES, KIND_LABEL, formatSize, latestRestorable, restoreBlockedReason, shortHash,
} from './auditUtils';
import { useVersionHistory } from './useLedger';
import { VersionDiff } from './VersionDiff';

const ARTIFACT_KINDS: VersionedKind[] = ['strategy', 'model', 'automation', 'dataset'];
const VIEW_CHAR_CAP = 60_000;

const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-0.5 font-mono text-[10px] text-text-secondary hover:text-text-primary hover:border-accent disabled:cursor-not-allowed disabled:opacity-40';
const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export interface VersionHistoryProps {
  kind: VersionedKind;
  /** Strategy path, estimator / recipe / dataset name, model id, automation id, notebook path. */
  id: string;
  /** Called after a restore has created the new version, so the host can reload what it shows. */
  onRestored?: (record: VersionDto) => void;
  buttonLabel?: string;
  /** Overrides the button test id (lists with several buttons of one kind). */
  testId?: string;
  /** Controlled mode: no button is rendered; the parent opens and closes the panel. */
  open?: boolean;
  onClose?: () => void;
  /** Version to show the content of when the panel opens. */
  initialVersion?: number;
  className?: string;
}

/**
 * Version history of one object: every save, delete and restore is a version, nothing is overwritten.
 * Pick two versions to see a diff, view any version's content, restore an old one as a NEW version.
 */
export const VersionHistory: React.FC<VersionHistoryProps> = ({
  kind, id, onRestored, buttonLabel = 'History', testId, open: controlledOpen, onClose, initialVersion, className,
}) => {
  const controlled = controlledOpen !== undefined;
  const [innerOpen, setInnerOpen] = useState(false);
  const open = controlled ? Boolean(controlledOpen) : innerOpen;
  const close = () => { if (controlled) onClose?.(); else setInnerOpen(false); };

  return (
    <>
      {!controlled && (
        <button
          type="button" className={className ?? btnCls} onClick={() => setInnerOpen(true)} disabled={!id}
          title="Every version of this is kept and can be brought back. Nothing is overwritten." data-testid={testId ?? `audit-history-open-${kind}`}
        >
          {buttonLabel}
        </button>
      )}
      {open && createPortal(<HistoryPanel kind={kind} id={id} onClose={close} onRestored={onRestored} initialVersion={initialVersion} />, document.body)}
    </>
  );
};

interface PanelProps { kind: VersionedKind; id: string; onClose: () => void; onRestored?: (record: VersionDto) => void; initialVersion?: number }

const HistoryPanel: React.FC<PanelProps> = ({ kind, id, onClose, onRestored, initialVersion }) => {
  const { versions, loading, error, reload } = useVersionHistory(kind, id);
  const [selected, setSelected] = useState<number[]>([]);
  const [viewing, setViewing] = useState<number | null>(initialVersion ?? null);
  const [content, setContent] = useState<VersionContentDto | null>(null);
  const [contentError, setContentError] = useState<string | null>(null);
  const [diffData, setDiffData] = useState<{ a: VersionContentDto; b: VersionContentDto } | null>(null);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const head = versions[0] ?? null;
  const deleted = head?.action === 'delete';
  const latest = useMemo(() => latestRestorable(versions), [versions]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  useEffect(() => {
    if (viewing === null) { setContent(null); setContentError(null); return; }
    let stale = false;
    setContent(null);
    setContentError(null);
    ledgerVersion(kind, id, viewing)
      .then((c) => { if (!stale) setContent(c); })
      .catch((e) => { if (!stale) setContentError(errorText(e)); });
    return () => { stale = true; };
  }, [kind, id, viewing]);

  const pair = selected.length === 2 ? [...selected].sort((a, b) => a - b) : null;
  const pairKey = pair ? pair.join('-') : '';
  useEffect(() => {
    if (!pair) { setDiffData(null); setDiffError(null); return; }
    let stale = false;
    setDiffData(null);
    setDiffError(null);
    Promise.all([ledgerVersion(kind, id, pair[0]), ledgerVersion(kind, id, pair[1])])
      .then(([a, b]) => { if (!stale) setDiffData({ a, b }); })
      .catch((e) => { if (!stale) setDiffError(errorText(e)); });
    return () => { stale = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind, id, pairKey]);

  const toggleSelect = (version: number) => setSelected((cur) => (cur.includes(version) ? cur.filter((v) => v !== version) : [...cur.slice(-1), version]));

  const doRestore = async (version: number) => {
    setBusy(true);
    setActionError(null);
    setMessage(null);
    try {
      const record = await ledgerRestore(kind, id, version);
      if (ARTIFACT_KINDS.includes(kind)) {
        useArtifactStore.getState().note({ kind: kind as ArtifactKind, id, action: `${kind}.restore`, actor: 'user' });
      }
      setConfirm(null);
      setSelected([]);
      setMessage(`Restored version ${version} as the new version ${record.version}. Nothing was overwritten.`);
      onRestored?.(record);
      reload();
    } catch (e) {
      setActionError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const confirmTarget = confirm !== null ? versions.find((v) => v.version === confirm) ?? null : null;
  const nextVersion = (head?.version ?? 0) + 1;
  const shown = content?.text ?? null;
  const cut = shown !== null && shown.length > VIEW_CHAR_CAP;

  return (
    <div className="fixed inset-0 z-[80] flex justify-end bg-black/50" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }} data-testid="audit-history-backdrop">
      <div
        role="dialog" aria-modal="true" aria-label={`Version history of ${id}`}
        className="flex h-full w-full max-w-[36rem] flex-col border-l border-[var(--border-color)] bg-surface-1 text-xs text-text-primary shadow-xl" data-testid="audit-history"
        data-kind={kind} data-object-id={id}
      >
        <div className="flex items-start gap-2 border-b border-[var(--border-color)] px-3 py-2">
          <div className="min-w-0 flex-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">Version history of this {KIND_LABEL[kind]}</div>
            <div className="truncate font-mono text-sm" title={id} data-testid="audit-history-title">{id}</div>
          </div>
          <button type="button" className={btnCls} onClick={reload} data-testid="audit-history-refresh">Refresh</button>
          <button type="button" className={btnCls} onClick={onClose} aria-label="Close history" data-testid="audit-history-close">×</button>
        </div>

        <div className="border-b border-[var(--border-color)] px-3 py-1.5 text-[11px] text-text-muted" data-testid="audit-history-note">
          This history is append-only. Every save, delete and restore adds a version; no version is edited or removed, and restoring never overwrites anything.
        </div>

        {deleted && head && (
          <div className="flex flex-wrap items-center gap-2 border-b border-bear/40 bg-bear/10 px-3 py-2 text-bear" data-testid="audit-history-deleted-banner">
            <span className="font-semibold uppercase tracking-wider">Deleted</span>
            <span className="min-w-0 flex-1 text-[11px]">Deleted {relativeTime(head.ts)} by {ACTOR_LABELS[head.actor] ?? head.actor}. The history is kept and the {KIND_LABEL[kind]} can be restored.</span>
            <button
              type="button" className={btnCls} disabled={!latest || busy} onClick={() => latest && setConfirm(latest.version)}
              title={latest ? `Brings version ${latest.version} back as a new version` : 'No version with restorable content'} data-testid="audit-history-restore-latest"
            >
              Restore latest version
            </button>
          </div>
        )}

        {message && <div className="border-b border-bull/40 bg-bull/10 px-3 py-1.5 text-[11px] text-bull" role="status" data-testid="audit-history-message">{message}</div>}
        {(actionError || error) && <div className="border-b border-bear/40 bg-bear/10 px-3 py-1.5 text-[11px] text-bear" role="alert" data-testid="audit-history-error">{actionError ?? error}</div>}

        {confirmTarget && (
          <div className="space-y-1.5 border-b border-warning/40 bg-warning/10 px-3 py-2 text-[11px] text-warning" role="alertdialog" aria-label="Confirm restore" data-testid="audit-history-confirm">
            <div data-testid="audit-history-confirm-text">
              Restore version {confirmTarget.version}? This creates a new version ({nextVersion}) with the same content as version {confirmTarget.version}; nothing is overwritten and every earlier version stays in the history.
            </div>
            <div className="flex gap-2">
              <button type="button" className={btnCls} disabled={busy} onClick={() => void doRestore(confirmTarget.version)} data-testid="audit-history-confirm-yes">{busy ? 'Restoring…' : `Create version ${nextVersion} from version ${confirmTarget.version}`}</button>
              <button type="button" className={btnCls} disabled={busy} onClick={() => setConfirm(null)} data-testid="audit-history-confirm-no">Cancel</button>
            </div>
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-auto">
          {loading && versions.length === 0 && <div className="p-3 text-text-muted" data-testid="audit-history-loading">Loading history…</div>}
          {!loading && !error && versions.length === 0 && <div className="p-3 text-text-muted" data-testid="audit-history-empty">No versions recorded yet.</div>}
          <div className="px-3 pt-1.5 text-[10px] text-text-muted" data-testid="audit-history-count">
            {versions.length} version{versions.length === 1 ? '' : 's'}. Tick two to compare them.
          </div>
          <ul>
            {versions.map((v) => {
              const blocked = restoreBlockedReason(v, head);
              const isHead = head?.version === v.version;
              return (
                <li key={v.version} className={`border-b border-[var(--border-color)] px-3 py-1.5 ${viewing === v.version ? 'bg-accent/5' : ''}`} data-testid="audit-version-row" data-version={v.version} data-action={v.action} data-actor={v.actor}>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <input
                      type="checkbox" checked={selected.includes(v.version)} onChange={() => toggleSelect(v.version)}
                      aria-label={`Select version ${v.version} to compare`} data-testid={`audit-version-select-${v.version}`}
                    />
                    <span className="font-mono text-[11px] font-semibold" data-testid="audit-version-number">v{v.version}</span>
                    {isHead && <span className="border border-[var(--border-color)] px-1 font-mono text-[9px] uppercase text-text-muted" data-testid="audit-version-current">current</span>}
                    <span className={`border px-1.5 py-px font-mono text-[10px] uppercase ${ACTION_TONE[v.action]}`} data-testid="audit-version-action">{v.action}</span>
                    <span className={`border px-1.5 py-px font-mono text-[10px] uppercase ${ACTOR_TONES[v.actor] ?? ACTOR_TONES.system}`} data-testid="audit-version-actor" title={v.actor === 'external' ? 'Changed outside APEX and picked up as a new version' : undefined}>{ACTOR_LABELS[v.actor] ?? v.actor}</span>
                    <span className="font-mono text-[10px] text-text-muted" title={exactTime(v.ts)} data-testid="audit-version-time">{relativeTime(v.ts)}</span>
                  </div>
                  <div className="mt-0.5 break-words text-text-primary" data-testid="audit-version-summary">
                    {v.summary}{v.restored_from !== null && <span className="text-text-muted"> (content of version {v.restored_from})</span>}
                  </div>
                  <div className="mt-0.5 flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-text-muted">
                    <span title={v.content_hash ?? 'delete marker'} data-testid="audit-version-hash">{shortHash(v.content_hash)}</span>
                    <span data-testid="audit-version-size">{v.action === 'delete' ? 'no content' : formatSize(v.size)}</span>
                    {!v.retained && <span className="text-warning" data-testid="audit-version-retained">hash only: the original file is write-once and referenced by this hash</span>}
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-1.5">
                    <button type="button" className={btnCls} disabled={v.action === 'delete'} onClick={() => setViewing(viewing === v.version ? null : v.version)} data-testid={`audit-version-view-${v.version}`}>{viewing === v.version ? 'Hide content' : 'View content'}</button>
                    {v.version > 1 && <button type="button" className={btnCls} onClick={() => setSelected([v.version - 1, v.version])} data-testid={`audit-version-diffprev-${v.version}`}>Diff vs previous</button>}
                    <button
                      type="button" className={btnCls} disabled={blocked !== null || busy} title={blocked ?? `Create a new version with the content of version ${v.version}`}
                      onClick={() => setConfirm(v.version)} data-testid={`audit-version-restore-${v.version}`}
                    >
                      Restore
                    </button>
                    {blocked && <span className="text-[10px] text-text-muted" data-testid={`audit-version-restore-reason-${v.version}`}>{blocked}</span>}
                  </div>
                </li>
              );
            })}
          </ul>

          {pair && (
            <div className="space-y-1 p-3" data-testid="audit-history-diff-section">
              <div className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">Changes from version {pair[0]} to version {pair[1]}</div>
              {diffError && <div className="text-bear" data-testid="audit-history-diff-error">{diffError}</div>}
              {!diffData && !diffError && <div className="text-text-muted">Loading…</div>}
              {diffData && (diffData.a.text === null || diffData.b.text === null) && (
                <div className="border border-[var(--border-color)] bg-surface-0 p-2 text-text-muted" data-testid="audit-history-diff-unavailable">
                  One of these versions has no stored text (a delete marker, or a version where the ledger keeps only the hash), so there is nothing to compare.
                </div>
              )}
              {diffData && diffData.a.text !== null && diffData.b.text !== null && (
                <VersionDiff before={diffData.a.text} after={diffData.b.text} beforeLabel={`v${pair[0]}`} afterLabel={`v${pair[1]}`} />
              )}
            </div>
          )}

          {viewing !== null && (
            <div className="space-y-1 p-3" data-testid="audit-history-content-section">
              <div className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">Content of version {viewing} (read-only)</div>
              {contentError && <div className="text-bear" data-testid="audit-history-content-error">{contentError}</div>}
              {!content && !contentError && <div className="text-text-muted">Loading…</div>}
              {content && shown === null && (
                <div className="border border-[var(--border-color)] bg-surface-0 p-2 text-text-muted" data-testid="audit-history-content-none">
                  {content.binary ? 'This content is binary and is not shown.' : 'The ledger holds no text for this version.'}
                </div>
              )}
              {shown !== null && (
                <>
                  {(content?.truncated || cut) && (
                    <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[10px] text-warning" data-testid="audit-history-truncated">
                      Only the first part is shown. The stored version is complete.
                    </div>
                  )}
                  <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words border border-[var(--border-color)] bg-surface-0 p-2 font-mono text-[11px]" data-testid="audit-history-content">{cut ? shown.slice(0, VIEW_CHAR_CAP) : shown}</pre>
                </>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
