import { test, expect } from '@playwright/test';

async function openSettings(page: import('@playwright/test').Page) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  await page.getByTestId('open-settings').click();
  await expect(page.getByTestId('adapter-manager')).toBeVisible();
}

test.describe('Adapter manager', () => {
  test('lists adapters from their schemas', async ({ page }) => {
    await openSettings(page);
    await expect(page.getByTestId('adapter-card-zerodha')).toBeVisible();
    await expect(page.getByTestId('adapter-card-binance')).toBeVisible();
    await expect(page.getByTestId('adapter-card-llm:copilot')).toBeVisible();
  });

  test('saving a key stores it without ever showing it again', async ({ page }) => {
    await openSettings(page);
    const key = page.getByTestId('adapter-field-zerodha-api_key');
    await expect(page.getByTestId('adapter-save-zerodha')).toBeDisabled();
    await key.fill('kite-key-123');
    await page.getByTestId('adapter-field-zerodha-api_secret').fill('kite-secret-456');
    await page.getByTestId('adapter-save-zerodha').click();
    await expect(page.getByTestId('adapter-notice-zerodha')).toContainText('Saved');
    await expect(key).toHaveValue('');
    await expect(key).toHaveAttribute('placeholder', /saved/);
    // The daily-login helper appears once both credentials exist.
    await expect(page.getByTestId('zerodha-open-login')).toBeVisible();
  });

  test('remove clears the stored credentials', async ({ page }) => {
    await openSettings(page);
    await page.getByTestId('adapter-field-binance-api_key').fill('b-key');
    await page.getByTestId('adapter-field-binance-api_secret').fill('b-secret');
    await page.getByTestId('adapter-save-binance').click();
    await expect(page.getByTestId('adapter-notice-binance')).toContainText('Saved');
    await page.getByTestId('adapter-remove-binance').click();
    await expect(page.getByTestId('adapter-notice-binance')).toContainText('removed');
    await expect(page.getByTestId('adapter-field-binance-api_key')).toHaveAttribute('placeholder', /Required/);
  });

  test('locking the vault disables editing', async ({ page }) => {
    await openSettings(page);
    await page.getByTestId('vault-lock').click();
    await expect(page.getByTestId('vault-state')).toContainText('locked');
    await expect(page.getByTestId('adapter-field-zerodha-api_key')).toBeDisabled();
  });
});

test.describe('Live trading safeguards', () => {
  test('live orders need arming and an explicit confirmation', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('open-settings').click();
    await page.getByTestId('broker-token-zerodha').fill('session-token');
    await page.getByTestId('broker-connect-zerodha').click();
    await expect(page.getByTestId('broker-card-zerodha')).toContainText(/connected/i, { timeout: 5000 });
    await page.keyboard.press('Escape');
    await page.getByRole('button', { name: 'Close' }).last().click().catch(() => undefined);

    await page.getByTestId('order-broker-select').selectOption('zerodha');
    await page.getByTestId('order-quantity-input').fill('1');
    await page.getByTestId('order-symbol-input').fill('RELIANCE.NS');
    await expect(page.getByTestId('order-product-select')).toBeVisible();

    // Not armed: the preview refuses to issue a confirmation.
    await page.getByTestId('order-submit-button').click();
    await expect(page.getByTestId('order-error-message')).toContainText('not armed');
    await expect(page.getByTestId('order-confirm-dialog')).toHaveCount(0);

    // Arm, then the confirmation dialog appears and the order goes through.
    await page.getByTestId('live-arm-toggle').click();
    await expect(page.getByTestId('live-arm-toggle')).toContainText('armed');
    await page.getByTestId('order-submit-button').click();
    await expect(page.getByTestId('order-confirm-dialog')).toBeVisible();
    await page.getByTestId('order-confirm-submit').click();
    await expect(page.getByTestId('order-confirmation')).toContainText('Submitted');
  });

  test('the kill switch disarms live trading', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('live-arm-toggle').click();
    await expect(page.getByTestId('live-arm-toggle')).toContainText('armed');
    await page.getByTestId('kill-switch').click();
    await expect(page.getByTestId('live-message')).toContainText('Kill switch');
    await expect(page.getByTestId('live-arm-toggle')).toContainText('off');
  });
});
