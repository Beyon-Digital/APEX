import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { positionState } from '../src/components/monitor/positionState';
import { actionLabel } from '../src/components/activity/activityUtils';
import type { ModelDetailDto, PositionDto } from '../src/lib/types';

type Ipc = { cmd: string; args?: Record<string, unknown> };

async function boot(page: Page) {
  await page.addInitScript(() => { (window as unknown as { __ipcLog: Ipc[] }).__ipcLog = []; });
  await page.goto('/');
  await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
}
const hooks = (page: Page, name: string, ...args: unknown[]) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);
const make = (page: Page, patch: Partial<ModelDetailDto> = {}) => hooks(page, 'trainModelCustom', patch) as Promise<string>;
const createCalls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __ipcLog: Ipc[] }).__ipcLog.filter((c) => c.cmd === 'create_automation').map((c) => (c.args as { request: Record<string, unknown> }).request));

async function openModel(page: Page) {
  await page.getByTestId('tab-ml').click();
  await page.getByTestId('ml-tab-models').click();
  await page.getByTestId('ml-model-row').first().click();
  await expect(page.getByTestId('auto-form')).toBeVisible();
}
async function openAutomations(page: Page) {
  await page.getByTestId('tab-automations').click();
  await expect(page.getByTestId('automations-panel')).toBeVisible();
}
async function createRule(page: Page, symbol: string, broker: string, short: boolean) {
  await page.getByTestId('automation-symbol').fill(symbol);
  await page.getByTestId('automation-broker').fill(broker);
  const box = page.getByTestId('automation-allow-short');
  if (short) await box.check(); else if (await box.isChecked()) await box.uncheck();
  await page.getByTestId('automation-create').click();
}

test.describe('position state helper', () => {
  const pos = (over: Partial<PositionDto>): PositionDto => ({ symbol: 'INFY.NS', quantity: 5, avg_price: 1, side: 'Buy', pnl: 0, pnl_pct: 0, broker_id: 'paper', ...over });
  const rule = { symbol: 'infy.ns', broker_id: 'paper' };
  test('long, short (negative quantity or sell side), flat and unknown', () => {
    expect(positionState(rule, [pos({})], 'paper')).toBe('long');
    expect(positionState(rule, [pos({ quantity: -3 })], 'paper')).toBe('short');
    expect(positionState(rule, [pos({ side: 'Sell' })], 'paper')).toBe('short');
    expect(positionState(rule, [pos({ quantity: 0 })], 'paper')).toBe('flat');
    expect(positionState(rule, [], 'paper')).toBe('flat');
    expect(positionState(rule, [pos({ broker_id: 'zerodha' })], 'paper')).toBe('flat');
    expect(positionState({ ...rule, broker_id: 'zerodha' }, [], 'paper')).toBeNull();
  });
  test('the activity panel labels the shorting action', () => {
    expect(actionLabel('automation.short')).toBe('Changed automation shorting');
  });
});

test.describe('create automation from a model: Allow shorting', () => {
  test.beforeEach(async ({ page }) => { await boot(page); });

  test('paper: enabled, off by default for a long/flat model, sends allow_short when ticked', async ({ page }) => {
    await make(page, { display_name: 'LF', trade_mode: 'long_flat', label: { horizon_bars: 5, kind: 'direction', threshold_pct: 0 } });
    await openModel(page);
    const box = page.getByTestId('auto-allow-short');
    await expect(box).toBeEnabled();
    await expect(box).not.toBeChecked();
    await expect(page.getByTestId('auto-short-help')).toContainText('Class 0 sells short');
    await expect(page.getByTestId('auto-short-help')).toContainText('long / flat');
    await page.getByTestId('auto-symbol').fill('INFY.NS');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-created')).toBeVisible();
    expect((await createCalls(page))[0].allow_short).toBe(false);

    await box.check();
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('used-by-row')).toHaveCount(2);
    expect((await createCalls(page))[1].allow_short).toBe(true);
  });

  test('a long/short model defaults the box on for paper, and the user can still turn it off', async ({ page }) => {
    await make(page, { display_name: 'LS', trade_mode: 'long_short', label: { horizon_bars: 5, kind: 'return_threshold', threshold_pct: 1 } });
    await openModel(page);
    const box = page.getByTestId('auto-allow-short');
    await expect(box).toBeChecked();
    await expect(page.getByTestId('auto-short-help')).toContainText('Class -1 sells short');
    await box.uncheck();
    await page.getByTestId('auto-symbol').fill('INFY.NS');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-created')).toBeVisible();
    expect((await createCalls(page))[0].allow_short).toBe(false);
  });

  test('a broker that cannot short disables the box with a reason, and sends allow_short false', async ({ page }) => {
    await make(page, { display_name: 'LS', trade_mode: 'long_short' });
    await openModel(page);
    await expect(page.getByTestId('auto-allow-short')).toBeChecked();
    await expect(page.getByTestId('auto-short-reason')).toHaveCount(0);
    await expect(page.getByTestId('auto-broker').locator('option[value="zerodha"]')).toHaveCount(1);
    await page.getByTestId('auto-broker').selectOption('zerodha');
    const box = page.getByTestId('auto-allow-short');
    await expect(box).toBeDisabled();
    await expect(box).not.toBeChecked();
    await expect(page.getByTestId('auto-short-reason')).toContainText('zerodha cannot short');
    await expect(page.getByTestId('auto-short-reason')).toContainText('allow_live_shorting');
    await page.getByTestId('auto-symbol').fill('INFY.NS');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-created')).toBeVisible();
    const call = (await createCalls(page))[0];
    expect(call.broker_id).toBe('zerodha');
    expect(call.allow_short).toBe(false);
  });

  test('a live broker that can short enables the box and shows the live warning', async ({ page }) => {
    await hooks(page, 'setBrokerCanShort', 'zerodha', true);
    await make(page, { display_name: 'LS', trade_mode: 'long_short' });
    await openModel(page);
    await page.getByTestId('auto-broker').selectOption('zerodha');
    await expect(page.getByTestId('auto-allow-short')).toBeEnabled();
    await expect(page.getByTestId('auto-allow-short')).toBeChecked();
    await expect(page.getByTestId('auto-short-live-warning')).toContainText('allow_live_shorting');
    await page.getByTestId('auto-symbol').fill('INFY.NS');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-created')).toBeVisible();
    expect((await createCalls(page))[0].allow_short).toBe(true);
  });

  test('the model detail notes when its automation cannot short because of the broker', async ({ page }) => {
    await make(page, { display_name: 'LS', trade_mode: 'long_short' });
    await openModel(page);
    await page.getByTestId('auto-symbol').fill('INFY.NS');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-created')).toBeVisible();
    await expect(page.getByTestId('model-shorting-note')).toHaveCount(0);
    await hooks(page, 'setBrokerCanShort', 'paper', false);
    await page.getByTestId('model-detail-close').click();
    await page.getByTestId('ml-model-row').first().click();
    await expect(page.getByTestId('model-shorting-note')).toContainText('paper, which cannot short');
  });
});

test.describe('Automations panel: shorting', () => {
  test.beforeEach(async ({ page }) => { await boot(page); await make(page, { display_name: 'M' }); });

  test('create form: checkbox sends allow_short and rows show a Long only / Long & short badge', async ({ page }) => {
    await openAutomations(page);
    await expect(page.getByTestId('automation-allow-short')).toBeEnabled();
    await createRule(page, 'INFY.NS', 'paper', false);
    await expect(page.getByTestId('automation-row')).toHaveCount(1);
    await createRule(page, 'TCS.NS', 'paper', true);
    await expect(page.getByTestId('automation-row')).toHaveCount(2);
    const calls = await createCalls(page);
    expect(calls.map((c) => c.allow_short)).toEqual([false, true]);
    const badges = page.getByTestId('automation-short-badge');
    await expect(badges.nth(0)).toHaveText('Long only');
    await expect(badges.nth(1)).toHaveText('Long & short');
  });

  test('the form disables shorting for a broker that cannot short and says why', async ({ page }) => {
    await openAutomations(page);
    await page.getByTestId('automation-broker').fill('zerodha');
    await expect(page.getByTestId('automation-allow-short')).toBeDisabled();
    await expect(page.getByTestId('automation-short-help')).toContainText('zerodha cannot short');
    await page.getByTestId('automation-broker').fill('paper');
    await expect(page.getByTestId('automation-allow-short')).toBeEnabled();
  });

  test('per-row toggle turns shorting on and off', async ({ page }) => {
    await openAutomations(page);
    await createRule(page, 'INFY.NS', 'paper', false);
    const row = page.getByTestId('automation-row');
    await expect(row.getByTestId('automation-short-badge')).toHaveText('Long only');
    await row.getByTestId('automation-short-toggle').click();
    await expect(row.getByTestId('automation-short-badge')).toHaveText('Long & short');
    await expect(row.getByTestId('automation-short-error')).toHaveCount(0);
    const sent = await page.evaluate(() => (window as unknown as { __ipcLog: Ipc[] }).__ipcLog.filter((c) => c.cmd === 'set_automation_short').map((c) => c.args));
    expect(sent).toEqual([{ id: expect.any(String), allow: true }]);
    await row.getByTestId('automation-short-toggle').click();
    await expect(row.getByTestId('automation-short-badge')).toHaveText('Long only');
  });

  test('per-row toggle shows the backend error inline and leaves the rule long only', async ({ page }) => {
    await openAutomations(page);
    await createRule(page, 'INFY.NS', 'zerodha', false);
    const row = page.getByTestId('automation-row');
    await row.getByTestId('automation-short-toggle').click();
    await expect(row.getByTestId('automation-short-error')).toHaveText('zerodha cannot hold short positions');
    await expect(row.getByTestId('automation-short-badge')).toHaveText('Long only');
    await expect(page.getByTestId('automations-error')).toHaveCount(0);
  });

  test('live broker with shorting enabled shows the warning chip; paper and long-only rules do not', async ({ page }) => {
    await hooks(page, 'setBrokerCanShort', 'zerodha', true);
    await openAutomations(page);
    // The broker list is polled, so wait until the form sees zerodha as able to short.
    await page.getByTestId('automation-broker').fill('zerodha');
    await expect(page.getByTestId('automation-allow-short')).toBeEnabled({ timeout: 10000 });
    await expect(page.getByTestId('automation-short-live-warning')).toHaveCount(0);
    await page.getByTestId('automation-allow-short').check();
    await expect(page.getByTestId('automation-short-live-warning')).toContainText('allow_live_shorting');
    await page.getByTestId('automation-symbol').fill('INFY.NS');
    await page.getByTestId('automation-create').click();
    await expect(page.getByTestId('automation-row')).toHaveCount(1);
    await expect(page.getByTestId('automation-short-live-chip')).toContainText('allow_live_shorting');
    await expect(page.getByTestId('automation-short-live-chip')).toContainText('live trading armed');

    await createRule(page, 'TCS.NS', 'paper', true);
    await createRule(page, 'HDFCBANK.NS', 'zerodha', false);
    await expect(page.getByTestId('automation-row')).toHaveCount(3);
    await expect(page.getByTestId('automation-short-live-chip')).toHaveCount(1);
  });

  test('rows show the current position state from the positions store', async ({ page }) => {
    await openAutomations(page);
    await createRule(page, 'INFY.NS', 'paper', true);
    await createRule(page, 'TCS.NS', 'paper', true);
    await createRule(page, 'HDFCBANK.NS', 'paper', true);
    await expect(page.getByTestId('automation-row')).toHaveCount(3);
    await hooks(page, 'setMockPositions', [
      { symbol: 'INFY.NS', quantity: 5, avg_price: 1500, side: 'Buy', pnl: 0, pnl_pct: 0, broker_id: 'paper' },
      { symbol: 'TCS.NS', quantity: -3, avg_price: 3500, side: 'Sell', pnl: 0, pnl_pct: 0, broker_id: 'paper' },
    ]);
    const state = (symbol: string) => page.getByTestId('automation-row').filter({ hasText: symbol }).getByTestId('automation-position');
    await expect(state('INFY.NS')).toHaveAttribute('data-state', 'long', { timeout: 10000 });
    await expect(state('TCS.NS')).toHaveAttribute('data-state', 'short');
    await expect(state('HDFCBANK.NS')).toHaveAttribute('data-state', 'flat');
    await expect(state('TCS.NS')).toContainText('short');
  });
});
