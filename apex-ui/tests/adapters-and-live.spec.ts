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

test.describe('Live safeguards in the real layout', () => {
  test('the kill-switch hotkey works while typing in the strategy editor', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('live-arm-toggle').click();
    await expect(page.getByTestId('live-arm-toggle')).toContainText('armed');
    await page.getByTestId('tab-strategy').click();
    const editor = page.getByTestId('strategy-editor');
    await expect(editor.locator('.monaco-editor')).toBeVisible({ timeout: 15000 });
    await editor.locator('.view-lines').click();
    const textBefore = await editor.locator('.view-lines').innerText();
    // The editor binds Ctrl+Shift+K to "Delete Line" and swallows it before page-level
    // listeners run, so use the real Ctrl chord (not ControlOrMeta, which is Cmd on a Mac).
    await page.keyboard.press('Control+Shift+K');
    await expect(page.getByTestId('live-message')).toContainText('Kill switch');
    await expect(page.getByTestId('live-arm-toggle')).toContainText('off');
    // Compare the text, not a count of rendered rows (the editor virtualizes rows).
    expect(await editor.locator('.view-lines').innerText()).toBe(textBefore);
  });

  test('the live order confirmation is a page-level modal whose button can be clicked', async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.getByTestId('open-settings').click();
    await page.getByTestId('session-tokens-details').locator('summary').click();
    await page.getByTestId('broker-token-zerodha').fill('session-token');
    await page.getByTestId('broker-connect-zerodha').click();
    await expect(page.getByTestId('broker-card-zerodha')).toContainText(/connected/i, { timeout: 5000 });
    await page.getByTestId('settings-close-x').click();
    await page.getByTestId('live-arm-toggle').click();
    await page.getByTestId('order-broker-select').selectOption('zerodha');
    await page.getByTestId('order-symbol-input').fill('RELIANCE.NS');
    await page.getByTestId('order-quantity-input').fill('1');
    await page.getByTestId('order-submit-button').click();
    await expect(page.getByTestId('order-confirm-dialog')).toBeVisible();
    const state = await page.getByTestId('order-confirm-submit').evaluate((button) => {
      const rect = button.getBoundingClientRect();
      const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
      return {
        // A modal belongs to the page, not to whichever grid tile opened it. (We checked: the
        // button was not actually clipped when it lived inside the tile, so this guards the
        // structure and usability rather than a reproduced clipping bug.)
        insideTile: Boolean(button.closest('[data-testid^="tile-"]')),
        clickable: hit === button || button.contains(hit),
        withinViewport: rect.x >= 0 && rect.y >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight,
      };
    });
    expect(state).toEqual({ insideTile: false, clickable: true, withinViewport: true });
    await page.getByTestId('order-confirm-submit').click();
    await expect(page.getByTestId('order-confirmation')).toContainText('Submitted');
  });

  test('a STOP order from the command bar sends its number as the stop trigger, not a limit price', async ({ page }) => {
    await page.addInitScript(() => { (window as unknown as { __ipcLog: unknown[] }).__ipcLog = []; });
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    const input = page.getByTestId('command-bar-input');
    await input.click();
    await input.fill('SELL AAPL 5 STOP 300');
    await input.press('Enter');
    await expect(page.getByTestId('command-bar-feedback')).toContainText('Order placed');
    const request = await page.evaluate(() => {
      const log = (window as unknown as { __ipcLog: { cmd: string; args: { request: Record<string, unknown> } }[] }).__ipcLog;
      return log.filter((entry) => entry.cmd === 'place_order').pop()?.args.request;
    });
    expect(request).toMatchObject({ symbol: 'AAPL', side: 'Sell', order_type: 'Stop', quantity: 5, price: null, stop_price: 300 });
  });
});
