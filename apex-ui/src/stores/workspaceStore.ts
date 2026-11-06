import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { PRESETS, placeTile, type PanelId, type TileItem } from '../components/workspace/panels';

interface WorkspaceLayout {
  name: string;
  config: Record<string, unknown>;
  timestamp: number;
}

interface WorkspaceState {
  layouts: WorkspaceLayout[];
  currentLayout: string | null;
  /** Symbol requested by CommandBar — Workspace picks this up */
  commandSymbol: string | null;
  /** Tab requested by CommandBar — Workspace picks this up */
  commandTab: string | null;
  /** "tiles" shows a draggable multi-panel grid; "focus" shows one panel at a time. */
  viewMode: 'tiles' | 'focus';
  tiles: TileItem[];
  /** Panel the user just asked for (tab click / command); the grid highlights it. */
  highlightedTile: PanelId | null;
  setViewMode: (mode: 'tiles' | 'focus') => void;
  setTiles: (tiles: TileItem[]) => void;
  applyPreset: (name: string) => void;
  ensureTile: (id: PanelId) => void;
  removeTile: (id: PanelId) => void;
  setHighlightedTile: (id: PanelId | null) => void;
  saveLayout: (name: string, config: Record<string, unknown>) => void;
  loadLayout: (name: string) => WorkspaceLayout | undefined;
  deleteLayout: (name: string) => void;
  getLayouts: () => WorkspaceLayout[];
  setCommandSymbol: (symbol: string | null) => void;
  setCommandTab: (tab: string | null) => void;
}

export const useWorkspaceStore = create<WorkspaceState>()(
  persist(
    (set, get) => ({
      layouts: [],
      currentLayout: null,
      commandSymbol: null,
      commandTab: null,
      viewMode: 'tiles',
      tiles: PRESETS.trading.tiles,
      highlightedTile: null,

      setViewMode: (mode) => set({ viewMode: mode }),
      setTiles: (tiles) => set({ tiles }),
      applyPreset: (name) => {
        const preset = PRESETS[name];
        if (preset) set({ tiles: preset.tiles.map((t) => ({ ...t })) });
      },
      ensureTile: (id) => {
        set((state) =>
          state.tiles.some((t) => t.i === id)
            ? { highlightedTile: id }
            : { tiles: [...state.tiles, placeTile(state.tiles, id)], highlightedTile: id },
        );
      },
      removeTile: (id) => set((state) => ({ tiles: state.tiles.filter((t) => t.i !== id) })),
      setHighlightedTile: (id) => set({ highlightedTile: id }),

      saveLayout: (name: string, config: Record<string, unknown>) => {
        set((state) => {
          const existingIndex = state.layouts.findIndex((l) => l.name === name);
          const newLayout: WorkspaceLayout = {
            name,
            config,
            timestamp: Date.now(),
          };

          if (existingIndex >= 0) {
            const updatedLayouts = [...state.layouts];
            updatedLayouts[existingIndex] = newLayout;
            return { layouts: updatedLayouts, currentLayout: name };
          }

          return {
            layouts: [...state.layouts, newLayout],
            currentLayout: name,
          };
        });
      },

      loadLayout: (name: string) => {
        const layout = get().layouts.find((l) => l.name === name);
        if (layout) {
          set({ currentLayout: name });
        }
        return layout;
      },

      deleteLayout: (name: string) => {
        set((state) => ({
          layouts: state.layouts.filter((l) => l.name !== name),
          currentLayout: state.currentLayout === name ? null : state.currentLayout,
        }));
      },

      getLayouts: () => get().layouts,

      setCommandSymbol: (symbol) => set({ commandSymbol: symbol }),
      setCommandTab: (tab) => set({ commandTab: tab }),
    }),
    {
      name: 'workspace-storage',
      partialize: (state) => ({
        layouts: state.layouts,
        currentLayout: state.currentLayout,
        viewMode: state.viewMode,
        tiles: state.tiles,
      }),
    }
  )
);
