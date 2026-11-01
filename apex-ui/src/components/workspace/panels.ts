import type { CenterTab } from './workspaceTabs';

/** Panels that can be tiled. Tab ids plus the fixed side panels. */
export type PanelId = CenterTab | 'watchlist' | 'orders' | 'positions' | 'alerts';

export interface PanelDef {
  id: PanelId;
  title: string;
  /** Default size in grid cells (12 columns, 24 rows). */
  w: number;
  h: number;
}

export const PANEL_DEFS: PanelDef[] = [
  { id: 'watchlist', title: 'Watchlist', w: 2, h: 14 },
  { id: 'chart', title: 'Chart', w: 7, h: 14 },
  { id: 'orders', title: 'Order Ticket', w: 3, h: 14 },
  { id: 'positions', title: 'Positions', w: 4, h: 10 },
  { id: 'blotter', title: 'Blotter', w: 5, h: 10 },
  { id: 'alerts', title: 'Alerts', w: 3, h: 10 },
  { id: 'market', title: 'Market', w: 6, h: 12 },
  { id: 'news', title: 'News', w: 4, h: 12 },
  { id: 'scanner', title: 'Scanner', w: 4, h: 12 },
  { id: 'book', title: 'Order Book', w: 4, h: 12 },
  { id: 'graph', title: 'Graph', w: 6, h: 12 },
  { id: 'analytics', title: 'Analytics', w: 6, h: 12 },
  { id: 'backtest', title: 'Backtest', w: 8, h: 14 },
  { id: 'strategy', title: 'Strategy IDE', w: 8, h: 16 },
  { id: 'ml', title: 'ML Workbench', w: 8, h: 14 },
  { id: 'data', title: 'Stored Data', w: 6, h: 12 },
  { id: 'automations', title: 'Automations', w: 6, h: 12 },
  { id: 'notebook', title: 'Notebook', w: 8, h: 14 },
  { id: 'health', title: 'Health', w: 6, h: 12 },
];

export const PANEL_TITLES: Record<string, string> = Object.fromEntries(PANEL_DEFS.map((p) => [p.id, p.title]));

export interface TileItem {
  i: PanelId;
  x: number;
  y: number;
  w: number;
  h: number;
}

export const GRID_COLS = 12;
export const GRID_ROWS = 24;

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
  const def = PANEL_DEFS.find((p) => p.id === id) ?? { w: 4, h: 10 };
  const w = Math.min(def.w, GRID_COLS);
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
