import { test, expect, type Page } from '@playwright/test';

/**
 * ML workbench shell and dataset builder (web mode: the Rust side is replaced by lib/mockMl.ts).
 * Magic dataset names understood by the mock: "boom" fails the build, "skew" gives a 5/90/5 class split.
 */

async function openWorkbench(page: Page) {
  await page.addInitScript(() => { (window as unknown as { __ipcLog: unknown[] }).__ipcLog = []; });
  await page.goto('/');
  await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  await page.getByTestId('tab-ml').click();
  await expect(page.getByTestId('ml-workbench')).toBeVisible({ timeout: 10000 });
}

async function addSymbol(page: Page, symbol: string) {
  await page.getByTestId('ml-ds-symbol-input').fill(symbol);
  await page.getByTestId('ml-ds-symbol-input').press('Enter');
  await expect(page.getByTestId(`ml-ds-symbol-chip-${symbol}`)).toBeVisible();
}

const ipc = (page: Page, cmd: string) =>
  page.evaluate((c) => ((window as unknown as { __ipcLog: { cmd: string; args: any }[] }).__ipcLog).filter((e) => e.cmd === c), cmd);

test.describe('ML workbench shell', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); });

  test('has Data, Train and Models tabs and opens on Data', async ({ page }) => {
    for (const id of ['data', 'train', 'models']) await expect(page.getByTestId(`ml-tab-${id}`)).toBeVisible();
    await expect(page.getByTestId('ml-tab-registry')).toHaveCount(0);
    await expect(page.getByTestId('ml-dataset-builder')).toBeVisible();
    await expect(page.getByTestId('ml-training-form')).toBeHidden();
  });

  test('switching tabs shows the matching panel', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-training-form')).toBeVisible();
    await expect(page.getByTestId('ml-dataset-builder')).toBeHidden();
    await page.getByTestId('ml-tab-models').click();
    await expect(page.getByTestId('ml-models-tab')).toBeVisible();
    await expect(page.getByTestId('ml-training-form')).toBeHidden();
    await page.getByTestId('ml-tab-data').click();
    await expect(page.getByTestId('ml-dataset-builder')).toBeVisible();
  });

  test('keeps a half-filled dataset form when switching tabs', async ({ page }) => {
    await addSymbol(page, 'MSFT');
    await page.getByTestId('ml-ds-bars').fill('777');
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-tab-data').click();
    await expect(page.getByTestId('ml-ds-symbol-chip-MSFT')).toBeVisible();
    await expect(page.getByTestId('ml-ds-bars')).toHaveValue('777');
  });

  test('navigating to a model opens the Models tab', async ({ page }) => {
    await page.evaluate(() => (window as any).__apexMlTest.open('model', 'some_model'));
    await expect(page.getByTestId('ml-models-tab')).toBeVisible();
  });

  test('model count on the Models tab follows models the Copilot trains', async ({ page }) => {
    await expect(page.getByTestId('ml-tab-models')).toHaveText('Models (0)');
    await page.evaluate(() => (window as any).__apexMock.trainModelAsCopilot('xgboost'));
    await expect(page.getByTestId('ml-tab-models')).toHaveText('Models (1)');
  });
});

test.describe('ML dataset builder', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); });

  test('builds a dataset from the UI and shows the result card and preview', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await page.getByTestId('ml-ds-timeframe').selectOption('h1');
    await page.getByTestId('ml-ds-bars').fill('1000');
    await expect(page.getByTestId('ml-ds-name')).toHaveValue('aapl_h1_h5');
    await page.getByTestId('ml-ds-build').click();

    const card = page.getByTestId('ml-ds-result');
    await expect(card).toBeVisible();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('aapl_h1_h5');
    await expect(page.getByTestId('ml-ds-result-rows')).toHaveText('935'); // mock: 1000 - 60 - 5
    await expect(page.getByTestId('ml-ds-result-range')).toContainText('2024-01-02 to 2026-01-02');
    await expect(page.getByTestId('ml-ds-class-0')).toContainText('Down');
    await expect(page.getByTestId('ml-ds-class-1')).toContainText('Up');
    await expect(page.getByTestId('ml-ds-balance-warn')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-preview-table')).toBeVisible();
    await expect(page.getByTestId('ml-ds-preview-row')).toHaveCount(12);
    await expect(page.getByTestId('ml-ds-preview-table')).toContainText('rsi_14');
    await expect(page.getByTestId('ml-dataset-aapl_h1_h5')).toBeVisible();

    const calls = await ipc(page, 'build_dataset');
    expect(calls).toHaveLength(1);
    expect(calls[0].args.request).toMatchObject({
      name: 'aapl_h1_h5', symbols: ['AAPL'], timeframe: 'h1', bars: 1000,
      label: { horizon_bars: 5, kind: 'direction', threshold_pct: 0 },
    });
    expect(calls[0].args.request.features).toHaveLength(15);
  });

  test('warns when one class is rare', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await page.getByTestId('ml-ds-label-kind').selectOption('return_threshold');
    await page.getByTestId('ml-ds-name').fill('skew-test');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-class--1')).toContainText('Down');
    await expect(page.getByTestId('ml-ds-class-0')).toContainText('Flat');
    await expect(page.getByTestId('ml-ds-balance-warn')).toContainText('under 15%');
    const req = (await ipc(page, 'build_dataset'))[0].args.request;
    expect(req.label).toEqual({ horizon_bars: 5, kind: 'return_threshold', threshold_pct: 0.5 });
  });

  test('shows backend build errors inline and lets the user retry', async ({ page }) => {
    await addSymbol(page, 'ZZZZ');
    await page.getByTestId('ml-ds-name').fill('boom');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-error')).toContainText('No market data available for ZZZZ');
    await expect(page.getByTestId('ml-ds-result')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled();
  });

  test('blocks the build until symbols, features, label and name are valid', async ({ page }) => {
    const build = page.getByTestId('ml-ds-build');
    await expect(build).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Add at least one symbol');

    await addSymbol(page, 'AAPL');
    await expect(build).toBeEnabled();
    await expect(page.getByTestId('ml-ds-validation')).toHaveCount(0);

    // features
    await page.getByTestId('ml-ds-preset-none').click();
    await expect(build).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Select at least one feature');
    await page.getByTestId('ml-ds-preset-everything').click();
    await expect(build).toBeEnabled();

    // label
    await page.getByTestId('ml-ds-horizon').fill('0');
    await expect(build).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Horizon must be a whole number from 1 to 100');
    await page.getByTestId('ml-ds-horizon').fill('5');
    await page.getByTestId('ml-ds-label-kind').selectOption('return_threshold');
    await page.getByTestId('ml-ds-threshold').fill('0');
    await expect(build).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Threshold must be above 0%');
    await page.getByTestId('ml-ds-threshold').fill('1');
    await expect(build).toBeEnabled();

    // bars
    await page.getByTestId('ml-ds-bars').fill('50');
    await expect(build).toBeDisabled();
    await page.getByTestId('ml-ds-bars').fill('500');

    // name rule: [a-z0-9_-], max 64
    await page.getByTestId('ml-ds-name').fill('Bad Name');
    await expect(page.getByTestId('ml-ds-name-error')).toContainText('lowercase');
    await expect(build).toBeDisabled();
    await page.getByTestId('ml-ds-name').fill('a'.repeat(65));
    await expect(page.getByTestId('ml-ds-name-error')).toContainText('too long');
    await page.getByTestId('ml-ds-name').fill('a'.repeat(64));
    await expect(page.getByTestId('ml-ds-name-error')).toHaveCount(0);
    await expect(build).toBeEnabled();
    await page.getByTestId('ml-ds-name').fill('');
    await expect(page.getByTestId('ml-ds-name-error')).toContainText('Give the dataset a name');
    await page.getByTestId('ml-ds-name-suggest').click();
    await expect(page.getByTestId('ml-ds-name')).toHaveValue('aapl_d1_h5');
    await expect(build).toBeEnabled();
  });

  test('estimates rows from bars, symbols, warm-up and horizon, and warns when thin', async ({ page }) => {
    const rows = page.getByTestId('ml-ds-estimated-rows');
    await addSymbol(page, 'AAPL');
    await page.getByTestId('ml-ds-bars').fill('500');
    await expect(rows).toHaveText('445'); // 500 - 50 (sma_ratio_50 warm-up) - 5
    await expect(page.getByTestId('ml-ds-warn-rows')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-warn-features')).toHaveCount(0);

    await addSymbol(page, 'MSFT');
    await expect(rows).toHaveText('890');

    await page.getByTestId('ml-ds-symbol-remove-MSFT').click();
    await expect(rows).toHaveText('445');

    await page.getByTestId('ml-ds-horizon').fill('20');
    await expect(rows).toHaveText('430');

    await page.getByTestId('ml-ds-horizon').fill('5');
    await page.getByTestId('ml-ds-preset-none').click();
    await page.getByTestId('ml-ds-feature-rsi_14').check(); // warm-up 14 only
    await expect(rows).toHaveText('481');

    // thin data: 200 bars, everything selected
    await page.getByTestId('ml-ds-preset-everything').click();
    await page.getByTestId('ml-ds-bars').fill('200');
    await expect(rows).toHaveText('145');
    await expect(page.getByTestId('ml-ds-warn-rows')).toContainText('Under 300 rows');
    await expect(page.getByTestId('ml-ds-warn-features')).toContainText('15 features');

    // enough rows overall but too many features for them
    await page.getByTestId('ml-ds-bars').fill('350');
    await expect(rows).toHaveText('295');
    await expect(page.getByTestId('ml-ds-warn-rows')).toBeVisible();
    await page.getByTestId('ml-ds-bars').fill('500');
    await expect(page.getByTestId('ml-ds-warn-rows')).toHaveCount(0);
  });

  test('feature presets, per-group select-all and search', async ({ page }) => {
    const count = page.getByTestId('ml-ds-feature-count');
    await expect(count).toHaveText('15 of 15 selected');

    await page.getByTestId('ml-ds-preset-trend').click();
    await expect(count).toHaveText('5 of 15 selected');
    await expect(page.getByTestId('ml-ds-feature-sma_ratio_50')).toBeChecked();
    await expect(page.getByTestId('ml-ds-feature-rsi_14')).not.toBeChecked();

    await page.getByTestId('ml-ds-preset-momentum').click();
    await expect(count).toHaveText('5 of 15 selected');
    await expect(page.getByTestId('ml-ds-feature-rsi_14')).toBeChecked();
    await expect(page.getByTestId('ml-ds-feature-sma_ratio_50')).not.toBeChecked();

    await page.getByTestId('ml-ds-preset-volatility').click();
    await expect(page.getByTestId('ml-ds-feature-atr_pct_14')).toBeChecked();

    await page.getByTestId('ml-ds-preset-none').click();
    await expect(count).toHaveText('0 of 15 selected');
    await page.getByTestId('ml-ds-group-all-Volume').click();
    await expect(page.getByTestId('ml-ds-feature-vol_ratio_20')).toBeChecked();
    await expect(count).toHaveText('1 of 15 selected');
    await page.getByTestId('ml-ds-group-all-Volume').click(); // second click clears the group
    await expect(count).toHaveText('0 of 15 selected');

    await page.getByTestId('ml-ds-group-all-Momentum').click();
    await expect(count).toHaveText('3 of 15 selected');

    await page.getByTestId('ml-ds-feature-search').fill('bollinger');
    await expect(page.getByTestId('ml-ds-feature-bb_pos_20')).toBeVisible();
    await expect(page.getByTestId('ml-ds-feature-rsi_14')).toHaveCount(0);
    await page.getByTestId('ml-ds-feature-search').fill('zzz');
    await expect(page.getByTestId('ml-ds-feature-nomatch')).toBeVisible();
    await page.getByTestId('ml-ds-feature-search').fill('');

    await page.getByTestId('ml-ds-preset-everything').click();
    await expect(count).toHaveText('15 of 15 selected');
  });

  test('symbols are removable chips and duplicates are ignored', async ({ page }) => {
    await page.getByTestId('ml-ds-symbol-input').fill('aapl'); // stored upper-case
    await page.getByTestId('ml-ds-symbol-add').click();
    await expect(page.getByTestId('ml-ds-symbol-chip-AAPL')).toBeVisible();
    await page.getByTestId('ml-ds-symbol-input').fill('AAPL');
    await page.getByTestId('ml-ds-symbol-add').click();
    await addSymbol(page, 'BTC-USD');
    await expect(page.locator('[data-testid^="ml-ds-symbol-chip-"]')).toHaveCount(2);
    await expect(page.getByTestId('ml-ds-name')).toHaveValue('aapl_btc-usd_d1_h5');
    await page.getByTestId('ml-ds-symbol-remove-AAPL').click();
    await expect(page.getByTestId('ml-ds-symbol-chip-AAPL')).toHaveCount(0);
  });

  test('lists, selects, previews and deletes datasets (with confirmation)', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await page.getByTestId('ml-ds-name').fill('first');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-dataset-first')).toBeVisible();
    await page.getByTestId('ml-ds-name').fill('second');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('second');

    await page.getByTestId('ml-ds-select-first').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('first');
    await expect(page.getByTestId('ml-ds-preview-row')).toHaveCount(12);

    await page.getByTestId('ml-ds-delete-first').click();
    await page.getByTestId('ml-ds-delete-cancel-first').click();
    await expect(page.getByTestId('ml-dataset-first')).toBeVisible();

    await page.getByTestId('ml-ds-delete-first').click();
    await page.getByTestId('ml-ds-delete-confirm-first').click();
    await expect(page.getByTestId('ml-dataset-first')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-result')).toHaveCount(0); // the shown dataset was the deleted one
    await expect(page.getByTestId('ml-dataset-second')).toBeVisible();
    expect((await ipc(page, 'delete_dataset')).map((c) => c.args.name)).toEqual(['first']);
  });

  test('navigating to a dataset selects it on the Data tab', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await page.getByTestId('ml-ds-name').fill('nav-target');
    await page.getByTestId('ml-ds-build').click();
    await page.getByTestId('ml-ds-name').fill('other');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('other');
    await page.getByTestId('ml-tab-train').click();
    await page.evaluate(() => (window as any).__apexMlTest.open('dataset', 'nav-target'));
    await expect(page.getByTestId('ml-dataset-builder')).toBeVisible();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('nav-target');
  });
});
