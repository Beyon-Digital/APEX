import { test, expect } from '@playwright/test';

// The app's default is the tiled workspace; override the focus-mode seed from the config.
test.use({ storageState: { cookies: [], origins: [] } });

test.describe('Tiled workspace', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
  });

  test('opens the trading preset with the core panels', async ({ page }) => {
    for (const id of ['watchlist', 'chart', 'orders', 'positions', 'blotter', 'alerts']) {
      await expect(page.getByTestId(`tile-${id}`)).toBeVisible();
    }
    await expect(page.getByTestId('watchlist-panel')).toBeVisible();
    await expect(page.getByTestId('order-entry-panel')).toBeVisible();
  });

  test('closing a tile removes it and the panel menu can reopen it', async ({ page }) => {
    await page.getByTestId('tile-close-alerts').click();
    await expect(page.getByTestId('tile-alerts')).toHaveCount(0);

    await page.getByTestId('tiles-add-menu').click();
    await page.getByTestId('tiles-add-alerts').click();
    await expect(page.getByTestId('tile-alerts')).toBeVisible();
  });

  test('clicking a tab opens that panel as a tile', async ({ page }) => {
    await expect(page.getByTestId('tile-news')).toHaveCount(0);
    await page.getByTestId('tab-news').click();
    await expect(page.getByTestId('tile-news')).toBeVisible();
  });

  test('presets replace the layout', async ({ page }) => {
    await page.getByTestId('tiles-preset-monitor').click();
    await expect(page.getByTestId('tile-health')).toBeVisible();
    await expect(page.getByTestId('tile-market')).toBeVisible();
    await expect(page.getByTestId('tile-chart')).toHaveCount(0);
  });

  test('the layout survives a reload', async ({ page }) => {
    await page.getByTestId('tiles-preset-research').click();
    await expect(page.getByTestId('tile-analytics')).toBeVisible();
    await page.reload();
    await page.waitForLoadState('networkidle');
    await expect(page.getByTestId('tile-analytics')).toBeVisible();
    await expect(page.getByTestId('tile-orders')).toHaveCount(0);
  });

  test('the view toggle switches to the single-panel focus view and back', async ({ page }) => {
    await page.getByTestId('view-mode-toggle').click();
    await expect(page.getByTestId('tile-chart')).toHaveCount(0);
    await expect(page.getByTestId('workspace-panel')).toBeVisible();
    await page.getByTestId('view-mode-toggle').click();
    await expect(page.getByTestId('tile-chart')).toBeVisible();
  });

  test('selecting a watchlist symbol keeps tiles mounted', async ({ page }) => {
    await page.getByTestId('watchlist-item-TCS.NS').locator('td').first().click();
    await expect(page.getByTestId('tile-chart')).toBeVisible();
  });

  test('every tile is a labelled group with collapse, maximize and close buttons', async ({ page }) => {
    for (const [id, title] of [['watchlist', 'Watchlist'], ['chart', 'Chart'], ['orders', 'Order Ticket']]) {
      await expect(page.getByRole('group', { name: title, exact: true })).toBeVisible();
      await expect(page.getByTestId(`tile-collapse-${id}`)).toHaveAttribute('aria-expanded', 'true');
      await expect(page.getByTestId(`tile-maximize-${id}`)).toBeVisible();
      await expect(page.getByTestId(`tile-close-${id}`)).toBeVisible();
    }
  });

  test('a collapsed tile still closes and reopens from the panel menu', async ({ page }) => {
    await page.getByTestId('tile-collapse-alerts').click();
    await expect(page.getByTestId('tile-alerts')).toHaveAttribute('data-collapsed', 'true');
    await page.getByTestId('tile-close-alerts').click();
    await expect(page.getByTestId('tile-alerts')).toHaveCount(0);
    await page.getByTestId('tiles-add-menu').click();
    await page.getByTestId('tiles-add-alerts').click();
    await expect(page.getByTestId('tile-alerts')).toBeVisible();
  });
});
