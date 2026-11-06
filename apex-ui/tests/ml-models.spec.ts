import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { bestIndexes, modelVerdict, paramRows, relativeTime } from '../src/components/ml/modelUtils';
import type { CvFoldDto, ModelDetailDto } from '../src/lib/types';

type Hooks = {
  trainModelAsCopilot: (algorithm?: string) => string;
  trainModelCustom: (patch?: Partial<ModelDetailDto>, actor?: string) => string;
  createAutomationAsCopilot: (name?: string, modelId?: string) => string;
  failNext: (cmd: string, message?: string) => void;
  delayNext: (cmd: string, ms?: number) => void;
  editModelAsCopilot: (modelId: string, patch: Partial<ModelDetailDto>) => void;
};
const hook = <K extends keyof Hooks>(page: Page, name: K, ...args: Parameters<Hooks[K]>) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);

const fold = (n: number, accuracy: number, baseline: number): CvFoldDto => ({
  fold: n, train_rows: 200, test_rows: 60, accuracy, f1: accuracy - 0.02, precision: accuracy, recall: accuracy,
  baseline_accuracy: baseline, trade_return_pct: n % 2 ? 1.5 : -0.8, hit_rate: 0.52, trades: 12,
});

const trading = (over: Partial<NonNullable<ModelDetailDto['trading']>> = {}) => ({
  strategy_return_pct: 8, buy_hold_return_pct: 5, hit_rate: 0.55, trades: 60, max_drawdown_pct: -4, sharpe: 1.2, ...over,
});

async function make(page: Page, patch: Partial<ModelDetailDto> = {}, actor = 'user'): Promise<string> {
  return page.evaluate(([p, a]) => (window as unknown as { __apexMock: Hooks }).__apexMock.trainModelCustom(p as Partial<ModelDetailDto>, a as string), [patch, actor] as const);
}

async function openModels(page: Page) {
  await page.getByTestId('tab-ml').click();
  await page.getByTestId('ml-tab-models').click();
  await expect(page.getByTestId('ml-models-tab')).toBeVisible();
}

test.describe('verdict and compare logic', () => {
  const base = { rows: 500, trading: trading() };
  const folds = [fold(1, 0.6, 0.5), fold(2, 0.6, 0.5)];

  test('beats the majority class', () => {
    const v = modelVerdict({ ...base, metrics: { accuracy: 0.6 }, cv_folds: folds });
    expect(v.tone).toBe('good');
    expect(v.text).toContain('Beats always predicting the majority class by 10.0 pts');
    expect(v.warnings).toHaveLength(0);
  });

  test('no edge and worse than baseline are warned about', () => {
    expect(modelVerdict({ ...base, metrics: { accuracy: 0.505 }, cv_folds: folds }).tone).toBe('warn');
    const bad = modelVerdict({ ...base, metrics: { accuracy: 0.4 }, cv_folds: folds });
    expect(bad.tone).toBe('bad');
    expect(bad.text).toContain('Worse than always predicting the majority class by 10.0 pts');
  });

  test('few trades or few rows add warnings; behind buy-and-hold downgrades a good model', () => {
    const v = modelVerdict({ metrics: { accuracy: 0.6 }, cv_folds: folds, rows: 120, trading: trading({ trades: 12, strategy_return_pct: 1, buy_hold_return_pct: 4 }) });
    expect(v.warnings.map((w) => w.id).sort()).toEqual(['rows', 'trades']);
    expect(v.tone).toBe('warn');
    expect(v.text).toContain('behind buy and hold');
    expect(modelVerdict({ ...base, trading: trading({ trades: 30 }), metrics: { accuracy: 0.6 }, cv_folds: folds }).warnings).toHaveLength(0);
  });

  test('no baseline recorded is reported as such', () => {
    expect(modelVerdict({ rows: null, trading: null, metrics: { accuracy: 0.6 }, cv_folds: [] }).tone).toBe('info');
  });

  test('bestIndexes picks the leaders and ignores ties and gaps', () => {
    expect(bestIndexes([0.5, 0.7, 0.6])).toEqual([1]);
    expect(bestIndexes([-4, -2, -9])).toEqual([1]);
    expect(bestIndexes([3, 1], false)).toEqual([1]);
    expect(bestIndexes([0.5, 0.5])).toEqual([]);
    expect(bestIndexes([0.5, null])).toEqual([]);
  });

  test('paramRows marks changed hyperparameters against defaults', () => {
    const rows = paramRows('random_forest', { n_estimators: 300, max_depth: null, extra: 1 });
    const by = Object.fromEntries(rows.map((r) => [r.key, r]));
    expect(by.n_estimators.changed).toBe(true);
    expect(by.max_depth.changed).toBe(false);
    expect(by.extra.changed).toBe(true);
    expect(by.min_samples_leaf.changed).toBe(false);
  });

  test('relativeTime', () => {
    const now = Date.parse('2026-01-01T12:00:00Z');
    expect(relativeTime('2026-01-01T11:59:50Z', now)).toBe('just now');
    expect(relativeTime('2026-01-01T11:00:00Z', now)).toBe('1 h ago');
  });
});

test.describe('Models tab', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  });

  test('empty state points to the Train tab', async ({ page }) => {
    await openModels(page);
    await expect(page.getByTestId('ml-models-empty')).toBeVisible();
    await page.getByTestId('ml-models-empty-train').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
  });

  test('open a model from the list and see every section', async ({ page }) => {
    const id = await make(page, { display_name: 'Alpha run', notes: 'first try' });
    await openModels(page);
    const row = page.getByTestId('ml-model-row');
    await expect(row).toHaveCount(1);
    await expect(row.getByTestId('ml-model-name')).toHaveText('Alpha run');
    await expect(row.getByTestId('ml-model-source')).toHaveText('You');
    await expect(row.getByTestId('ml-model-accuracy')).not.toHaveText('n/a');
    await expect(page.getByTestId('model-detail')).toHaveCount(0);
    await row.click();
    const detail = page.getByTestId('model-detail');
    await expect(detail).toHaveAttribute('data-model-id', id);
    await expect(page.getByTestId('model-name')).toHaveText('Alpha run');
    await expect(page.getByTestId('model-notes')).toHaveValue('first try');
    for (const m of ['accuracy', 'f1', 'precision', 'recall', 'trading-return', 'hit-rate', 'max-drawdown', 'sharpe']) {
      await expect(page.getByTestId(`model-metric-${m}`)).toBeVisible();
    }
    await expect(page.getByTestId('cv-row')).toHaveCount(5);
    await expect(page.getByTestId('cv-accuracy-chart')).toBeVisible();
    await expect(page.getByTestId('feature-bars-row')).toHaveCount(3);
    await expect(page.getByTestId('config-algorithm')).toHaveText('Random Forest');
    await expect(page.getByTestId('prov-size')).toHaveText('470.8 KB');
    await expect(page.getByTestId('used-by-empty')).toBeVisible();
    await expect(page.getByTestId('model-activity-row').first()).toContainText('Trained random_forest');
    await page.getByTestId('model-section-toggle-log').click();
    await expect(page.getByTestId('model-log')).toContainText('Starting training run');
    await page.getByTestId('model-detail-close').click();
    await expect(page.getByTestId('model-detail')).toHaveCount(0);
  });

  test('name, tags and notes are saved and survive reopening the tab', async ({ page }) => {
    await make(page, {});
    await openModels(page);
    await page.getByTestId('ml-model-row').click();

    await page.getByTestId('model-name').click();
    await page.getByTestId('model-name-input').fill('Momentum v2');
    await page.getByTestId('model-name-input').press('Enter');
    await expect(page.getByTestId('model-name')).toHaveText('Momentum v2');

    await page.getByTestId('model-tag-input').fill('intraday');
    await page.getByTestId('model-tag-input').press('Enter');
    await page.getByTestId('model-tag-input').fill('keep');
    await page.getByTestId('model-tag-input').press('Enter');
    await expect(page.getByTestId('model-tag-intraday')).toBeVisible();
    await page.getByTestId('model-tag-remove-keep').click();
    await expect(page.getByTestId('model-tag-keep')).toHaveCount(0);

    await expect(page.getByTestId('model-notes-save')).toBeDisabled();
    await page.getByTestId('model-notes').fill('Trained on 2y of NIFTY. Watch the fold 3 dip.');
    await expect(page.getByTestId('model-notes-unsaved')).toBeVisible();
    await page.getByTestId('model-notes-save').click();
    await expect(page.getByTestId('model-notes-unsaved')).toHaveCount(0);

    // Leave the tab and come back: the data must come from the store, not component state.
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-tab-models').click();
    await expect(page.getByTestId('ml-model-name')).toHaveText('Momentum v2');
    await expect(page.getByTestId('ml-model-tags')).toContainText('intraday');
    await page.getByTestId('ml-model-row').click();
    await expect(page.getByTestId('model-name')).toHaveText('Momentum v2');
    await expect(page.getByTestId('model-tag-intraday')).toBeVisible();
    await expect(page.getByTestId('model-tag-keep')).toHaveCount(0);
    await expect(page.getByTestId('model-notes')).toHaveValue('Trained on 2y of NIFTY. Watch the fold 3 dip.');
    await expect(page.getByTestId('model-activity')).toContainText('Edited details');
  });

  test('a failed save rolls the optimistic edit back and says so', async ({ page }) => {
    await make(page, { display_name: 'Stable name' });
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await page.evaluate(() => (window as unknown as { __apexMock: Hooks }).__apexMock.failNext('update_ml_model', 'disk is read-only'));
    await page.getByTestId('model-name').click();
    await page.getByTestId('model-name-input').fill('Doomed name');
    await page.getByTestId('model-name-input').press('Enter');
    await expect(page.getByTestId('model-save-error')).toContainText('disk is read-only');
    await expect(page.getByTestId('model-name')).toHaveText('Stable name');
    await expect(page.getByTestId('ml-model-name')).toHaveText('Stable name');
  });

  test('verdict, warnings and legacy note in the detail view', async ({ page }) => {
    const good = await make(page, { display_name: 'good', metrics: { accuracy: 0.62, f1: 0.6, precision: 0.6, recall: 0.6 }, cv_folds: [fold(1, 0.62, 0.5), fold(2, 0.62, 0.5)], rows: 800, trading: trading() });
    await make(page, { display_name: 'thin', metrics: { accuracy: 0.5, f1: 0.5, precision: 0.5, recall: 0.5 }, cv_folds: [fold(1, 0.5, 0.5)], rows: 120, trading: trading({ trades: 9 }) });
    await make(page, { display_name: 'old', cv_folds: [], feature_importance: [], trading: null, rows: null, source: 'unknown' });
    await openModels(page);

    await page.locator(`[data-model-id="${good}"]`).click();
    await expect(page.getByTestId('model-verdict')).toHaveAttribute('data-tone', 'good');
    await expect(page.getByTestId('model-verdict')).toContainText('Beats always predicting the majority class by 12.0 pts');
    await expect(page.getByTestId('model-warning-trades')).toHaveCount(0);
    await expect(page.getByTestId('model-warning-rows')).toHaveCount(0);

    await page.locator('[data-testid="ml-model-row"]', { hasText: 'thin' }).click();
    await expect(page.getByTestId('model-verdict')).toHaveAttribute('data-tone', 'warn');
    await expect(page.getByTestId('model-verdict')).toContainText('No better than always predicting');
    await expect(page.getByTestId('model-warning-trades')).toContainText('Only 9 trades');
    await expect(page.getByTestId('model-warning-rows')).toContainText('Only 120 rows');

    await page.locator('[data-testid="ml-model-row"]', { hasText: 'old' }).click();
    await expect(page.getByTestId('model-legacy-banner')).toContainText('recorded before detailed run data was kept');
    await expect(page.getByTestId('cv-legacy-note')).toBeVisible();
    await expect(page.getByTestId('cv-table')).toHaveCount(0);
    await expect(page.getByTestId('cv-accuracy-chart')).toHaveCount(0);
    await expect(page.getByTestId('features-empty')).toBeVisible();
    await expect(page.getByTestId('model-metric-trading-return-value')).toHaveText('n/a');
    await expect(page.getByTestId('model-source')).toHaveText('Unknown');
  });

  test('hyperparameters show defaults against changed values', async ({ page }) => {
    await make(page, { algorithm: 'random_forest', hyperparams: { n_estimators: 300 }, lag_periods: [1, 5], n_splits: 4 });
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await expect(page.getByTestId('hyper-row-n_estimators')).toHaveAttribute('data-changed', 'true');
    await expect(page.getByTestId('hyper-row-n_estimators')).toContainText('300');
    await expect(page.getByTestId('hyper-row-n_estimators')).toContainText('100');
    await expect(page.getByTestId('hyper-row-min_samples_leaf')).toHaveAttribute('data-changed', 'false');
    await expect(page.getByTestId('config-lags')).toHaveText('1, 5');
    await expect(page.getByTestId('config-splits')).toHaveText('4');
  });

  test('search, sort and filters narrow the registry', async ({ page }) => {
    const a = await make(page, { display_name: 'rf low', algorithm: 'random_forest', metrics: { accuracy: 0.52 }, tags: ['swing'], trading: trading({ strategy_return_pct: 2 }) });
    const b = await make(page, { display_name: 'gb high', algorithm: 'gradient_boosting', metrics: { accuracy: 0.71 }, tags: ['intraday'], trading: trading({ strategy_return_pct: 4 }) }, 'copilot');
    const c = await make(page, { display_name: 'lr mid', algorithm: 'logistic_regression', metrics: { accuracy: 0.6 }, trading: trading({ strategy_return_pct: 9 }) });
    await openModels(page);
    const ids = async () => page.getByTestId('ml-model-row').evaluateAll((els) => els.map((e) => e.getAttribute('data-model-id')));
    await expect(page.getByTestId('ml-models-count')).toHaveText('3 models');

    await page.getByTestId('ml-models-sort').selectOption('accuracy');
    expect(await ids()).toEqual([b, c, a]);
    await page.getByTestId('ml-models-sort').selectOption('trading');
    expect(await ids()).toEqual([c, b, a]);

    await page.getByTestId('ml-models-search').fill('boost');
    expect(await ids()).toEqual([b]);
    await expect(page.getByTestId('ml-models-count')).toHaveText('1 of 3 models');
    await page.getByTestId('ml-models-search').fill('zzz-nothing');
    await expect(page.getByTestId('ml-models-no-match')).toBeVisible();
    await page.getByTestId('ml-models-no-match-clear').click();
    await expect(page.getByTestId('ml-model-row')).toHaveCount(3);

    await page.getByTestId('ml-models-filter-algorithm').selectOption('random_forest');
    expect(await ids()).toEqual([a]);
    await page.getByTestId('ml-models-clear-filters').click();
    await page.getByTestId('ml-models-filter-source').selectOption('copilot');
    expect(await ids()).toEqual([b]);
    await page.getByTestId('ml-models-clear-filters').click();
    await page.getByTestId('ml-models-filter-tag').selectOption('swing');
    expect(await ids()).toEqual([a]);
  });

  test('arrow keys move the selection and Escape closes the detail', async ({ page }) => {
    const a = await make(page, { display_name: 'older' });
    const b = await make(page, { display_name: 'newer' });
    await openModels(page);
    await page.locator(`[data-row-id="${b}"]`).focus();
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('model-detail')).toHaveAttribute('data-model-id', b);
    await page.keyboard.press('ArrowDown');
    await expect(page.getByTestId('model-detail')).toHaveAttribute('data-model-id', a);
    await page.keyboard.press('ArrowUp');
    await expect(page.getByTestId('model-detail')).toHaveAttribute('data-model-id', b);
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('model-detail')).toHaveCount(0);
  });

  test('compare highlights the best value per metric and aligns features and config', async ({ page }) => {
    const mk = (name: string, acc: number, ret: number, dd: number, hp: number) => make(page, {
      display_name: name, metrics: { accuracy: acc, f1: acc - 0.05, precision: acc, recall: acc },
      cv_folds: [fold(1, acc, 0.5)], trading: trading({ strategy_return_pct: ret, max_drawdown_pct: dd }),
      hyperparams: { n_estimators: hp }, feature_names: ['ret_1', 'rsi_14'],
      feature_importance: name === 'C' ? [{ feature: 'rsi_14', importance: 0.9 }, { feature: 'vol_20', importance: 0.1 }] : [{ feature: 'ret_1', importance: 0.6 }, { feature: 'rsi_14', importance: 0.4 }],
    });
    const a = await mk('A', 0.55, 12, -3, 100);
    const b = await mk('B', 0.65, 4, -9, 100);
    const c = await mk('C', 0.6, 7, -5, 250);
    await openModels(page);

    await expect(page.getByTestId('ml-models-compare')).toBeDisabled();
    const checks = page.getByTestId('ml-model-check');
    await checks.nth(0).check();
    await expect(page.getByTestId('ml-models-compare-hint')).toBeVisible();
    await expect(page.getByTestId('ml-models-compare')).toBeDisabled();
    await checks.nth(1).check();
    await checks.nth(2).check();
    await page.getByTestId('ml-models-compare').click();
    await expect(page.getByTestId('model-compare')).toBeVisible();

    const best = (metric: string, id: string) => page.getByTestId(`compare-cell-${metric}-${id}`);
    await expect(best('accuracy', b)).toHaveAttribute('data-best', 'true');
    await expect(best('accuracy', a)).toHaveAttribute('data-best', 'false');
    await expect(best('trading-return', a)).toHaveAttribute('data-best', 'true');
    await expect(best('trading-return', b)).toHaveAttribute('data-best', 'false');
    await expect(best('max-drawdown', a)).toHaveAttribute('data-best', 'true');
    await expect(best('hit-rate', a)).toHaveAttribute('data-best', 'false');
    await expect(best('accuracy', b)).toHaveText('65.0%');

    await expect(page.getByTestId('compare-feature-row')).toHaveCount(3);
    await expect(page.getByTestId(`compare-feature-vol_20-${c}`)).toHaveAttribute('data-value', '0.1');
    await expect(page.getByTestId(`compare-feature-vol_20-${a}`)).toHaveAttribute('data-value', '');

    await expect(page.getByTestId(`compare-config-hp-n_estimators-${c}`)).toHaveAttribute('data-differs', 'true');
    await expect(page.getByTestId(`compare-config-n_splits-${c}`)).toHaveAttribute('data-differs', 'false');
    await page.getByTestId('compare-only-diffs').check();
    await expect(page.getByTestId(`compare-config-n_splits-${c}`)).toHaveCount(0);
    await expect(page.getByTestId(`compare-config-hp-n_estimators-${a}`)).toBeVisible();

    await page.getByTestId(`compare-open-${b}`).click();
    await expect(page.getByTestId('model-detail')).toHaveAttribute('data-model-id', b);
  });

  test('delete asks for confirmation and lists the automations that use the model', async ({ page }) => {
    const id = await make(page, { display_name: 'Used model' });
    const other = await make(page, { display_name: 'Spare model' });
    await page.evaluate(([n, m]) => (window as unknown as { __apexMock: Hooks }).__apexMock.createAutomationAsCopilot(n, m), ['Nightly NIFTY', id]);
    await openModels(page);
    await page.locator(`[data-model-id="${id}"]`).click();
    await expect(page.getByTestId('used-by-row')).toHaveCount(1);
    await expect(page.getByTestId('used-by-name')).toHaveText('Nightly NIFTY');
    await expect(page.getByTestId('used-by-state')).toHaveText('enabled');

    await page.getByTestId('model-delete').click();
    await expect(page.getByTestId('ml-delete-dialog')).toBeVisible();
    await expect(page.getByTestId('ml-delete-automation')).toContainText('Nightly NIFTY');
    await expect(page.getByTestId('ml-delete-warning')).toContainText('will stop working');
    await page.getByTestId('ml-delete-cancel').click();
    await expect(page.getByTestId('ml-delete-dialog')).toHaveCount(0);
    await expect(page.locator(`[data-model-id="${id}"]`).first()).toBeVisible();

    // A model nothing uses gets the plain confirmation.
    await page.getByTestId('ml-model-check').nth(0).check();
    await page.getByTestId('ml-models-bulk-delete').click();
    await expect(page.getByTestId('ml-delete-no-usage')).toBeVisible();
    await page.getByTestId('ml-delete-cancel').click();
    await page.getByTestId('ml-model-check').nth(0).uncheck();

    await page.getByTestId('model-delete').click();
    await page.getByTestId('ml-delete-confirm').click();
    await expect(page.getByTestId('ml-delete-dialog')).toHaveCount(0);
    await expect(page.locator(`[data-model-id="${id}"]`)).toHaveCount(0);
    await expect(page.locator(`[data-model-id="${other}"]`).first()).toBeVisible();
    await expect(page.getByTestId('model-detail')).toHaveCount(0);
  });

  test('bulk delete removes every ticked model', async ({ page }) => {
    await make(page, { display_name: 'one' });
    await make(page, { display_name: 'two' });
    const keep = await make(page, { display_name: 'keep' });
    await openModels(page);
    await page.locator('[data-testid="ml-model-row"]', { hasText: 'one' }).getByTestId('ml-model-check').check();
    await page.locator('[data-testid="ml-model-row"]', { hasText: 'two' }).getByTestId('ml-model-check').check();
    await page.getByTestId('ml-models-bulk-delete').click();
    await expect(page.getByTestId('ml-delete-models')).toContainText('one');
    await expect(page.getByTestId('ml-delete-models')).toContainText('two');
    await page.getByTestId('ml-delete-confirm').click();
    await expect(page.getByTestId('ml-model-row')).toHaveCount(1);
    await expect(page.locator(`[data-model-id="${keep}"]`)).toBeVisible();
  });

  test('a model the Copilot trains shows a new badge until it is opened', async ({ page }) => {
    await openModels(page);
    await expect(page.getByTestId('ml-models-empty')).toBeVisible();
    const id = await page.evaluate(() => (window as unknown as { __apexMock: Hooks }).__apexMock.trainModelAsCopilot('gradient_boosting'));
    const row = page.locator(`[data-model-id="${id}"]`).first();
    await expect(row).toBeVisible();
    await expect(row.getByTestId('ml-model-new-badge')).toBeVisible();
    await expect(row.getByTestId('ml-model-source')).toHaveText('Copilot');
    await row.click();
    await expect(page.getByTestId('model-detail')).toBeVisible();
    await expect(row.getByTestId('ml-model-new-badge')).toHaveCount(0);
    await expect(page.getByTestId('model-source')).toHaveText('Copilot');
  });

  test('a model that you train does not get the new badge', async ({ page }) => {
    await openModels(page);
    await make(page, {}, 'user');
    await expect(page.getByTestId('ml-model-row')).toHaveCount(1);
    await expect(page.getByTestId('ml-model-new-badge')).toHaveCount(0);
  });

  test('Score now and Signal history', async ({ page }) => {
    await make(page, {});
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await page.getByTestId('try-score').click();
    await expect(page.getByTestId('try-error')).toContainText('Enter a symbol');
    await page.getByTestId('try-symbol').fill('RELIANCE.NS');
    await page.getByTestId('try-score').click();
    await expect(page.getByTestId('try-result')).toBeVisible();
    await expect(page.getByTestId('try-result-signal')).toHaveText(/LONG|SHORT|FLAT/);
    await expect(page.getByTestId('try-result-probability')).toHaveText(/\d+\.\d%/);
    await page.getByTestId('try-history').click();
    await expect(page.getByTestId('signal-chart')).toBeVisible();
    await expect(page.getByTestId('signal-price-line')).toBeVisible();
    expect(await page.getByTestId('signal-marker-long').count()).toBeGreaterThan(0);
    expect(await page.getByTestId('signal-marker-short').count()).toBeGreaterThan(0);
    expect(await page.getByTestId('signal-marker-flat').count()).toBeGreaterThan(0);
  });

  test('create an automation from a model and see it under Used by', async ({ page }) => {
    const id = await make(page, { display_name: 'Auto source' });
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await expect(page.getByTestId('used-by-empty')).toBeVisible();
    await expect(page.getByTestId('auto-note')).toContainText('paper broker');
    await expect(page.getByTestId('auto-broker')).toHaveValue('paper');

    await page.getByTestId('auto-name').fill('Morning momentum');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-error')).toContainText('symbol');
    await page.getByTestId('auto-symbol').fill('INFY.NS');
    await page.getByTestId('auto-qty').fill('3');
    await page.getByTestId('auto-threshold').fill('1.5');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-error')).toContainText('between 0 and 1');
    await page.getByTestId('auto-threshold').fill('0.7');
    await page.getByTestId('auto-create').click();
    await expect(page.getByTestId('auto-created')).toContainText('Morning momentum');
    await expect(page.getByTestId('used-by-row')).toHaveCount(1);
    await expect(page.getByTestId('used-by-name')).toHaveText('Morning momentum');
    await page.getByTestId('model-detail-close').click();
    await expect(page.getByTestId('ml-model-used')).toHaveText('1');
    await page.getByTestId('ml-model-row').click();

    await page.getByTestId('used-by-open').click();
    await expect(page.getByTestId('automations-panel')).toBeVisible();
    await expect(page.getByTestId('automation-row')).toContainText('Morning momentum');
    await expect(page.getByTestId('automation-row')).toContainText(id);
  });

  test('the dataset link and activity link navigate away', async ({ page }) => {
    await make(page, { dataset: 'nifty_daily' });
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await page.getByTestId('config-dataset-link').click();
    await expect(page.getByTestId('ml-tab-data')).toHaveAttribute('aria-selected', 'true');
    await page.getByTestId('ml-tab-models').click();
    await page.getByTestId('ml-model-row').click();
    await page.getByTestId('model-activity-open').click();
    await expect(page.getByTestId('activity-panel')).toBeVisible();
    await expect(page.getByTestId('activity-target-chip')).toBeVisible();
  });

  test('in a narrow window the list and the detail take turns', async ({ page }) => {
    await page.setViewportSize({ width: 520, height: 800 });
    const id = await make(page, { display_name: 'Narrow one' });
    await openModels(page);
    await expect(page.getByTestId('ml-models-registry')).toBeVisible();
    await page.locator(`[data-model-id="${id}"]`).click();
    await expect(page.getByTestId('model-detail')).toBeVisible();
    await expect(page.getByTestId('ml-models-registry')).toBeHidden();
    await page.getByTestId('model-detail-close').click();
    await expect(page.getByTestId('ml-models-registry')).toBeVisible();
    await expect(page.getByTestId('model-detail')).toHaveCount(0);
  });

  test('Retrain hands the model to the workbench', async ({ page }) => {
    await make(page, { display_name: 'Retrain me' });
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
  });

  test('notes edited elsewhere replace a clean draft, and warn about a draft being typed', async ({ page }) => {
    const id = await make(page, { display_name: 'Notes', notes: 'x' });
    await openModels(page);
    await page.getByTestId('ml-model-row').click();
    await expect(page.getByTestId('model-notes')).toHaveValue('x');

    await hook(page, 'editModelAsCopilot', id, { notes: 'y' });
    await expect(page.getByTestId('model-notes')).toHaveValue('y');
    await expect(page.getByTestId('model-notes-unsaved')).toHaveCount(0);
    await expect(page.getByTestId('model-notes-save')).toBeDisabled();

    await page.getByTestId('model-notes').fill('my own text');
    await hook(page, 'editModelAsCopilot', id, { notes: 'z' });
    await expect(page.getByTestId('model-notes-changed')).toBeVisible();
    await expect(page.getByTestId('model-notes')).toHaveValue('my own text');
    await page.getByTestId('model-notes-use-latest').click();
    await expect(page.getByTestId('model-notes')).toHaveValue('z');
    await expect(page.getByTestId('model-notes-changed')).toHaveCount(0);
  });

  test('a slow refresh from an earlier Delete cannot re-target the dialog', async ({ page }) => {
    await make(page, { display_name: 'Model AAA' });
    await make(page, { display_name: 'Model BBB' });
    await openModels(page);
    const rowA = page.locator('[data-testid="ml-model-row"]', { hasText: 'Model AAA' });
    const rowB = page.locator('[data-testid="ml-model-row"]', { hasText: 'Model BBB' });

    await rowA.click();
    await hook(page, 'delayNext', 'get_ml_model', 700);
    await page.getByTestId('model-delete').click();
    await expect(page.getByTestId('ml-delete-models')).toContainText('Model AAA');
    await page.getByTestId('ml-delete-cancel').click();

    await rowB.click();
    await page.getByTestId('model-delete').click();
    await expect(page.getByTestId('ml-delete-models')).toContainText('Model BBB');
    await page.waitForTimeout(1000); // A's slow answer has landed
    await expect(page.getByTestId('ml-delete-models')).toContainText('Model BBB');
    await expect(page.getByTestId('ml-delete-models')).not.toContainText('Model AAA');
  });

  test('coming back to the Models tab does not re-select the model that was requested on an earlier visit', async ({ page }) => {
    const a = await make(page, { display_name: 'Model AAA' });
    const b = await make(page, { display_name: 'Model BBB' });
    await page.getByTestId('tab-ml').click();
    await page.evaluate((id) => (window as any).__apexMlTest.open('model', id), a);
    await expect(page.locator(`[data-testid="ml-model-row"][data-model-id="${a}"]`)).toHaveAttribute('data-selected', 'true');
    await page.locator(`[data-testid="ml-model-row"][data-model-id="${b}"]`).click();
    await expect(page.locator(`[data-testid="ml-model-row"][data-model-id="${b}"]`)).toHaveAttribute('data-selected', 'true');
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-tab-models').click();
    // The tab remounts with nothing selected; it must not jump back to the earlier request (model A).
    await expect(page.getByTestId('ml-models-tab')).toBeVisible();
    await expect(page.locator('[data-testid="ml-model-row"][data-selected="true"]')).toHaveCount(0);
  });
});
