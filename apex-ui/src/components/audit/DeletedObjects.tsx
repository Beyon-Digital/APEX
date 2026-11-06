import React, { useState } from 'react';
import { ledgerHistory, ledgerRestore } from '../../lib/tauri';
import type { ArtifactKind, VersionDto, VersionedKind } from '../../lib/types';
import { useArtifactStore } from '../../stores/artifactStore';
import { relativeTime } from '../ml/modelUtils';
import { KIND_LABEL, latestRestorable } from './auditUtils';
import { useDeletedObjects } from './useLedger';
import { VersionHistory } from './VersionHistory';

const ARTIFACT_KINDS: VersionedKind[] = ['strategy', 'model', 'automation', 'dataset'];
const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-0.5 font-mono text-[10px] text-text-secondary hover:text-text-primary hover:border-accent disabled:cursor-not-allowed disabled:opacity-40';

interface Props {
  kind: VersionedKind;
  /** Called after an object is brought back, so the host list can reload. */
  onRestored?: (record: VersionDto) => void;
  className?: string;
}

/**
 * "Show deleted" toggle for a list of versioned objects. Deleting only adds a delete marker to the
 * history, so a deleted object is listed here and can be restored (as a new version).
 */
export const DeletedObjects: React.FC<Props> = ({ kind, onRestored, className }) => {
  const [shown, setShown] = useState(false);
  const { objects, error, reload } = useDeletedObjects(kind, shown);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  // The history panel lives here, not inside the list row: restoring removes the row, and the panel must stay open to show the result.
  const [historyId, setHistoryId] = useState<string | null>(null);

  const restore = async (id: string) => {
    setBusyId(id);
    setProblem(null);
    try {
      const target = latestRestorable(await ledgerHistory(kind, id));
      if (!target) throw new Error('No version of this has content that can be restored.');
      const record = await ledgerRestore(kind, id, target.version);
      if (ARTIFACT_KINDS.includes(kind)) useArtifactStore.getState().note({ kind: kind as ArtifactKind, id, action: `${kind}.restore`, actor: 'user' });
      onRestored?.(record);
      reload();
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className={className ?? 'text-[11px]'} data-testid={`audit-deleted-${kind}`}>
      <button type="button" className={btnCls} aria-pressed={shown} onClick={() => setShown((s) => !s)} data-testid={`audit-show-deleted-${kind}`}>
        {shown ? 'Hide deleted' : 'Show deleted'}
      </button>
      {shown && (
        <div className="mt-1 space-y-1" data-testid={`audit-deleted-list-${kind}`}>
          {(error || problem) && <div className="text-bear" role="alert" data-testid={`audit-deleted-error-${kind}`}>{problem ?? error}</div>}
          {objects.length === 0 && !error && <div className="text-text-muted" data-testid={`audit-deleted-empty-${kind}`}>No deleted {KIND_LABEL[kind]}s. A deleted one keeps its history and shows up here.</div>}
          {objects.map((o) => (
            <div key={o.object_id} className="flex flex-wrap items-center gap-2 border border-dashed border-[var(--border-color)] bg-surface-0 px-2 py-1" data-testid={`audit-deleted-item-${kind}`} data-object-id={o.object_id}>
              <span className="min-w-0 flex-1 truncate font-mono text-text-secondary line-through" title={o.object_id}>{o.object_id}</span>
              <span className="font-mono text-[10px] text-text-muted">deleted {relativeTime(o.updated_at)}, {o.versions} versions</span>
              <button type="button" className={btnCls} onClick={() => setHistoryId(o.object_id)} data-testid={`audit-history-open-${kind}-${o.object_id}`} title="Every version is kept and can be brought back">History</button>
              <button type="button" className={btnCls} disabled={busyId === o.object_id} onClick={() => void restore(o.object_id)} data-testid={`audit-restore-deleted-${kind}-${o.object_id}`}>
                Restore
              </button>
            </div>
          ))}
        </div>
      )}
      {historyId && <VersionHistory kind={kind} id={historyId} open onClose={() => setHistoryId(null)} onRestored={onRestored} />}
    </div>
  );
};
