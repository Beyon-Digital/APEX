import { test, expect, type Page } from '@playwright/test';

// Things the Copilot (or anything else) creates must show up in the open panels without a reload.
// The web mock exposes window.__apexMock to act as the Copilot.

type Mock = {
  createStrategyAsCopilot: (name?: string) => string;
  writeStrategyAsCopilot: (name: string, content: string) => string;
  createAutomationAsCopilot: (name?: string, modelId?: string) => string;
  trainModelAsCopilot: (algorithm?: string) => string;
};

const copilot = <K extends keyof Mock>(page: Page, fn: K, ...args: Parameters<Mock[K]>) =>
  page.evaluate(
    ([name, a]) => (window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[name as string](...(a as unknown[])),
    [fn, args] as const,
  );

async function openIde(page: Page) {
  await page.goto('/');
  await page.getByTestId('tab-strategy').click();
  await expect(page.getByTestId('strategy-editor').locator('.monaco-editor')).toBeVisible({ timeout: 15000 });
}

async function typeInEditor(page: Page, text: string) {
  const editor = page.getByTestId('strategy-editor');
  await editor.locator('.view-lines').click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type(text);
  await expect(editor.locator('.view-lines')).toContainText(text.replace('\n', '').trim());
}

const lines = (page: Page) => page.getByTestId('strategy-editor').locator('.view-lines');

test.describe('Strategy IDE follows the Copilot', () => {
  test('a new file appears without reload, carries a Copilot badge, and the badge clears when opened', async ({ page }) => {
    await openIde(page);
    await expect(page.getByTestId('file-from_copilot.py')).toHaveCount(0);
    await copilot(page, 'createStrategyAsCopilot', 'from_copilot.py');

    await expect(page.getByTestId('file-from_copilot.py')).toBeVisible({ timeout: 3000 });
    await expect(page.getByTestId('copilot-badge-from_copilot.py')).toBeVisible();
    // Selection is unchanged and the other file has no badge.
    await expect(page.getByTestId('file-my_strategy.py')).toHaveAttribute('aria-current', 'true');
    await expect(page.getByTestId('copilot-badge-my_strategy.py')).toHaveCount(0);

    await page.getByTestId('file-from_copilot.py').click();
    await expect(page.getByTestId('file-from_copilot.py')).toHaveAttribute('aria-current', 'true');
    await expect(page.getByTestId('copilot-badge-from_copilot.py')).toHaveCount(0);
  });

  test('a clean open file reloads silently when the Copilot rewrites it', async ({ page }) => {
    await openIde(page);
    await expect(lines(page)).toContainText('Strategy');
    await copilot(page, 'writeStrategyAsCopilot', 'my_strategy.py', '# rewritten by copilot\n');
    await expect(lines(page)).toContainText('rewritten by copilot', { timeout: 3000 });
    await expect(page.getByTestId('copilot-changed-banner')).toHaveCount(0);
  });

  test('a file with unsaved edits is never overwritten: banner, Keep mine, then Reload', async ({ page }) => {
    await openIde(page);
    await typeInEditor(page, '\n# my unsaved edit');
    await copilot(page, 'writeStrategyAsCopilot', 'my_strategy.py', '# rewritten by copilot\n');

    const banner = page.getByTestId('copilot-changed-banner');
    await expect(banner).toBeVisible({ timeout: 3000 });
    await expect(banner).toContainText('Changed by Copilot');
    await expect(lines(page)).toContainText('my unsaved edit');
    await expect(lines(page)).not.toContainText('rewritten by copilot');

    await page.getByTestId('copilot-changed-keep').click();
    await expect(banner).toHaveCount(0);
    await expect(lines(page)).toContainText('my unsaved edit');

    // Another change from the Copilot raises the banner again; this time take theirs.
    await copilot(page, 'writeStrategyAsCopilot', 'my_strategy.py', '# second rewrite\n');
    await expect(banner).toBeVisible({ timeout: 3000 });
    await expect(lines(page)).toContainText('my unsaved edit');
    await page.getByTestId('copilot-changed-reload').click();
    await expect(banner).toHaveCount(0);
    await expect(lines(page)).toContainText('second rewrite');
    await expect(lines(page)).not.toContainText('my unsaved edit');
  });

  test('unrelated changes keep the selection and the unsaved edits, and show no banner', async ({ page }) => {
    await openIde(page);
    await typeInEditor(page, '\n# edit in progress');
    await copilot(page, 'createStrategyAsCopilot', 'unrelated.py');
    await expect(page.getByTestId('file-unrelated.py')).toBeVisible({ timeout: 3000 });
    await expect(page.getByTestId('file-my_strategy.py')).toHaveAttribute('aria-current', 'true');
    await expect(page.getByTestId('copilot-changed-banner')).toHaveCount(0);
    await expect(lines(page)).toContainText('edit in progress');
    // Typing continues where the cursor was.
    await page.keyboard.type(' more');
    await expect(lines(page)).toContainText('edit in progress more');
  });

  test('the user saving a file does not raise the banner', async ({ page }) => {
    await openIde(page);
    await typeInEditor(page, '\n# saved by me');
    await page.getByTestId('save-strategy').click();
    await expect(page.getByTestId('save-confirmation')).toBeVisible();
    await page.waitForTimeout(400);
    await expect(page.getByTestId('copilot-changed-banner')).toHaveCount(0);
    await expect(lines(page)).toContainText('saved by me');
  });

  test('the Backtest strategy list refreshes', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-backtest').click();
    const select = page.getByTestId('backtest-strategy-select');
    await expect(select.locator('option')).toHaveCount(1);
    await copilot(page, 'createStrategyAsCopilot', 'bt_new.py');
    await expect(select.locator('option')).toHaveCount(2, { timeout: 3000 });
    await expect(select).toContainText('bt_new.py');
  });
});

test.describe('Automations follow the Copilot', () => {
  test('a rule created by the Copilot appears within seconds with a badge and an Open model button', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-automations').click();
    await expect(page.getByTestId('automations-empty')).toBeVisible();
    // Faster than the 5 s status poll, so only the artifact counter can explain it.
    await copilot(page, 'createAutomationAsCopilot', 'Copilot rule', 'model_x');
    const row = page.getByTestId('automation-row');
    await expect(row).toHaveCount(1, { timeout: 2500 });
    await expect(row).toContainText('Copilot rule');
    await expect(row.getByTestId('automation-copilot-badge')).toBeVisible();

    await row.getByTestId('automation-open-model').click();
    await expect(page.getByTestId('ml-workbench')).toBeVisible();
  });

  test('the model dropdown picks up a model trained by the Copilot', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-automations').click();
    const select = page.getByTestId('automation-model');
    await expect(select.locator('option')).toHaveCount(1);
    const modelId = (await copilot(page, 'trainModelAsCopilot', 'random_forest')) as string;
    await expect(select.locator(`option[value="${modelId}"]`)).toHaveCount(1, { timeout: 2500 });
  });

  test('opening an automation from elsewhere scrolls to and highlights its row', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-chart').click();
    const id = (await copilot(page, 'createAutomationAsCopilot', 'Target rule', 'model_x')) as string;
    await page.evaluate(async (ruleId) => {
      const nav = await import('/src/stores/navStore.ts');
      nav.openItem('automation', ruleId);
    }, id);
    const row = page.locator(`[data-testid="automation-row"][data-rule-id="${id}"]`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-highlighted', 'true');
    await expect(row).toHaveAttribute('data-highlighted', 'false', { timeout: 6000 });
  });
});
