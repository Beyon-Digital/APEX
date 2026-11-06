import { test, expect, type Page } from '@playwright/test';

/**
 * Algorithm builder: assemble an algorithm from parts, save it as a recipe, train with it
 * (web mode: the backend is replaced by lib/mockMl.ts, including MOCK_BLOCKS).
 */

async function openWorkbench(page: Page) {
  await page.addInitScript(() => { (window as unknown as { __ipcLog: unknown[] }).__ipcLog = []; });
  await page.goto('/');
  await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  await page.getByTestId('tab-ml').click();
  await expect(page.getByTestId('ml-workbench')).toBeVisible({ timeout: 10000 });
}

const ipc = (page: Page, cmd: string) =>
  page.evaluate((c) => ((window as unknown as { __ipcLog: { cmd: string; args: any }[] }).__ipcLog).filter((e) => e.cmd === c), cmd);
const lastCall = async (page: Page, cmd: string) => { const all = await ipc(page, cmd); return all[all.length - 1]; };
const savedRecipe = async (page: Page) => (await lastCall(page, 'save_recipe')).args.recipe;

const mockHook = (page: Page, name: string, ...args: unknown[]) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);

const BASE = {
  name: 'base_algo', label: 'Base', description: 'seeded', steps: [{ kind: 'standard_scaler', params: {} }],
  model: { kind: 'single', algorithm: 'random_forest', params: {} }, calibrate: 'none',
};

/** Seed recipes straight into the mock and show them in the Train tab. */
async function seed(page: Page, ...recipes: object[]) {
  for (const r of recipes) await mockHook(page, 'seedRecipe', r);
  await page.getByTestId('ml-tab-train').click();
  await page.evaluate(() => (window as any).__apexMlTest.refreshRecipes());
}

async function openNewBuilder(page: Page) {
  await page.getByTestId('ml-tab-train').click();
  await page.getByTestId('ml-algorithm-select').selectOption('__build__');
  await expect(page.getByTestId('rb-card-model')).toBeVisible();
}

async function openSavedBuilder(page: Page, name: string) {
  await page.getByTestId('ml-algorithm-select').selectOption(`recipe:${name}`);
  await page.getByTestId('ml-recipe-edit').click();
  await expect(page.getByTestId('rb-card-model')).toBeVisible();
}

const stepKinds = (page: Page) => page.locator('[data-testid^="rb-card-step-"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-kind')));

async function addStep(page: Page, kind: string) {
  await page.getByTestId('rb-add-step').click();
  await page.getByTestId(`rb-add-step-${kind}`).click();
}

async function buildDataset(page: Page, name: string) {
  await page.getByTestId('ml-tab-data').click();
  await page.getByTestId('ml-ds-symbol-input').fill('AAPL');
  await page.getByTestId('ml-ds-symbol-input').press('Enter');
  await page.getByTestId('ml-ds-name').fill(name);
  await page.getByTestId('ml-ds-build').click();
  await expect(page.getByTestId('ml-ds-result-name')).toHaveText(name);
  await page.getByTestId('ml-ds-result-train').click();
  await expect(page.getByTestId('ml-train-dataset')).toHaveValue(name);
}

test.describe('Algorithm select and empty state', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); });

  test('has a My algorithms group with saved recipes and a Build entry', async ({ page }) => {
    await seed(page, BASE, { ...BASE, name: 'zeta', label: 'Zeta mix' });
    const group = page.getByTestId('ml-algorithm-group-recipes');
    await expect(group.locator('option')).toHaveText(['Base', 'Zeta mix', 'Build an algorithm...']);
    await expect(page.getByTestId('ml-algorithm-group-builtin').locator('option').first()).toHaveText('Random forest');
    await expect(page.getByTestId('ml-recipe-empty')).toHaveCount(0);
  });

  test('with no saved recipes it offers the four examples, and an example opens the builder filled in', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-recipe-empty')).toBeVisible();
    for (const id of ['scaled_logistic', 'voting_mix', 'stacked_trees', 'pca_forest']) await expect(page.getByTestId(`ml-recipe-template-${id}`)).toBeVisible();
    await expect(page.getByTestId('ml-algorithm-group-recipes').locator('option')).toHaveText(['Build an algorithm...']);
    await page.getByTestId('ml-recipe-template-pca_forest').click();
    await expect(page.getByTestId('ml-recipe-builder')).toBeVisible();
    expect(await stepKinds(page)).toEqual(['standard_scaler', 'pca']);
    await expect(page.getByTestId('rb-s1-hp-n_components')).toHaveValue('0.9');
    await expect(page.getByTestId('rb-name')).toHaveValue('pca_forest');
    await expect(page.getByTestId('rb-model-kind-single')).toBeChecked();
  });

  test('choosing Build an algorithm opens a panel inside the Train tab and hides the form', async ({ page }) => {
    await openNewBuilder(page);
    await expect(page.getByTestId('ml-panel-train').getByTestId('ml-recipe-builder')).toBeVisible();
    await expect(page.getByTestId('ml-train-algorithm')).toBeHidden();
    await expect(page.getByTestId('rb-mode')).toHaveText('New algorithm');
    // The select still shows the algorithm that was chosen before.
    await page.getByTestId('rb-discard').click();
    await expect(page.getByTestId('ml-train-algorithm')).toBeVisible();
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('random_forest');
  });
});

test.describe('Building a recipe', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await openNewBuilder(page); });

  test('a single-model recipe built from scratch saves exactly the expected payload', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Trend Mix');
    await expect(page.getByTestId('rb-name')).toHaveValue('trend_mix'); // suggested from the label
    await page.getByTestId('rb-description').fill('Scaled, then selected');
    await addStep(page, 'standard_scaler');
    await page.getByTestId('rb-s0-hp-with_mean').uncheck(); // default is on, so this is a change
    await addStep(page, 'select_k_best');
    await page.getByTestId('rb-s1-hp-k').fill('5');
    await addStep(page, 'clip_outliers');
    await page.getByTestId('rb-s2-hp-lower_pct').fill('1'); // equals the default, so nothing is stored
    await page.getByTestId('rb-model-kind-single').check();
    await page.getByTestId('rb-single-algo').selectOption('logistic_regression');
    await page.getByTestId('rb-single-hp-C').fill('0.5');
    await page.getByTestId('rb-calibrate').selectOption('sigmoid');
    await expect(page.getByTestId('rb-check-status')).toHaveText('No problems found');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toHaveText('Saved "trend_mix".');
    expect(await savedRecipe(page)).toEqual({
      name: 'trend_mix', label: 'Trend Mix', description: 'Scaled, then selected',
      steps: [
        { kind: 'standard_scaler', params: { with_mean: false } },
        { kind: 'select_k_best', params: { k: 5 } },
        { kind: 'clip_outliers', params: {} },
      ],
      model: { kind: 'single', algorithm: 'logistic_regression', params: { C: 0.5 } },
      calibrate: 'sigmoid',
    });
    // Now it is listed and the builder is clean and in edit mode.
    await expect(page.getByTestId('rb-mode')).toHaveText('Editing trend_mix');
    await expect(page.getByTestId('rb-dirty')).toHaveCount(0);
    await page.getByTestId('rb-discard').click();
    await expect(page.getByTestId('ml-algorithm-group-recipes').locator('option')).toHaveText(['Trend Mix', 'Build an algorithm...']);
  });

  test('voting: soft or hard, 2 to 6 members, each with its own weight and parameters', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Vote');
    await page.getByTestId('rb-model-kind-voting').check();
    await expect(page.getByTestId('rb-voting-soft')).toBeChecked();
    await expect(page.getByTestId('rb-member-count')).toContainText('2 of 6');
    await expect(page.getByTestId('rb-m0-remove')).toHaveCount(0); // at the minimum
    await page.getByTestId('rb-m0-weight').fill('2');
    await page.getByTestId('rb-m0-hp-n_estimators').fill('50');
    await page.getByTestId('rb-add-member').click();
    await page.getByTestId('rb-m2-algo').selectOption('gradient_boosting');
    await page.getByTestId('rb-m2-weight').fill('1.5');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toBeVisible();
    expect((await savedRecipe(page)).model).toEqual({
      kind: 'voting', voting: 'soft',
      members: [
        { algorithm: 'random_forest', params: { n_estimators: 50 }, weight: 2 },
        { algorithm: 'logistic_regression', params: {}, weight: 1 },
        { algorithm: 'gradient_boosting', params: {}, weight: 1.5 },
      ],
    });
    // Hard voting has no probabilities: calibration is flagged as ignored.
    await page.getByTestId('rb-voting-hard').check();
    await page.getByTestId('rb-calibrate').selectOption('isotonic');
    await expect(page.getByTestId('rb-calibrate-ignored')).toBeVisible();
    await page.getByTestId('rb-voting-soft').check();
    await expect(page.getByTestId('rb-calibrate-ignored')).toHaveCount(0);
    // Up to 6 members.
    for (let i = 0; i < 3; i++) await page.getByTestId('rb-add-member').click();
    await expect(page.getByTestId('rb-member-count')).toContainText('6 of 6');
    await expect(page.getByTestId('rb-add-member')).toBeDisabled();
    await page.getByTestId('rb-m5-remove').click();
    await expect(page.getByTestId('rb-member-count')).toContainText('5 of 6');
  });

  test('stacking: members, a final estimator, blocks, and a plain-words note', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Stack');
    await page.getByTestId('rb-model-kind-stacking').check();
    await expect(page.getByTestId('rb-stacking-note')).toContainText('never peeks ahead');
    await expect(page.getByTestId('rb-m0-weight')).toHaveCount(0); // weights are for voting
    await page.getByTestId('rb-final-algo').selectOption('gradient_boosting');
    await page.getByTestId('rb-final-hp-learning_rate').fill('0.05');
    await page.getByTestId('rb-blocks').fill('5');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toBeVisible();
    expect((await savedRecipe(page)).model).toEqual({
      kind: 'stacking',
      members: [{ algorithm: 'random_forest', params: {} }, { algorithm: 'logistic_regression', params: {} }],
      final: { algorithm: 'gradient_boosting', params: { learning_rate: 0.05 } },
      blocks: 5,
    });
  });

  test('stacking blocks outside 3 to 8 mark the model card and block Save', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Stack');
    await page.getByTestId('rb-model-kind-stacking').check();
    await page.getByTestId('rb-blocks').fill('2');
    await expect(page.getByTestId('rb-card-model')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('rb-issues-model')).toContainText('from 3 to 8');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
    await page.getByTestId('rb-blocks').fill('8');
    await expect(page.getByTestId('rb-card-model')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('rb-save')).toBeEnabled();
  });

  test('the add menu is grouped from the building blocks and stops at the step limit', async ({ page }) => {
    await page.getByTestId('rb-add-step').click();
    await expect(page.getByTestId('rb-add-group-scale').locator('button')).toHaveCount(5);
    await expect(page.getByTestId('rb-add-group-clean').locator('button')).toHaveText(['Clip outliers']);
    await expect(page.getByTestId('rb-add-group-select').locator('button')).toHaveCount(2);
    await expect(page.getByTestId('rb-add-group-reduce').locator('button')).toHaveText(['PCA']);
    await page.getByTestId('rb-add-step-robust_scaler').click();
    for (let i = 1; i < 8; i++) await addStep(page, 'minmax_scaler');
    await expect(page.getByTestId('rb-step-count')).toHaveText('8 of 8');
    await expect(page.getByTestId('rb-add-step')).toBeDisabled();
    await expect(page.getByTestId('rb-steps-full')).toBeVisible();
    await page.getByTestId('rb-step-remove-7').click();
    await expect(page.getByTestId('rb-add-step')).toBeEnabled();
  });

  test('steps reorder with the buttons and with Alt+arrow keys; focus follows the moved card', async ({ page }) => {
    await addStep(page, 'standard_scaler');
    await addStep(page, 'clip_outliers');
    await addStep(page, 'pca');
    expect(await stepKinds(page)).toEqual(['standard_scaler', 'clip_outliers', 'pca']);
    await page.getByTestId('rb-step-down-0').click();
    expect(await stepKinds(page)).toEqual(['clip_outliers', 'standard_scaler', 'pca']);
    await expect(page.getByTestId('rb-step-up-0')).toBeDisabled();
    await expect(page.getByTestId('rb-step-down-2')).toBeDisabled();
    // Parameters travel with their card.
    await page.getByTestId('rb-s0-hp-upper_pct').fill('95');
    await page.getByTestId('rb-step-up-2').click();
    expect(await stepKinds(page)).toEqual(['clip_outliers', 'pca', 'standard_scaler']);
    await expect(page.getByTestId('rb-s0-hp-upper_pct')).toHaveValue('95');

    await page.getByTestId('rb-card-step-2').focus();
    await page.keyboard.press('Alt+ArrowUp');
    expect(await stepKinds(page)).toEqual(['clip_outliers', 'standard_scaler', 'pca']);
    await expect(page.getByTestId('rb-card-step-1')).toBeFocused();
    await page.keyboard.press('Alt+ArrowUp');
    expect(await stepKinds(page)).toEqual(['standard_scaler', 'clip_outliers', 'pca']);
    await page.keyboard.press('Alt+ArrowUp'); // already first: nothing happens
    expect(await stepKinds(page)).toEqual(['standard_scaler', 'clip_outliers', 'pca']);
    await page.keyboard.press('Alt+ArrowDown');
    expect(await stepKinds(page)).toEqual(['clip_outliers', 'standard_scaler', 'pca']);
    // Plain arrows do nothing, and typing in a field is not mistaken for a move.
    await page.keyboard.press('ArrowDown');
    expect(await stepKinds(page)).toEqual(['clip_outliers', 'standard_scaler', 'pca']);
    await page.getByTestId('rb-s0-hp-upper_pct').focus();
    await page.keyboard.press('Alt+ArrowDown');
    expect(await stepKinds(page)).toEqual(['clip_outliers', 'standard_scaler', 'pca']);
    // The saved order is the order on screen.
    await page.getByTestId('rb-label').fill('Order');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toBeVisible();
    expect((await savedRecipe(page)).steps.map((s: { kind: string }) => s.kind)).toEqual(['clip_outliers', 'standard_scaler', 'pca']);
  });

  test('the pipeline diagram follows the recipe and jumps to a card', async ({ page }) => {
    await expect(page.getByTestId('rb-diagram')).toBeVisible();
    await addStep(page, 'pca');
    await page.getByTestId('rb-model-kind-voting').check();
    for (const id of ['data', 'step-0', 'model', 'calibration', 'output']) await expect(page.getByTestId(`rb-diagram-node-${id}`)).toBeVisible();
    await expect(page.getByTestId('rb-diagram-node-model')).toContainText('Voting');
    await expect(page.getByTestId('rb-diagram-node-step-0')).toContainText('PCA');
    await page.getByTestId('rb-diagram-node-calibration').click();
    await expect(page.getByTestId('rb-card-calibration')).toBeFocused();
    await page.getByTestId('rb-diagram-node-step-0').click();
    await expect(page.getByTestId('rb-card-step-0')).toBeFocused();
  });

  test('the calibration select explains each option in one line', async ({ page }) => {
    await expect(page.getByTestId('rb-calibrate-help')).toContainText('as they are');
    await page.getByTestId('rb-calibrate').selectOption('sigmoid');
    await expect(page.getByTestId('rb-calibrate-help')).toContainText('smooth curve');
    await page.getByTestId('rb-calibrate').selectOption('isotonic');
    await expect(page.getByTestId('rb-calibrate-help')).toContainText('step curve');
  });
});

test.describe('Check and validation', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await openNewBuilder(page); });

  test('a bad step parameter marks its own card, not the others, and blocks Save', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Check me');
    await addStep(page, 'standard_scaler');
    await addStep(page, 'select_k_best');
    await expect(page.getByTestId('rb-save')).toBeEnabled();
    await page.getByTestId('rb-s1-hp-k').fill('0'); // min is 1
    await expect(page.getByTestId('rb-card-step-1')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('rb-card-step-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('rb-card-model')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('rb-issues-step-1')).toContainText('steps[1].params.k');
    await expect(page.getByTestId('rb-diagram-node-step-1')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('rb-check-status')).toHaveText('1 problem to fix');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
    await page.getByTestId('rb-s1-hp-k').fill('3');
    await expect(page.getByTestId('rb-card-step-1')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('rb-save')).toBeEnabled();
  });

  test('a bad member parameter marks that member card; the name and label rules mark the name card', async ({ page }) => {
    await page.getByTestId('rb-model-kind-voting').check();
    await page.getByTestId('rb-m1-weight').fill('0');
    await expect(page.getByTestId('rb-card-member-1')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('rb-card-member-0')).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('rb-issues-member-1')).toContainText('model.members[1].weight');
    // No label or name yet.
    await expect(page.getByTestId('rb-card-identity')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('rb-issues-identity')).toContainText('Give the algorithm a name.');
    await page.getByTestId('rb-name').fill('Bad Name!');
    await expect(page.getByTestId('rb-issues-identity')).toContainText('lowercase letters');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
  });

  test('the Check shows step and model counts and the size estimate from the backend', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Counts');
    await addStep(page, 'robust_scaler');
    await addStep(page, 'pca');
    await page.getByTestId('rb-model-kind-stacking').check();
    await page.getByTestId('rb-add-member').click();
    // 3 members + the final estimator; the mock estimates 4 MB per model.
    await expect(page.getByTestId('rb-check-counts')).toHaveText('2 steps, 4 models');
    await expect(page.getByTestId('rb-check-size')).toHaveText('about 16 MB when trained');
    // The mock warns about PCA hiding the features.
    await expect(page.getByTestId('rb-check-warnings')).toContainText('PCA hides the original features');
  });

  test('the Check is debounced: typing sends one validate request, not one per key', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Debounce');
    await expect(page.getByTestId('rb-check-status')).toHaveText('No problems found');
    const before = (await ipc(page, 'validate_recipe')).length;
    await page.getByTestId('rb-description').pressSequentially('hello world', { delay: 20 });
    await expect(page.getByTestId('rb-check-status')).toHaveText('No problems found');
    await expect.poll(async () => (await ipc(page, 'validate_recipe')).length).toBe(before + 1);
    // Save waits for the Check on the latest edit.
    await page.getByTestId('rb-description').fill('x');
    await expect(page.getByTestId('rb-check-status')).toHaveText('Checking...');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
    await expect(page.getByTestId('rb-save')).toBeEnabled();
  });

  test('if the backend check itself fails, the user is told and Save stays possible', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Flaky');
    await expect(page.getByTestId('rb-check-status')).toHaveText('No problems found');
    await mockHook(page, 'failNext', 'validate_recipe', 'check crashed');
    await page.getByTestId('rb-description').fill('again');
    await expect(page.getByTestId('rb-check-error')).toContainText('check crashed');
    await expect(page.getByTestId('rb-save')).toBeEnabled();
  });

  test('a failed backend check drops the previous check result instead of keeping its warnings and size', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Stale');
    await addStep(page, 'pca');
    await expect(page.getByTestId('rb-check-warnings')).toContainText('PCA hides the original features');
    await expect(page.getByTestId('rb-check-size')).toHaveText('about 4 MB when trained');
    await mockHook(page, 'failNext', 'validate_recipe', 'check crashed');
    await page.getByTestId('rb-description').fill('again');
    await expect(page.getByTestId('rb-check-error')).toContainText('check crashed');
    // The old result described an older recipe: it must not keep contradicting "Saving will check again".
    await expect(page.getByTestId('rb-check-warnings')).toHaveCount(0);
    await expect(page.getByTestId('rb-check-size')).toHaveText('size unknown');
    await expect(page.getByTestId('rb-save')).toBeEnabled();
    // The next successful check brings the result back.
    await page.getByTestId('rb-description').fill('and again');
    await expect(page.getByTestId('rb-check-warnings')).toContainText('PCA hides the original features');
    await expect(page.getByTestId('rb-check-error')).toHaveCount(0);
  });

  test('a save error from the backend is shown inline and the draft stays dirty', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Will fail');
    await expect(page.getByTestId('rb-check-status')).toHaveText('No problems found');
    await mockHook(page, 'failNext', 'save_recipe', 'Disk is full');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-error')).toHaveText('Disk is full');
    await expect(page.getByTestId('rb-mode')).toHaveText('New algorithm');
    await expect(page.getByTestId('rb-dirty')).toBeVisible();
    await page.getByTestId('rb-save').click(); // a retry works
    await expect(page.getByTestId('rb-info')).toBeVisible();
  });

  test('very long names: the suggestion is cut to the limit and a too-long typed name is rejected', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Q'.repeat(120));
    const label = await page.getByTestId('rb-label').inputValue();
    expect(label.length).toBe(80);
    const name = await page.getByTestId('rb-name').inputValue();
    expect(name.length).toBeLessThanOrEqual(41);
    expect(name).toMatch(/^[a-z][a-z0-9_]*$/);
    await expect(page.getByTestId('rb-save')).toBeEnabled();
    await page.getByTestId('rb-name').fill('a'.repeat(60));
    await expect(page.getByTestId('rb-issues-identity')).toContainText('up to 41 characters');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
  });

  test('a name already used by another saved recipe is refused', async ({ page }) => {
    await mockHook(page, 'seedRecipe', { ...BASE, name: 'base' });
    await page.evaluate(() => (window as any).__apexMlTest.refreshRecipes());
    await page.getByTestId('rb-label').fill('Base');
    await expect(page.getByTestId('rb-name')).toHaveValue('base_2'); // the suggestion avoids the clash
    await page.getByTestId('rb-name').fill('base');
    await expect(page.getByTestId('rb-issues-identity')).toContainText('already exists');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
  });
});

test.describe('Loading and unavailable parts', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); });

  test('shows a loading state while the building blocks load', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await mockHook(page, 'delayNext', 'recipe_building_blocks', 900);
    await page.getByTestId('ml-algorithm-select').selectOption('__build__');
    await expect(page.getByTestId('rb-loading')).toBeVisible();
    await expect(page.getByTestId('rb-card-model')).toHaveCount(0);
    await expect(page.getByTestId('rb-card-model')).toBeVisible();
    await expect(page.getByTestId('rb-loading')).toHaveCount(0);
  });

  test('if the building blocks cannot load, it says so and Try again works', async ({ page }) => {
    await page.getByTestId('ml-tab-train').click();
    await mockHook(page, 'failNext', 'recipe_building_blocks', 'backend unreachable');
    await page.getByTestId('ml-algorithm-select').selectOption('__build__');
    await expect(page.getByTestId('rb-blocks-error')).toContainText('backend unreachable');
    await page.getByTestId('rb-blocks-retry').click();
    await expect(page.getByTestId('rb-card-model')).toBeVisible();
  });

  test('a member whose algorithm is gone shows a warning on its card, the Check reports it, and Save is blocked until it is replaced', async ({ page }) => {
    await seed(page, {
      ...BASE, name: 'with_cat', label: 'With cat',
      model: { kind: 'voting', voting: 'soft', members: [{ algorithm: 'random_forest', params: {}, weight: 1 }, { algorithm: 'catboost', params: { depth: 4 }, weight: 1 }] },
    });
    await openSavedBuilder(page, 'with_cat');
    await expect(page.getByTestId('rb-m1-unavailable')).toContainText('catboost');
    await expect(page.getByTestId('rb-m1-unavailable')).toContainText('kept in the Advanced box');
    await expect(page.getByTestId('rb-m1-adv-json')).toHaveValue(/"depth": 4/);
    await expect(page.getByTestId('rb-m0-unavailable')).toHaveCount(0);
    await expect(page.getByTestId('rb-card-member-1')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('rb-check-status')).toHaveText('1 problem to fix');
    await expect(page.getByTestId('rb-check-errors')).toContainText('catboost');
    await expect(page.getByTestId('rb-save')).toBeDisabled();
    await page.getByTestId('rb-m1-algo').selectOption('extra_trees');
    await expect(page.getByTestId('rb-m1-unavailable')).toHaveCount(0);
    await expect(page.getByTestId('rb-save')).toBeEnabled();
  });
});

test.describe('Templates', () => {
  test.beforeEach(async ({ page }) => { await openWorkbench(page); await openNewBuilder(page); });

  test('each example loads fully editable and saves unchanged', async ({ page }) => {
    await page.getByTestId('rb-template-scaled_logistic').click();
    await expect(page.getByTestId('rb-label')).toHaveValue('Scaled logistic regression');
    expect(await stepKinds(page)).toEqual(['standard_scaler']);
    await expect(page.getByTestId('rb-single-algo')).toHaveValue('logistic_regression');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toBeVisible();
    expect(await savedRecipe(page)).toMatchObject({ name: 'scaled_logistic', steps: [{ kind: 'standard_scaler', params: {} }], model: { kind: 'single', algorithm: 'logistic_regression', params: {} }, calibrate: 'none' });
  });

  test('the voting and stacking examples fill the model card; replacing unsaved work asks first', async ({ page }) => {
    await page.getByTestId('rb-template-stacked_trees').click();
    await expect(page.getByTestId('rb-model-kind-stacking')).toBeChecked();
    await expect(page.getByTestId('rb-blocks')).toHaveValue('4');
    await expect(page.getByTestId('rb-member-count')).toContainText('3 of 6');
    await expect(page.getByTestId('rb-final-algo')).toHaveValue('logistic_regression');
    // The draft now differs from a blank one: choosing another example asks.
    await page.getByTestId('rb-template-voting_mix').click();
    await expect(page.getByTestId('rb-template-confirm-bar')).toBeVisible();
    await page.getByTestId('rb-template-keep').click();
    await expect(page.getByTestId('rb-model-kind-stacking')).toBeChecked();
    await page.getByTestId('rb-template-voting_mix').click();
    await page.getByTestId('rb-template-confirm').click();
    await expect(page.getByTestId('rb-model-kind-voting')).toBeChecked();
    await expect(page.getByTestId('rb-voting-soft')).toBeChecked();
    await expect(page.getByTestId('rb-m2-algo')).toHaveValue('logistic_regression');
    // Editable: change one part and save.
    await page.getByTestId('rb-m0-weight').fill('3');
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toBeVisible();
    expect((await savedRecipe(page)).model.members[0]).toEqual({ algorithm: 'random_forest', params: {}, weight: 3 });
  });

  test('an example whose name is taken gets a free one', async ({ page }) => {
    await mockHook(page, 'seedRecipe', { ...BASE, name: 'pca_forest' });
    await page.evaluate(() => (window as any).__apexMlTest.refreshRecipes());
    await page.getByTestId('rb-template-pca_forest').click();
    await expect(page.getByTestId('rb-name')).toHaveValue('pca_forest_2');
    await expect(page.getByTestId('rb-save')).toBeEnabled();
  });
});

test.describe('Save as copy, delete, discard, unsaved changes', () => {
  test.beforeEach(async ({ page }) => {
    await openWorkbench(page);
    await seed(page, BASE, { ...BASE, name: 'other', label: 'Other' });
    await openSavedBuilder(page, 'base_algo');
  });

  test('loading a saved recipe shows its parts, clean', async ({ page }) => {
    await expect(page.getByTestId('rb-mode')).toHaveText('Editing base_algo');
    await expect(page.getByTestId('rb-label')).toHaveValue('Base');
    await expect(page.getByTestId('rb-description')).toHaveValue('seeded');
    expect(await stepKinds(page)).toEqual(['standard_scaler']);
    await expect(page.getByTestId('rb-dirty')).toHaveCount(0);
    await expect(page.getByTestId('rb-discard')).toHaveText('Close');
    await expect(page.getByTestId('rb-template-scaled_logistic')).toHaveCount(0); // examples are for new recipes
    // Editing the label does not rename a saved recipe.
    await page.getByTestId('rb-label').fill('Base v2');
    await expect(page.getByTestId('rb-name')).toHaveValue('base_algo');
    await expect(page.getByTestId('rb-dirty')).toBeVisible();
    await page.getByTestId('rb-save').click();
    await expect(page.getByTestId('rb-info')).toBeVisible();
    expect(await savedRecipe(page)).toMatchObject({ name: 'base_algo', label: 'Base v2' });
    await expect(page.getByTestId('rb-dirty')).toHaveCount(0);
  });

  test('Save as copy suggests a name, refuses a duplicate, then saves a second recipe and leaves the first', async ({ page }) => {
    await page.getByTestId('rb-description').fill('changed before copying');
    await page.getByTestId('rb-save-copy').click();
    await expect(page.getByTestId('rb-copy-name')).toHaveValue('base_algo_copy');
    await page.getByTestId('rb-copy-name').fill('other');
    await page.getByTestId('rb-copy-confirm').click();
    await expect(page.getByTestId('rb-copy-error')).toContainText('already exists');
    expect(await ipc(page, 'save_recipe')).toHaveLength(0);
    await page.getByTestId('rb-copy-name').fill('Not Valid');
    await page.getByTestId('rb-copy-confirm').click();
    await expect(page.getByTestId('rb-copy-error')).toContainText('lowercase letters');
    await page.getByTestId('rb-copy-name').fill('base_two');
    await page.getByTestId('rb-copy-confirm').click();
    await expect(page.getByTestId('rb-info')).toHaveText('Saved a copy as "base_two".');
    const calls = await ipc(page, 'save_recipe');
    expect(calls).toHaveLength(1);
    expect(calls[0].args.recipe).toMatchObject({ name: 'base_two', label: 'Base (copy)', description: 'changed before copying' });
    await expect(page.getByTestId('rb-mode')).toHaveText('Editing base_two');
    await expect(page.getByTestId('rb-dirty')).toHaveCount(0);
    await page.getByTestId('rb-discard').click();
    await expect(page.getByTestId('ml-algorithm-group-recipes').locator('option')).toHaveText(['Base', 'Base (copy)', 'Other', 'Build an algorithm...']);
    // The original is untouched.
    await page.getByTestId('ml-algorithm-select').selectOption('recipe:base_algo');
    await page.getByTestId('ml-recipe-edit').click();
    await expect(page.getByTestId('rb-description')).toHaveValue('seeded');
  });

  test('Delete asks first; Cancel keeps it; Confirm removes it and closes the builder', async ({ page }) => {
    await page.getByTestId('rb-delete').click();
    await expect(page.getByTestId('rb-delete-bar')).toContainText('base_algo');
    await page.getByTestId('rb-delete-cancel').click();
    await expect(page.getByTestId('rb-delete-bar')).toHaveCount(0);
    expect(await ipc(page, 'delete_recipe')).toHaveLength(0);
    await page.getByTestId('rb-delete').click();
    await page.getByTestId('rb-delete-confirm').click();
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
    expect((await lastCall(page, 'delete_recipe')).args).toEqual({ name: 'base_algo' });
    await expect(page.getByTestId('ml-algorithm-group-recipes').locator('option')).toHaveText(['Other', 'Build an algorithm...']);
    // It was the selected algorithm: the form falls back to a built-in one.
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('random_forest');
  });

  test('a brand-new recipe has no Delete or Save as copy', async ({ page }) => {
    await page.getByTestId('rb-discard').click();
    await page.getByTestId('ml-algorithm-select').selectOption('__build__');
    await expect(page.getByTestId('rb-delete')).toHaveCount(0);
    await expect(page.getByTestId('rb-save-copy')).toHaveCount(0);
  });

  test('Discard changes asks when dirty and closes at once when clean', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Edited');
    await expect(page.getByTestId('rb-discard')).toHaveText('Discard changes');
    await page.getByTestId('rb-discard').click();
    await expect(page.getByTestId('rb-discard-bar')).toBeVisible();
    await page.getByTestId('rb-discard-keep').click();
    await expect(page.getByTestId('rb-label')).toHaveValue('Edited');
    await page.getByTestId('rb-discard').click();
    await page.getByTestId('rb-discard-confirm').click();
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
    // Reopening shows the saved version again.
    await page.getByTestId('ml-recipe-edit').click();
    await expect(page.getByTestId('rb-label')).toHaveValue('Base');
    await page.getByTestId('rb-discard').click();
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
  });

  test('leaving the Train tab with unsaved changes asks first; Stay keeps the draft, Leave drops it', async ({ page }) => {
    await page.getByTestId('rb-label').fill('Unsaved work');
    await page.getByTestId('ml-tab-models').click();
    await expect(page.getByTestId('ml-leave-guard')).toBeVisible();
    await expect(page.getByTestId('ml-models-tab')).toHaveCount(0);
    await page.getByTestId('ml-leave-stay').click();
    await expect(page.getByTestId('ml-leave-guard')).toHaveCount(0);
    await expect(page.getByTestId('rb-label')).toHaveValue('Unsaved work');
    // Other navigation inside the workbench is guarded too.
    await page.evaluate(() => (window as any).__apexMlTest.open('model', 'some_model'));
    await expect(page.getByTestId('ml-leave-guard')).toBeVisible();
    await page.getByTestId('ml-leave-stay').click();
    await page.getByTestId('ml-tab-data').click();
    await page.getByTestId('ml-leave-confirm').click();
    await expect(page.getByTestId('ml-dataset-builder')).toBeVisible();
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
  });

  test('a clean builder lets you switch tabs freely and the draft survives going away and back', async ({ page }) => {
    await page.getByTestId('ml-tab-models').click();
    await expect(page.getByTestId('ml-models-tab')).toBeVisible();
    await expect(page.getByTestId('ml-leave-guard')).toHaveCount(0);
    await page.getByTestId('ml-tab-train').click();
    await expect(page.getByTestId('rb-label')).toHaveValue('Base');
  });
});

test.describe('Training with a recipe', () => {
  test.beforeEach(async ({ page }) => {
    await openWorkbench(page);
    await seed(page, BASE, { ...BASE, name: 'other', label: 'Other' });
  });

  test('choosing a recipe replaces the parameter editor with a read-only summary and Edit recipe', async ({ page }) => {
    await page.getByTestId('ml-algorithm-select').selectOption('recipe:base_algo');
    await expect(page.getByTestId('ml-recipe-summary')).toBeVisible();
    await expect(page.getByTestId('ml-recipe-label')).toHaveText('Base');
    await expect(page.getByTestId('ml-recipe-step-0')).toHaveText('Standard scaler');
    await expect(page.getByTestId('ml-recipe-model')).toContainText('Random forest');
    await expect(page.getByTestId('ml-recipe-calibration')).toHaveText('No calibration');
    await expect(page.getByTestId('ml-recipe-diagram-node-step-0')).toBeVisible();
    await expect(page.getByTestId('ml-param-editor')).toHaveCount(0);
    await expect(page.getByTestId('ml-adv-json')).toHaveCount(0);
    await expect(page.getByTestId('ml-algorithm-desc')).toHaveText('seeded');
    await page.getByTestId('ml-recipe-edit').click();
    await expect(page.getByTestId('rb-mode')).toHaveText('Editing base_algo');
    // Back to a built-in algorithm: the editor returns.
    await page.getByTestId('rb-discard').click();
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await expect(page.getByTestId('ml-param-editor')).toBeVisible();
    await expect(page.getByTestId('ml-recipe-summary')).toHaveCount(0);
  });

  test('the training request carries algorithm "recipe", the recipe name and empty hyperparameters', async ({ page }) => {
    await buildDataset(page, 'rc-train');
    await page.getByTestId('ml-algorithm-select').selectOption('recipe:other');
    await page.getByTestId('ml-train-button').click();
    await expect(page.getByTestId('ml-train-result')).toBeVisible();
    const req = (await lastCall(page, 'train_ml_model')).args.request;
    expect(req.algorithm).toBe('recipe');
    expect(req.recipe).toBe('other');
    expect(req.hyperparams).toEqual({});
    expect(req.dataset).toBe('rc-train');
    // Built-in training does not send a recipe.
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await page.getByTestId('ml-train-button').click();
    await expect.poll(async () => (await ipc(page, 'train_ml_model')).length).toBe(2);
    const second = (await lastCall(page, 'train_ml_model')).args.request;
    expect(second.algorithm).toBe('random_forest');
    expect('recipe' in second).toBe(false);
  });

  test('Train with this saves the draft, selects it in the Train form and closes the builder', async ({ page }) => {
    await page.getByTestId('ml-algorithm-select').selectOption('__build__');
    await page.getByTestId('rb-template-pca_forest').click();
    await page.getByTestId('rb-train-with').click();
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
    expect((await lastCall(page, 'save_recipe')).args.recipe.name).toBe('pca_forest');
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('recipe:pca_forest');
    await expect(page.getByTestId('ml-recipe-summary')).toBeVisible();
    // A clean saved recipe is selected without saving again.
    await page.getByTestId('ml-recipe-edit').click();
    const saves = (await ipc(page, 'save_recipe')).length;
    await page.getByTestId('rb-train-with').click();
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
    expect((await ipc(page, 'save_recipe')).length).toBe(saves);
  });

  test('Train with this is blocked while the draft has errors', async ({ page }) => {
    await page.getByTestId('ml-algorithm-select').selectOption('__build__');
    await expect(page.getByTestId('rb-check-status')).toContainText('problem');
    await expect(page.getByTestId('rb-train-with')).toBeDisabled();
  });

  test('the compare queue can include saved recipes', async ({ page }) => {
    await buildDataset(page, 'rc-compare');
    await expect(page.getByTestId('ml-compare-algo-recipe:base_algo')).toBeVisible();
    await expect(page.getByTestId('ml-compare-params-recipe:base_algo')).toHaveText('my algorithm');
    // Default queue is random forest + gradient boosting; add one recipe, drop gradient boosting.
    await page.getByTestId('ml-compare-algo-gradient_boosting').uncheck();
    await page.getByTestId('ml-compare-algo-recipe:other').check();
    await page.getByTestId('ml-compare-run').click();
    await expect(page.getByTestId('ml-leaderboard-status-recipe:other')).toHaveText('done');
    await expect(page.getByTestId('ml-leaderboard-row-recipe:other')).toContainText('Other');
    const reqs = (await ipc(page, 'train_ml_model')).map((c) => c.args.request);
    expect(reqs.map((r: { algorithm: string }) => r.algorithm)).toEqual(['random_forest', 'recipe']);
    expect(reqs[1].recipe).toBe('other');
    expect(reqs[1].hyperparams).toEqual({});
    await expect(page.getByTestId('ml-leaderboard-open-recipe:other')).toBeVisible();
  });
});

test.describe('A remembered recipe that no longer exists', () => {
  test('is reported on the Train tab and blocks Train until another is chosen', async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem('apex-ml-train-settings', JSON.stringify({ algorithm: 'recipe', recipe: 'ghost' })));
    await openWorkbench(page);
    await buildDataset(page, 'rc-ghost');
    await expect(page.getByTestId('ml-recipe-missing')).toContainText('"ghost" no longer exists');
    await expect(page.getByTestId('ml-algorithm-select')).toHaveValue('recipe:ghost');
    await expect(page.getByTestId('ml-train-button')).toBeDisabled();
    await expect(page.getByTestId('ml-train-validation')).toContainText('"ghost" no longer exists');
    await page.getByTestId('ml-algorithm-select').selectOption('random_forest');
    await expect(page.getByTestId('ml-train-button')).toBeEnabled();
  });
});
