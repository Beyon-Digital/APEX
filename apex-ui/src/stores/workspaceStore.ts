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
  /** Focus mode: the Copilot docks as a side panel next to the focused panel. */
  copilotSideOpen: boolean;
  copilotSideWidth: number;
  setCopilotSideOpen: (open: boolean) => void;
  setCopilotSideWidth: (width: number) => void;
  /** Show the Copilot if hidden, hide it if shown: a tile in tiles mode, the side panel in focus mode. */
  toggleCopilot: () => void;
  /** Make sure the Copilot is visible (never hides it). */
  showCopilot: () => void;
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
      copilotSideOpen: false,
      copilotSideWidth: 420,

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
      setCopilotSideOpen: (open) => set({ copilotSideOpen: open }),
      setCopilotSideWidth: (width) => set({ copilotSideWidth: Math.round(Math.min(900, Math.max(280, width))) }),
      toggleCopilot: () => {
        const { viewMode, tiles, copilotSideOpen } = get();
        if (viewMode === 'focus') {
          set({ copilotSideOpen: !copilotSideOpen });
        } else if (tiles.some((t) => t.i === 'copilot')) {
          set({ tiles: tiles.filter((t) => t.i !== 'copilot') });
        } else {
          get().ensureTile('copilot');
        }
      },
      showCopilot: () => {
        if (get().viewMode === 'focus') set({ copilotSideOpen: true });
        else get().ensureTile('copilot');
      },

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
        copilotSideOpen: state.copilotSideOpen,
        copilotSideWidth: state.copilotSideWidth,
      }),
    }
  )
);
