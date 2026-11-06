import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { activityToCsv } from '../src/components/activity/activityUtils';
import type { ActivityDto } from '../src/lib/types';

type Hooks = {
  trainModelAsCopilot: (algorithm?: string) => string;
  createStrategyAsCopilot: (name?: string) => string;
  createAutomationAsCopilot: (name?: string, modelId?: string) => string;
  recordActivity: (actor: string, action: string, kind: string, target: string, summary: string, ok?: boolean, details?: string | null) => ActivityDto;
  activity: () => ActivityDto[];
};

const hook = <K extends keyof Hooks>(page: Page, name: K, ...args: Parameters<Hooks[K]>) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);

async function openActivity(page: Page) {
  await page.getByTestId('tab-activity').click();
  await expect(page.getByTestId('activity-panel')).toBeVisible();
}

test.describe('CSV export helper', () => {
  test('escapes quotes, commas, newlines and spreadsheet formulas', () => {
    const csv = activityToCsv([
      { id: '1', ts: '2026-01-01T00:00:00Z', actor: 'copilot', action: 'strategy.save', kind: 'strategy', target_id: 'a.py', summary: 'Saved "a", twice', ok: true, details: null },
      { id: '2', ts: '2026-01-01T00:00:01Z', actor: 'user', action: 'model.train', kind: 'model', target_id: 'm', summary: '=HYPERLINK("http://x")', ok: false, details: 'line1\nline2' },
    ]);
    const lines = csv.split('\n');
    expect(lines[0]).toBe('time,actor,action,kind,target,status,summary,details');
    expect(lines[1]).toBe('2026-01-01T00:00:00Z,copilot,strategy.save,strategy,a.py,ok,"Saved ""a"", twice",');
    expect(csv).toContain(`"'=HYPERLINK(""http://x"")"`);
    expect(csv).toContain('"line1\nline2"');
  });
});

test.describe('Activity panel', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-activity')).toBeVisible({ timeout: 10000 });
  });

  test('empty state', async ({ page }) => {
    await openActivity(page);
    await expect(page.getByTestId('activity-empty')).toBeVisible();
    await expect(page.getByTestId('activity-export-csv')).toBeDisabled();
  });

  test('shows what the Copilot did, newest first, and updates live', async ({ page }) => {
    await openActivity(page);
    await hook(page, 'createStrategyAsCopilot', 'alpha.py');
    const rows = page.getByTestId('activity-row');
    await expect(rows).toHaveCount(1);
    await expect(rows.first().getByTestId('activity-actor')).toHaveText('Copilot');
    await expect(rows.first().getByTestId('activity-action')).toHaveText('Saved strategy');
    await expect(rows.first().getByTestId('activity-summary')).toContainText('alpha.py');
    await expect(rows.first().getByTestId('activity-status')).toHaveText('ok');

    const modelId = (await hook(page, 'trainModelAsCopilot', 'xgboost')) as string;
    await expect(rows).toHaveCount(2);
    await expect(rows.first().getByTestId('activity-target')).toContainText(modelId);
    await expect(rows.nth(1).getByTestId('activity-summary')).toContainText('alpha.py');

    await hook(page, 'createAutomationAsCopilot', 'Watcher', modelId);
    await expect(rows).toHaveCount(3);
    await expect(rows.first().getByTestId('activity-summary')).toContainText('Watcher');
    await expect(page.getByTestId('activity-count')).toHaveText('3 entries');
  });

  test('filters by kind, actor, status, text and date', async ({ page }) => {
    await hook(page, 'createStrategyAsCopilot', 'alpha.py');
    await hook(page, 'recordActivity', 'user', 'model.train', 'model', 'm1', 'Trained random_forest on nifty');
    await hook(page, 'recordActivity', 'automation', 'order.place', 'order', 'o1', 'Automation Watcher bought 1 INFY', true);
    await hook(page, 'recordActivity', 'copilot', 'model.train', 'model', 'm2', 'Train failed', false, 'Traceback: not enough rows');
    // An entry from last month: backdate it directly in the mock log.
    await hook(page, 'recordActivity', 'system', 'system.start', 'system', 'app', 'App started long ago');
    await page.evaluate(() => {
      const list = (window as unknown as { __apexMock: Hooks }).__apexMock.activity();
      list[0].ts = new Date(Date.now() - 30 * 86_400_000).toISOString();
    });
    await openActivity(page);
    const rows = page.getByTestId('activity-row');
    await expect(rows).toHaveCount(5);

    await page.getByTestId('activity-filter-kind').selectOption('model');
    await expect(rows).toHaveCount(2);
    await page.getByTestId('activity-filter-actor').selectOption('copilot');
    await expect(rows).toHaveCount(1);
    await expect(rows.first()).toContainText('Train failed');
    await expect(page.getByTestId('activity-count')).toHaveText('1 of 5 entries');
    await page.getByTestId('activity-clear-filters').click();
    await expect(rows).toHaveCount(5);

    await page.getByTestId('activity-filter-status').selectOption('failed');
    await expect(rows).toHaveCount(1);
    await page.getByTestId('activity-filter-status').selectOption('ok');
    await expect(rows).toHaveCount(4);
    await page.getByTestId('activity-filter-status').selectOption('');

    await page.getByTestId('activity-search').fill('traceback');
    await expect(rows).toHaveCount(1);
    await page.getByTestId('activity-search').fill('INFY');
    await expect(rows).toHaveCount(1);
    await expect(rows.first().getByTestId('activity-actor')).toHaveText('Automation');
    await page.getByTestId('activity-search').fill('no such text');
    await expect(page.getByTestId('activity-no-match')).toBeVisible();
    await page.getByTestId('activity-no-match-clear').click();
    await expect(rows).toHaveCount(5);

    await page.getByTestId('activity-filter-date').selectOption('week');
    await expect(rows).toHaveCount(4);
    await expect(page.getByText('App started long ago')).toHaveCount(0);
    await page.getByTestId('activity-filter-date').selectOption('all');
    await expect(rows).toHaveCount(5);
  });

  test('failed entries stand out and show their error; details expand', async ({ page }) => {
    await hook(page, 'recordActivity', 'user', 'model.train', 'model', 'm1', 'Trained fine', true, '{"rows":480,"algorithm":"random_forest"}');
    await hook(page, 'recordActivity', 'copilot', 'strategy.save', 'strategy', 'strategies/x.py', 'Could not save x.py', false, 'PermissionError: read-only\n  at <script>alert(1)</script>');
    await openActivity(page);
    const failed = page.locator('[data-testid="activity-row"][data-ok="false"]');
    const fine = page.locator('[data-testid="activity-row"][data-ok="true"]');
    await expect(failed).toHaveCount(1);
    await expect(failed.getByTestId('activity-status')).toHaveText('failed');
    await expect(failed.getByTestId('activity-error-preview')).toContainText('PermissionError: read-only');
    await expect(fine.getByTestId('activity-error-preview')).toHaveCount(0);
    const failedBg = await failed.evaluate((el) => getComputedStyle(el).backgroundColor);
    const fineBg = await fine.evaluate((el) => getComputedStyle(el).backgroundColor);
    expect(failedBg).not.toBe(fineBg);

    await expect(failed.getByTestId('activity-details')).toHaveCount(0);
    await failed.getByTestId('activity-row-toggle').click();
    const details = failed.getByTestId('activity-details');
    await expect(details).toContainText('PermissionError: read-only');
    // Untrusted text is shown as text, never as markup.
    await expect(details).toContainText('<script>alert(1)</script>');
    expect(await details.evaluate((el) => el.querySelector('script'))).toBeNull();
    expect(await details.evaluate((el) => getComputedStyle(el).fontFamily.toLowerCase())).toContain('mono');

    await fine.getByTestId('activity-row-toggle').click();
    await expect(fine.getByTestId('activity-details')).toContainText('"algorithm": "random_forest"');
    await fine.getByTestId('activity-row-toggle').click();
    await expect(fine.getByTestId('activity-details')).toHaveCount(0);
  });

  test('exports the filtered list as CSV and copies it as JSON', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await hook(page, 'recordActivity', 'user', 'model.train', 'model', 'm1', 'Trained, with a comma');
    await hook(page, 'createStrategyAsCopilot', 'alpha.py');
    await openActivity(page);
    await page.getByTestId('activity-filter-actor').selectOption('copilot');
    await expect(page.getByTestId('activity-row')).toHaveCount(1);

    const [download] = await Promise.all([page.waitForEvent('download'), page.getByTestId('activity-export-csv').click()]);
    expect(download.suggestedFilename()).toMatch(/^apex-activity-.*\.csv$/);
    const path = await download.path();
    const text = (await import('node:fs')).readFileSync(path, 'utf8');
    expect(text.split('\n')).toHaveLength(2);
    expect(text).toContain('copilot,strategy.save,strategy,strategies/alpha.py,ok,Saved alpha.py');
    expect(text).not.toContain('with a comma');
    await expect(page.getByTestId('activity-notice')).toContainText('Exported 1 entry');

    await page.getByTestId('activity-copy-json').click();
    await expect(page.getByTestId('activity-notice')).toContainText('Copied 1 entry as JSON');
    const clip = JSON.parse(await page.evaluate(() => navigator.clipboard.readText())) as ActivityDto[];
    expect(clip).toHaveLength(1);
    expect(clip[0]).toMatchObject({ actor: 'copilot', action: 'strategy.save', ok: true });

    await page.getByTestId('activity-clear-filters').click();
    await page.getByTestId('activity-copy-json').click();
    const all = JSON.parse(await page.evaluate(() => navigator.clipboard.readText())) as ActivityDto[];
    expect(all).toHaveLength(2);
  });

  test('clicking a target opens the strategy or the model', async ({ page }) => {
    const modelId = (await hook(page, 'trainModelAsCopilot', 'random_forest')) as string;
    await hook(page, 'createStrategyAsCopilot', 'alpha.py');
    await openActivity(page);
    await page.locator('[data-testid="activity-row"]', { hasText: 'alpha.py' }).getByTestId('activity-target').click();
    await expect(page.getByTestId('strategy-ide')).toBeVisible();

    await openActivity(page);
    await page.locator('[data-testid="activity-row"]', { hasText: 'Trained random_forest' }).getByTestId('activity-target').click();
    await expect(page.getByTestId('ml-workbench')).toBeVisible();
    await expect(page.getByTestId('model-detail')).toHaveAttribute('data-model-id', modelId);
  });

  test('show only one model\'s activity, from a row and from the model page', async ({ page }) => {
    const a = (await hook(page, 'trainModelAsCopilot', 'random_forest')) as string;
    const b = (await hook(page, 'trainModelAsCopilot', 'xgboost')) as string;
    await hook(page, 'createAutomationAsCopilot', 'Watcher', a);
    await openActivity(page);
    const rows = page.getByTestId('activity-row');
    await expect(rows).toHaveCount(3);

    await page.locator('[data-testid="activity-row"]', { hasText: b }).getByTestId('activity-only-target').click();
    await expect(rows).toHaveCount(1);
    await expect(page.getByTestId('activity-target-chip')).toContainText(b);
    await page.getByTestId('activity-target-clear').click();
    await expect(rows).toHaveCount(3);

    // Deep link from the model detail.
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-models').click();
    await page.locator(`[data-model-id="${a}"]`).click();
    await page.getByTestId('model-activity-open').click();
    await expect(page.getByTestId('activity-panel')).toBeVisible();
    await expect(page.getByTestId('activity-target-chip')).toContainText(a);
    await expect(rows).toHaveCount(1);
    await expect(rows.first().getByTestId('activity-target')).toContainText(a);
  });

  test('a user edit in the Models tab is attributed to You', async ({ page }) => {
    const id = (await hook(page, 'trainModelAsCopilot', 'random_forest')) as string;
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-models').click();
    await page.locator(`[data-model-id="${id}"]`).click();
    await page.getByTestId('model-name').click();
    await page.getByTestId('model-name-input').fill('Renamed');
    await page.getByTestId('model-name-input').press('Enter');
    await expect(page.getByTestId('model-name')).toHaveText('Renamed');
    await openActivity(page);
    const first = page.getByTestId('activity-row').first();
    await expect(first.getByTestId('activity-actor')).toHaveText('You');
    await expect(first.getByTestId('activity-action')).toHaveText('Edited model');
  });

  test('an entry without a target has no Open button', async ({ page }) => {
    await hook(page, 'recordActivity', 'user', 'model.train', 'model', '', 'Training random_forest failed', false, 'boom');
    await openActivity(page);
    const row = page.getByTestId('activity-row').filter({ hasText: 'Training random_forest failed' });
    await expect(row).toHaveCount(1);
    await expect(row.getByTestId('activity-target')).toHaveCount(0);
    await expect(row.getByTestId('activity-only-target')).toHaveCount(0);
    await expect(row.getByTestId('activity-target-plain')).toHaveText('model');
  });
});
