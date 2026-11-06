import { useCallback, useEffect, useState } from 'react';
import { create } from 'zustand';
import { ledgerHistory, ledgerObjects, ledgerStatus, ledgerVerify } from '../../lib/tauri';
import type { LedgerStatusDto, LedgerVerifyDto, VersionDto, VersionedKind, VersionedObjectDto } from '../../lib/types';
import { useArtifactStore } from '../../stores/artifactStore';

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** Versions of one object, newest first. Reloads when anything is recorded (the activity counter moves). */
export function useVersionHistory(kind: VersionedKind, id: string, enabled = true) {
  const activityVersion = useArtifactStore((s) => s.activityVersion);
  const [versions, setVersions] = useState<VersionDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    if (!enabled || !id) return;
    let stale = false;
    setLoading(true);
    ledgerHistory(kind, id)
      .then((list) => { if (!stale) { setVersions([...list].sort((a, b) => b.version - a.version)); setError(null); } })
      .catch((e) => { if (!stale) setError(errorText(e)); })
      .finally(() => { if (!stale) setLoading(false); });
    return () => { stale = true; };
  }, [kind, id, enabled, tick, activityVersion]);

  return { versions, loading, error, reload };
}

/** Objects of one kind that are currently deleted (their history is kept and they can be restored). */
export function useDeletedObjects(kind: VersionedKind, enabled: boolean) {
  const activityVersion = useArtifactStore((s) => s.activityVersion);
  const [objects, setObjects] = useState<VersionedObjectDto[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    if (!enabled) return;
    let stale = false;
    ledgerObjects(kind, true)
      .then((list) => { if (!stale) { setObjects(list.filter((o) => o.deleted)); setError(null); } })
      .catch((e) => { if (!stale) setError(errorText(e)); });
    return () => { stale = true; };
  }, [kind, enabled, tick, activityVersion]);

  return { objects, error, reload };
}

/** Shared ledger status and verification, so every badge on screen shows the same state. */
interface LedgerState {
  status: LedgerStatusDto | null;
  result: LedgerVerifyDto | null;
  verifying: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  verify: () => Promise<void>;
}

export const useLedgerStore = create<LedgerState>((set) => ({
  status: null,
  result: null,
  verifying: false,
  error: null,
  refresh: async () => {
    try { set({ status: await ledgerStatus(), error: null }); } catch (e) { set({ error: errorText(e) }); }
  },
  verify: async () => {
    set({ verifying: true, error: null });
    try {
      const result = await ledgerVerify();
      set({ result });
      set({ status: await ledgerStatus() });
    } catch (e) {
      set({ error: errorText(e) });
    } finally {
      set({ verifying: false });
    }
  },
}));

/** Status that refreshes on mount and whenever something new is recorded. */
export function useLedgerStatus() {
  const activityVersion = useArtifactStore((s) => s.activityVersion);
  const state = useLedgerStore();
  const refresh = state.refresh;
  useEffect(() => { void refresh(); }, [refresh, activityVersion]);
  return state;
}
