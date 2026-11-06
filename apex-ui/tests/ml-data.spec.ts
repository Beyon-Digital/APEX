import { test, expect, type Page } from '@playwright/test';

/**
 * Data tab: free ticker and period selection, stored-data availability, history fetch,
 * custom feature formulas, label/fee options and re-using a dataset's settings.
 * Web mode: the Rust side is replaced by lib/mockMl.ts. In the mock, AAPL / MSFT / BTC-USD ... have
 * 520 daily bars stored (2024-01-02 to 2026-01-02), any other ticker has none, and symbols starting
 * with NOPE or BAD cannot be fetched.
 */

async function openData(page: Page) {
  await page.addInitScript(() => { (window as unknown as { __ipcLog: unknown[] }).__ipcLog = []; });
  await page.goto('/');
  await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 15000 });
  await page.getByTestId('tab-ml').click();
  await expect(page.getByTestId('ml-dataset-builder')).toBeVisible({ timeout: 15000 });
}

async function addSymbol(page: Page, symbol: string, prefix = 'ml-ds-symbol') {
  await page.getByTestId(`${prefix}-input`).fill(symbol);
  await page.getByTestId(`${prefix}-input`).press('Enter');
  await expect(page.getByTestId(`${prefix}-chip-${symbol}`)).toBeVisible();
}

const ipc = (page: Page, cmd: string) =>
  page.evaluate((c) => ((window as unknown as { __ipcLog: { cmd: string; args: any }[] }).__ipcLog).filter((e) => e.cmd === c), cmd);

const lastBuild = async (page: Page) => {
  const calls = await ipc(page, 'build_dataset');
  return calls[calls.length - 1].args.request;
};

const mock = (page: Page, fn: string, ...args: unknown[]) =>
  page.evaluate(([f, a]) => (window as any).__apexMock[f as string](...(a as unknown[])), [fn, args] as const);

async function addFormula(page: Page, i: number, name: string, expr: string) {
  if ((await page.getByTestId(`ml-cf-row-${i}`).count()) === 0) await page.getByTestId('ml-cf-add').click();
  await page.getByTestId(`ml-cf-name-${i}`).fill(name);
  await page.getByTestId(`ml-cf-expr-${i}`).fill(expr);
}

test.describe('Data tab: symbols and stored data', () => {
  test.beforeEach(async ({ page }) => { await openData(page); });

  test('colours each symbol by what is stored: ok, none and thin', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    const aapl = page.getByTestId('ml-ds-symbol-data-AAPL');
    await expect(aapl).toHaveAttribute('data-status', 'ok');
    await expect(aapl).toHaveText('520 bars, 2024-01-02 to 2026-01-02');
    await expect(page.getByTestId('ml-ds-symbol-fetch-AAPL')).toHaveCount(0);

    await addSymbol(page, 'NEWCO');
    const newco = page.getByTestId('ml-ds-symbol-data-NEWCO');
    await expect(newco).toHaveAttribute('data-status', 'none');
    await expect(newco).toContainText('no d1 data stored');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NEWCO')).toBeVisible();

    // fetching only 200 bars leaves it thin (under 300), and it can still be topped up
    await page.getByTestId('ml-ds-bars').fill('200');
    await page.getByTestId('ml-ds-symbol-fetch-NEWCO').click();
    await expect(newco).toHaveAttribute('data-status', 'thin');
    await expect(newco).toContainText('200 bars');
    await expect(newco).toContainText('thin');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NEWCO')).toBeVisible();

    await page.getByTestId('ml-ds-bars').fill('1000');
    await page.getByTestId('ml-ds-symbol-fetch-NEWCO').click();
    await expect(newco).toHaveAttribute('data-status', 'ok');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NEWCO')).toHaveCount(0);
  });

  test('fetch history shows progress, sends symbol, timeframe and bars, then refreshes the row', async ({ page }) => {
    await addSymbol(page, 'NEWCO');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NEWCO')).toHaveText('Fetch history (1000 bars)');
    await mock(page, 'delayNext', 'fetch_symbol_history', 700);
    await page.getByTestId('ml-ds-symbol-fetch-NEWCO').click();

    await expect(page.getByTestId('ml-ds-symbol-fetching-NEWCO')).toContainText('Downloading 1000 d1 bars for NEWCO');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NEWCO')).toBeDisabled();
    await expect(page.getByTestId('ml-ds-symbol-data-NEWCO')).toHaveAttribute('data-status', 'none'); // not yet

    await expect(page.getByTestId('ml-ds-symbol-data-NEWCO')).toHaveAttribute('data-status', 'ok');
    await expect(page.getByTestId('ml-ds-symbol-data-NEWCO')).toHaveAttribute('data-bars', '1000');
    await expect(page.getByTestId('ml-ds-symbol-fetching-NEWCO')).toHaveCount(0);

    const fetches = await ipc(page, 'fetch_symbol_history');
    expect(fetches).toHaveLength(1);
    expect(fetches[0].args).toMatchObject({ symbol: 'NEWCO', timeframe: 'd1', bars: 1000 });
    // the row was refreshed from storage after the download
    const lookups = await ipc(page, 'list_symbol_data');
    expect(lookups[lookups.length - 1].args).toMatchObject({ symbols: ['NEWCO'], timeframe: 'd1' });
  });

  test('caps the history download at 5000 bars and says so', async ({ page }) => {
    await addSymbol(page, 'NEWCO');
    await page.getByTestId('ml-ds-bars').fill('20000');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NEWCO')).toHaveText('Fetch history (5000 bars)');
    await expect(page.getByTestId('ml-ds-symbol-fetch-cap-NEWCO')).toContainText('Only the most recent 5000 bars can be downloaded for this timeframe');
    await page.getByTestId('ml-ds-symbol-fetch-NEWCO').click();
    await expect(page.getByTestId('ml-ds-symbol-data-NEWCO')).toHaveAttribute('data-bars', '5000');
    expect((await ipc(page, 'fetch_symbol_history'))[0].args.bars).toBe(5000);
    // a size within the limit has no note
    await addSymbol(page, 'OTHERCO');
    await page.getByTestId('ml-ds-bars').fill('3000');
    await expect(page.getByTestId('ml-ds-symbol-fetch-OTHERCO')).toHaveText('Fetch history (3000 bars)');
    await expect(page.getByTestId('ml-ds-symbol-fetch-cap-OTHERCO')).toHaveCount(0);
  });

  test('shows a fetch failure inline and leaves the row without data', async ({ page }) => {
    await addSymbol(page, 'NOPE1');
    await page.getByTestId('ml-ds-symbol-fetch-NOPE1').click();
    await expect(page.getByTestId('ml-ds-symbol-fetch-error-NOPE1')).toContainText('No history available for NOPE1');
    await expect(page.getByTestId('ml-ds-symbol-data-NOPE1')).toHaveAttribute('data-status', 'none');
    await expect(page.getByTestId('ml-ds-symbol-fetch-NOPE1')).toBeEnabled();
    // a retry clears the old error before it runs
    await page.getByTestId('ml-ds-symbol-remove-NOPE1').click();
    await expect(page.getByTestId('ml-ds-symbol-fetch-error-NOPE1')).toHaveCount(0);
  });

  test('looks the data up again when the timeframe changes', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await expect(page.getByTestId('ml-ds-symbol-data-AAPL')).toHaveAttribute('data-status', 'ok');
    await page.getByTestId('ml-ds-symbol-fetch-AAPL').count();
    await page.getByTestId('ml-ds-timeframe').selectOption('h1');
    await expect.poll(async () => (await ipc(page, 'list_symbol_data')).some((c) => c.args.timeframe === 'h1')).toBe(true);
  });

  test('context symbols are a separate list, show their data and are not dataset rows', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await addSymbol(page, 'SPYX', 'ml-ds-ctx');
    await expect(page.getByTestId('ml-ds-ctx-data-SPYX')).toHaveAttribute('data-status', 'none');
    await page.getByTestId('ml-ds-ctx-fetch-SPYX').click();
    await expect(page.getByTestId('ml-ds-ctx-data-SPYX')).toHaveAttribute('data-status', 'ok');
    // only AAPL counts toward the rows
    await page.getByTestId('ml-ds-bars').fill('500');
    await expect(page.getByTestId('ml-ds-estimated-rows')).toHaveText('445');
    await expect(page.locator('[data-testid^="ml-ds-symbol-chip-"]')).toHaveCount(1);
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toBeVisible();
    const req = await lastBuild(page);
    expect(req.symbols).toEqual(['AAPL']);
    expect(req.context_symbols).toEqual(['SPYX']);
    await expect(page.getByTestId('ml-ds-result-context')).toHaveText('SPYX');
    await page.getByTestId('ml-ds-ctx-remove-SPYX').click();
    await expect(page.getByTestId('ml-ds-ctx-chip-SPYX')).toHaveCount(0);
  });
});

test.describe('Data tab: period', () => {
  test.beforeEach(async ({ page }) => { await openData(page); await addSymbol(page, 'AAPL'); });

  test('validates the date range and blocks the build until it is valid', async ({ page }) => {
    const build = page.getByTestId('ml-ds-build');
    await expect(build).toBeEnabled();
    await page.getByTestId('ml-ds-period-range').check();
    await expect(page.getByTestId('ml-ds-bars')).toHaveCount(0);
    await expect(build).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Choose both a from and a to date');

    await page.getByTestId('ml-ds-from').fill('2025-06-01');
    await page.getByTestId('ml-ds-to').fill('2025-01-01');
    await expect(page.getByTestId('ml-ds-range-error')).toContainText('from date must be before the to date');
    await expect(build).toBeDisabled();

    await page.getByTestId('ml-ds-to').fill('2025-06-01'); // equal is not a range either
    await expect(page.getByTestId('ml-ds-range-error')).toBeVisible();

    await page.getByTestId('ml-ds-to').fill('2025-12-01');
    await expect(page.getByTestId('ml-ds-range-error')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-validation')).toHaveCount(0);
    await expect(build).toBeEnabled();

    await page.getByTestId('ml-ds-period-bars').check(); // switching back needs no dates
    await expect(page.getByTestId('ml-ds-bars')).toBeVisible();
    await expect(build).toBeEnabled();
  });

  test('warns when the range is outside the stored data and counts only stored bars in it', async ({ page }) => {
    await page.getByTestId('ml-ds-period-range').check();
    await page.getByTestId('ml-ds-from').fill('2024-07-01');
    await page.getByTestId('ml-ds-to').fill('2025-07-01');
    // inside 2024-01-02..2026-01-02: no warning. 366 of 732 days of the 520 stored bars = 260, minus 50 warm-up and 5 horizon
    await expect(page.getByTestId('ml-ds-range-warn-AAPL')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-estimated-rows')).toHaveText('205');

    await page.getByTestId('ml-ds-from').fill('2023-06-01');
    await expect(page.getByTestId('ml-ds-range-warn-AAPL')).toHaveAttribute('data-coverage', 'partial');
    await expect(page.getByTestId('ml-ds-range-warn-AAPL')).toContainText('reaches outside the stored data (2024-01-02 to 2026-01-02)');
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled(); // a warning, not a block

    await page.getByTestId('ml-ds-from').fill('2027-01-01');
    await page.getByTestId('ml-ds-to').fill('2027-06-01');
    await expect(page.getByTestId('ml-ds-range-warn-AAPL')).toHaveAttribute('data-coverage', 'outside');
    await expect(page.getByTestId('ml-ds-range-warn-AAPL')).toContainText('no stored bars fall in this range');
    await expect(page.getByTestId('ml-ds-estimated-rows')).toHaveText('0');
    await expect(page.getByTestId('ml-ds-warn-rows')).toBeVisible();
  });

  test('a range for a symbol with no data points at Fetch history, and fetch size follows the range', async ({ page }) => {
    await addSymbol(page, 'NEWCO');
    await page.getByTestId('ml-ds-period-range').check();
    await page.getByTestId('ml-ds-from').fill('2025-01-01');
    await page.getByTestId('ml-ds-to').fill('2025-06-01');
    await expect(page.getByTestId('ml-ds-range-nodata')).toContainText('NEWCO');
    const label = await page.getByTestId('ml-ds-symbol-fetch-NEWCO').innerText();
    const n = Number(/\((\d+) bars\)/.exec(label)?.[1]);
    expect(n).toBeGreaterThanOrEqual(100);
    expect(n).not.toBe(1000);
    await page.getByTestId('ml-ds-symbol-fetch-NEWCO').click();
    await expect(page.getByTestId('ml-ds-symbol-data-NEWCO')).not.toHaveAttribute('data-status', 'none');
    const fetches = await ipc(page, 'fetch_symbol_history');
    expect(fetches[0].args.bars).toBe(n);
  });

  test('stored data is found when the backend answers with its canonical timeframe id', async ({ page }) => {
    // The mock answers "1d" for d1 and "1h" for h1, like the real list_symbol_data.
    await addSymbol(page, 'AAPL');
    await expect(page.getByTestId('ml-ds-symbol-data-AAPL')).toHaveAttribute('data-status', 'ok');
    await page.getByTestId('ml-ds-timeframe').selectOption('h1');
    await expect(page.getByTestId('ml-ds-symbol-data-AAPL')).toHaveAttribute('data-status', 'ok');
    await expect(page.getByTestId('ml-ds-symbol-data-AAPL')).toHaveText('520 bars, 2024-01-02 to 2026-01-02');
    await addSymbol(page, 'NEWCO');
    await page.getByTestId('ml-ds-symbol-fetch-NEWCO').click();
    await expect(page.getByTestId('ml-ds-symbol-data-NEWCO')).toHaveAttribute('data-status', 'ok');
  });

  test('a built dataset reported as "1d" loads back as the d1 option', async ({ page }) => {
    await page.getByTestId('ml-ds-name').fill('canon');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('canon');
    await page.getByTestId('ml-ds-timeframe').selectOption('m15');
    await page.getByTestId('ml-ds-duplicate-canon').click();
    await expect(page.getByTestId('ml-ds-dup-note')).toContainText('Settings loaded from canon');
    await expect(page.getByTestId('ml-ds-timeframe')).toHaveValue('d1');
  });

  test('date range mode never sends an invalid hidden bars value', async ({ page }) => {
    for (const bad of ['', '50000', '7']) {
      await page.getByTestId('ml-ds-period-bars').check();
      await page.getByTestId('ml-ds-bars').fill(bad);
      await expect(page.getByTestId('ml-ds-build')).toBeDisabled(); // bars mode: the value is validated
      await page.getByTestId('ml-ds-period-range').check();
      await page.getByTestId('ml-ds-from').fill('2024-07-01');
      await page.getByTestId('ml-ds-to').fill('2025-07-01');
      await page.getByTestId('ml-ds-name').fill(`ranged-${bad || 'empty'}`);
      await page.getByTestId('ml-ds-build').click();
      await expect(page.getByTestId('ml-ds-result-name')).toHaveText(`ranged-${bad || 'empty'}`);
      const req = await lastBuild(page);
      expect(req).toMatchObject({ from: '2024-07-01', to: '2025-07-01' });
      expect(Number.isInteger(req.bars) && req.bars >= 100 && req.bars <= 20000).toBe(true);
    }
  });

  test('sends from and to only in date range mode', async ({ page }) => {
    await page.getByTestId('ml-ds-name').fill('latest-only');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-dataset-latest-only')).toBeVisible();
    let req = await lastBuild(page);
    expect(req.from).toBeUndefined();
    expect(req.to).toBeUndefined();

    await page.getByTestId('ml-ds-period-range').check();
    await page.getByTestId('ml-ds-from').fill('2024-07-01');
    await page.getByTestId('ml-ds-to').fill('2025-07-01');
    await page.getByTestId('ml-ds-name').fill('ranged');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('ranged');
    req = await lastBuild(page);
    expect(req).toMatchObject({ from: '2024-07-01', to: '2025-07-01', timeframe: 'd1' });
    await expect(page.getByTestId('ml-ds-result-range')).toHaveText('2024-07-01 to 2025-07-01');
  });
});

test.describe('Data tab: custom features', () => {
  test.beforeEach(async ({ page }) => { await openData(page); await addSymbol(page, 'AAPL'); });

  test('checks a formula live: error with a marker at the position, then warm-up and sample values', async ({ page }) => {
    await page.getByTestId('ml-cf-add').click();
    await page.getByTestId('ml-cf-name-0').fill('my_feat');
    await page.getByTestId('ml-cf-expr-0').fill('sma(close, 20');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'checking'); // debounced
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('ml-cf-error-0')).toContainText('Missing )');
    const marker = page.getByTestId('ml-cf-marker-0');
    await expect(marker).toHaveAttribute('data-pos', '13');
    expect(await marker.innerText()).toBe(`sma(close, 20\n${' '.repeat(13)}^`);

    await page.getByTestId('ml-cf-expr-0').fill('foo(close)');
    await expect(page.getByTestId('ml-cf-error-0')).toContainText('Unknown function foo()');
    await expect(page.getByTestId('ml-cf-marker-0')).toHaveAttribute('data-pos', '0');

    await page.getByTestId('ml-cf-expr-0').fill('sma(close, 20) / close - 1');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ml-cf-warmup-0')).toHaveText('Warm-up 20 bars.');
    await expect(page.getByTestId('ml-cf-sample-0')).toContainText('Last values on AAPL');
    await expect(page.getByTestId('ml-cf-sample-0')).toContainText('2026-01-01 0.0000');
    await expect(page.getByTestId('ml-cf-sample-0').locator('span[title]')).toHaveCount(5);
    await expect(page.getByTestId('ml-cf-marker-0')).toHaveCount(0);

    const checks = await ipc(page, 'check_expression');
    expect(checks.every((c) => c.args.symbol === 'AAPL' && c.args.timeframe === 'd1')).toBe(true);
    expect(checks.map((c) => c.args.expr)).toContain('sma(close, 20) / close - 1');
  });

  test('debounces typing: one check per settled formula, none for text typed through', async ({ page }) => {
    await page.getByTestId('ml-cf-add').click();
    await page.getByTestId('ml-cf-name-0').fill('typed');
    await page.getByTestId('ml-cf-expr-0').pressSequentially('close / sma(close, 10)', { delay: 30 });
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    const checks = await ipc(page, 'check_expression');
    expect(checks.length).toBeLessThanOrEqual(2);
    expect(checks[checks.length - 1].args.expr).toBe('close / sma(close, 10)');
  });

  test('blocks the build while a formula is invalid and unblocks it once fixed', async ({ page }) => {
    await addFormula(page, 0, 'bad_one', 'closee / open'); // unknown name
    const build = page.getByTestId('ml-ds-build');
    await expect(build).toBeDisabled(); // already blocked while the check is pending or failing
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Custom feature 1 (bad_one): fix the formula');
    await expect(build).toBeDisabled();
    await page.getByTestId('ml-cf-expr-0').fill('close / open');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ml-ds-validation')).toHaveCount(0);
    await expect(build).toBeEnabled();
    // an empty formula blocks too
    await page.getByTestId('ml-cf-expr-0').fill('');
    await expect(build).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('enter a formula');
  });

  test('does not allow building while a check is still pending', async ({ page }) => {
    await mock(page, 'delayNext', 'check_expression', 800);
    await addFormula(page, 0, 'slow_one', 'close / open');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'checking');
    await expect(page.getByTestId('ml-ds-build')).toBeDisabled();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('still being checked');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled();
  });

  test('validates feature names: format, built-in clash and duplicates', async ({ page }) => {
    await addFormula(page, 0, 'Bad Name', 'close');
    await expect(page.getByTestId('ml-cf-name-error-0')).toContainText('lowercase letter first');
    await page.getByTestId('ml-cf-name-0').fill('1abc');
    await expect(page.getByTestId('ml-cf-name-error-0')).toContainText('lowercase letter first');
    await page.getByTestId('ml-cf-name-0').fill('a'.repeat(42));
    await expect(page.getByTestId('ml-cf-name-error-0')).toContainText('at most 41 characters');
    await page.getByTestId('ml-cf-name-0').fill('a'.repeat(41));
    await expect(page.getByTestId('ml-cf-name-error-0')).toHaveCount(0);

    await page.getByTestId('ml-cf-name-0').fill('rsi_14');
    await expect(page.getByTestId('ml-cf-name-error-0')).toContainText('already a built-in feature');
    await expect(page.getByTestId('ml-ds-build')).toBeDisabled();

    await page.getByTestId('ml-cf-name-0').fill('same');
    await addFormula(page, 1, 'same', 'open');
    await expect(page.getByTestId('ml-cf-name-error-0')).toContainText('Another custom feature already uses this name');
    await expect(page.getByTestId('ml-cf-name-error-1')).toContainText('Another custom feature already uses this name');
    await expect(page.getByTestId('ml-ds-build')).toBeDisabled();
    await page.getByTestId('ml-cf-name-1').fill('other');
    await expect(page.getByTestId('ml-cf-name-error-0')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-validation')).toHaveCount(0);
  });

  test('example formulas add rows with unique names; reference entries insert at the cursor', async ({ page }) => {
    await page.getByTestId('ml-cf-example-dist_sma20').click();
    await expect(page.getByTestId('ml-cf-name-0')).toHaveValue('dist_sma20');
    await expect(page.getByTestId('ml-cf-expr-0')).toHaveValue('close / sma(close, 20) - 1');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await page.getByTestId('ml-cf-example-dist_sma20').click();
    await expect(page.getByTestId('ml-cf-name-1')).toHaveValue('dist_sma20_2');
    await expect(page.getByTestId('ml-cf-count')).toHaveText('2 custom');

    await expect(page.getByTestId('ml-cf-reference')).toHaveCount(0);
    await page.getByTestId('ml-cf-reference-toggle').click();
    for (const fn of ['sma', 'ema', 'std', 'sum', 'max', 'min', 'zscore', 'rsi', 'ret', 'shift', 'abs', 'log', 'sqrt', 'atr', 'other']) {
      await expect(page.getByTestId(`ml-cf-ref-${fn}`)).toBeVisible();
    }
    // insert in the middle of the formula that was edited last
    await page.getByTestId('ml-cf-expr-1').fill('1 + ');
    await page.getByTestId('ml-cf-expr-1').evaluate((el: HTMLInputElement) => el.setSelectionRange(4, 4));
    await page.getByTestId('ml-cf-ref-rsi').click();
    await expect(page.getByTestId('ml-cf-expr-1')).toHaveValue('1 + rsi(close, 14)');
    await expect(page.getByTestId('ml-cf-expr-0')).toHaveValue('close / sma(close, 20) - 1');
    await expect(page.getByTestId('ml-cf-expr-1')).toBeFocused();
    await page.getByTestId('ml-cf-reference-toggle').click();
    await expect(page.getByTestId('ml-cf-reference')).toHaveCount(0);
  });

  test('formulas are checked again after history is fetched for a context symbol', async ({ page }) => {
    await addFormula(page, 0, 'rel_zz', 'ret(close, 5) - ret(other("ZZNEW", "close"), 5)');
    await page.getByTestId('ml-cf-context-add-0-ZZNEW').click();
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('ml-cf-error-0')).toContainText('no stored bars for ZZNEW');
    await expect(page.getByTestId('ml-ds-build')).toBeDisabled();
    // fetching the history fixes it without touching the formula text
    await page.getByTestId('ml-ds-ctx-fetch-ZZNEW').click();
    await expect(page.getByTestId('ml-ds-ctx-data-ZZNEW')).toHaveAttribute('data-status', 'ok');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled();
  });

  test('a formula that reads another symbol offers to add it as a context symbol', async ({ page }) => {
    await page.getByTestId('ml-cf-example-rel_spy').click();
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ml-cf-context-warn-0-SPY')).toContainText('SPY, which is not a context symbol yet');
    await page.getByTestId('ml-cf-context-add-0-SPY').click();
    await expect(page.getByTestId('ml-ds-ctx-chip-SPY')).toBeVisible();
    await expect(page.getByTestId('ml-cf-context-warn-0-SPY')).toHaveCount(0);
  });

  test('without a symbol formulas wait for one, and the build says to add a symbol', async ({ page }) => {
    await page.getByTestId('ml-ds-symbol-remove-AAPL').click();
    await addFormula(page, 0, 'early', 'close / open');
    await expect(page.getByTestId('ml-cf-nosymbol-0')).toBeVisible();
    expect(await ipc(page, 'check_expression')).toHaveLength(0);
    await addSymbol(page, 'MSFT');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    expect((await ipc(page, 'check_expression'))[0].args.symbol).toBe('MSFT');
  });

  test('counts custom warm-up and custom features in the estimate', async ({ page }) => {
    await page.getByTestId('ml-ds-bars').fill('500');
    await page.getByTestId('ml-ds-preset-none').click();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Select at least one feature or add a custom feature');
    await expect(page.getByTestId('ml-ds-build')).toBeDisabled();
    await addFormula(page, 0, 'wide', 'sma(close, 120)');
    await expect(page.getByTestId('ml-cf-warmup-0')).toHaveText('Warm-up 120 bars.');
    await expect(page.getByTestId('ml-ds-warmup')).toHaveText('120');
    await expect(page.getByTestId('ml-ds-estimated-rows')).toHaveText('375'); // 500 - 120 - 5
    await expect(page.getByTestId('ml-ds-feature-total')).toHaveText('1 feature (0 built-in + 1 custom)');
    await page.getByTestId('ml-cf-remove-0').click();
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Select at least one feature');
  });
});

test.describe('Data tab: label, fee and building', () => {
  test.beforeEach(async ({ page }) => { await openData(page); await addSymbol(page, 'AAPL'); });

  test('builds from custom features only and sends exactly that request', async ({ page }) => {
    await page.getByTestId('ml-ds-preset-none').click();
    await page.getByTestId('ml-cf-example-dist_sma20').click();
    await page.getByTestId('ml-cf-example-vol_spike').click();
    await expect(page.getByTestId('ml-cf-status-1')).toHaveAttribute('data-state', 'ok');
    await page.getByTestId('ml-ds-name').fill('custom-only');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('custom-only');

    const calls = await ipc(page, 'build_dataset');
    expect(calls).toHaveLength(1);
    expect(calls[0].args.request).toEqual({
      name: 'custom-only', symbols: ['AAPL'], timeframe: 'd1', bars: 1000, features: [],
      label: { horizon_bars: 5, kind: 'direction', threshold_pct: 0 },
      custom_features: [
        { name: 'dist_sma20', expr: 'close / sma(close, 20) - 1' },
        { name: 'vol_spike', expr: 'volume / sma(volume, 20)' },
      ],
      context_symbols: [], fee_pct: 0,
    });
    await expect(page.getByTestId('ml-ds-result-features')).toHaveText('0 + 2 custom');
    await expect(page.getByTestId('ml-ds-result-custom-dist_sma20')).toContainText('close / sma(close, 20) - 1');
    await expect(page.getByTestId('ml-ds-preview-table')).toContainText('vol_spike');
  });

  test('sends asymmetric thresholds, fee and the horizon as typed', async ({ page }) => {
    await expect(page.getByTestId('ml-ds-asym')).toHaveCount(0); // only for the threshold label
    await page.getByTestId('ml-ds-label-kind').selectOption('return_threshold');
    await page.getByTestId('ml-ds-horizon').fill('12');
    await page.getByTestId('ml-ds-threshold').fill('0.6');
    await page.getByTestId('ml-ds-asym').check();
    await expect(page.getByTestId('ml-ds-threshold')).toHaveCount(0);
    await expect(page.getByTestId('ml-ds-up-threshold')).toHaveValue('0.6'); // starts from the symmetric value
    await expect(page.getByTestId('ml-ds-down-threshold')).toHaveValue('0.6');
    await page.getByTestId('ml-ds-up-threshold').fill('0.8');
    await page.getByTestId('ml-ds-down-threshold').fill('0.3');
    await page.getByTestId('ml-ds-fee').fill('0.1');
    await expect(page.getByTestId('ml-ds-label-help')).toContainText('above +0.8% after the fee, -1 if a short would still earn more than 0.3% after paying the fee');
    await expect(page.getByTestId('ml-ds-fee-help')).toContainText('net of it');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toBeVisible();
    const req = await lastBuild(page);
    expect(req.label).toEqual({ horizon_bars: 12, kind: 'return_threshold', threshold_pct: 0.8, up_threshold_pct: 0.8, down_threshold_pct: 0.3 });
    expect(req.fee_pct).toBe(0.1);
    await expect(page.getByTestId('ml-ds-result-label')).toHaveText('12 bars ahead, up above +0.8%, down below -0.3%');
    await expect(page.getByTestId('ml-ds-result-fee')).toHaveText('0.1% round trip');

    // symmetric again: no up/down keys
    await page.getByTestId('ml-ds-asym').uncheck();
    await expect(page.getByTestId('ml-ds-threshold')).toHaveValue('0.8');
    await page.getByTestId('ml-ds-name').fill('sym-again');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('sym-again');
    expect((await lastBuild(page)).label).toEqual({ horizon_bars: 12, kind: 'return_threshold', threshold_pct: 0.8 });
  });

  test('validates asymmetric thresholds and the fee', async ({ page }) => {
    const build = page.getByTestId('ml-ds-build');
    await page.getByTestId('ml-ds-label-kind').selectOption('return_threshold');
    await page.getByTestId('ml-ds-asym').check();
    await page.getByTestId('ml-ds-down-threshold').fill('0');
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Down threshold must be above 0%');
    await expect(build).toBeDisabled();
    await page.getByTestId('ml-ds-down-threshold').fill('0.5');
    await page.getByTestId('ml-ds-up-threshold').fill('60');
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Up threshold must be above 0%');
    await page.getByTestId('ml-ds-up-threshold').fill('0.5');
    await expect(build).toBeEnabled();
    await page.getByTestId('ml-ds-fee').fill('-1');
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Fee must be a number from 0 to 10 percent');
    await expect(build).toBeDisabled();
    await page.getByTestId('ml-ds-fee').fill('11');
    await expect(build).toBeDisabled();
    await page.getByTestId('ml-ds-fee').fill('0.05');
    await expect(build).toBeEnabled();
  });

  test('accepts any horizon within the limit and rejects the rest', async ({ page }) => {
    await page.getByTestId('ml-ds-horizon').fill('60');
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled();
    await page.getByTestId('ml-ds-horizon').fill('2.5');
    await expect(page.getByTestId('ml-ds-validation')).toContainText('Horizon must be a whole number');
    await page.getByTestId('ml-ds-horizon').fill('1');
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled();
  });
});

test.describe('Data tab: duplicate settings', () => {
  test.beforeEach(async ({ page }) => { await openData(page); });

  async function buildRich(page: Page) {
    await addSymbol(page, 'AAPL');
    await addSymbol(page, 'MSFT');
    await addSymbol(page, 'SPY', 'ml-ds-ctx');
    await page.getByTestId('ml-ds-timeframe').selectOption('h1');
    await page.getByTestId('ml-ds-bars').fill('800');
    await page.getByTestId('ml-ds-preset-momentum').click();
    await page.getByTestId('ml-cf-example-dist_sma20').click();
    await addFormula(page, 1, 'rel_spy', 'ret(close, 5) - ret(other("SPY", "close"), 5)');
    await page.getByTestId('ml-ds-horizon').fill('8');
    await page.getByTestId('ml-ds-label-kind').selectOption('return_threshold');
    await page.getByTestId('ml-ds-asym').check();
    await page.getByTestId('ml-ds-up-threshold').fill('0.9');
    await page.getByTestId('ml-ds-down-threshold').fill('0.4');
    await page.getByTestId('ml-ds-fee').fill('0.12');
    await page.getByTestId('ml-ds-name').fill('rich-one');
    await expect(page.getByTestId('ml-cf-status-1')).toHaveAttribute('data-state', 'ok');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('rich-one');
  }

  test('lists show custom features, context, fee and range; the card shows them too', async ({ page }) => {
    await buildRich(page);
    const meta = page.getByTestId('ml-ds-meta-rich-one');
    await expect(meta).toContainText('AAPL+MSFT');
    await expect(meta).toContainText('1h'); // the backend reports the canonical id
    await expect(meta).toContainText('custom: dist_sma20, rel_spy');
    await expect(meta).toContainText('context: SPY');
    await expect(meta).toContainText('fee 0.12%');
    await expect(meta).toContainText('2024-01-02 to 2026-01-02');
    await expect(page.getByTestId('ml-ds-result-context')).toHaveText('SPY');
    await expect(page.getByTestId('ml-ds-result-fee')).toHaveText('0.12% round trip');
    await expect(page.getByTestId('ml-ds-result-custom-rel_spy')).toContainText('other("SPY", "close")');
  });

  test('loads a dataset back into the form, and rebuilding under a new name sends the same settings', async ({ page }) => {
    await buildRich(page);
    const first = await lastBuild(page);

    // wreck the form so the round trip is visible
    await page.getByTestId('ml-ds-symbol-remove-AAPL').click();
    await page.getByTestId('ml-ds-symbol-remove-MSFT').click();
    await page.getByTestId('ml-ds-ctx-remove-SPY').click();
    await page.getByTestId('ml-cf-remove-0').click();
    await page.getByTestId('ml-cf-remove-0').click();
    await page.getByTestId('ml-ds-label-kind').selectOption('direction');
    await page.getByTestId('ml-ds-fee').fill('0');
    await page.getByTestId('ml-ds-preset-everything').click();
    await page.getByTestId('ml-ds-timeframe').selectOption('d1');

    await page.getByTestId('ml-ds-duplicate-rich-one').click();
    await expect(page.getByTestId('ml-ds-dup-note')).toContainText('Settings loaded from rich-one');
    await expect(page.getByTestId('ml-ds-symbol-chip-AAPL')).toBeVisible();
    await expect(page.getByTestId('ml-ds-symbol-chip-MSFT')).toBeVisible();
    await expect(page.getByTestId('ml-ds-ctx-chip-SPY')).toBeVisible();
    await expect(page.getByTestId('ml-ds-timeframe')).toHaveValue('h1');
    await expect(page.getByTestId('ml-ds-horizon')).toHaveValue('8');
    await expect(page.getByTestId('ml-ds-label-kind')).toHaveValue('return_threshold');
    await expect(page.getByTestId('ml-ds-asym')).toBeChecked();
    await expect(page.getByTestId('ml-ds-up-threshold')).toHaveValue('0.9');
    await expect(page.getByTestId('ml-ds-down-threshold')).toHaveValue('0.4');
    await expect(page.getByTestId('ml-ds-fee')).toHaveValue('0.12');
    await expect(page.getByTestId('ml-ds-feature-count')).toHaveText('5 of 15 selected');
    await expect(page.getByTestId('ml-ds-feature-rsi_14')).toBeChecked();
    await expect(page.getByTestId('ml-ds-feature-ret_10')).not.toBeChecked();
    await expect(page.getByTestId('ml-cf-name-0')).toHaveValue('dist_sma20');
    await expect(page.getByTestId('ml-cf-expr-1')).toHaveValue('ret(close, 5) - ret(other("SPY", "close"), 5)');
    await expect(page.getByTestId('ml-ds-name')).toHaveValue('rich-one_copy');
    await expect(page.getByTestId('ml-ds-build')).toHaveText('Build dataset'); // a new name, not a rebuild

    // tweak one thing and rebuild
    await page.getByTestId('ml-ds-fee').fill('0.2');
    await expect(page.getByTestId('ml-cf-status-1')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ml-ds-build')).toBeEnabled();
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('rich-one_copy');
    const second = await lastBuild(page);
    expect(second).toMatchObject({
      name: 'rich-one_copy', symbols: first.symbols, timeframe: first.timeframe, features: first.features,
      custom_features: first.custom_features, context_symbols: first.context_symbols, label: first.label, fee_pct: 0.2,
    });
    expect(first.fee_pct).toBe(0.12);
    await expect(page.getByTestId('ml-dataset-rich-one')).toBeVisible();
    await expect(page.getByTestId('ml-dataset-rich-one_copy')).toBeVisible();

    // duplicating the copy picks the next free name
    await page.getByTestId('ml-ds-result-duplicate').click();
    await expect(page.getByTestId('ml-ds-name')).toHaveValue('rich-one_copy2');
  });

  test('duplicating a plain dataset restores symmetric thresholds and range-free bars', async ({ page }) => {
    await addSymbol(page, 'AAPL');
    await page.getByTestId('ml-ds-label-kind').selectOption('return_threshold');
    await page.getByTestId('ml-ds-threshold').fill('1.5');
    await page.getByTestId('ml-ds-bars').fill('600');
    await page.getByTestId('ml-ds-name').fill('plain');
    await page.getByTestId('ml-ds-build').click();
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('plain');
    await page.getByTestId('ml-ds-period-range').check();
    await page.getByTestId('ml-ds-duplicate-plain').click();
    await expect(page.getByTestId('ml-ds-period-bars')).toBeChecked();
    await expect(page.getByTestId('ml-ds-asym')).not.toBeChecked();
    await expect(page.getByTestId('ml-ds-threshold')).toHaveValue('1.5');
    await expect(page.getByTestId('ml-cf-empty')).toBeVisible();
    // the mock keeps 600 - 60 - 5 = 535 rows; bars are rebuilt from the rows: 535 + 50 warm-up + 5 horizon, rounded up to 10
    await expect(page.getByTestId('ml-ds-bars')).toHaveValue('590');
  });

  test('the form is usable with the keyboard only', async ({ page }) => {
    await page.getByTestId('ml-ds-symbol-input').focus();
    await page.keyboard.type('AAPL');
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('ml-ds-symbol-chip-AAPL')).toBeVisible();
    await page.getByTestId('ml-cf-add').focus();
    await page.keyboard.press('Enter'); // the new row's name field gets the cursor
    await expect(page.getByTestId('ml-cf-name-0')).toBeFocused();
    await page.keyboard.type('kb_feat');
    await page.keyboard.press('Tab'); // -> formula
    await page.keyboard.type('close / open');
    await expect(page.getByTestId('ml-cf-status-0')).toHaveAttribute('data-state', 'ok');
    await page.getByTestId('ml-ds-period-range').focus();
    await page.keyboard.press('Space');
    await expect(page.getByTestId('ml-ds-from')).toBeVisible();
    await page.getByTestId('ml-ds-period-bars').focus();
    await page.keyboard.press('Space');
    await page.getByTestId('ml-ds-build').focus();
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('ml-ds-result-name')).toBeVisible();
    expect((await lastBuild(page)).custom_features).toEqual([{ name: 'kb_feat', expr: 'close / open' }]);
  });
});
