import { test, expect } from '@playwright/test';

test.describe('Broker connectivity settings', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
  });

  test('should connect a live broker and use it as the active execution target', async ({ page }) => {
    await page.getByTestId('open-settings').click();

    await expect(page.getByTestId('settings-panel')).toBeVisible({ timeout: 5000 });
    // Session tokens live under the Accounts tab, in an advanced section.
    await page.getByTestId('session-tokens-details').locator('summary').click();
    await expect(page.getByTestId('broker-settings-section')).toBeVisible();

    await page.getByTestId('broker-token-zerodha').fill('mock-zerodha-session-token');
    await page.getByTestId('broker-connect-zerodha').click();

    const zerodhaCard = page.getByTestId('broker-card-zerodha');
    await expect(zerodhaCard).toContainText(/connected/i, { timeout: 5000 });

    const activeBrokerSelect = page.getByTestId('active-broker-select');
    await activeBrokerSelect.selectOption('zerodha');
    await expect(activeBrokerSelect).toHaveValue('zerodha');

    await page.getByText('Close', { exact: true }).click();

    const orderBrokerSelect = page.getByTestId('order-broker-select');
    await expect(orderBrokerSelect).toHaveValue('zerodha');
  });
});
