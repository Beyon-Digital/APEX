import { test, expect } from '@playwright/test';

test.describe('Symbol entry', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
  });

  test('search suggests matches and Enter adds the highlighted one', async ({ page }) => {
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').fill('sbi');
    await expect(page.getByTestId('symbol-search-option-SBIN.NS')).toBeVisible();
    await page.getByTestId('watchlist-symbol-input').press('Enter');
    await expect(page.getByTestId('watchlist-item-SBIN.NS')).toBeVisible();
  });

  test('clicking a suggestion adds it', async ({ page }) => {
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').fill('tesla');
    await page.getByTestId('symbol-search-option-TSLA').click();
    await expect(page.getByTestId('watchlist-item-TSLA')).toBeVisible();
  });

  test('a duplicate symbol is rejected with a message', async ({ page }) => {
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').fill('TCS.NS');
    await page.getByTestId('watchlist-symbol-input').press('Enter');
    await expect(page.getByTestId('watchlist-add-status')).toContainText('already');
  });

  test('command bar ADD and RM edit the watchlist', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('ADD MSTR');
    await input.press('Enter');
    await expect(page.getByTestId('watchlist-item-MSTR')).toBeVisible({ timeout: 8000 });

    await input.click();
    await input.fill('RM MSTR');
    await input.press('Enter');
    await expect(page.getByTestId('watchlist-item-MSTR')).toHaveCount(0);
  });

  test('Ctrl+K focuses the command bar even while typing elsewhere', async ({ page }) => {
    await page.getByTestId('watchlist-add-button').click();
    await page.getByTestId('watchlist-symbol-input').focus();
    await page.keyboard.press('Control+k');
    await expect(page.getByTestId('command-bar-input')).toBeFocused();
  });

  test('colon prefix suggests panels and Tab completes', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill(':HEA');
    await expect(page.getByTestId('command-bar-suggestions')).toContainText(':HEALTH');
    await input.press('Tab');
    await expect(input).toHaveValue(':HEALTH');
  });

  test('unknown panels report an error', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill(':NOPE');
    await input.press('Enter');
    await expect(page.getByTestId('command-bar-feedback')).toContainText('Unknown panel');
  });

  test('ArrowUp recalls the previous command', async ({ page }) => {
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill(':NEWS');
    await input.press('Enter');
    await input.click();
    await input.press('ArrowUp');
    await expect(input).toHaveValue(':NEWS');
  });
});
