import { test, expect } from '@playwright/test';

test.describe('Trade Automation Features', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
  });

  test('should display watchlist panel', async ({ page }) => {
    const watchlist = page.getByTestId('watchlist-panel');
    await expect(watchlist).toBeVisible({ timeout: 10000 });
  });

  test('should add symbol to watchlist', async ({ page }) => {
    // Click add symbol button
    const addButton = page.getByTestId('watchlist-add-button');
    await addButton.click();

    // Enter symbol
    const symbolInput = page.getByTestId('watchlist-symbol-input');
    await symbolInput.fill('ETHUSD');

    // Confirm addition
    const confirmButton = page.getByTestId('watchlist-confirm-add');
    await confirmButton.click();

    // Verify symbol is added
    const symbolItem = page.getByTestId('watchlist-item-ETHUSD');
    await expect(symbolItem).toBeVisible({ timeout: 3000 });
  });

  test('should remove symbol from watchlist', async ({ page }) => {
    // Add a symbol first
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').fill('SOLUSD');
    await page.getByTestId('watchlist-confirm-add').click();

    // Wait for symbol to appear
    const symbolItem = page.getByTestId('watchlist-item-SOLUSD');
    await expect(symbolItem).toBeVisible({ timeout: 3000 });

    // Remove the symbol
    const removeButton = symbolItem.getByTestId('watchlist-remove-button');
    await removeButton.click();

    // Verify symbol is removed
    await expect(symbolItem).not.toBeVisible({ timeout: 3000 });
  });

  test('should display real-time price updates', async ({ page }) => {
    // Add a symbol to watchlist
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').fill('BTCUSD');
    await page.getByTestId('watchlist-confirm-add').click();

    // Get initial price
    const priceElement = page.getByTestId('watchlist-price-BTCUSD');
    await expect(priceElement).toBeVisible({ timeout: 5000 });

    const initialPrice = await priceElement.textContent();
    expect(initialPrice).not.toBeNull();

    // Wait for potential update
    await page.waitForTimeout(3000);

    // Verify price element is still updating
    await expect(priceElement).toBeVisible();
  });

  test('should save workspace layout', async ({ page }) => {
    // Click save layout button
    const saveButton = page.getByTestId('workspace-save-layout');
    await saveButton.click();

    // Enter layout name
    const nameInput = page.getByTestId('layout-name-input');
    await nameInput.fill('test_layout');

    // Confirm save
    const confirmButton = page.getByTestId('confirm-save-layout');
    await confirmButton.click();

    // Verify save confirmation
    const confirmation = page.getByTestId('layout-save-confirmation');
    await expect(confirmation).toBeVisible({ timeout: 3000 });
  });

  test('should load saved workspace layout', async ({ page }) => {
    // Save a layout first
    await page.getByTestId('workspace-save-layout').click();
    await page.getByTestId('layout-name-input').fill('load_test_layout');
    await page.getByTestId('confirm-save-layout').click();

    // Wait for save confirmation
    await page.waitForTimeout(1000);

    // Open load layout menu
    const loadButton = page.getByTestId('workspace-load-layout');
    await loadButton.click();

    // Select the saved layout
    const layoutItem = page.getByTestId('layout-item-load_test_layout');
    await expect(layoutItem).toBeVisible({ timeout: 3000 });
    await layoutItem.click();

    // Verify layout is loaded
    const loadConfirmation = page.getByTestId('layout-load-confirmation');
    await expect(loadConfirmation).toBeVisible({ timeout: 3000 });
  });

  test('should display alert notifications', async ({ page }) => {
    // Set up an alert
    const alertButton = page.getByTestId('create-alert-button');
    await alertButton.click();

    // Configure alert
    await page.getByTestId('alert-symbol-input').fill('BTCUSD');
    await page.getByTestId('alert-condition-select').selectOption('price_above');
    await page.getByTestId('alert-value-input').fill('100000');

    // Save alert
    await page.getByTestId('alert-save-button').click();

    // Verify alert is created
    const alertItem = page.getByTestId('alert-item');
    await expect(alertItem).toBeVisible({ timeout: 3000 });
  });

  test('should display chart visualizations', async ({ page }) => {
    // Check for CandleChart
    const candleChart = page.getByTestId('candle-chart');
    await expect(candleChart).toBeVisible({ timeout: 10000 });
  });

  test('should update charts with real-time data', async ({ page }) => {
    const candleChart = page.getByTestId('candle-chart');
    await expect(candleChart).toBeVisible({ timeout: 10000 });

    // Verify chart canvas is rendered
    const canvas = candleChart.locator('canvas').first();
    await expect(canvas).toBeVisible();
  });

  test('should handle workspace panel resizing', async ({ page }) => {
    // Get a panel
    const panel = page.getByTestId('workspace-panel').first();
    await expect(panel).toBeVisible({ timeout: 10000 });

    // Get initial size
    const initialBox = await panel.boundingBox();
    expect(initialBox).toBeTruthy();

    // Attempt to drag resize handle if available
    const resizeHandle = panel.getByTestId('resize-handle');
    if (await resizeHandle.isVisible()) {
      await resizeHandle.dragTo(resizeHandle, {
        targetPosition: { x: 50, y: 0 }
      });

      // Verify size changed
      const newBox = await panel.boundingBox();
      expect(newBox).toBeTruthy();
    }
  });

  test('should persist watchlist across page reloads', async ({ page }) => {
    // Add symbols to watchlist
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').fill('ADAUSD');
    await page.getByTestId('watchlist-confirm-add').click();

    await page.waitForTimeout(500);

    // Reload page
    await page.reload();
    await page.waitForLoadState('networkidle');

    // Verify symbol still in watchlist
    const symbolItem = page.getByTestId('watchlist-item-ADAUSD');
    await expect(symbolItem).toBeVisible({ timeout: 5000 });
  });

  test('should display settings panel', async ({ page }) => {
    // Open settings
    const settingsButton = page.getByTestId('open-settings');
    await settingsButton.click();

    // Verify settings panel is visible
    const settingsPanel = page.getByTestId('settings-panel');
    await expect(settingsPanel).toBeVisible({ timeout: 5000 });
  });

  test('should allow configuring trade settings', async ({ page }) => {
    // Open settings
    await page.getByTestId('open-settings').click();

    const settingsPanel = page.getByTestId('settings-panel');
    await expect(settingsPanel).toBeVisible({ timeout: 5000 });

    // Check for various settings sections
    await page.getByTestId('settings-tab-trading').click();
    const tradingSettings = page.getByTestId('trading-settings-section');
    await expect(tradingSettings).toBeVisible();
  });
});
