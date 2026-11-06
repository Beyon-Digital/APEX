import { useEffect } from 'react';
import { ACTIVITY_EVENT, ARTIFACT_EVENT } from '../lib/mockMl';
import { useArtifactStore } from '../stores/artifactStore';
import type { ArtifactEventDto } from '../lib/types';

const IS_TAURI = typeof window !== 'undefined' && '__TAURI__' in window;

/**
 * Tells the open panels when a strategy, model, dataset or automation is created, changed or
 * deleted by anything (the Copilot, an automation, another panel), so they refetch instead of
 * showing the list they loaded at startup. Desktop: Tauri events. Browser/mock: window events.
 */
export function useArtifactEvents() {
  useEffect(() => {
    const { note, bumpActivity } = useArtifactStore.getState();
    const onArtifact = (e: Event) => note((e as CustomEvent<ArtifactEventDto>).detail);
    const onActivity = () => bumpActivity();
    window.addEventListener(ARTIFACT_EVENT, onArtifact);
    window.addEventListener(ACTIVITY_EVENT, onActivity);

    let disposed = false;
    const unlisten: Array<() => void> = [];
    if (IS_TAURI) {
      void import('@tauri-apps/api/event').then(async ({ listen }) => {
        const a = await listen<ArtifactEventDto>('artifacts-changed', (event) => note(event.payload));
        const b = await listen('activity-added', () => bumpActivity());
        if (disposed) { a(); b(); } else { unlisten.push(a, b); }
      }).catch(() => undefined);
    }
    return () => {
      disposed = true;
      window.removeEventListener(ARTIFACT_EVENT, onArtifact);
      window.removeEventListener(ACTIVITY_EVENT, onActivity);
      unlisten.forEach((fn) => fn());
    };
  }, []);
}
