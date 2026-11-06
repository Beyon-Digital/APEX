import { test, expect, type Page } from '@playwright/test';

/**
 * ML workbench Train tab: free algorithm and parameter choice, your own presets, evaluation
 * controls, custom estimators (web mode: the backend is replaced by lib/mockMl.ts).
 */

async function openWorkbench(page: Page) {
  await page.addInitScript(() => { (window as unknown as { __ipcLog: unknown[] }).__ipcLog = []; });
  await page.goto('/');
  await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  await page.getByTestId('tab-ml').click();
  await expect(page.getByTestId('ml-workbench')).toBeVisible({ timeout: 10000 });
}

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

const trainRequests = async (page: Page) => (await ipc(page, 'train_ml_model')).map((c) => c.args.request);
const lastTrainRequest = async (page: Page) => {
  const all = await trainRequests(page);
  return all[all.length - 1];
};

/** Click Train and wait for the Nth request to have been sent and the result to show. */
async function trainAndWait(page: Page, nth = 1) {
  await page.getByTestId('ml-train-button').click();
  await expect.poll(async () => (await ipc(page, 'train_ml_model')).length).toBe(nth);
  await expect(page.getByTestId('ml-train-result')).toBeVisible();
}

const mockHook = (page: Page, name: string, ...args: unknown[]) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);

test.describe('Parameter editor generated from the algorithm catalog', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await buildAndTrainOn(page, 'ce-params'); });

  test('renders int, bool and choice fields with the default as placeholder', async ({ page }) => {
    // Random forest in the mock catalog: 6 parameters of int, bool and choice types.
    await expect(page.getByTestId('ml-param-shown')).toHaveText('Showing 6 of 6');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveAttribute('type', 'number');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveAttribute('placeholder', '100');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveAttribute('min', '1');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveAttribute('max', '2000');
    await expect(page.getByTestId('ml-hp-bootstrap')).toHaveAttribute('type', 'checkbox');
    await expect(page.getByTestId('ml-hp-bootstrap')).toBeChecked(); // default true
    // Choice with no default: an "Unset" option first, then the choices.
    await expect(page.getByTestId('ml-hp-class_weight').locator('option')).toHaveText(['Unset', 'balanced', 'balanced_subsample']);
    // Choice with a default: the first option names it.
    await expect(page.getByTestId('ml-hp-criterion').locator('option').first()).toHaveText('Default (gini)');
    await expect(page.getByTestId('ml-hp-changed-count')).toContainText('0 changed');

    // A different algorithm shows its own generated fields.
    await page.getByTestId('ml-algorithm-select').selectOption('hist_gradient_boosting');
    await expect(page.getByTestId('ml-hp-l2_regularization')).toBeVisible();
    await expect(page.getByTestId('ml-hp-l2_regularization')).toHaveAttribute('type', 'number');
    await expect(page.getByTestId('ml-hp-bootstrap')).toHaveCount(0);
  });

  test('search filters the list, changed-only narrows to edits, per-field and all reset work', async ({ page }) => {
    await page.getByTestId('ml-param-search').fill('depth');
    await expect(page.getByTestId('ml-param-shown')).toHaveText('Showing 1 of 6');
    await expect(page.getByTestId('ml-hp-max_depth')).toBeVisible();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveCount(0);
    await page.getByTestId('ml-param-search').fill('zzz-nothing');
    await expect(page.getByTestId('ml-param-empty')).toBeVisible();
    await page.getByTestId('ml-param-search').fill('');

    await page.getByTestId('ml-hp-n_estimators').fill('150');
    await page.getByTestId('ml-hp-bootstrap').uncheck();
    await page.getByTestId('ml-hp-criterion').selectOption('entropy');
    await expect(page.getByTestId('ml-hp-changed-count')).toContainText('3 changed');

    await page.getByTestId('ml-param-changed-only').check();
    await expect(page.getByTestId('ml-param-shown')).toHaveText('Showing 3 of 6');
    await expect(page.getByTestId('ml-hp-max_depth')).toHaveCount(0);

    await page.getByTestId('ml-hp-reset-bootstrap').click();
    await expect(page.getByTestId('ml-param-shown')).toHaveText('Showing 2 of 6');
    await expect(page.getByTestId('ml-hp-changed-count')).toContainText('2 changed');

    await page.getByTestId('ml-hp-reset-all').click();
    await expect(page.getByTestId('ml-hp-changed-count')).toContainText('0 changed');
    await expect(page.getByTestId('ml-param-shown')).toHaveText('Showing 0 of 6');
    await page.getByTestId('ml-param-changed-only').uncheck();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('');
  });

  test('only values that differ from the default are sent', async ({ page }) => {
    await page.getByTestId('ml-hp-n_estimators').fill('150');
    await page.getByTestId('ml-hp-max_depth').fill('0'); // equals the default (0 = unlimited): not a change
    await page.getByTestId('ml-hp-bootstrap').uncheck(); // true -> false is a change
    await page.getByTestId('ml-hp-criterion').selectOption('entropy');
    await page.getByTestId('ml-hp-class_weight').selectOption('balanced');
    await page.getByTestId('ml-hp-class_weight').selectOption(''); // back to unset
    await expect(page.getByTestId('ml-hp-changed-count')).toContainText('3 changed');
    await trainAndWait(page);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({ n_estimators: 150, bootstrap: false, criterion: 'entropy' });

    await page.getByTestId('ml-hp-bootstrap').check(); // back to the default
    await page.getByTestId('ml-hp-reset-all').click();
    await trainAndWait(page, 2);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({});
  });

  test('Advanced JSON is merged with the fields; invalid JSON and key collisions block Train', async ({ page }) => {
    const train = page.getByTestId('ml-train-button');
    await page.getByTestId('ml-hp-n_estimators').fill('150');
    await page.getByTestId('ml-adv-json').fill('{"n_jobs": 2, "ccp_alpha": 0.001, "warm_start": true, "note": "x"}');
    await expect(page.getByTestId('ml-adv-count')).toHaveText('4 keys');
    await expect(train).toBeEnabled();
    await trainAndWait(page);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({
      n_estimators: 150, n_jobs: 2, ccp_alpha: 0.001, warm_start: true, note: 'x',
    });

    // Invalid JSON.
    await page.getByTestId('ml-adv-json').fill('{"n_jobs": ');
    await expect(page.getByTestId('ml-adv-error')).toContainText('not valid JSON');
    await expect(train).toBeDisabled();
    await expect(page.getByTestId('ml-train-validation')).toContainText('not valid JSON');
    // Valid JSON that is not an object.
    await page.getByTestId('ml-adv-json').fill('[1, 2]');
    await expect(page.getByTestId('ml-adv-error')).toContainText('must be a JSON object');
    await expect(train).toBeDisabled();
    // Nested values are refused.
    await page.getByTestId('ml-adv-json').fill('{"a": {"b": 1}}');
    await expect(page.getByTestId('ml-adv-error')).toContainText('"a" must be a number, text, true/false or null');
    await expect(train).toBeDisabled();

    // Collision with a field that has a changed value.
    await page.getByTestId('ml-adv-json').fill('{"n_estimators": 5}');
    await expect(page.getByTestId('ml-adv-collision')).toContainText('"n_estimators" is set both in its field above and in the JSON');
    await expect(train).toBeDisabled();
    expect(await ipc(page, 'train_ml_model')).toHaveLength(1); // nothing new was sent

    // Resolve it by clearing the field: the JSON now supplies the value.
    await page.getByTestId('ml-hp-reset-n_estimators').click();
    await expect(page.getByTestId('ml-adv-collision')).toHaveCount(0);
    await expect(train).toBeEnabled();
    await trainAndWait(page, 2);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({ n_estimators: 5 });

    // Empty text is fine and sends nothing extra.
    await page.getByTestId('ml-adv-json').fill('');
    await trainAndWait(page, 3);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({});
  });

  test('Reset all also clears the Advanced JSON', async ({ page }) => {
    await page.getByTestId('ml-adv-json').fill('{"n_jobs": 2}');
    await page.getByTestId('ml-hp-reset-all').click();
    await expect(page.getByTestId('ml-adv-json')).toHaveValue('');
  });

  test('the Fast / Balanced / Thorough quick-fills only set ordinary editable values', async ({ page }) => {
    await page.getByTestId('ml-preset-balanced').click();
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('200');
    await page.getByTestId('ml-hp-n_estimators').fill('321'); // still editable
    await trainAndWait(page);
    expect((await lastTrainRequest(page)).hyperparams).toMatchObject({ n_estimators: 321, max_depth: 8, class_weight: 'balanced' });
    // No quick-fill for an algorithm that has none.
    await page.getByTestId('ml-algorithm-select').selectOption('extra_trees');
    await expect(page.getByTestId('ml-preset-fast')).toHaveCount(0);
  });
});

test.describe('Algorithm list fallback', () => {
  test('shows the built-in list with a notice when the backend call fails', async ({ page }) => {
    await page.addInitScript(() => {
      const t = setInterval(() => {
        const hook = (window as unknown as { __apexMock?: { failNext: (c: string, m: string) => void } }).__apexMock;
        if (hook) { hook.failNext('list_algorithms', 'backend down'); clearInterval(t); }
      }, 1);
    });
    await openWorkbench(page);
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-algorithms-notice')).toContainText('built-in list');
    await expect(page.getByTestId('ml-algorithm-select').locator('option')).toHaveText([
      'Random Forest', 'Gradient Boosting', 'Logistic Regression', 'XGBoost', 'Custom estimator (Python file)',
    ]);
    await expect(page.getByTestId('ml-hp-n_estimators')).toBeVisible();
    await page.getByTestId('ml-tab-data').click();
    await buildAndTrainOn(page, 'ce-fallback');
    await page.getByTestId('ml-hp-n_estimators').fill('150');
    await trainAndWait(page);
    expect((await lastTrainRequest(page)).hyperparams).toEqual({ n_estimators: 150 });
  });
});

test.describe('Your own presets', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await buildAndTrainOn(page, 'ce-presets'); });

  async function configure(page: Page) {
    await page.getByTestId('ml-algorithm-select').selectOption('gradient_boosting');
    await page.getByTestId('ml-hp-learning_rate').fill('0.05');
    await page.getByTestId('ml-adv-json').fill('{"n_iter_no_change": 5}');
    await page.getByTestId('ml-cv-splits').fill('7');
    await page.getByTestId('ml-trade-mode-long_short').check();
    await page.getByTestId('ml-holdout-pct').fill('15');
    await page.getByTestId('ml-random-state').fill('11');
    await page.getByTestId('ml-lag-periods').fill('1,5');
    await page.getByTestId('ml-train-feature-rsi_14').uncheck();
    await page.getByTestId('ml-train-feature-vol_10').uncheck();
  }

  async function disturb(page: Page) {
    await page.getByTestId('ml-algorithm-select').selectOption('xgboost');
    await page.getByTestId('ml-cv-splits').fill('3');
    await page.getByTestId('ml-trade-mode-long_flat').check();
    await page.getByTestId('ml-holdout-pct').fill('0');
    await page.getByTestId('ml-random-state').fill('');
    await page.getByTestId('ml-lag-periods').fill('');
    await page.getByTestId('ml-train-features-all').click();
  }

  test('save, load, rename and delete a configuration', async ({ page }) => {
    await expect(page.getByTestId('ml-upreset-empty')).toBeVisible();
    await expect(page.getByTestId('ml-upreset-save')).toBeDisabled();
    await configure(page);
    await page.getByTestId('ml-upreset-name').fill('GB long-short');
    await page.getByTestId('ml-upreset-save').click();
    await expect(page.getByTestId('ml-upreset-row-gb-long-short')).toBeVisible();
    await expect(page.getByTestId('ml-upreset-name')).toHaveValue('');

    // Duplicate names (ignoring case) are refused.
    await page.getByTestId('ml-upreset-name').fill('gb LONG-short');
    await page.getByTestId('ml-upreset-save').click();
    await expect(page.getByTestId('ml-upreset-error')).toContainText('already exists');
    await page.getByTestId('ml-upreset-name').fill('');

    await disturb(page);
    await page.getByTestId('ml-upreset-load-gb-long-short').click();
    await expect(page.getByTestId('ml-upreset-info')).toContainText('Loaded "GB long-short"');
    await expect(page.getByTestId('ml-upreset-notes')).toHaveCount(0);
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('gradient_boosting');
    await expect(page.getByTestId('ml-hp-learning_rate')).toHaveValue('0.05');
    await expect(page.getByTestId('ml-adv-json')).toHaveValue('{"n_iter_no_change": 5}');
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('7');
    await expect(page.getByTestId('ml-trade-mode-long_short')).toBeChecked();
    await expect(page.getByTestId('ml-holdout-pct')).toHaveValue('15');
    await expect(page.getByTestId('ml-random-state')).toHaveValue('11');
    await expect(page.getByTestId('ml-lag-periods')).toHaveValue('1,5');
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('13 of 15');
    await expect(page.getByTestId('ml-train-feature-rsi_14')).not.toBeChecked();

    // What was loaded is what is sent.
    await trainAndWait(page);
    expect(await lastTrainRequest(page)).toMatchObject({
      algorithm: 'gradient_boosting', hyperparams: { learning_rate: 0.05, n_iter_no_change: 5 }, n_splits: 7,
      trade_mode: 'long_short', holdout_pct: 15, random_state: 11, lag_periods: [1, 5],
    });

    // Rename.
    await page.getByTestId('ml-upreset-rename-gb-long-short').click();
    await page.getByTestId('ml-upreset-rename-input').fill('Boosting, shorting');
    await page.getByTestId('ml-upreset-rename-ok').click();
    await expect(page.getByTestId('ml-upreset-row-boosting-shorting')).toBeVisible();
    await expect(page.getByTestId('ml-upreset-row-gb-long-short')).toHaveCount(0);

    // Renaming onto another preset's name is refused.
    await page.getByTestId('ml-upreset-name').fill('Second');
    await page.getByTestId('ml-upreset-save').click();
    await page.getByTestId('ml-upreset-rename-second').click();
    await page.getByTestId('ml-upreset-rename-input').fill('boosting, SHORTING');
    await page.getByTestId('ml-upreset-rename-ok').click();
    await expect(page.getByTestId('ml-upreset-rename-error')).toContainText('already exists');
    await page.getByTestId('ml-upreset-rename-cancel').click();

    // Survives a reload.
    await page.reload();
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-upreset-row-boosting-shorting')).toBeVisible();
    await expect(page.getByTestId('ml-upreset-row-second')).toBeVisible();

    // Delete.
    await page.getByTestId('ml-upreset-delete-second').click();
    await expect(page.getByTestId('ml-upreset-row-second')).toHaveCount(0);
    await page.getByTestId('ml-upreset-delete-boosting-shorting').click();
    await expect(page.getByTestId('ml-upreset-empty')).toBeVisible();
    await page.reload();
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-upreset-empty')).toBeVisible();
  });

  test('export gives JSON that import restores, and bad input is explained', async ({ page }) => {
    await configure(page);
    await page.getByTestId('ml-upreset-name').fill('Exported one');
    await page.getByTestId('ml-upreset-save').click();
    await page.getByTestId('ml-upreset-export').click();
    const json = await page.getByTestId('ml-upreset-io').inputValue();
    const parsed = JSON.parse(json);
    expect(parsed.presets).toHaveLength(1);
    expect(parsed.presets[0]).toMatchObject({
      name: 'Exported one', algorithm: 'gradient_boosting', params: { learning_rate: '0.05' }, nSplits: '7', tradeMode: 'long_short', holdoutPct: '15', randomState: '11',
    });
    expect(parsed.presets[0].features).toHaveLength(13);
    expect(parsed.presets[0].id).toBeUndefined();

    // Importing the same file again keeps both, renaming the newcomer.
    await page.getByTestId('ml-upreset-import').click();
    await expect(page.getByTestId('ml-upreset-io-message')).toContainText('Imported 1 preset');
    await expect(page.getByTestId('ml-upreset-row-exported-one')).toBeVisible();
    await expect(page.getByTestId('ml-upreset-row-exported-one-2')).toBeVisible();

    // After deleting everything, import brings the preset back and it loads correctly.
    await page.getByTestId('ml-upreset-delete-exported-one').click();
    await page.getByTestId('ml-upreset-delete-exported-one-2').click();
    await expect(page.getByTestId('ml-upreset-empty')).toBeVisible();
    await page.getByTestId('ml-upreset-io').fill(json);
    await page.getByTestId('ml-upreset-import').click();
    await disturb(page);
    await page.getByTestId('ml-upreset-load-exported-one').click();
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('7');
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('gradient_boosting');

    // Bad input.
    const bad: [string, string][] = [
      ['', 'Paste the exported JSON'],
      ['{nope', 'not valid JSON'],
      ['{"hello": 1}', 'No presets found'],
      ['{"presets": []}', 'no presets'],
      ['{"presets": [{"name": "x"}]}', 'Preset 1 has no algorithm'],
      ['{"presets": [{"name": "x", "algorithm": "random_forest", "tradeMode": "sideways"}]}', 'tradeMode'],
      ['{"presets": [{"name": "ok", "algorithm": "random_forest"}, {"algorithm": "random_forest"}]}', 'Preset 2 has no name'],
    ];
    for (const [text, expected] of bad) {
      await page.getByTestId('ml-upreset-io').fill(text);
      if (text === '') await expect(page.getByTestId('ml-upreset-import')).toBeDisabled();
      else {
        await page.getByTestId('ml-upreset-import').click();
        await expect(page.getByTestId('ml-upreset-io-message')).toContainText(expected);
      }
    }
    await expect(page.getByTestId('ml-upreset-list').locator('li')).toHaveCount(1); // nothing partial was added
  });

  test('loading onto a dataset that lacks some features drops them and says so', async ({ page }) => {
    const preset = {
      format: 'apex-training-presets', version: 1,
      presets: [
        { name: 'Wide', algorithm: 'random_forest', features: ['ret_1', 'rsi_14', 'gone_feature_a', 'gone_feature_b'], nSplits: '4' },
        { name: 'Alien', algorithm: 'random_forest', features: ['only_elsewhere'] },
        { name: 'Every', algorithm: 'random_forest', features: null },
      ],
    };
    await page.getByTestId('ml-upreset-io').fill(JSON.stringify(preset));
    await page.getByTestId('ml-upreset-import').click();

    await page.getByTestId('ml-upreset-load-wide').click();
    await expect(page.getByTestId('ml-upreset-notes')).toContainText('2 features from the preset are not in this dataset and were dropped: gone_feature_a, gone_feature_b');
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('2 of 15');
    await expect(page.getByTestId('ml-train-feature-ret_1')).toBeChecked();
    await expect(page.getByTestId('ml-train-feature-vol_10')).not.toBeChecked();
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('4');
    await trainAndWait(page);
    expect((await lastTrainRequest(page)).feature_columns).toEqual(['ret_1', 'rsi_14']);

    // Nothing matches: do not leave the form empty, select all and say so.
    await page.getByTestId('ml-upreset-load-alien').click();
    await expect(page.getByTestId('ml-upreset-notes')).toContainText('None of the preset');
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('15 of 15');
    await expect(page.getByTestId('ml-train-button')).toBeEnabled();

    // "Every feature" carries over as all of the new dataset's features, with no note.
    await page.getByTestId('ml-train-features-none').click();
    await page.getByTestId('ml-upreset-load-every').click();
    await expect(page.getByTestId('ml-upreset-notes')).toHaveCount(0);
    await expect(page.getByTestId('ml-train-feature-count')).toContainText('15 of 15');
  });

  test('loading a preset whose algorithm is unknown keeps the current algorithm and says so', async ({ page }) => {
    await page.getByTestId('ml-upreset-io').fill(JSON.stringify({ presets: [{ name: 'Ghost', algorithm: 'quantum_forest', params: { n_estimators: 5 }, nSplits: '3' }] }));
    await page.getByTestId('ml-upreset-import').click();
    await page.getByTestId('ml-upreset-load-ghost').click();
    await expect(page.getByTestId('ml-upreset-notes')).toContainText('"quantum_forest" is not available');
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('random_forest');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('');
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('3');
  });
});

test.describe('Evaluation controls', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await buildAndTrainOn(page, 'ce-eval'); });

  test('defaults: long / flat, no hold-out, no random state', async ({ page }) => {
    await expect(page.getByTestId('ml-trade-mode-long_flat')).toBeChecked();
    await expect(page.getByTestId('ml-trade-mode-warning')).toHaveCount(0);
    await expect(page.getByTestId('ml-holdout-help')).toContainText('Off');
    await trainAndWait(page);
    const req = await lastTrainRequest(page);
    expect(req).toMatchObject({ trade_mode: 'long_flat', holdout_pct: 0 });
    expect(req.random_state).toBeUndefined();
    expect(req.estimator_file).toBeUndefined();
  });

  test('trade mode, hold-out and random state go into the request', async ({ page }) => {
    await page.getByTestId('ml-trade-mode-long_short').check();
    await expect(page.getByTestId('ml-trade-mode-warning')).toContainText('only if its rule allows shorting and its broker can short');
    await page.getByTestId('ml-holdout-slider').fill('25');
    await expect(page.getByTestId('ml-holdout-pct')).toHaveValue('25');
    await expect(page.getByTestId('ml-holdout-help')).toContainText('Scored once on the last 25% of time, never used for training or tuning');
    await page.getByTestId('ml-holdout-pct').fill('20');
    await expect(page.getByTestId('ml-holdout-slider')).toHaveValue('20');
    await page.getByTestId('ml-random-state').fill('42');
    await trainAndWait(page);
    expect(await lastTrainRequest(page)).toMatchObject({ trade_mode: 'long_short', holdout_pct: 20, random_state: 42 });

    await page.getByTestId('ml-trade-mode-long_flat').check();
    await expect(page.getByTestId('ml-trade-mode-warning')).toHaveCount(0);
    await page.getByTestId('ml-random-state').fill('');
    await trainAndWait(page, 2);
    const req = await lastTrainRequest(page);
    expect(req.trade_mode).toBe('long_flat');
    expect(req.random_state).toBeUndefined();
  });

  test('lag periods outside 1 to 250 block Train, as the backend would reject them', async ({ page }) => {
    const train = page.getByTestId('ml-train-button');
    for (const bad of ['0', '251', '1, 5, 1000']) {
      await page.getByTestId('ml-lag-periods').fill(bad);
      await expect(page.getByTestId('ml-train-validation')).toContainText('lag_periods values must be between 1 and 250');
      await expect(train).toBeDisabled();
    }
    await page.getByTestId('ml-lag-periods').fill('1, 250');
    await expect(train).toBeEnabled();
  });

  test('invalid hold-out, random state and splits block Train', async ({ page }) => {
    const train = page.getByTestId('ml-train-button');
    for (const bad of ['41', '-1', '']) {
      await page.getByTestId('ml-holdout-pct').fill(bad);
      await expect(page.getByTestId('ml-train-validation')).toContainText('Hold-out must be a number from 0 to 40');
      await expect(train).toBeDisabled();
    }
    await page.getByTestId('ml-holdout-pct').fill('40');
    await expect(train).toBeEnabled();
    // Same range and wording as the backend: 0 to 2147483647.
    for (const bad of ['-3', '1.5', '2147483648', '4294967295']) {
      await page.getByTestId('ml-random-state').fill(bad);
      await expect(page.getByTestId('ml-train-validation')).toContainText('random_state must be an integer from 0 to 2147483647');
      await expect(train).toBeDisabled();
    }
    await page.getByTestId('ml-random-state').fill('2147483647');
    await expect(train).toBeEnabled();
    await page.getByTestId('ml-random-state').fill('0');
    await expect(train).toBeEnabled();
    // The same seed in the JSON box and in the field would be ambiguous.
    await page.getByTestId('ml-adv-json').fill('{"random_state": 3}');
    await expect(page.getByTestId('ml-train-validation')).toContainText('Random state is set both');
    await expect(train).toBeDisabled();
    expect(await ipc(page, 'train_ml_model')).toHaveLength(0);
  });
});

test.describe('Custom estimator', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await buildAndTrainOn(page, 'ce-custom'); });

  const GOOD = 'from sklearn.tree import DecisionTreeClassifier\n\n\ndef make_estimator(params):\n    return DecisionTreeClassifier(max_depth=int(params.get("max_depth", 3)))\n';

  test('create from template, validation errors, save, train with it, then delete', async ({ page }) => {
    const train = page.getByTestId('ml-train-button');
    await page.getByTestId('ml-algorithm-select').selectOption('custom');
    await expect(page.getByTestId('ml-estimator-editor')).toBeVisible();
    await expect(page.getByTestId('ml-est-contract')).toContainText('make_estimator(params: dict)');
    await expect(page.getByTestId('ml-est-warning')).toContainText('arbitrary Python that runs on this machine');
    await expect(page.getByTestId('ml-est-empty')).toBeVisible();
    await expect(page.getByTestId('ml-param-none')).toBeVisible(); // only the JSON box supplies params
    await expect(train).toBeDisabled();
    await expect(page.getByTestId('ml-train-validation')).toContainText('Choose or create an estimator file');

    // The template comes from the backend.
    await page.getByTestId('ml-est-new').click();
    await expect(page.getByTestId('ml-est-code')).toHaveValue(/ExtraTreesClassifier/);
    await expect(page.getByTestId('ml-est-code')).toHaveValue(/def make_estimator/);

    // Client check: a name is required.
    await page.getByTestId('ml-est-save').click();
    await expect(page.getByTestId('ml-est-error')).toContainText('Give the file a name');

    // Backend check: bad names are rejected inline.
    await page.getByTestId('ml-est-name').fill('Bad Name!');
    await page.getByTestId('ml-est-save').click();
    await expect(page.getByTestId('ml-est-error')).toContainText('Estimator names look like my_model.py');
    await expect(page.getByTestId('ml-est-empty')).toBeVisible();

    // Backend check: the contract.
    await page.getByTestId('ml-est-name').fill('my_model');
    await page.getByTestId('ml-est-code').fill('print("hello")\n');
    await page.getByTestId('ml-est-save').click();
    await expect(page.getByTestId('ml-est-error')).toContainText('must define make_estimator');
    await expect(train).toBeDisabled();

    // Valid: saved, listed, and selected for training.
    await page.getByTestId('ml-est-code').fill(GOOD);
    await page.getByTestId('ml-est-save').click();
    await expect(page.getByTestId('ml-est-error')).toHaveCount(0);
    await expect(page.getByTestId('ml-est-item-my_model.py')).toBeVisible();
    await expect(page.getByTestId('ml-est-name')).toHaveValue('my_model.py');
    await expect(page.getByTestId('ml-est-selected')).toContainText('my_model.py');
    await expect(train).toBeEnabled();
    const saves = await ipc(page, 'save_estimator_file');
    expect(saves[saves.length - 1].args).toEqual({ name: 'my_model.py', content: GOOD });

    // JSON params reach the request together with the estimator file.
    await page.getByTestId('ml-adv-json').fill('{"max_depth": 4}');
    await trainAndWait(page);
    expect(await lastTrainRequest(page)).toMatchObject({ algorithm: 'custom', estimator_file: 'my_model.py', hyperparams: { max_depth: 4 } });

    // A second file; choose between them.
    await page.getByTestId('ml-est-new').click();
    await page.getByTestId('ml-est-name').fill('other.py');
    await page.getByTestId('ml-est-code').fill(GOOD.replace('3', '5'));
    await page.getByTestId('ml-est-save').click();
    await expect(page.getByTestId('ml-est-selected')).toContainText('other.py');
    await page.getByTestId('ml-est-use-my_model.py').click();
    await expect(page.getByTestId('ml-est-selected')).toContainText('my_model.py');
    await trainAndWait(page, 2);
    expect((await lastTrainRequest(page)).estimator_file).toBe('my_model.py');

    // Open a file for editing.
    await page.getByTestId('ml-est-open-other.py').click();
    await expect(page.getByTestId('ml-est-name')).toHaveValue('other.py');
    await expect(page.getByTestId('ml-est-code')).toHaveValue(/max_depth=int\(params.get\("max_depth", 5\)\)/);

    // Delete asks first.
    await page.getByTestId('ml-est-delete-my_model.py').click();
    await page.getByTestId('ml-est-delete-cancel-my_model.py').click();
    await expect(page.getByTestId('ml-est-item-my_model.py')).toBeVisible();
    await page.getByTestId('ml-est-delete-my_model.py').click();
    await page.getByTestId('ml-est-delete-confirm-my_model.py').click();
    await expect(page.getByTestId('ml-est-item-my_model.py')).toHaveCount(0);
    // The deleted file was the selected one: nothing is selected and Train is blocked again.
    await expect(page.getByTestId('ml-est-selected')).toContainText('No file selected');
    await expect(train).toBeDisabled();
    await page.getByTestId('ml-est-use-other.py').click();
    await expect(train).toBeEnabled();
    expect((await ipc(page, 'delete_estimator_file')).map((c) => c.args)).toEqual([{ name: 'my_model.py' }]);

    // Other algorithms do not send an estimator file.
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await trainAndWait(page, 3);
    expect((await lastTrainRequest(page)).estimator_file).toBeUndefined();
  });
});

test.describe('Compare queue honours the settings', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await buildAndTrainOn(page, 'ce-queue'); });

  test('shared evaluation settings apply to every run; parameters only to their own algorithm', async ({ page }) => {
    await expect(page.getByTestId('ml-compare-note')).toContainText('apply only to the algorithm they were set for');
    await page.getByTestId('ml-trade-mode-long_short').check();
    await page.getByTestId('ml-holdout-pct').fill('10');
    await page.getByTestId('ml-random-state').fill('5');
    await page.getByTestId('ml-hp-n_estimators').fill('150'); // random forest
    await page.getByTestId('ml-adv-json').fill('{"n_jobs": 2}');
    await expect(page.getByTestId('ml-compare-params-random_forest')).toHaveText('2 custom');
    await expect(page.getByTestId('ml-compare-params-gradient_boosting')).toHaveText('defaults');

    await page.getByTestId('ml-compare-algo-extra_trees').check();
    await page.getByTestId('ml-compare-run').click();
    await expect(page.getByTestId('ml-leaderboard-status-extra_trees')).toHaveText('done');
    const reqs = await trainRequests(page);
    expect(reqs.map((r) => r.algorithm)).toEqual(['random_forest', 'gradient_boosting', 'extra_trees']);
    for (const r of reqs) expect(r).toMatchObject({ trade_mode: 'long_short', holdout_pct: 10, random_state: 5, n_splits: 5 });
    expect(reqs[0].hyperparams).toEqual({ n_estimators: 150, n_jobs: 2 });
    expect(reqs[1].hyperparams).toEqual({});
    expect(reqs[2].hyperparams).toEqual({});
  });

  test('a queued algorithm with a bad parameter blocks the run and says why', async ({ page }) => {
    await page.getByTestId('ml-algorithm-select').selectOption('gradient_boosting');
    await page.getByTestId('ml-hp-learning_rate').fill('5');
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await expect(page.getByTestId('ml-compare-run')).toBeDisabled();
    await expect(page.getByTestId('ml-compare-validation')).toContainText('Learning rate must be a number between 0.001 and 1');
    await page.getByTestId('ml-compare-algo-gradient_boosting').uncheck();
    await expect(page.getByTestId('ml-compare-run')).toBeEnabled();
  });

  test('custom estimator in the queue needs a file', async ({ page }) => {
    await page.getByTestId('ml-compare-algo-custom').check();
    await expect(page.getByTestId('ml-compare-run')).toBeDisabled();
    await expect(page.getByTestId('ml-compare-validation')).toContainText('estimator file');
  });
});

test.describe('Retrain prefill of the new fields', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await buildAndTrainOn(page, 'ce-retrain'); });

  test('restores trade mode, hold-out, random state, parameters and extra JSON', async ({ page }) => {
    const id = await mockHook(page, 'trainModelCustom', {
      dataset: 'ce-retrain', feature_names: ['ret_1', 'rsi_14'], rows: 480, n_splits: 6,
      trade_mode: 'long_short', holdout: { rows: 96, accuracy: 0.5, baseline_accuracy: 0.5, trading: null },
      hyperparams: { n_estimators: 150, criterion: 'entropy', random_state: 7, n_jobs: 2 },
    }) as string;
    await page.evaluate((m) => (window as any).__apexMlTest.retrain(m), id);
    await expect(page.getByTestId('ml-training-form')).toBeVisible();
    await expect(page.getByTestId('ml-trade-mode-long_short')).toBeChecked();
    await expect(page.getByTestId('ml-holdout-pct')).toHaveValue('20'); // 96 of 480 rows
    await expect(page.getByTestId('ml-random-state')).toHaveValue('7');
    await expect(page.getByTestId('ml-hp-n_estimators')).toHaveValue('150');
    await expect(page.getByTestId('ml-hp-criterion')).toHaveValue('entropy');
    await expect(page.getByTestId('ml-cv-splits')).toHaveValue('6');
    expect(JSON.parse(await page.getByTestId('ml-adv-json').inputValue())).toEqual({ n_jobs: 2 });

    await trainAndWait(page);
    expect(await lastTrainRequest(page)).toMatchObject({
      algorithm: 'random_forest', trade_mode: 'long_short', holdout_pct: 20, random_state: 7, n_splits: 6,
      hyperparams: { n_estimators: 150, criterion: 'entropy', n_jobs: 2 },
    });
  });

  test('a model without hold-out or random state resets those fields; a custom model restores its estimator file', async ({ page }) => {
    await page.getByTestId('ml-trade-mode-long_short').check();
    await page.getByTestId('ml-holdout-pct').fill('30');
    await page.getByTestId('ml-random-state').fill('9');
    const plain = await mockHook(page, 'trainModelCustom', { dataset: 'ce-retrain', feature_names: ['ret_1'], trade_mode: 'long_flat', holdout: null, hyperparams: {} }) as string;
    await page.evaluate((m) => (window as any).__apexMlTest.retrain(m), plain);
    await expect(page.getByTestId('ml-trade-mode-long_flat')).toBeChecked();
    await expect(page.getByTestId('ml-holdout-pct')).toHaveValue('0');
    await expect(page.getByTestId('ml-random-state')).toHaveValue('');

    // Save an estimator, then retrain from a model that used a file with that name.
    await page.getByTestId('ml-algorithm-select').selectOption('custom');
    await page.getByTestId('ml-est-new').click();
    await page.getByTestId('ml-est-name').fill('mine');
    await page.getByTestId('ml-est-save').click();
    await expect(page.getByTestId('ml-est-item-mine.py')).toBeVisible();
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    const custom = await mockHook(page, 'trainModelCustom', {
      algorithm: 'custom', dataset: 'ce-retrain', feature_names: ['ret_1'], estimator_file: 'mine.py', hyperparams: { max_depth: 4 },
    }) as string;
    await page.evaluate((m) => (window as any).__apexMlTest.retrain(m), custom);
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('custom');
    await expect(page.getByTestId('ml-est-selected')).toContainText('mine.py');
    expect(JSON.parse(await page.getByTestId('ml-adv-json').inputValue())).toEqual({ max_depth: 4 });
    await trainAndWait(page);
    expect(await lastTrainRequest(page)).toMatchObject({ algorithm: 'custom', estimator_file: 'mine.py', hyperparams: { max_depth: 4 } });
  });
});
