import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import GridLayout, { WidthProvider, type Layout } from 'react-grid-layout';
import 'react-grid-layout/css/styles.css';
import 'react-resizable/css/styles.css';
import { ErrorBoundary } from '../common/ErrorBoundary';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { GRID_COLS, GRID_ROWS, PANEL_DEFS, PANEL_TITLES, PRESETS, type PanelId, type TileItem } from './panels';

const AutoWidthGrid = WidthProvider(GridLayout);
const MARGIN = 4;

interface TiledWorkspaceProps {
  renderPanel: (id: PanelId) => React.ReactNode;
  selectedSymbol: string;
}

/** Draggable, resizable multi-panel grid. Panels share the selected symbol. */
export const TiledWorkspace: React.FC<TiledWorkspaceProps> = ({ renderPanel, selectedSymbol }) => {
  const tiles = useWorkspaceStore((s) => s.tiles);
  const setTiles = useWorkspaceStore((s) => s.setTiles);
  const applyPreset = useWorkspaceStore((s) => s.applyPreset);
  const ensureTile = useWorkspaceStore((s) => s.ensureTile);
  const removeTile = useWorkspaceStore((s) => s.removeTile);
  const highlighted = useWorkspaceStore((s) => s.highlightedTile);
  const setHighlighted = useWorkspaceStore((s) => s.setHighlightedTile);
  const [menuOpen, setMenuOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(600);

  // Size rows so the preset grids fill the viewport exactly.
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => setHeight(el.clientHeight);
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!highlighted) return;
    document.querySelector(`[data-testid="tile-${highlighted}"]`)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    const timer = setTimeout(() => setHighlighted(null), 1200);
    return () => clearTimeout(timer);
  }, [highlighted, setHighlighted]);

  const rowHeight = Math.max(12, (height - MARGIN * (GRID_ROWS + 1)) / GRID_ROWS);

  const onLayoutChange = useCallback((next: Layout[]) => {
    const byId = new Map(next.map((l) => [l.i, l]));
    const merged: TileItem[] = useWorkspaceStore.getState().tiles.map((t) => {
      const l = byId.get(t.i);
      return l ? { i: t.i, x: l.x, y: l.y, w: l.w, h: l.h } : t;
    });
    const changed = merged.some((m, idx) => {
      const old = useWorkspaceStore.getState().tiles[idx];
      return old.x !== m.x || old.y !== m.y || old.w !== m.w || old.h !== m.h;
    });
    if (changed) setTiles(merged);
  }, [setTiles]);

  const closed = PANEL_DEFS.filter((p) => !tiles.some((t) => t.i === p.id));

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="workspace-panel">
      <div className="flex shrink-0 items-center gap-1 border-b border-[var(--border-color)] bg-surface-1 px-2 py-0.5 text-[11px]">
        <span className="mr-1 uppercase tracking-wider text-text-muted">Layout</span>
        {Object.entries(PRESETS).map(([key, preset]) => (
          <button key={key} type="button" onClick={() => applyPreset(key)} data-testid={`tiles-preset-${key}`}
            className="border border-[var(--border-color)] px-2 py-0.5 text-text-secondary hover:border-accent hover:text-accent">
            {preset.label}
          </button>
        ))}
        <div className="relative">
          <button type="button" onClick={() => setMenuOpen((o) => !o)} aria-expanded={menuOpen} data-testid="tiles-add-menu"
            className="border border-[var(--border-color)] px-2 py-0.5 text-text-secondary hover:border-accent hover:text-accent">
            + Panel
          </button>
          {menuOpen && (
            <ul className="absolute left-0 top-full z-40 mt-px max-h-72 w-44 overflow-auto border border-[var(--border-color)] bg-surface-1 shadow-lg">
              {closed.length === 0 && <li className="px-2 py-1 text-text-muted">All panels are open</li>}
              {closed.map((panel) => (
                <li key={panel.id}>
                  <button type="button" data-testid={`tiles-add-${panel.id}`}
                    onClick={() => { ensureTile(panel.id); setMenuOpen(false); }}
                    className="block w-full px-2 py-1 text-left text-text-secondary hover:bg-surface-3 hover:text-text-primary">
                    {panel.title}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
        <span className="ml-auto font-mono text-text-muted">{selectedSymbol}</span>
      </div>

      <div ref={containerRef} className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden bg-surface-0">
        <AutoWidthGrid
          className="layout"
          layout={tiles.map((t) => ({ ...t, minW: 2, minH: 4 }))}
          cols={GRID_COLS}
          rowHeight={rowHeight}
          margin={[MARGIN, MARGIN]}
          containerPadding={[MARGIN, MARGIN]}
          draggableHandle=".tile-drag"
          draggableCancel=".tile-close"
          onLayoutChange={onLayoutChange}
          compactType="vertical"
          resizeHandles={['se']}
        >
          {tiles.map((t) => (
            <div
              key={t.i}
              data-testid={`tile-${t.i}`}
              className={`flex min-h-0 min-w-0 flex-col overflow-hidden border bg-surface-1 ${
                highlighted === t.i ? 'border-accent shadow-[0_0_0_1px_var(--color-accent)]' : 'border-[var(--border-color)]'
              }`}
            >
              <div className="tile-drag flex h-5 shrink-0 cursor-move select-none items-center justify-between border-b border-[var(--border-color)] bg-surface-2 px-2">
                <span className="text-[10px] font-semibold uppercase tracking-widest text-accent">{PANEL_TITLES[t.i] ?? t.i}</span>
                <button type="button" className="tile-close px-1 text-xs leading-none text-text-muted hover:text-bear"
                  aria-label={`Close ${PANEL_TITLES[t.i] ?? t.i}`} data-testid={`tile-close-${t.i}`}
                  onClick={() => removeTile(t.i)}>
                  ✕
                </button>
              </div>
              <div className="min-h-0 flex-1 overflow-hidden">
                <ErrorBoundary label={PANEL_TITLES[t.i] ?? t.i}>{renderPanel(t.i)}</ErrorBoundary>
              </div>
            </div>
          ))}
        </AutoWidthGrid>
      </div>
    </div>
  );
};
