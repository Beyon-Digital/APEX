import { test, expect, type Locator, type Page } from '@playwright/test';

// Resizable, collapsible, maximizable tiles. Real mouse drags throughout.
test.use({ storageState: { cookies: [], origins: [] }, viewport: { width: 1500, height: 900 } });

type Box = { x: number; y: number; width: number; height: number };
type Grid = { x: number; y: number; w: number; h: number };
type Seed = { i: string; x: number; y: number; w: number; h: number; [k: string]: unknown };

const MARGIN = 4;

async function box(locator: Locator): Promise<Box> {
  const b = await locator.boundingBox();
  if (!b) throw new Error('element has no bounding box');
  return b;
}

const tile = (page: Page, id: string) => page.getByTestId(`tile-${id}`);

/** Grid position and size from the tile's data attributes. */
async function grid(page: Page, id: string): Promise<Grid> {
  const el = tile(page, id);
  const n = async (a: string) => Number(await el.getAttribute(a));
  return { x: await n('data-x'), y: await n('data-y'), w: await n('data-w'), h: await n('data-h') };
}

/** Pixels per grid column and per grid row (including the margin), measured from the DOM. */
async function units(page: Page, id: string) {
  const g = await grid(page, id);
  const b = await box(tile(page, id));
  return { ux: (b.width + MARGIN) / g.w, uy: (b.height + MARGIN) / g.h };
}

const overlaps = (a: Box, b: Box) =>
  a.x < b.x + b.width - 1 && b.x < a.x + a.width - 1 && a.y < b.y + b.height - 1 && b.y < a.y + a.height - 1;

async function expectNoOverlapAnywhere(page: Page) {
  await expect
    .poll(async () => {
      const ids = await page.locator('[data-testid^="tile-"][role="group"]').evaluateAll((els) =>
        els.map((e) => (e.getAttribute('data-testid') ?? '').replace('tile-', '')),
      );
      const boxes = await Promise.all(ids.map(async (id) => [id, await box(tile(page, id))] as const));
      const hits: string[] = [];
      for (let i = 0; i < boxes.length; i += 1) {
        for (let j = i + 1; j < boxes.length; j += 1) {
          if (overlaps(boxes[i][1], boxes[j][1])) hits.push(`${boxes[i][0]}/${boxes[j][0]}`);
        }
      }
      return hits;
    }, { message: 'no two tiles may overlap' })
    .toEqual([]);
}

async function openTiles(page: Page, seed?: Seed[]) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  if (seed) {
    await page.evaluate((tiles) => {
      localStorage.setItem('workspace-storage', JSON.stringify({ state: { viewMode: 'tiles', tiles }, version: 0 }));
    }, seed);
    await page.reload();
    await page.waitForLoadState('networkidle');
  }
  await expect(page.getByTestId('tile-area')).toBeVisible();
}

/** Drag the middle of a handle by (dx, dy) screen pixels with the real mouse. */
async function dragHandle(page: Page, id: string, axis: string, dx: number, dy: number) {
  // Tiles animate to their new cell for 200ms: wait until the box stops moving.
  let last = '';
  await expect.poll(async () => {
    const now = JSON.stringify(await box(tile(page, id)));
    const settled = now === last;
    last = now;
    return settled;
  }).toBe(true);
  const handle = tile(page, id).locator(`.react-resizable-handle-${axis}`);
  await handle.scrollIntoViewIfNeeded();
  const b = await box(handle);
  const cx = b.x + b.width / 2;
  const cy = b.y + b.height / 2;
  // Keep the pointer inside the window: events outside it are not delivered.
  const vp = page.viewportSize()!;
  const tx = Math.min(vp.width - 2, Math.max(2, cx + dx));
  const ty = Math.min(vp.height - 2, Math.max(2, cy + dy));
  await page.mouse.move(cx, cy);
  await page.mouse.down();
  await page.mouse.move((cx + tx) / 2, (cy + ty) / 2, { steps: 6 });
  await page.mouse.move(tx, ty, { steps: 6 });
  await page.mouse.up();
}

/** Spaced layout: a wide anchor on top and a target with free columns on both sides. */
const SPACED: Seed[] = [
  { i: 'market', x: 0, y: 0, w: 12, h: 6 },
  { i: 'news', x: 4, y: 6, w: 4, h: 8 },
];

test.describe('Tile minimum sizes', () => {
  // Expected minimums are spelled out here on purpose: they are the contract, not a re-read of PANEL_DEFS.
  const cases = [
    { id: 'watchlist', minW: 2, minH: 8 },
    { id: 'orders', minW: 3, minH: 10 },
    { id: 'chart', minW: 4, minH: 8 },
  ];

  for (const c of cases) {
    test(`${c.id} cannot be shrunk below ${c.minW}x${c.minH}`, async ({ page }) => {
      await openTiles(page);
      await expect(tile(page, c.id)).toHaveAttribute('data-min-w', String(c.minW));
      await expect(tile(page, c.id)).toHaveAttribute('data-min-h', String(c.minH));
      const before = await box(tile(page, c.id));
      const { ux, uy } = await units(page, c.id);

      // Drag the bottom-right corner up and left by the whole tile size, far past the minimum.
      await dragHandle(page, c.id, 'se', -before.width + 8, -before.height + 8);

      await expect.poll(() => grid(page, c.id)).toMatchObject({ w: c.minW, h: c.minH });
      const after = await box(tile(page, c.id));
      // Grid cells are rounded to whole pixels.
      expect(Math.abs(after.width - (c.minW * ux - MARGIN))).toBeLessThan(1.5);
      expect(Math.abs(after.height - (c.minH * uy - MARGIN))).toBeLessThan(1.5);
      expect(after.height).toBeLessThan(before.height);
      expect(after.width).toBeLessThanOrEqual(before.width);
    });
  }

  test('the north-west corner cannot shrink a tile below its minimum either', async ({ page }) => {
    await openTiles(page, [{ i: 'strategy', x: 0, y: 0, w: 9, h: 16 }]);
    const before = await box(tile(page, 'strategy'));
    await dragHandle(page, 'strategy', 'nw', before.width - 8, before.height - 8);
    await expect.poll(() => grid(page, 'strategy')).toMatchObject({ w: 5, h: 10 });
  });
});

test.describe('Growing tiles', () => {
  test('a tile grows to the full width and taller than the viewport', async ({ page }) => {
    await openTiles(page, SPACED);
    const start = await grid(page, 'news');
    const { ux } = await units(page, 'news');

    await dragHandle(page, 'news', 'se', ux * 2, 0);
    await expect.poll(async () => (await grid(page, 'news')).w).toBe(start.w + 2);

    await dragHandle(page, 'news', 'e', ux * 20, 0);
    await expect.poll(() => grid(page, 'news')).toMatchObject({ x: 4, w: 8 });
    await dragHandle(page, 'news', 'w', -ux * 20, 0);
    await expect.poll(() => grid(page, 'news')).toMatchObject({ x: 0, w: 12 });
    const area = await box(page.getByTestId('tile-area'));
    // Full width: the area minus its 4px padding on both sides (the tile animates there, so poll).
    await expect.poll(async () => Math.abs((await box(tile(page, 'news'))).width - (area.width - 8))).toBeLessThan(2);

    // Taller than the viewport: hold the bottom handle at the window edge and the workspace scrolls with it.
    const handle = tile(page, 'news').locator('.react-resizable-handle-s');
    const hb = await box(handle);
    const vh = page.viewportSize()!.height;
    await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2);
    await page.mouse.down();
    await page.mouse.move(hb.x + hb.width / 2, vh - 40, { steps: 8 });
    await page.mouse.move(hb.x + hb.width / 2, vh - 6, { steps: 4 });
    // The grid commits to the store on release; while dragging the live size is in the DOM.
    await expect.poll(async () => (await box(tile(page, 'news'))).height, { timeout: 8000 }).toBeGreaterThan(vh);
    await page.mouse.up();
    await page.waitForTimeout(400);
    const g = await grid(page, 'news');
    expect(g.h).toBeGreaterThan(start.h);
    expect(g.h).toBeLessThanOrEqual(72);
    expect((await box(tile(page, 'news'))).height).toBeGreaterThan(vh);
    // The workspace scrolls to reach it.
    const scrollable = await page.getByTestId('tile-area').evaluate((el) => el.scrollHeight > el.clientHeight);
    expect(scrollable).toBe(true);
  });

  test('growing a tile pushes the tiles below it down instead of overlapping them', async ({ page }) => {
    await openTiles(page);
    const blottersY = (await grid(page, 'blotter')).y;
    const chart = await grid(page, 'chart');
    const { uy } = await units(page, 'chart');

    await dragHandle(page, 'chart', 's', 0, uy * 3);
    await expect.poll(async () => (await grid(page, 'chart')).h).toBe(chart.h + 3);
    await expect.poll(async () => (await grid(page, 'blotter')).y).toBeGreaterThan(blottersY);
    await expectNoOverlapAnywhere(page);
  });

  test('growing a tile sideways into a neighbour pushes the neighbour down, never over it', async ({ page }) => {
    await openTiles(page);
    const watch = await grid(page, 'watchlist');
    const { ux } = await units(page, 'watchlist');
    await dragHandle(page, 'watchlist', 'e', ux * 2, 0);
    await expect.poll(async () => (await grid(page, 'watchlist')).w).toBe(watch.w + 2);
    await expectNoOverlapAnywhere(page);
  });
});

test.describe('Resize handles', () => {
  // dirX/dirY: which way the handle moves an edge when dragged outward (grow).
  const handles = [
    { axis: 'e', dirX: 1, dirY: 0 },
    { axis: 'w', dirX: -1, dirY: 0 },
    { axis: 's', dirX: 0, dirY: 1 },
    { axis: 'n', dirX: 0, dirY: -1 },
    { axis: 'se', dirX: 1, dirY: 1 },
    { axis: 'sw', dirX: -1, dirY: 1 },
    { axis: 'ne', dirX: 1, dirY: -1 },
    { axis: 'nw', dirX: -1, dirY: -1 },
  ];

  for (const hd of handles) {
    test(`the ${hd.axis} handle grows the tile outward and shrinks it back`, async ({ page }) => {
      await openTiles(page, SPACED);
      const start = await grid(page, 'news');
      const { ux, uy } = await units(page, 'news');
      await expect(tile(page, 'news').locator(`.react-resizable-handle-${hd.axis}`)).toHaveCount(1);

      await dragHandle(page, 'news', hd.axis, hd.dirX * ux * 2, hd.dirY * uy * 2);
      await expect
        .poll(() => grid(page, 'news'))
        .toMatchObject({
          w: start.w + (hd.dirX ? 2 : 0),
          h: start.h + (hd.dirY ? 2 : 0),
          // The left edge moves with a west handle; the top edge stays put because the anchor tile above it cannot be pushed.
          x: start.x + (hd.dirX < 0 ? -2 : 0),
          y: start.y,
        });

      // And back in.
      await dragHandle(page, 'news', hd.axis, -hd.dirX * ux * 2, -hd.dirY * uy * 2);
      await expect.poll(() => grid(page, 'news')).toEqual(start);
    });
  }

  test('edge handles carry the right resize cursors and stay inside the tile bounds by a few pixels only', async ({ page }) => {
    await openTiles(page, SPACED);
    const cursors: Record<string, string> = {
      n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize',
      nw: 'nwse-resize', se: 'nwse-resize', ne: 'nesw-resize', sw: 'nesw-resize',
    };
    const t = await box(tile(page, 'news'));
    for (const [axis, cursor] of Object.entries(cursors)) {
      const handle = tile(page, 'news').locator(`.react-resizable-handle-${axis}`);
      await expect(handle).toHaveCSS('cursor', cursor);
      const b = await box(handle);
      // Not covering content: at most 6px thick on the edges, overhanging the border by 3px.
      if (axis.length === 1) expect(Math.min(b.width, b.height)).toBeLessThanOrEqual(6);
      expect(b.x).toBeGreaterThanOrEqual(t.x - 3.5);
      expect(b.y).toBeGreaterThanOrEqual(t.y - 3.5);
      expect(b.x + b.width).toBeLessThanOrEqual(t.x + t.width + 3.5);
      expect(b.y + b.height).toBeLessThanOrEqual(t.y + t.height + 3.5);
    }
  });
});

test.describe('Collapse', () => {
  test('the header chevron collapses a tile to its header and restores the height', async ({ page }) => {
    await openTiles(page);
    const before = await grid(page, 'chart');
    const blotterBefore = await grid(page, 'blotter');
    const button = page.getByTestId('tile-collapse-chart');
    await expect(button).toHaveAttribute('aria-expanded', 'true');
    await expect(button).toHaveAccessibleName('Collapse Chart');

    await button.click();
    await expect(button).toHaveAttribute('aria-expanded', 'false');
    await expect(button).toHaveAccessibleName('Expand Chart');
    await expect(tile(page, 'chart')).toHaveAttribute('data-collapsed', 'true');
    await expect.poll(async () => (await box(tile(page, 'chart'))).height).toBeLessThanOrEqual(32);
    await expect(tile(page, 'chart').locator('.tile-drag')).toBeVisible();
    await expect(tile(page, 'chart').getByText('Chart', { exact: true })).toBeVisible();
    // Tiles below move up into the freed space.
    await expect.poll(async () => (await grid(page, 'blotter')).y).toBeLessThan(blotterBefore.y);

    await button.click();
    await expect(tile(page, 'chart')).toHaveAttribute('data-collapsed', 'false');
    await expect.poll(() => grid(page, 'chart')).toEqual(before);
    await expect.poll(() => grid(page, 'blotter')).toEqual(blotterBefore);
    await expectNoOverlapAnywhere(page);
  });

  test('double-clicking the header collapses and restores', async ({ page }) => {
    await openTiles(page);
    const before = await grid(page, 'positions');
    const header = tile(page, 'positions').locator('.tile-drag');
    await header.dblclick({ position: { x: 90, y: 8 } });
    await expect(tile(page, 'positions')).toHaveAttribute('data-collapsed', 'true');
    await expect.poll(async () => (await box(tile(page, 'positions'))).height).toBeLessThanOrEqual(32);
    await header.dblclick({ position: { x: 90, y: 8 } });
    await expect(tile(page, 'positions')).toHaveAttribute('data-collapsed', 'false');
    await expect.poll(() => grid(page, 'positions')).toEqual(before);
  });

  test('a collapsed tile has no vertical handles, stays horizontally resizable and draggable', async ({ page }) => {
    await openTiles(page, SPACED);
    await page.getByTestId('tile-collapse-news').click();
    const t = tile(page, 'news');
    for (const axis of ['n', 's', 'ne', 'nw', 'se', 'sw']) {
      await expect(t.locator(`.react-resizable-handle-${axis}`)).toHaveCount(0);
    }
    for (const axis of ['e', 'w']) await expect(t.locator(`.react-resizable-handle-${axis}`)).toHaveCount(1);

    const start = await grid(page, 'news');
    expect(start.h).toBe(1);
    const { ux } = await units(page, 'news');
    await dragHandle(page, 'news', 'e', ux * 2, 0);
    await expect.poll(() => grid(page, 'news')).toMatchObject({ w: start.w + 2, h: 1 });

    // Still draggable by its header.
    const bar = await box(t.locator('.tile-drag'));
    await page.mouse.move(bar.x + 60, bar.y + bar.height / 2);
    await page.mouse.down();
    await page.mouse.move(bar.x - 100, bar.y + bar.height / 2, { steps: 10 });
    await page.mouse.move(bar.x - 200, bar.y + bar.height / 2, { steps: 10 });
    await page.mouse.up();
    await expect.poll(async () => (await grid(page, 'news')).x).toBeLessThan(start.x);
    await expect.poll(async () => (await grid(page, 'news')).h).toBe(1);
  });

  test('collapsing a tile that holds focus moves focus to its header button', async ({ page }) => {
    await openTiles(page);
    const input = tile(page, 'orders').locator('input, select, button:not(.tile-btn)').first();
    await input.focus();
    await expect(input).toBeFocused();
    // dispatchEvent leaves focus where it is, as a click does in WebKit (the Tauri webview on macOS).
    await page.getByTestId('tile-collapse-orders').dispatchEvent('click');
    await expect(tile(page, 'orders')).toHaveAttribute('data-collapsed', 'true');
    await expect(page.getByTestId('tile-collapse-orders')).toBeFocused();
  });

  test('a collapsed tile body is out of the tab order and the accessibility tree', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('tile-collapse-orders').click();
    await expect(page.getByTestId('order-entry-panel')).toBeHidden();
    await page.getByTestId('tile-collapse-orders').click();
    await expect(page.getByTestId('order-entry-panel')).toBeVisible();
  });
});

test.describe('Persistence', () => {
  test('collapsed state, previous height and sizes survive a reload', async ({ page }) => {
    await openTiles(page, SPACED);
    const { ux, uy } = await units(page, 'news');
    await dragHandle(page, 'news', 'se', ux * 2, uy * 3);
    await expect.poll(() => grid(page, 'news')).toMatchObject({ w: 6, h: 11 });
    await page.getByTestId('tile-collapse-news').click();
    await expect(tile(page, 'news')).toHaveAttribute('data-collapsed', 'true');

    await page.reload();
    await page.waitForLoadState('networkidle');
    await expect(tile(page, 'news')).toHaveAttribute('data-collapsed', 'true');
    expect(await grid(page, 'news')).toMatchObject({ w: 6, h: 1 });
    const persisted = await page.evaluate(() => JSON.parse(localStorage.getItem('workspace-storage') ?? '{}').state.tiles);
    expect(persisted.find((t: Seed) => t.i === 'news')).toMatchObject({ collapsed: true, prevH: 11, h: 1 });

    await page.getByTestId('tile-collapse-news').click();
    await expect.poll(() => grid(page, 'news')).toMatchObject({ w: 6, h: 11 });
  });
});

test.describe('Maximize', () => {
  test('maximize fills the workspace area, hides the others, and restores with the button', async ({ page }) => {
    await openTiles(page);
    const before = await grid(page, 'chart');
    const watchBefore = await grid(page, 'watchlist');
    await page.getByTestId('tile-maximize-chart').click();

    await expect(tile(page, 'chart')).toHaveAttribute('data-maximized', 'true');
    await expect(page.getByTestId('tile-maximize-chart')).toHaveAccessibleName('Restore Chart');
    await expect(page.getByTestId('tile-maximize-chart')).toHaveAttribute('aria-pressed', 'true');
    const area = await box(page.getByTestId('tile-area'));
    await expect.poll(() => box(tile(page, 'chart'))).toMatchObject({
      x: expect.closeTo(area.x, 0), y: expect.closeTo(area.y, 0),
      width: expect.closeTo(area.width, 0), height: expect.closeTo(area.height, 0),
    });
    // The other tiles are hidden but stay mounted (unsaved edits survive), and stay in the layout.
    await expect(page.getByTestId('watchlist-panel')).toBeHidden();
    await expect(page.getByTestId('watchlist-panel')).toHaveCount(1);
    await expect(page.getByTestId('order-entry-panel')).toBeHidden();
    await expect(page.getByTestId('order-entry-panel')).toHaveCount(1);
    await expect(tile(page, 'watchlist')).toBeHidden();
    await expect(tile(page, 'watchlist')).toHaveCount(1);
    // No resize handles while maximized.
    await expect(tile(page, 'chart').locator('.react-resizable-handle-se')).toBeHidden();

    await page.getByTestId('tile-maximize-chart').click();
    await expect(tile(page, 'chart')).toHaveAttribute('data-maximized', 'false');
    await expect(page.getByTestId('watchlist-panel')).toBeVisible();
    expect(await grid(page, 'chart')).toEqual(before);
    expect(await grid(page, 'watchlist')).toEqual(watchBefore);
  });

  test('maximizing another tile keeps what was typed in a hidden one', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('order-quantity-input').fill('37');
    await page.getByTestId('tile-maximize-chart').click();
    await expect(tile(page, 'chart')).toHaveAttribute('data-maximized', 'true');
    await page.getByTestId('tile-maximize-chart').click();
    await expect(tile(page, 'chart')).toHaveAttribute('data-maximized', 'false');
    await expect(page.getByTestId('order-quantity-input')).toHaveValue('37');
  });

  test('Escape closes an open History panel first and only then restores the maximized tile', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('tile-maximize-orders').click();
    await expect(tile(page, 'orders')).toHaveAttribute('data-maximized', 'true');
    // Any modal dialog stands in for History / the order audit drawer, which are modal portals too.
    await page.evaluate(() => {
      const d = document.createElement('div');
      d.setAttribute('role', 'dialog');
      d.setAttribute('aria-modal', 'true');
      d.setAttribute('data-testid', 'test-modal');
      window.addEventListener('keydown', (e) => { if (e.key === 'Escape') d.remove(); });
      document.body.appendChild(d);
    });
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('test-modal')).toHaveCount(0);
    await expect(tile(page, 'orders')).toHaveAttribute('data-maximized', 'true');
    await page.keyboard.press('Escape');
    await expect(tile(page, 'orders')).toHaveAttribute('data-maximized', 'false');
  });

  test('Escape restores a maximized tile', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('tile-maximize-orders').click();
    await expect(tile(page, 'orders')).toHaveAttribute('data-maximized', 'true');
    await page.keyboard.press('Escape');
    await expect(tile(page, 'orders')).toHaveAttribute('data-maximized', 'false');
    await expect(page.getByTestId('watchlist-panel')).toBeVisible();
  });

  test('a maximized tile is not restored as maximized after a reload', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('tile-maximize-chart').click();
    await expect(tile(page, 'chart')).toHaveAttribute('data-maximized', 'true');
    await page.reload();
    await page.waitForLoadState('networkidle');
    await expect(tile(page, 'chart')).toHaveAttribute('data-maximized', 'false');
    await expect(page.getByTestId('watchlist-panel')).toBeVisible();
    const persisted = await page.evaluate(() => JSON.stringify(JSON.parse(localStorage.getItem('workspace-storage') ?? '{}').state));
    expect(persisted).not.toContain('maximized');
  });
});

test.describe('Keyboard resize', () => {
  test('Alt+arrows move the right/bottom edge, Shift+Alt+arrows the left/top edge', async ({ page }) => {
    await openTiles(page, SPACED);
    const header = page.getByTestId('tile-header-news');
    await header.focus();
    const start = await grid(page, 'news');

    await page.keyboard.press('Alt+ArrowRight');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ w: start.w + 1, x: start.x });
    await page.keyboard.press('Alt+ArrowLeft');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ w: start.w });
    await page.keyboard.press('Alt+ArrowDown');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ h: start.h + 1 });
    await page.keyboard.press('Alt+ArrowUp');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ h: start.h });

    await page.keyboard.press('Shift+Alt+ArrowLeft');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ x: start.x - 1, w: start.w + 1 });
    await page.keyboard.press('Shift+Alt+ArrowRight');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ x: start.x, w: start.w });
    await expect(header).toBeFocused();
  });

  test('keyboard resizing stops at the minimum size and at the grid edge', async ({ page }) => {
    await openTiles(page, SPACED);
    const header = page.getByTestId('tile-header-news');
    await header.focus();
    for (let i = 0; i < 8; i += 1) await page.keyboard.press('Alt+ArrowLeft');
    await expect.poll(async () => (await grid(page, 'news')).w).toBe(2);
    await expect(page.getByTestId('tile-resize-status')).toContainText('limit');
    for (let i = 0; i < 12; i += 1) await page.keyboard.press('Alt+ArrowUp');
    await expect.poll(async () => (await grid(page, 'news')).h).toBe(5);
    for (let i = 0; i < 16; i += 1) await page.keyboard.press('Alt+ArrowRight');
    await expect.poll(async () => (await grid(page, 'news')).w).toBe(8);
    expect((await grid(page, 'news')).x + 8).toBe(12);
  });

  test('keyboard growth pushes neighbours down without overlap', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('tile-header-chart').focus();
    const blotterY = (await grid(page, 'blotter')).y;
    for (let i = 0; i < 3; i += 1) await page.keyboard.press('Alt+ArrowDown');
    await expect.poll(async () => (await grid(page, 'blotter')).y).toBeGreaterThan(blotterY);
    await expectNoOverlapAnywhere(page);
  });

  test('a collapsed tile ignores vertical keys but resizes horizontally', async ({ page }) => {
    await openTiles(page, SPACED);
    await page.getByTestId('tile-collapse-news').click();
    await page.getByTestId('tile-header-news').focus();
    await page.keyboard.press('Alt+ArrowDown');
    await page.keyboard.press('Alt+ArrowRight');
    await expect.poll(() => grid(page, 'news')).toMatchObject({ w: 5, h: 1 });
  });
});

test.describe('Accessibility', () => {
  test('tiles are labelled groups with labelled header buttons and nothing is disabled', async ({ page }) => {
    await openTiles(page);
    for (const [id, title] of [['chart', 'Chart'], ['orders', 'Order Ticket'], ['watchlist', 'Watchlist']]) {
      await expect(page.getByRole('group', { name: title, exact: true })).toHaveCount(1);
      await expect(page.getByTestId(`tile-maximize-${id}`)).toHaveAccessibleName(`Maximize ${title}`);
      await expect(page.getByTestId(`tile-close-${id}`)).toHaveAccessibleName(`Close ${title}`);
      await expect(page.getByTestId(`tile-collapse-${id}`)).toHaveAttribute('aria-expanded', 'true');
    }
    expect(await tile(page, 'chart').locator('.tile-btn[disabled]').count()).toBe(0);
    await expect(tile(page, 'chart').locator('.tile-drag')).toHaveAttribute('tabindex', '0');
  });
});

test.describe('Legacy layouts', () => {
  test('a layout saved before minimum sizes and collapse existed is clamped, not lost', async ({ page }) => {
    const legacy: Seed[] = [
      { i: 'watchlist', x: 0, y: 0, w: 1, h: 2 }, // below its 2x8 minimum
      { i: 'orders', x: 9, y: 0, w: 3, h: 3 }, // below 3x10
      { i: 'chart', x: 10, y: 0, w: 20, h: 14, moved: false, static: false }, // wider than the grid, extra RGL fields
      { i: 'news', x: 2, y: 20, w: 4, h: 6 },
      { i: 'strategy', x: 0, y: 30, w: 8 } as unknown as Seed, // height missing entirely
    ];
    await openTiles(page, legacy);
    for (const id of ['watchlist', 'orders', 'chart', 'news', 'strategy']) await expect(tile(page, id)).toBeVisible();

    const mins: Record<string, [number, number]> = { watchlist: [2, 8], orders: [3, 10], chart: [4, 8], news: [2, 5], strategy: [5, 10] };
    for (const [id, [minW, minH]] of Object.entries(mins)) {
      const g = await grid(page, id);
      expect(g.w, `${id} width`).toBeGreaterThanOrEqual(minW);
      expect(g.h, `${id} height`).toBeGreaterThanOrEqual(minH);
      expect(g.x + g.w, `${id} right edge`).toBeLessThanOrEqual(12);
      await expect(tile(page, id)).toHaveAttribute('data-collapsed', 'false');
    }
    expect((await grid(page, 'watchlist')).h).toBe(8);
    expect((await grid(page, 'chart')).w).toBe(12);
    await expectNoOverlapAnywhere(page);

    // The migrated layout is what gets persisted from now on.
    await page.getByTestId('tile-collapse-watchlist').click();
    const persisted = await page.evaluate(() => JSON.parse(localStorage.getItem('workspace-storage') ?? '{}').state.tiles);
    expect(persisted.map((t: Seed) => t.i).sort()).toEqual(['chart', 'news', 'orders', 'strategy', 'watchlist']);
    expect(persisted.find((t: Seed) => t.i === 'watchlist')).toMatchObject({ collapsed: true, prevH: 8 });
    expect(persisted.every((t: Seed) => !('moved' in t) && !('static' in t))).toBe(true);
  });

  test('presets and newly added panels respect the minimum sizes', async ({ page }) => {
    await openTiles(page);
    for (const preset of ['trading', 'research', 'build', 'monitor']) {
      await page.getByTestId(`tiles-preset-${preset}`).click();
      const els = await page.locator('[data-testid^="tile-"][role="group"]').evaluateAll((nodes) =>
        nodes.map((n) => ({
          id: n.getAttribute('data-testid'),
          w: Number(n.getAttribute('data-w')), h: Number(n.getAttribute('data-h')),
          minW: Number(n.getAttribute('data-min-w')), minH: Number(n.getAttribute('data-min-h')),
        })),
      );
      expect(els.length).toBeGreaterThan(0);
      for (const e of els) {
        expect(e.w, `${preset} ${e.id} width`).toBeGreaterThanOrEqual(e.minW);
        expect(e.h, `${preset} ${e.id} height`).toBeGreaterThanOrEqual(e.minH);
      }
    }
    await page.getByTestId('tiles-add-menu').click();
    await page.getByTestId('tiles-add-ml').click();
    const ml = await grid(page, 'ml');
    expect(ml.w).toBeGreaterThanOrEqual(5);
    expect(ml.h).toBeGreaterThanOrEqual(10);
  });

  test('asking for a collapsed panel (tab click) opens it', async ({ page }) => {
    await openTiles(page);
    await page.getByTestId('tile-collapse-blotter').click();
    await expect(tile(page, 'blotter')).toHaveAttribute('data-collapsed', 'true');
    await page.getByTestId('tab-blotter').click();
    await expect(tile(page, 'blotter')).toHaveAttribute('data-collapsed', 'false');
  });
});

test.describe('Header buttons stay clickable', () => {
  test('corner resize handles do not cover the header buttons', async ({ page }) => {
    await openTiles(page);
    for (const id of ['watchlist', 'chart', 'orders', 'alerts']) {
      for (const kind of ['collapse', 'maximize', 'close']) {
        const btn = page.getByTestId(`tile-${kind}-${id}`);
        const b = await box(btn);
        const hit = await page.evaluate(([x, y]) => document.elementFromPoint(x, y)?.closest('button')?.getAttribute('data-testid'), [b.x + b.width / 2, b.y + b.height / 2]);
        expect(hit, `${kind} button of ${id} at its centre`).toBe(`tile-${kind}-${id}`);
      }
    }
  });
});
