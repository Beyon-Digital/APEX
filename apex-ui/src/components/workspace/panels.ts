import type { CenterTab } from './workspaceTabs';

/** Panels that can be tiled. Tab ids plus the fixed side panels. */
export type PanelId = CenterTab | 'watchlist' | 'orders' | 'positions' | 'alerts';

export interface PanelDef {
  id: PanelId;
  title: string;
  /** Default size in grid cells (12 columns, 24 rows). */
  w: number;
  h: number;
  /** Smallest size in grid cells at which the panel is still usable. The grid enforces it. */
  minW: number;
  minH: number;
}

export const PANEL_DEFS: PanelDef[] = [
  { id: 'watchlist', title: 'Watchlist', w: 2, h: 14, minW: 2, minH: 8 },
  { id: 'chart', title: 'Chart', w: 7, h: 14, minW: 4, minH: 8 },
  { id: 'orders', title: 'Order Ticket', w: 3, h: 14, minW: 3, minH: 10 },
  { id: 'positions', title: 'Positions', w: 4, h: 10, minW: 3, minH: 5 },
  { id: 'blotter', title: 'Blotter', w: 5, h: 10, minW: 3, minH: 5 },
  { id: 'alerts', title: 'Alerts', w: 3, h: 10, minW: 2, minH: 5 },
  { id: 'market', title: 'Market', w: 6, h: 12, minW: 3, minH: 6 },
  { id: 'news', title: 'News', w: 4, h: 12, minW: 2, minH: 5 },
  { id: 'scanner', title: 'Scanner', w: 4, h: 12, minW: 3, minH: 6 },
  { id: 'book', title: 'Order Book', w: 4, h: 12, minW: 2, minH: 8 },
  { id: 'graph', title: 'Graph', w: 6, h: 12, minW: 3, minH: 8 },
  { id: 'analytics', title: 'Analytics', w: 6, h: 12, minW: 3, minH: 8 },
  { id: 'backtest', title: 'Backtest', w: 8, h: 14, minW: 4, minH: 8 },
  { id: 'strategy', title: 'Strategy IDE', w: 8, h: 16, minW: 5, minH: 10 },
  { id: 'ml', title: 'ML Workbench', w: 8, h: 14, minW: 5, minH: 10 },
  { id: 'data', title: 'Stored Data', w: 6, h: 12, minW: 3, minH: 6 },
  { id: 'automations', title: 'Automations', w: 6, h: 12, minW: 3, minH: 6 },
  { id: 'notebook', title: 'Notebook', w: 8, h: 14, minW: 4, minH: 8 },
  { id: 'health', title: 'Health', w: 6, h: 12, minW: 3, minH: 6 },
  { id: 'copilot', title: 'Copilot', w: 4, h: 20, minW: 3, minH: 8 },
  { id: 'activity', title: 'Activity', w: 5, h: 12, minW: 3, minH: 5 },
];

export const PANEL_TITLES: Record<string, string> = Object.fromEntries(PANEL_DEFS.map((p) => [p.id, p.title]));

export interface TileItem {
  i: PanelId;
  x: number;
  y: number;
  w: number;
  h: number;
  /** Collapsed to the header row (h is then COLLAPSED_H). */
  collapsed?: boolean;
  /** Height to restore when a collapsed tile is expanded. */
  prevH?: number;
}

export const GRID_COLS = 12;
export const GRID_ROWS = 24;
/** A tile can be taller than the viewport (the workspace scrolls): up to three screens of rows. */
export const MAX_TILE_H = GRID_ROWS * 3;
/** A collapsed tile is one grid row: just its header. */
export const COLLAPSED_H = 1;

const FALLBACK_DEF = { w: 4, h: 10, minW: 2, minH: 4 };
const defFor = (id: string) => PANEL_DEFS.find((p) => p.id === id) ?? FALLBACK_DEF;

/** Smallest size of a panel in grid cells. */
export function panelMin(id: string): { minW: number; minH: number } {
  const d = defFor(id);
  return { minW: d.minW, minH: d.minH };
}

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
const whole = (v: unknown, fallback: number) => (typeof v === 'number' && Number.isFinite(v) ? Math.round(v) : fallback);

/**
 * Bring one persisted or loaded tile into the valid range: fields added later (collapsed, prevH)
 * default sensibly, sizes are clamped to the panel's minimum and the grid, and unknown
 * extra fields are dropped. Tiles from before minimum sizes existed are clamped, never discarded.
 */
export function normalizeTile(raw: unknown): TileItem | null {
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.i !== 'string' || !r.i) return null;
  const d = defFor(r.i);
  const w = clamp(whole(r.w, d.w), d.minW, GRID_COLS);
  const x = clamp(whole(r.x, 0), 0, GRID_COLS - w);
  const y = Math.max(0, whole(r.y, 0));
  if (r.collapsed === true) {
    return { i: r.i as PanelId, x, y, w, h: COLLAPSED_H, collapsed: true, prevH: clamp(whole(r.prevH, d.h), d.minH, MAX_TILE_H) };
  }
  return { i: r.i as PanelId, x, y, w, h: clamp(whole(r.h, d.h), d.minH, MAX_TILE_H) };
}

/** Normalize a whole layout: drops malformed entries and repeated panel ids, keeps everything else. */
export function normalizeTiles(raw: unknown): TileItem[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const out: TileItem[] = [];
  for (const entry of raw) {
    const t = normalizeTile(entry);
    if (t && !seen.has(t.i)) {
      seen.add(t.i);
      out.push(t);
    }
  }
  return out;
}

export type ResizeKey = 'ArrowLeft' | 'ArrowRight' | 'ArrowUp' | 'ArrowDown';

/**
 * Keyboard resize by one grid unit. Without startEdge the right/bottom edge moves (Right/Down grow,
 * Left/Up shrink). With startEdge the left/top edge moves in the arrow's direction (Left/Up grow,
 * Right/Down shrink). Returns null when the move would break a minimum, a maximum or the grid bounds.
 * Collapsed tiles only resize horizontally. Neighbours are handled by the grid's compaction: they
 * are pushed down, never overlapped.
 */
export function resizeTileByKey(t: TileItem, key: ResizeKey, startEdge: boolean): TileItem | null {
  const { minW, minH } = panelMin(t.i);
  let { x, y, w, h } = t;
  const vertical = key === 'ArrowUp' || key === 'ArrowDown';
  if (vertical && t.collapsed) return null;
  if (!vertical) {
    if (!startEdge) w += key === 'ArrowRight' ? 1 : -1;
    else if (key === 'ArrowLeft') { x -= 1; w += 1; } else { x += 1; w -= 1; }
  } else if (!startEdge) {
    h += key === 'ArrowDown' ? 1 : -1;
  } else if (key === 'ArrowUp') { y -= 1; h += 1; } else { y += 1; h -= 1; }
  if (w < minW || w > GRID_COLS || x < 0 || x + w > GRID_COLS) return null;
  if (!t.collapsed && (h < minH || h > MAX_TILE_H)) return null;
  if (y < 0) return null;
  return { ...t, x, y, w, h };
}

const tile = (i: PanelId, x: number, y: number, w: number, h: number): TileItem => ({ i, x, y, w, h });

export const PRESETS: Record<string, { label: string; tiles: TileItem[] }> = {
  trading: {
    label: 'Trading',
    tiles: [
      tile('watchlist', 0, 0, 2, 14),
      tile('chart', 2, 0, 7, 14),
      tile('orders', 9, 0, 3, 14),
      tile('positions', 0, 14, 4, 10),
      tile('blotter', 4, 14, 5, 10),
      tile('alerts', 9, 14, 3, 10),
    ],
  },
  research: {
    label: 'Research',
    tiles: [
      tile('chart', 0, 0, 6, 12),
      tile('analytics', 6, 0, 6, 12),
      tile('news', 0, 12, 4, 12),
      tile('scanner', 4, 12, 4, 12),
      tile('watchlist', 8, 12, 4, 12),
    ],
  },
  build: {
    label: 'Build',
    tiles: [
      tile('chart', 0, 0, 8, 12),
      tile('strategy', 0, 12, 8, 12),
      tile('copilot', 8, 0, 4, 24),
    ],
  },
  monitor: {
    label: 'Monitor',
    tiles: [
      tile('market', 0, 0, 6, 12),
      tile('health', 6, 0, 6, 12),
      tile('news', 0, 12, 6, 12),
      tile('blotter', 6, 12, 6, 12),
    ],
  },
};

/** Place a new tile in the first free spot, scanning rows then columns. */
export function placeTile(existing: TileItem[], id: PanelId): TileItem {
  const def = defFor(id);
  const w = clamp(def.w, def.minW, GRID_COLS);
  const occupied = (x: number, y: number) =>
    existing.some((t) => x < t.x + t.w && x + w > t.x && y < t.y + t.h && y + def.h > t.y);
  const bottom = existing.reduce((max, t) => Math.max(max, t.y + t.h), 0);
  for (let y = 0; y <= bottom; y += 1) {
    for (let x = 0; x + w <= GRID_COLS; x += 1) {
      if (!occupied(x, y)) return { i: id, x, y, w, h: def.h };
    }
  }
  return { i: id, x: 0, y: bottom, w, h: def.h };
}
