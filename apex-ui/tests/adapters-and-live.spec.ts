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
    await page.getByTestId('adapter-expand-zerodha').click();
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
    await page.getByTestId('adapter-expand-binance').click();
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
    await page.getByTestId('adapter-expand-zerodha').click();
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
    await page.getByTestId('session-tokens-details').locator('summary').click();
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

test.describe('Settings layout', () => {
  test('is split into short tabs instead of one long page', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('open-settings').click();
    for (const tab of ['accounts', 'data', 'ai', 'trading', 'general']) {
      await expect(page.getByTestId(`settings-tab-${tab}`)).toBeVisible();
    }
    // Only the active tab's content is mounted.
    await expect(page.getByTestId('adapter-manager')).toBeVisible();
    await expect(page.getByTestId('chatgpt-card')).toHaveCount(0);
    await page.getByTestId('settings-tab-ai').click();
    await expect(page.getByTestId('chatgpt-card')).toBeVisible();
    await expect(page.getByTestId('adapter-manager')).toHaveCount(0);
    // The panel itself no longer needs a long scroll to reach anything.
    const overflow = await page.getByTestId('settings-panel').evaluate((el) => el.scrollHeight - el.clientHeight);
    expect(overflow).toBeLessThanOrEqual(1);
  });

  test('the dead placeholder controls are gone', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('open-settings').click();
    await page.getByTestId('settings-tab-trading').click();
    await expect(page.getByTestId('live-summary')).toContainText('OFF');
    await expect(page.getByText('Enable Confirmations')).toHaveCount(0);
    await expect(page.getByText('Default Order Size')).toHaveCount(0);
    await expect(page.getByTestId('settings-max-daily-loss')).toBeVisible();
  });

  test('market data tab explains how to choose a feed per ticker', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('open-settings').click();
    await page.getByTestId('settings-tab-data').click();
    await expect(page.getByTestId('settings-market-data-adapter')).toBeVisible();
    await expect(page.getByTestId('data-feed-yahoo')).toContainText('delayed');
    await expect(page.getByTestId('data-feed-zerodha_kite')).toContainText('Not connected');
  });
});

test.describe('ChatGPT plan sign-in', () => {
  test('signs in, then lets the copilot use the plan and pick a model', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('open-settings').click();
    await page.getByTestId('settings-tab-ai').click();
    await expect(page.getByTestId('chatgpt-status')).toContainText('signed out');
    await page.getByTestId('chatgpt-sign-in').click();
    await expect(page.getByTestId('chatgpt-status')).toContainText('connected', { timeout: 8000 });
    // A model is chosen automatically from the account's list.
    await expect(page.getByTestId('chatgpt-model')).toHaveValue('gpt-demo', { timeout: 8000 });
    await page.getByTestId('chatgpt-use-plan').check();
    await expect(page.getByTestId('chatgpt-use-plan')).toBeChecked();
    await page.getByTestId('chatgpt-model').selectOption('gpt-demo-mini');
    await expect(page.getByTestId('chatgpt-model')).toHaveValue('gpt-demo-mini');
    await page.getByTestId('chatgpt-sign-out').click();
    await expect(page.getByTestId('chatgpt-status')).toContainText('signed out');
  });
});

test.describe('Per-ticker data feed', () => {
  test('the SRC badge opens a picker that explains unavailable feeds and can pin one', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    const badge = page.getByTestId('feed-badge-TCS.NS');
    await expect(badge).toBeVisible();
    await badge.click();
    await expect(page.getByTestId('feed-menu')).toBeVisible();
    // Zerodha covers the symbol but is not connected, so it is shown disabled with the reason.
    await expect(page.getByTestId('feed-option-zerodha_kite')).toBeDisabled();
    await expect(page.getByTestId('feed-option-zerodha_kite')).toContainText('Not connected');
    await page.getByTestId('feed-option-yahoo').click();
    await expect(page.getByTestId('feed-menu')).toHaveCount(0);
    await expect(badge).toHaveAttribute('title', /pinned/);
    // Back to automatic.
    await badge.click();
    await page.getByTestId('feed-auto').click();
    await expect(badge).toHaveAttribute('title', /automatic/);
  });
});
