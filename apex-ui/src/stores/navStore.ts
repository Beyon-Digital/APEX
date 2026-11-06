import { create } from 'zustand';
import { useWorkspaceStore } from './workspaceStore';

export type NavKind = 'strategy' | 'model' | 'dataset' | 'automation' | 'activity';

export interface NavRequest {
  kind: NavKind;
  /** Strategy path, model id, dataset name, automation id; empty for a plain panel switch. */
  id: string;
  /** Changes on every request so asking for the same item twice still fires. */
  nonce: number;
}

const PANEL_FOR: Record<NavKind, string> = {
  strategy: 'strategy',
  model: 'ml',
  dataset: 'ml',
  automation: 'automations',
  activity: 'activity',
};

interface NavState {
  request: NavRequest | null;
  /** Show the panel that owns this item (adding its tile if needed) and ask it to select the item. */
  open: (kind: NavKind, id?: string) => void;
  /** Called by the panel once it has handled a request of its kind. */
  clear: (nonce: number) => void;
}

let counter = 0;

export const useNavStore = create<NavState>((set) => ({
  request: null,
  open: (kind, id = '') => {
    counter += 1;
    set({ request: { kind, id, nonce: counter } });
    useWorkspaceStore.getState().setCommandTab(PANEL_FOR[kind]);
  },
  clear: (nonce) => set((state) => (state.request?.nonce === nonce ? { request: null } : state)),
}));

/** Convenience for components that just need to jump somewhere. */
export const openItem = (kind: NavKind, id?: string) => useNavStore.getState().open(kind, id);
