import { test, expect } from '@playwright/test';

test.describe('CommandBar Execution', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
  });

  test('should display command bar input', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await expect(input).toBeVisible({ timeout: 10000 });
  });

  test('space opens the command bar when focus is outside an editor', async ({ page }) => {
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.press('Space');
    await expect(page.getByTestId('command-bar-input')).toBeFocused();
  });

  test('should accept text input', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('AAPL');
    await expect(input).toHaveValue('AAPL');
  });

  test('should place order via command bar', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('BUY AAPL 10');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    // Should show feedback message
    const feedback = page.getByTestId('command-bar-feedback');
    await expect(feedback).toBeVisible({ timeout: 5000 });
    await expect(feedback).toContainText('Order placed');
  });

  test('should place limit order via command bar', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('SELL RELIANCE 5 LIMIT 2500');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    const feedback = page.getByTestId('command-bar-feedback');
    await expect(feedback).toBeVisible({ timeout: 5000 });
    await expect(feedback).toContainText('Order placed');
  });

  test('should switch to ML tab via system command', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill(':ML');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    // ML workbench should now be visible
    const mlWorkbench = page.getByTestId('ml-workbench');
    await expect(mlWorkbench).toBeVisible({ timeout: 5000 });
  });

  test('should switch to Health tab via system command', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill(':HEALTH');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    const healthMonitor = page.getByTestId('health-monitor');
    await expect(healthMonitor).toBeVisible({ timeout: 5000 });
  });

  test('should switch to Strategy tab via system command', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill(':STRATEGY');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    // Strategy IDE should be visible (check for strategy-ide or strategy editor)
    const strategyTab = page.getByTestId('tab-strategy');
    await expect(strategyTab).toHaveClass(/bg-accent/, { timeout: 5000 });
  });

  test('should show symbol chart via symbol command', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('INFY.NS');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    // Should show feedback about viewing the symbol
    const feedback = page.getByTestId('command-bar-feedback');
    await expect(feedback).toBeVisible({ timeout: 5000 });
    await expect(feedback).toContainText('Viewing');
  });

  test('should clear input after command execution', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('BUY AAPL 10');
    await page.getByTestId('command-bar-form').locator('input').press('Enter');

    // Input should be cleared
    await expect(input).toHaveValue('', { timeout: 2000 });
  });
});
