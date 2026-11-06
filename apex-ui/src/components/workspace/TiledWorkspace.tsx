import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import GridLayout, { WidthProvider, type Layout } from 'react-grid-layout';

type ResizeHandleAxis = NonNullable<Layout['resizeHandles']>[number];
import 'react-grid-layout/css/styles.css';
import 'react-resizable/css/styles.css';
import { ErrorBoundary } from '../common/ErrorBoundary';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import {
  COLLAPSED_H,
  GRID_COLS,
  GRID_ROWS,
  MAX_TILE_H,
  PANEL_DEFS,
  PANEL_TITLES,
  PRESETS,
  panelMin,
  resizeTileByKey,
  type PanelId,
  type ResizeKey,
  type TileItem,
} from './panels';

const AutoWidthGrid = WidthProvider(GridLayout);
const MARGIN = 4;
/** A one-row (collapsed) tile must still fit its 20px header plus the 1px border on each side. */
const MIN_ROW_HEIGHT = 22;
const ALL_HANDLES: ResizeHandleAxis[] = ['s', 'e', 'w', 'n', 'se', 'sw', 'ne', 'nw'];
const COLLAPSED_HANDLES: ResizeHandleAxis[] = ['e', 'w'];
const EDGE_SCROLL_ZONE = 28;
const EDGE_SCROLL_DELAY_MS = 400;
const ARROWS: ResizeKey[] = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'];

interface TiledWorkspaceProps {
  renderPanel: (id: PanelId) => React.ReactNode;
  selectedSymbol: string;
}

interface MaxRect {
  top: number;
  left: number;
  width: number;
  height: number;
}

/**
 * Draggable, resizable multi-panel grid. Panels share the selected symbol.
 *
 * Sizes: every panel has a minimum (PANEL_DEFS) the grid enforces; tiles grow up to the full width
 * and up to MAX_TILE_H rows (the workspace scrolls). Vertical compaction without collision
 * prevention means a tile that grows pushes its neighbours down instead of overlapping them, and
 * tiles below a collapsed tile move up. Resizing from the top or left edge is subject to the same
 * compaction: a tile that is stacked under another cannot push it away, so it grows downward.
 */
export const TiledWorkspace: React.FC<TiledWorkspaceProps> = ({ renderPanel, selectedSymbol }) => {
  const tiles = useWorkspaceStore((s) => s.tiles);
  const setTiles = useWorkspaceStore((s) => s.setTiles);
  const applyPreset = useWorkspaceStore((s) => s.applyPreset);
  const ensureTile = useWorkspaceStore((s) => s.ensureTile);
  const removeTile = useWorkspaceStore((s) => s.removeTile);
  const setTileCollapsed = useWorkspaceStore((s) => s.setTileCollapsed);
  const storedMaximized = useWorkspaceStore((s) => s.maximizedTile);
  const setMaximized = useWorkspaceStore((s) => s.setMaximizedTile);
  const highlighted = useWorkspaceStore((s) => s.highlightedTile);
  const setHighlighted = useWorkspaceStore((s) => s.setHighlightedTile);
  const [menuOpen, setMenuOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(600);
  const [maxRect, setMaxRect] = useState<MaxRect | null>(null);
  const [announcement, setAnnouncement] = useState('');
  const [resizing, setResizing] = useState(false);
  const stopAutoScroll = useRef<(() => void) | null>(null);

  const maximized = storedMaximized && tiles.some((t) => t.i === storedMaximized) ? storedMaximized : null;

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

  // A maximized tile is position: fixed over the workspace area (keeps its React state, unlike
  // moving it out of the grid). Track the area so it follows window resizes.
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!maximized || !el) {
      setMaxRect(null);
      return;
    }
    const update = () => {
      const r = el.getBoundingClientRect();
      setMaxRect({ top: r.top, left: r.left, width: r.width, height: r.height });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    window.addEventListener('resize', update);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', update);
    };
  }, [maximized]);

  useEffect(() => {
    if (!maximized) return;
    // Escape closes the topmost thing first. History, the order audit drawer and confirm dialogs close on
    // `window` (after this `document` listener runs, and without preventDefault), so while one is open
    // (modal ones, aria-modal="true") Escape belongs to it and the tile stays maximized.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      setMaximized(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [maximized, setMaximized]);

  useEffect(() => {
    if (!highlighted) return;
    document.querySelector(`[data-testid="tile-${highlighted}"]`)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    const timer = setTimeout(() => setHighlighted(null), 1200);
    return () => clearTimeout(timer);
  }, [highlighted, setHighlighted]);

  const rowHeight = Math.max(MIN_ROW_HEIGHT, (height - MARGIN * (GRID_ROWS + 1)) / GRID_ROWS);

  const layout = useMemo<Layout[]>(
    () =>
      tiles.map((t) => {
        const { minW, minH } = panelMin(t.i);
        return t.collapsed
          ? { i: t.i, x: t.x, y: t.y, w: t.w, h: COLLAPSED_H, minW, minH: COLLAPSED_H, maxH: COLLAPSED_H, resizeHandles: COLLAPSED_HANDLES }
          : { i: t.i, x: t.x, y: t.y, w: t.w, h: t.h, minW, minH, maxW: GRID_COLS, maxH: MAX_TILE_H, resizeHandles: ALL_HANDLES };
      }),
    [tiles],
  );

  const onLayoutChange = useCallback((next: Layout[]) => {
    const current = useWorkspaceStore.getState().tiles;
    const byId = new Map(next.map((l) => [l.i, l]));
    const merged: TileItem[] = current.map((t) => {
      const l = byId.get(t.i);
      return l ? { ...t, x: l.x, y: l.y, w: l.w, h: l.h } : t;
    });
    const changed = merged.some((m, idx) => {
      const old = current[idx];
      return old.x !== m.x || old.y !== m.y || old.w !== m.w || old.h !== m.h;
    });
    if (changed) setTiles(merged);
  }, [setTiles]);

  /**
   * A tile may be taller than the window, but the pointer cannot leave it. While a resize drag is
   * active, holding the pointer near the top or bottom edge of the workspace scrolls it (with room
   * added below the tiles) and replays a mouse-move so the grid keeps following the pointer.
   */
  const beginResize = useCallback((e: MouseEvent | undefined) => {
    const el = containerRef.current;
    if (!el) return;
    stopAutoScroll.current?.();
    setResizing(true);
    const pointer = { x: e?.clientX ?? 0, y: e?.clientY ?? 0 };
    const onMove = (ev: MouseEvent) => { pointer.x = ev.clientX; pointer.y = ev.clientY; };
    document.addEventListener('mousemove', onMove, true);
    let raf = 0;
    let enteredAt = 0;
    const tick = () => {
      const r = el.getBoundingClientRect();
      const fromBottom = pointer.y - (r.bottom - EDGE_SCROLL_ZONE);
      const fromTop = r.top + EDGE_SCROLL_ZONE - pointer.y;
      const inZone = fromBottom > 0 || fromTop > 0;
      // Only a deliberate hold scrolls: grabbing a handle that already sits at the window edge must not.
      if (!inZone) enteredAt = 0;
      else if (enteredAt === 0) enteredAt = performance.now();
      const holding = inZone && performance.now() - enteredAt >= EDGE_SCROLL_DELAY_MS;
      const speed = !holding ? 0 : fromBottom > 0 ? Math.min(28, 4 + fromBottom / 2) : -Math.min(28, 4 + fromTop / 2);
      if (speed !== 0) {
        const before = el.scrollTop;
        el.scrollTop += speed;
        if (el.scrollTop !== before) {
          document.dispatchEvent(new MouseEvent('mousemove', { clientX: pointer.x, clientY: pointer.y, bubbles: true }));
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    stopAutoScroll.current = () => {
      cancelAnimationFrame(raf);
      document.removeEventListener('mousemove', onMove, true);
      stopAutoScroll.current = null;
    };
  }, []);

  const endResize = useCallback(() => {
    stopAutoScroll.current?.();
    setResizing(false);
  }, []);

  useEffect(() => () => stopAutoScroll.current?.(), []);

  const toggleCollapsed = useCallback((id: PanelId, tileEl: HTMLElement | null) => {
    const tile = useWorkspaceStore.getState().tiles.find((t) => t.i === id);
    if (!tile) return;
    if (!tile.collapsed && tileEl) {
      // The body is about to be hidden: do not leave focus in it.
      const active = document.activeElement;
      if (active && tileEl.contains(active) && active !== tileEl) {
        tileEl.querySelector<HTMLElement>('[data-tile-collapse]')?.focus();
      }
    }
    setTileCollapsed(id, !tile.collapsed);
  }, [setTileCollapsed]);

  const onHeaderKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>, id: PanelId) => {
    if (e.target !== e.currentTarget) return;
    if (!e.altKey || !ARROWS.includes(e.key as ResizeKey)) return;
    e.preventDefault();
    const current = useWorkspaceStore.getState().tiles;
    const tile = current.find((t) => t.i === id);
    if (!tile) return;
    const next = resizeTileByKey(tile, e.key as ResizeKey, e.shiftKey);
    if (!next) {
      setAnnouncement(`${PANEL_TITLES[id] ?? id} is at its limit`);
      return;
    }
    setTiles(current.map((t) => (t.i === id ? next : t)));
    setAnnouncement(`${PANEL_TITLES[id] ?? id} ${next.w} by ${next.h}`);
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

      <div ref={containerRef} data-testid="tile-area" className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden bg-surface-0">
        <div style={{ paddingBottom: resizing ? Math.round(height * 0.6) : 0 }}>
        <AutoWidthGrid
          className="layout"
          layout={layout}
          cols={GRID_COLS}
          rowHeight={rowHeight}
          margin={[MARGIN, MARGIN]}
          containerPadding={[MARGIN, MARGIN]}
          draggableHandle=".tile-drag"
          draggableCancel=".tile-btn"
          onLayoutChange={onLayoutChange}
          compactType="vertical"
          preventCollision={false}
          isDraggable={!maximized}
          isResizable={!maximized}
          onResizeStart={(_l, _o, _n, _p, e) => beginResize(e as MouseEvent | undefined)}
          onResizeStop={endResize}
          resizeHandles={ALL_HANDLES}
        >
          {tiles.map((t) => {
            const title = PANEL_TITLES[t.i] ?? t.i;
            const { minW, minH } = panelMin(t.i);
            const isMax = maximized === t.i;
            const hidden = maximized !== null && !isMax;
            const style = isMax && maxRect
              ? ({ '--max-top': `${maxRect.top}px`, '--max-left': `${maxRect.left}px`, '--max-w': `${maxRect.width}px`, '--max-h': `${maxRect.height}px` } as React.CSSProperties)
              : undefined;
            return (
              <div
                key={t.i}
                data-testid={`tile-${t.i}`}
                role="group"
                aria-label={title}
                data-x={t.x}
                data-y={t.y}
                data-w={t.w}
                data-h={t.h}
                data-min-w={minW}
                data-min-h={minH}
                data-collapsed={t.collapsed ? 'true' : 'false'}
                data-maximized={isMax ? 'true' : 'false'}
                style={style}
                className={`flex min-h-0 min-w-0 flex-col border bg-surface-1 ${isMax ? 'tile-maximized' : ''} ${t.collapsed ? 'tile-collapsed' : ''} ${hidden ? 'tile-hidden' : ''} ${
                  highlighted === t.i ? 'border-accent shadow-[0_0_0_1px_var(--color-accent)]' : 'border-[var(--border-color)]'
                }`}
              >
                <div
                  className={`tile-drag flex h-5 shrink-0 select-none items-center justify-between bg-surface-2 px-2 outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent ${
                    t.collapsed ? '' : 'border-b border-[var(--border-color)]'
                  } ${isMax ? 'cursor-default' : 'cursor-move'}`}
                  tabIndex={0}
                  data-testid={`tile-header-${t.i}`}
                  title="Double-click to collapse. Alt+arrows resize, Shift+Alt+arrows move the left or top edge."
                  aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight Alt+ArrowUp Alt+ArrowDown"
                  onDoubleClick={(e) => toggleCollapsed(t.i, e.currentTarget.parentElement)}
                  onKeyDown={(e) => onHeaderKeyDown(e, t.i)}
                >
                  <div className="flex min-w-0 items-center">
                    <button type="button" className="tile-btn px-1 text-[10px] leading-none text-text-muted hover:text-accent"
                      data-tile-collapse
                      aria-label={`${t.collapsed ? 'Expand' : 'Collapse'} ${title}`} aria-expanded={!t.collapsed}
                      data-testid={`tile-collapse-${t.i}`}
                      onDoubleClick={(e) => e.stopPropagation()}
                      onClick={(e) => toggleCollapsed(t.i, e.currentTarget.closest<HTMLElement>('[role="group"]'))}>
                      {t.collapsed ? '▸' : '▾'}
                    </button>
                    <span className="truncate text-[10px] font-semibold uppercase tracking-widest text-accent">{title}</span>
                  </div>
                  <div className="flex shrink-0 items-center">
                    <button type="button" className="tile-btn px-1 text-[11px] leading-none text-text-muted hover:text-accent"
                      aria-label={`${isMax ? 'Restore' : 'Maximize'} ${title}`} aria-pressed={isMax}
                      title={isMax ? 'Restore (Esc)' : 'Maximize'}
                      data-testid={`tile-maximize-${t.i}`}
                      onDoubleClick={(e) => e.stopPropagation()}
                      onClick={() => setMaximized(isMax ? null : t.i)}>
                      {isMax ? '❐' : '□'}
                    </button>
                    <button type="button" className="tile-btn tile-close px-1 text-xs leading-none text-text-muted hover:text-bear"
                      aria-label={`Close ${title}`} data-testid={`tile-close-${t.i}`}
                      onDoubleClick={(e) => e.stopPropagation()}
                      onClick={() => removeTile(t.i)}>
                      ✕
                    </button>
                  </div>
                </div>
                {/* Kept mounted while collapsed AND while another tile is maximized, so unsaved edits and
                    form state survive both (the tile-hidden CSS hides it and takes it out of tab order). */}
                <div hidden={Boolean(t.collapsed)} className="min-h-0 flex-1 overflow-hidden">
                  <ErrorBoundary label={title}>{renderPanel(t.i)}</ErrorBoundary>
                </div>
              </div>
            );
          })}
        </AutoWidthGrid>
        </div>
      </div>
      <div className="sr-only" role="status" aria-live="polite" data-testid="tile-resize-status">{announcement}</div>
    </div>
  );
};
