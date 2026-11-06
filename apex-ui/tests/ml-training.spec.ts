import { test, expect, type Page } from '@playwright/test';

/**
 * ML workbench Train tab (web mode: the trainer is replaced by lib/mockMl.ts).
 * Magic dataset names understood by the mock: "slow" makes training take about 1.2 s,
 * "failxgb" makes xgboost fail while the other algorithms succeed.
 */

async function openWorkbench(page: Page) {
  await page.addInitScript(() => { (window as unknown as { __ipcLog: unknown[] }).__ipcLog = []; });
  await page.goto('/');
  await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  await page.getByTestId('tab-ml').click();
  await expect(page.getByTestId('ml-workbench')).toBeVisible({ timeout: 10000 });
}

/** Build a dataset through the Data tab and jump to Train with it selected. */
async function buildAndTrainOn(page: Page, name: string) {
  await page.getByTestId('ml-ds-symbol-input').fill('AAPL');
  await page.getByTestId('ml-ds-symbol-input').press('Enter');
  await page.getByTestId('ml-ds-name').fill(name);
  await page.getByTestId('ml-ds-build').click();
  await expect(page.getByTestId('ml-ds-result-name')).toHaveText(name);
  await page.getByTestId('ml-ds-result-train').click();
  await expect(page.getByTestId('ml-training-form')).toBeVisible();
  await expect(page.getByTestId('ml-train-dataset')).toHaveValue(name);
}

const ipc = (page: Page, cmd: string) =>
  page.evaluate((c) => ((window as unknown as { __ipcLog: { cmd: string; args: any }[] }).__ipcLog).filter((e) => e.cmd === c), cmd);

const lastTrainRequest = async (page: Page) => {
  const calls = await ipc(page, 'train_ml_model');
  return calls[calls.length - 1].args.request;
};

test.describe('ML training form', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); });

  test('asks for a dataset first and links to the Data tab', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-train-no-datasets')).toBeVisible();
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
    await expect(page.getByTestId('ml-train-validation')).toContainText('Pick a dataset');
    await page.getByTestId('ml-train-go-data').click();
    await expect(page.getByTestId('ml-dataset-builder')).toBeVisible();
  });

  test('trains on a dataset, sends the dataset request and shows the result card', async ({ page }) => {
    await buildAndTrainOn(page, 'train-basic');
    await page.getByTestId('ml-train-button').click();

    const result = page.getByTestId('ml-train-result');
    await expect(result).toBeVisible();
    await expect(page.getByTestId('ml-result-accuracy')).toHaveText(/^\d+\.\d%$/);
    for (const k of ['f1', 'precision', 'recall']) await expect(page.getByTestId(`ml-result-${k}`)).toHaveText(/%$/);
    await expect(page.getByTestId('ml-result-trading')).toContainText('+8.40%'); // from the model detail
    await expect(page.getByTestId('ml-result-trading')).toContainText('+5.10%');

    const req = await lastTrainRequest(page);
    expect(req).toMatchObject({
      algorithm: 'random_forest', dataset: 'train-basic', data_path: 'datasets/train-basic.csv',
      target_column: 'target', return_column: 'fwd_ret', n_splits: 5, hyperparams: {}, lag_periods: [],
      trade_mode: 'long_flat', holdout_pct: 0,
    });
    expect(req.feature_columns).toHaveLength(15);

    const modelId = await page.getByTestId('ml-result-model-id').innerText();
    await page.getByTestId('ml-open-model').click();
    await expect(page.getByTestId('ml-models-tab')).toBeVisible();
    expect(modelId).toMatch(/^random_forest_/);
  });

  test('describes each algorithm and lists its own parameters (from the backend catalog)', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    const select = page.getByTestId('ml-algorithm-select');
    await expect(select).toHaveValue('random_forest');
    await expect(page.getByTestId('ml-algorithms-notice')).toHaveCount(0);
    await expect(select.locator('option')).toHaveText([
      'Random forest', 'Extra trees', 'Gradient boosting', 'Histogram gradient boosting', 'Logistic regression', 'XGBoost', 'Custom estimator (Python file)',
    ]);
    await expect(page.getByTestId('ml-algorithm-desc')).toContainText('decision trees');
    for (const k of ['n_estimators', 'max_depth', 'min_samples_leaf', 'class_weight', 'bootstrap', 'criterion']) await expect(page.getByTestId(`ml-hp-${k}`)).toBeVisible();
    await expect(page.getByTestId('ml-hp-learning_rate')).toHaveCount(0);

    await select.selectOption('gradient_boosting');
    await expect(page.getByTestId('ml-algorithm-desc')).toContainText('one at a time');
    for (const k of ['n_estimators', 'learning_rate', 'max_depth', 'subsample']) await expect(page.getByTestId(`ml-hp-${k}`)).toBeVisible();

    await select.selectOption('logistic_regression');
    for (const k of ['C', 'max_iter', 'class_weight']) await expect(page.getByTestId(`ml-hp-${k}`)).toBeVisible();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveCount(0);

    await select.selectOption('xgboost');
    await expect(page.getByTestId('ml-algorithm-desc')).toContainText('strong defaults');
    await expect(page.getByTestId('ml-hp-colsample_bytree')).toBeVisible();
  });

  test('invalid hyperparameters block Train with a message', async ({ page }) => {
    await buildAndTrainOn(page, 'hp-invalid');
    const train = page.getByTestId('ml-train-button');
    await expect(train).toBeEnabled();

    await page.getByTestId('ml-hp-n_estimators').fill('0');
    await expect(page.getByTestId('ml-hp-error-n_estimators')).toContainText('between 1 and 2000');
    await expect(page.getByTestId('ml-train-validation')).toContainText('Trees must be a whole number');
    await expect(train).toBeDisabled();

    await page.getByTestId('ml-hp-n_estimators').fill('150.5');
    await expect(page.getByTestId('ml-hp-error-n_estimators')).toContainText('whole number');
    await expect(train).toBeDisabled();

    await page.getByTestId('ml-hp-n_estimators').fill('');
    await expect(page.getByTestId('ml-hp-error-n_estimators')).toHaveCount(0); // empty means "use the default"
    await expect(train).toBeEnabled();

    await page.getByTestId('ml-hp-n_estimators').fill('3000');
    await expect(train).toBeDisabled();
    await page.getByTestId('ml-hp-reset-n_estimators').click();
    await expect(page.getByTestId('ml-hp-error-n_estimators')).toHaveCount(0);
    await expect(train).toBeEnabled();
    expect(await ipc(page, 'train_ml_model')).toHaveLength(0); // nothing was sent while blocked
  });

  test('CV splits are limited to 2-20', async ({ page }) => {
    await buildAndTrainOn(page, 'cv-limits');
    const train = page.getByTestId('ml-train-button');
    for (const bad of ['1', '21', '', '2.5']) {
      await page.getByTestId('ml-cv-splits').fill(bad);
      await expect(page.getByTestId('ml-train-validation')).toContainText('CV splits must be a whole number from 2 to 20');
      await expect(train).toBeDisabled();
    }
    await page.getByTestId('ml-cv-splits').fill('20');
    await expect(train).toBeEnabled();
    await expect(page.getByTestId('ml-cv-help')).toContainText('embargo');
    await train.click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    expect((await lastTrainRequest(page)).n_splits).toBe(20);
  });

  test('only changed hyperparameters are sent; reset and reset-all restore defaults', async ({ page }) => {
    await buildAndTrainOn(page, 'hp-changed');
    const count = page.getByTestId('ml-hp-changed-count');
    await expect(count).toContainText('0 changed');

    await page.getByTestId('ml-hp-n_estimators').fill('150');
    await page.getByTestId('ml-hp-max_depth').fill('8');
    await page.getByTestId('ml-hp-min_samples_leaf').fill('3');
    await page.getByTestId('ml-hp-min_samples_leaf').fill('1'); // edited back to the default: not a change
    await page.getByTestId('ml-hp-class_weight').selectOption('balanced');
    await expect(count).toContainText('3 changed');

    await page.getByTestId('ml-hp-reset-class_weight').click();
    await expect(page.getByTestId('ml-hp-class_weight')).toHaveValue('');
    await expect(count).toContainText('2 changed');

    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    expect((await lastTrainRequest(page)).hyperparams).toEqual({ n_estimators: 150, max_depth: 8 });

    await page.getByTestId('ml-hp-reset-all').click();
    await expect(count).toContainText('0 changed');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveAttribute('placeholder', '100'); // the default is shown as a placeholder
    await page.getByTestId('ml-train-button').click();
    await expect.poll(async () => (await ipc(page, 'train_ml_model')).length).toBe(2);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({});
  });

  test('hyperparameters are remembered per algorithm', async ({ page }) => {
    await buildAndTrainOn(page, 'hp-per-algo');
    await page.getByTestId('ml-hp-n_estimators').fill('300');
    await page.getByTestId('ml-algorithm-select').selectOption('xgboost');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('');
    await page.getByTestId('ml-hp-learning_rate').fill('0.05');
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('300');
    await page.getByTestId('ml-algorithm-select').selectOption('xgboost');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    expect((await lastTrainRequest(page)).hyperparams).toEqual({ learning_rate: 0.05 });
  });

  test('presets set sensible values for the chosen algorithm', async ({ page }) => {
    await buildAndTrainOn(page, 'presets');
    await page.getByTestId('ml-preset-fast').click();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('50');
    await expect(page.getByTestId('ml-hp-max_depth')).toHaveValue('6');

    await page.getByTestId('ml-preset-thorough').click();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('600');
    await expect(page.getByTestId('ml-hp-class_weight')).toHaveValue('balanced');

    await page.getByTestId('ml-preset-balanced').click();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('200');
    await expect(page.getByTestId('ml-hp-min_samples_leaf')).toHaveValue('10');
    await expect(page.getByTestId('ml-hp-changed-count')).toContainText('4 changed');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    expect((await lastTrainRequest(page)).hyperparams).toEqual({
      n_estimators: 200, max_depth: 8, min_samples_leaf: 10, class_weight: 'balanced',
    });

    await page.getByTestId('ml-algorithm-select').selectOption('gradient_boosting');
    await page.getByTestId('ml-preset-balanced').click();
    await expect(page.getByTestId('ml-hp-learning_rate')).toHaveValue('0.05');
    await expect(page.getByTestId('ml-hp-subsample')).toHaveValue('0.8');
  });

  test('a feature subset narrows feature_columns', async ({ page }) => {
    await buildAndTrainOn(page, 'subset');
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('15 of 15');
    await page.getByTestId('ml-train-feature-rsi_14').uncheck();
    await page.getByTestId('ml-train-feature-vol_10').uncheck();
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('13 of 15');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    const req = await lastTrainRequest(page);
    expect(req.feature_columns).toHaveLength(13);
    expect(req.feature_columns).not.toContain('rsi_14');

    await page.getByTestId('ml-train-features-none').click();
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
    await expect(page.getByTestId('ml-train-validation')).toContainText('Select at least one feature');
    await page.getByTestId('ml-train-features-all').click();
    await expect(page.getByTestId('ml-train-button')).toBeEnabled();
  });

  test('custom CSV mode sends the path, target column and feature list without a dataset', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-train-mode-csv').click();
    await expect(page.getByTestId('ml-data-path')).toHaveValue('data/sample.csv');
    await expect(page.getByTestId('ml-target-column')).toHaveValue('signal');
    await page.getByTestId('ml-features-text').fill('rsi_14, sma_20');
    await page.getByTestId('ml-lag-periods').fill('1, 5');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    const req = await lastTrainRequest(page);
    expect(req).toMatchObject({
      data_path: 'data/sample.csv', target_column: 'signal', feature_columns: ['rsi_14', 'sma_20'], lag_periods: [1, 5], n_splits: 5,
    });
    expect(req.dataset).toBeUndefined();
    expect(req.return_column).toBeUndefined();

    await page.getByTestId('ml-features-text').fill(' , ');
    await expect(page.getByTestId('ml-train-validation')).toContainText('at least one feature column');
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
  });

  test('a running job survives switching tabs, shows a timer and disables Train', async ({ page }) => {
    await buildAndTrainOn(page, 'slow-run');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
    await expect(page.getByTestId('ml-train-button')).toHaveText('Training...');
    await expect(page.getByTestId('ml-train-timer')).toHaveText(/^\d+s$/);
    await expect(page.getByTestId('ml-train-running')).toBeVisible();

    await page.getByTestId('ml-tab-data').click();
    await expect(page.getByTestId('ml-train-running')).toBeVisible(); // indicator stays on the tab bar
    await page.getByTestId('ml-tab-models').click();
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-training-status')).toBeVisible();
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
    await expect(page.getByTestId('ml-compare-run')).toBeDisabled();

    await expect(page.getByTestId('ml-train-result')).toBeVisible({ timeout: 8000 });
    await expect(page.getByTestId('ml-train-button')).toBeEnabled();
    await expect(page.getByTestId('ml-training-status')).toHaveCount(0);
    await expect(page.getByTestId('ml-train-running')).toHaveCount(0);
    expect(await ipc(page, 'train_ml_model')).toHaveLength(1);
  });

  test('shows a trainer failure inline and allows another run', async ({ page }) => {
    await buildAndTrainOn(page, 'failxgb-single');
    await page.getByTestId('ml-algorithm-select').selectOption('xgboost');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-training-error')).toContainText('xgboost is not installed');
    await expect(page.getByTestId('ml-train-result')).toHaveCount(0);
    await expect(page.getByTestId('ml-train-button')).toBeEnabled();
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    await expect(page.getByTestId('ml-training-error')).toHaveCount(0);
  });

  test('compare algorithms: leaderboard fills in and a failure does not stop the queue', async ({ page }) => {
    await buildAndTrainOn(page, 'failxgb-compare');
    // Queue order is the order ticked: xgboost (fails) first, then logistic regression, then random forest.
    await page.getByTestId('ml-compare-algo-gradient_boosting').uncheck();
    await page.getByTestId('ml-compare-algo-random_forest').uncheck();
    await page.getByTestId('ml-compare-algo-xgboost').check();
    await page.getByTestId('ml-compare-algo-logistic_regression').check();
    await page.getByTestId('ml-compare-algo-random_forest').check();
    await page.getByTestId('ml-compare-run').click();

    await expect(page.getByTestId('ml-leaderboard-status-xgboost')).toHaveText('failed');
    await expect(page.getByTestId('ml-leaderboard-error-xgboost')).toContainText('xgboost is not installed');
    await expect(page.getByTestId('ml-leaderboard-status-logistic_regression')).toHaveText('done');
    await expect(page.getByTestId('ml-leaderboard-status-random_forest')).toHaveText('done');
    await expect(page.getByTestId('ml-leaderboard-acc-logistic_regression')).toHaveText(/^\d+\.\d%$/);
    await expect(page.getByTestId('ml-leaderboard-ret-random_forest')).toHaveText('+8.40%');
    await expect(page.getByTestId('ml-leaderboard-open-xgboost')).toHaveCount(0);
    await expect(page.getByTestId('ml-compare-run')).toBeEnabled();
    await expect(page.getByTestId('ml-train-button')).toBeEnabled();

    const calls = await ipc(page, 'train_ml_model');
    expect(calls.map((c) => c.args.request.algorithm)).toEqual(['xgboost', 'logistic_regression', 'random_forest']);
    // Same dataset, split count and features for each.
    expect(new Set(calls.map((c) => JSON.stringify([c.args.request.dataset, c.args.request.n_splits, c.args.request.feature_columns]))).size).toBe(1);

    await page.getByTestId('ml-leaderboard-open-random_forest').click();
    await expect(page.getByTestId('ml-models-tab')).toBeVisible();
  });

  test('compare shows live progress while the queue runs', async ({ page }) => {
    await buildAndTrainOn(page, 'slow-compare');
    await page.getByTestId('ml-compare-run').click(); // random_forest + gradient_boosting by default
    await expect(page.getByTestId('ml-leaderboard-status-random_forest')).toHaveText('running');
    await expect(page.getByTestId('ml-leaderboard-status-gradient_boosting')).toHaveText('queued');
    await expect(page.getByTestId('ml-compare-run')).toBeDisabled();
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
    await expect(page.getByTestId('ml-leaderboard-status-random_forest')).toHaveText('done', { timeout: 5000 });
    await expect(page.getByTestId('ml-leaderboard-status-gradient_boosting')).toHaveText('running');
    await expect(page.getByTestId('ml-leaderboard-status-gradient_boosting')).toHaveText('done', { timeout: 5000 });
  });

  test('remembers the last-used settings across a reload', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-algorithm-select').selectOption('gradient_boosting');
    await page.getByTestId('ml-hp-learning_rate').fill('0.02');
    await page.getByTestId('ml-cv-splits').fill('7');
    await page.reload();
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('gradient_boosting');
    await expect(page.getByTestId('ml-hp-learning_rate')).toHaveValue('0.02');
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('7');
  });

  test('Train again from a model pre-fills the Train tab from its settings', async ({ page }) => {
    await buildAndTrainOn(page, 'retrain-src');
    await page.getByTestId('ml-hp-n_estimators').fill('150');
    await page.getByTestId('ml-hp-max_depth').fill('8');
    await page.getByTestId('ml-cv-splits').fill('4');
    await page.getByTestId('ml-train-feature-rsi_14').uncheck();
    await page.getByTestId('ml-train-feature-vol_10').uncheck();
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    const modelId = await page.getByTestId('ml-result-model-id').innerText();

    // Change the form to something else, then retrain from the model (what the Models tab's button calls).
    await page.getByTestId('ml-algorithm-select').selectOption('xgboost');
    await page.getByTestId('ml-cv-splits').fill('9');
    await page.getByTestId('ml-train-features-none').click();
    await page.getByTestId('ml-tab-models').click();
    await page.evaluate((id) => (window as any).__apexMlTest.retrain(id), modelId);

    await expect(page.getByTestId('ml-training-form')).toBeVisible();
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('random_forest');
    await expect(page.getByTestId('ml-train-dataset')).toHaveValue('retrain-src');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('150');
    await expect(page.getByTestId('ml-hp-max_depth')).toHaveValue('8');
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('4');
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('13 of 15');
    await expect(page.getByTestId('ml-train-feature-rsi_14')).not.toBeChecked();
    await expect(page.getByTestId('ml-train-feature-ret_1')).toBeChecked();

    await page.getByTestId('ml-train-button').click();
    await expect.poll(async () => (await ipc(page, 'train_ml_model')).length).toBe(2);
    const req = await lastTrainRequest(page);
    expect(req).toMatchObject({ algorithm: 'random_forest', n_splits: 4, hyperparams: { n_estimators: 150, max_depth: 8 }, dataset: 'retrain-src' });
    expect(req.feature_columns).toHaveLength(13);
  });
});

type MockHooks = {
  trainModelCustom: (patch?: Record<string, unknown>, actor?: string) => string;
  delayNext: (cmd: string, ms?: number) => void;
};
const mock = <K extends keyof MockHooks>(page: Page, name: K, ...args: Parameters<MockHooks[K]>) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);

test.describe('Retrain from a model that used lag periods', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); });

  test('a CSV model puts only its base columns back in the form, with the lags', async ({ page }) => {
    // The trainer stores the generated lag columns in feature_names.
    const id = await mock(page, 'trainModelCustom', {
      dataset: null, data_path: 'data/x.csv', target_column: 'y',
      feature_names: ['a', 'b', 'a_lag1', 'b_lag1', 'a_lag5', 'b_lag5'], lag_periods: [1, 5],
    }) as string;
    await page.evaluate((m) => (window as any).__apexMlTest.retrain(m), id);
    await expect(page.getByTestId('ml-features-text')).toHaveValue('a, b');
    await expect(page.getByTestId('ml-lag-periods')).toHaveValue('1, 5');
    await page.getByTestId('ml-train-button').click();
    await expect.poll(async () => (await ipc(page, 'train_ml_model')).length).toBe(1);
    const req = await lastTrainRequest(page);
    expect(req.feature_columns).toEqual(['a', 'b']);
    expect(req.lag_periods).toEqual([1, 5]);
  });

  test('a dataset model keeps its feature selection and its lags', async ({ page }) => {
    await buildAndTrainOn(page, 'lag-src');
    const id = await mock(page, 'trainModelCustom', {
      dataset: 'lag-src', feature_names: ['ret_1', 'rsi_14', 'ret_1_lag1', 'rsi_14_lag1'], lag_periods: [1],
    }) as string;
    await page.evaluate((m) => (window as any).__apexMlTest.retrain(m), id);
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('2 of 15');
    await expect(page.getByTestId('ml-lag-periods')).toHaveValue('1');
    await page.getByTestId('ml-train-button').click();
    await expect.poll(async () => (await ipc(page, 'train_ml_model')).length).toBe(1);
    const req = await lastTrainRequest(page);
    expect(req.feature_columns).toEqual(['ret_1', 'rsi_14']);
    expect(req.lag_periods).toEqual([1]);
  });
});

test.describe('Dataset previews arriving out of order', () => {
  test('the dataset clicked last stays selected when an earlier preview answers late', async ({ page }) => {
    await openWorkbench(page);
    for (const name of ['ds-a', 'ds-b']) {
      await page.getByTestId('ml-ds-symbol-input').fill('AAPL');
      await page.getByTestId('ml-ds-symbol-input').press('Enter');
      await page.getByTestId('ml-ds-name').fill(name);
      await page.getByTestId('ml-ds-build').click();
      await expect(page.getByTestId('ml-ds-result-name')).toHaveText(name);
    }
    await mock(page, 'delayNext', 'preview_dataset', 700);
    await page.getByTestId('ml-ds-select-ds-a').click(); // slow answer
    await page.getByTestId('ml-ds-select-ds-b').click(); // fast answer
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('ds-b');
    await page.waitForTimeout(1000); // the slow answer for ds-a has arrived by now
    await expect(page.getByTestId('ml-ds-result-name')).toHaveText('ds-b');
    await expect(page.getByTestId('ml-dataset-ds-b')).toHaveClass(/border-accent/);
  });
});
