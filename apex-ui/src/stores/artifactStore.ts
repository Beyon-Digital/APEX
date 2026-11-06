import { create } from 'zustand';
import type { ArtifactEventDto, ArtifactKind } from '../lib/types';

/** Panels that list strategies, models, datasets or automations refetch when their counter changes. */
interface ArtifactState {
  versions: Record<ArtifactKind, number>;
  /** Bumped for every new activity-log entry (any kind). */
  activityVersion: number;
  /** `${kind}:${id}` -> time the Copilot last created or changed it, for "new" badges. */
  copilotTouched: Record<string, number>;
  note: (event: ArtifactEventDto) => void;
  bumpActivity: () => void;
  dismissTouched: (kind: ArtifactKind, id: string) => void;
}

export const touchedKey = (kind: ArtifactKind, id: string) => `${kind}:${id}`;

export const useArtifactStore = create<ArtifactState>((set) => ({
  versions: { strategy: 0, model: 0, automation: 0, dataset: 0 },
  activityVersion: 0,
  copilotTouched: {},
  note: (event) =>
    set((state) => ({
      versions: { ...state.versions, [event.kind]: state.versions[event.kind] + 1 },
      copilotTouched:
        event.actor === 'copilot' && !event.action.endsWith('.delete')
          ? { ...state.copilotTouched, [touchedKey(event.kind, event.id)]: Date.now() }
          : state.copilotTouched,
    })),
  bumpActivity: () => set((state) => ({ activityVersion: state.activityVersion + 1 })),
  dismissTouched: (kind, id) =>
    set((state) => {
      const next = { ...state.copilotTouched };
      delete next[touchedKey(kind, id)];
      return { copilotTouched: next };
    }),
}));
