import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { actionLabel } from '../src/components/activity/activityUtils';
import {
  algorithmFilterKey, algorithmFilterOptions, describeStep, importanceReason, matchesAlgorithmFilter, matchesQuery, modelAlgorithmLabel,
  recipeFingerprint, recipeStatus, retrainDetailFor, suggestRecipeName,
} from '../src/components/ml/modelUtils';
import type { RegistryRow } from '../src/components/ml/modelUtils';
import { MOCK_BLOCKS } from '../src/lib/mockMl';
import type { ModelDetailDto, RecipeDto } from '../src/lib/types';

// ── Fixtures ────────────────────────────────────────────────────────

const recipe = (over: Partial<RecipeDto> & Pick<RecipeDto, 'name'>): RecipeDto => ({
  label: over.name, description: '', steps: [], calibrate: 'none',
  model: { kind: 'single', algorithm: 'random_forest', params: {} },
  ...over,
});

const SINGLE = recipe({
  name: 'rf_scaled', label: 'Scaled forest', description: 'Forest on scaled features',
  steps: [{ kind: 'standard_scaler', params: { with_std: false } }, { kind: 'select_k_best', params: { k: 5, score: 'f_classif' } }],
  model: { kind: 'single', algorithm: 'random_forest', params: { n_estimators: 300 } },
});
const VOTING = recipe({
  name: 'vote_mix', label: 'Vote mix', calibrate: 'sigmoid',
  steps: [{ kind: 'robust_scaler', params: {} }],
  model: {
    kind: 'voting', voting: 'soft',
    members: [{ algorithm: 'random_forest', params: { max_depth: 6 }, weight: 2 }, { algorithm: 'logistic_regression', params: {}, weight: 1 }],
  },
});
const STACKING = recipe({
  name: 'stack_one', label: 'Stack one', calibrate: 'isotonic',
  steps: [{ kind: 'clip_outliers', params: { lower_pct: 2, upper_pct: 98 } }],
  model: {
    kind: 'stacking', blocks: 5,
    members: [{ algorithm: 'random_forest', params: {} }, { algorithm: 'extra_trees', params: { n_estimators: 150 } }],
    final: { algorithm: 'logistic_regression', params: { C: 0.5 } },
  },
});
const PCA_HARD = recipe({
  name: 'pca_hard', label: 'PCA hard vote',
  steps: [{ kind: 'pca', params: { n_components: 0.9, whiten: true } }],
  model: { kind: 'voting', voting: 'hard', members: [{ algorithm: 'random_forest', params: {} }, { algorithm: 'extra_trees', params: {} }] },
  calibrate: 'sigmoid',
});

// ── Page helpers ────────────────────────────────────────────────────

type Hooks = { trainModelCustom: (patch: Partial<ModelDetailDto>) => string };

/** Create a model in the mock registry whose algorithm is "recipe" and which carries the given recipe copy. */
function makeRecipeModel(page: Page, copy: RecipeDto | null, patch: Partial<ModelDetailDto> = {}): Promise<string> {
  return page.evaluate(
    ([r, p]) => (window as unknown as { __apexMock: Hooks }).__apexMock.trainModelCustom({ algorithm: 'recipe', recipe: r, ...p } as Partial<ModelDetailDto>),
    [copy, patch] as const,
  );
}
function makeBuiltIn(page: Page, patch: Partial<ModelDetailDto> = {}): Promise<string> {
  return page.evaluate((p) => (window as unknown as { __apexMock: Hooks }).__apexMock.trainModelCustom(p), patch);
}

/** Run an API wrapper of lib/tauri.ts in the page (web mode talks to the in-memory mock). */
function api<T>(page: Page, fn: 'saveRecipe' | 'deleteRecipe' | 'listRecipes', arg?: unknown): Promise<T> {
  return page.evaluate(async ([name, a]) => {
    const path = '/src/lib/tauri.ts';
    const mod = await import(/* @vite-ignore */ path) as Record<string, (x?: unknown) => Promise<unknown>>;
    return mod[name as string](a);
  }, [fn, arg] as const) as Promise<T>;
}

async function openModels(page: Page) {
  await page.getByTestId('tab-ml').click();
  await page.getByTestId('ml-tab-models').click();
}
async function openModel(page: Page, modelId: string) {
  await page.locator(`[data-testid="ml-model-row"][data-model-id="${modelId}"]`).click();
  await expect(page.getByTestId('model-detail')).toHaveAttribute('data-model-id', modelId);
}

/** The Train tab has the recipe selected and does not report it as missing (so Train would send `recipe: <name>`). */
async function expectTrainFormUses(page: Page, name: string) {
  await expect(page.getByTestId('ml-algorithm-select')).toHaveValue(`recipe:${name}`);
  await expect(page.getByTestId('ml-recipe-summary')).toBeVisible();
  await expect(page.getByTestId('ml-recipe-missing')).toHaveCount(0);
}

// ── Pure helpers ────────────────────────────────────────────────────

test.describe('recipe helpers', () => {
  test('a recipe model is labelled with the recipe, other models with the algorithm', () => {
    expect(modelAlgorithmLabel({ algorithm: 'recipe', recipe: SINGLE })).toBe('Scaled forest');
    expect(modelAlgorithmLabel({ algorithm: 'recipe', recipe: null })).toBe('Recipe');
    expect(modelAlgorithmLabel({ algorithm: 'random_forest', recipe: SINGLE })).toBe('Random Forest');
  });

  test('the fingerprint ignores key order, default weights, label and timestamps but not parameters', () => {
    const a = recipe({
      name: 'x', label: 'One', created_at: 'a',
      model: { kind: 'voting', voting: 'soft', members: [{ algorithm: 'random_forest', params: { a: 1, b: 2 } }, { algorithm: 'extra_trees', params: {}, weight: 1 }] },
    });
    const same = recipe({
      name: 'x', label: 'Two', updated_at: 'b',
      model: { kind: 'voting', voting: 'soft', members: [{ algorithm: 'random_forest', params: { b: 2, a: 1 }, weight: 1 }, { algorithm: 'extra_trees', params: {} }] },
    });
    expect(recipeFingerprint(a)).toBe(recipeFingerprint(same));
    const other = recipe({
      name: 'x',
      model: { kind: 'voting', voting: 'soft', members: [{ algorithm: 'random_forest', params: { a: 1, b: 3 } }, { algorithm: 'extra_trees', params: {} }] },
    });
    expect(recipeFingerprint(a)).not.toBe(recipeFingerprint(other));
    expect(recipeFingerprint(recipe({ name: 'x', calibrate: 'sigmoid' }))).not.toBe(recipeFingerprint(recipe({ name: 'x' })));
    expect(recipeFingerprint(recipe({ name: 'x', steps: [{ kind: 'pca', params: {} }] }))).not.toBe(recipeFingerprint(recipe({ name: 'x' })));
  });

  test('status is same, edited or missing', () => {
    expect(recipeStatus(SINGLE, [SINGLE]).status).toBe('same');
    expect(recipeStatus(SINGLE, [{ ...SINGLE, steps: [] }]).status).toBe('edited');
    expect(recipeStatus(SINGLE, [VOTING]).status).toBe('missing');
    expect(recipeStatus(SINGLE, []).current).toBeNull();
  });

  test('a trained copy with every step default written out matches the recipe as it was typed', () => {
    // What the trainer stores: recipes.prepare fills each step's parameters; the saved file keeps what the user typed.
    const typed = recipe({
      name: 'filled', steps: [{ kind: 'standard_scaler', params: {} }, { kind: 'pca', params: { n_components: 0.9 } }, { kind: 'robust_scaler', params: {} }],
    });
    const trained = recipe({
      name: 'filled',
      steps: [
        { kind: 'standard_scaler', params: { with_mean: true, with_std: true } },
        { kind: 'pca', params: { n_components: 0.9, whiten: false } },
        { kind: 'robust_scaler', params: {} },
      ],
    });
    expect(recipeStatus(trained, [typed], MOCK_BLOCKS).status).toBe('same');
    expect(recipeFingerprint(typed, MOCK_BLOCKS)).toBe(recipeFingerprint(trained, MOCK_BLOCKS));
    // A real edit is still seen: the saved step now turns centring off, or asks for more variance.
    const centred = recipe({ ...typed, name: 'filled', steps: [{ kind: 'standard_scaler', params: { with_mean: false } }, ...typed.steps.slice(1)] });
    expect(recipeStatus(trained, [centred], MOCK_BLOCKS).status).toBe('edited');
    const wider = recipe({ ...typed, name: 'filled', steps: [typed.steps[0], { kind: 'pca', params: { n_components: 0.99 } }, typed.steps[2]] });
    expect(recipeStatus(trained, [wider], MOCK_BLOCKS).status).toBe('edited');
    // An omitted parameter means its default: PCA with nothing typed is 0.95, not 0.9.
    const bare = recipe({ ...typed, name: 'filled', steps: [typed.steps[0], { kind: 'pca', params: {} }, typed.steps[2]] });
    expect(recipeStatus(trained, [bare], MOCK_BLOCKS).status).toBe('edited');
  });

  test('copy names get a suffix, stay within 41 characters and skip taken names', () => {
    expect(suggestRecipeName('rf_scaled', [])).toBe('rf_scaled_copy');
    expect(suggestRecipeName('rf_scaled', ['rf_scaled_copy'])).toBe('rf_scaled_copy2');
    const long = 'a'.repeat(41);
    const name = suggestRecipeName(long, []);
    expect(name.length).toBeLessThanOrEqual(41);
    expect(name.endsWith('_copy')).toBe(true);
    expect(/^[a-z][a-z0-9_]{0,40}$/.test(suggestRecipeName(long, [name]))).toBe(true);
  });

  test('importances are unavailable with PCA or hard voting, and only then', () => {
    expect(importanceReason(PCA_HARD)).toContain('PCA');
    expect(importanceReason(PCA_HARD)).toContain('Hard voting');
    expect(importanceReason(recipe({ name: 'p', steps: [{ kind: 'pca', params: {} }] }))).not.toContain('Hard voting');
    expect(importanceReason(SINGLE)).toBeNull();
    expect(importanceReason(VOTING)).toBeNull();
    expect(importanceReason(null)).toBeNull();
  });

  test('step lines show only changed parameters', () => {
    expect(describeStep({ kind: 'standard_scaler', params: { with_mean: true, with_std: false } })).toBe('Standard scaler (with_std = false)');
    expect(describeStep({ kind: 'robust_scaler', params: {} })).toBe('Robust scaler');
    expect(describeStep({ kind: 'pca', params: { n_components: 0.95 } })).toBe('PCA');
  });

  test('the algorithm filter treats recipes by name and all together', () => {
    const rows: RegistryRow[] = [
      { summary: { model_id: 'a', algorithm: 'random_forest' } as RegistryRow['summary'], detail: null },
      { summary: { model_id: 'b', algorithm: 'recipe' } as RegistryRow['summary'], detail: { recipe: SINGLE } as ModelDetailDto },
      { summary: { model_id: 'c', algorithm: 'recipe' } as RegistryRow['summary'], detail: { recipe: VOTING } as ModelDetailDto },
    ];
    expect(algorithmFilterOptions(rows).map((o) => [o.value, o.label])).toEqual([
      ['random_forest', 'Random Forest'], ['recipe', 'All recipes'], ['recipe:rf_scaled', 'Scaled forest (recipe)'], ['recipe:vote_mix', 'Vote mix (recipe)'],
    ]);
    expect(algorithmFilterKey(rows[1].summary, rows[1].detail)).toBe('recipe:rf_scaled');
    expect(matchesAlgorithmFilter('recipe', rows[1].summary, rows[1].detail)).toBe(true);
    expect(matchesAlgorithmFilter('recipe', rows[0].summary, null)).toBe(false);
    expect(matchesAlgorithmFilter('recipe:vote_mix', rows[1].summary, rows[1].detail)).toBe(false);
    expect(matchesAlgorithmFilter('recipe:vote_mix', rows[2].summary, rows[2].detail)).toBe(true);
  });

  test('search finds a recipe model by its label and by a member algorithm', () => {
    const row: RegistryRow = { summary: { model_id: 'b', algorithm: 'recipe' } as RegistryRow['summary'], detail: { recipe: VOTING, tags: [], symbols: [] } as unknown as ModelDetailDto };
    expect(matchesQuery(row, 'vote mix')).toBe(true);
    expect(matchesQuery(row, 'logistic')).toBe(true);
    expect(matchesQuery(row, 'xgboost')).toBe(false);
  });

  test('retrain hands over the recipe to select', () => {
    const d = retrainDetailFor({ algorithm: 'recipe', recipe: SINGLE, model_id: 'm' } as ModelDetailDto, VOTING);
    expect(d.algorithm).toBe('recipe');
    expect(d.recipe?.name).toBe('vote_mix');
  });

  test('activity labels for recipe actions', () => {
    expect(actionLabel('recipe.save')).toBe('Saved algorithm recipe');
    expect(actionLabel('recipe.delete')).toBe('Deleted algorithm recipe');
    expect(actionLabel('recipe.other')).toBe('recipe other');
  });
});

// ── Models list ─────────────────────────────────────────────────────

test.describe('recipe models in the list', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  });

  test('shows the recipe label and a recipe badge, and none on built-in models', async ({ page }) => {
    const builtIn = await makeBuiltIn(page, {});
    const rec = await makeRecipeModel(page, SINGLE);
    await openModels(page);
    const recRow = page.locator(`[data-testid="ml-model-row"][data-model-id="${rec}"]`);
    await expect(recRow.getByTestId('ml-model-algorithm')).toHaveText('Scaled forest');
    await expect(recRow.getByTestId('ml-model-recipe-badge')).toHaveText('recipe');
    const builtRow = page.locator(`[data-testid="ml-model-row"][data-model-id="${builtIn}"]`);
    await expect(builtRow.getByTestId('ml-model-algorithm')).toHaveText('Random Forest');
    await expect(builtRow.getByTestId('ml-model-recipe-badge')).toHaveCount(0);
  });

  test('the algorithm filter lists recipes by label and filters', async ({ page }) => {
    const builtIn = await makeBuiltIn(page, {});
    const a = await makeRecipeModel(page, SINGLE);
    const b = await makeRecipeModel(page, VOTING);
    await openModels(page);
    const filter = page.getByTestId('ml-models-filter-algorithm');
    const labels = await filter.locator('option').allTextContents();
    expect(labels).toEqual(['All algorithms', 'Random Forest', 'All recipes', 'Scaled forest (recipe)', 'Vote mix (recipe)']);
    await filter.selectOption('recipe:vote_mix');
    await expect(page.getByTestId('ml-model-row')).toHaveCount(1);
    await expect(page.getByTestId('ml-model-row')).toHaveAttribute('data-model-id', b);
    await filter.selectOption('recipe');
    await expect(page.getByTestId('ml-model-row')).toHaveCount(2);
    await filter.selectOption('random_forest');
    await expect(page.getByTestId('ml-model-row')).toHaveCount(1);
    await expect(page.getByTestId('ml-model-row')).toHaveAttribute('data-model-id', builtIn);
    expect(a).not.toBe(b);
  });
});

// ── Detail ──────────────────────────────────────────────────────────

test.describe('recipe section in the model detail', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  });

  test('single model recipe: ordered steps with changed parameters, model, calibration, saved-copy note', async ({ page }) => {
    const id = await makeRecipeModel(page, SINGLE);
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('model-algorithm')).toHaveText('Scaled forest');
    await expect(page.getByTestId('model-recipe-badge')).toBeVisible();
    await expect(page.getByTestId('model-section-recipe')).toBeVisible();
    const steps = page.getByTestId('recipe-step');
    await expect(steps).toHaveCount(2);
    await expect(steps.nth(0).getByTestId('recipe-step-name')).toHaveText('Standard scaler');
    await expect(steps.nth(1).getByTestId('recipe-step-name')).toHaveText('Keep the best k features');
    // with_std = false differs from the default, k = 5 differs, score = f_classif does not.
    await expect(steps.nth(0).getByTestId('recipe-step-params').locator('[data-changed="true"]')).toHaveText('with_std = false');
    await expect(steps.nth(1).getByTestId('recipe-step-params').locator('[data-changed="true"]')).toHaveText('k = 5');
    await expect(steps.nth(1).getByTestId('recipe-step-params').locator('[data-changed="false"]')).toHaveText('score = f_classif');
    await expect(page.getByTestId('recipe-model-kind')).toHaveText('Single model');
    await expect(page.getByTestId('recipe-member')).toHaveCount(1);
    await expect(page.getByTestId('recipe-member-name')).toHaveText('Random Forest');
    await expect(page.getByTestId('recipe-member-params')).toContainText('n_estimators = 300');
    await expect(page.getByTestId('recipe-calibration')).toHaveText('none');
    await expect(page.getByTestId('recipe-saved-copy-note')).toContainText('Editing or deleting the recipe later does not change this model');
    // No empty hyperparameter table for a recipe model.
    await expect(page.getByTestId('hyper-table')).toHaveCount(0);
    await expect(page.getByTestId('config-recipe-params')).toBeVisible();
    await expect(page.getByTestId('config-algorithm')).toHaveText('Scaled forest (recipe)');
  });

  test('a built-in model has no recipe section and keeps its hyperparameter table', async ({ page }) => {
    const id = await makeBuiltIn(page, { hyperparams: { n_estimators: 300 } });
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('model-section-recipe')).toHaveCount(0);
    await expect(page.getByTestId('model-recipe-badge')).toHaveCount(0);
    await expect(page.getByTestId('model-nav-recipe')).toHaveCount(0);
    await expect(page.getByTestId('hyper-table')).toBeVisible();
    await expect(page.getByTestId('features-empty')).toHaveCount(0);
  });

  test('voting recipe shows the weights and calibration', async ({ page }) => {
    const id = await makeRecipeModel(page, VOTING);
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('recipe-model-kind')).toContainText('Voting ensemble, soft (2 models)');
    const members = page.getByTestId('recipe-member');
    await expect(members).toHaveCount(2);
    await expect(members.nth(0).getByTestId('recipe-member-name')).toHaveText('Random Forest');
    await expect(members.nth(0).getByTestId('recipe-member-weight')).toHaveText('weight 2');
    await expect(members.nth(0).getByTestId('recipe-member-params')).toContainText('max_depth = 6');
    await expect(members.nth(1).getByTestId('recipe-member-name')).toHaveText('Logistic Regression');
    await expect(members.nth(1).getByTestId('recipe-member-weight')).toHaveText('weight 1');
    await expect(members.nth(1).getByTestId('recipe-member-params')).toHaveText('default settings');
    await expect(page.getByTestId('recipe-calibration')).toHaveText('sigmoid');
    await expect(page.getByTestId('recipe-calibration-ignored')).toHaveCount(0);
  });

  test('stacking recipe shows members, the final estimator and the number of blocks', async ({ page }) => {
    const id = await makeRecipeModel(page, STACKING);
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('recipe-model-kind')).toContainText('Stacking ensemble, 5 blocks');
    await expect(page.getByTestId('recipe-member')).toHaveCount(2);
    await expect(page.getByTestId('recipe-member-weight')).toHaveCount(0);
    await expect(page.getByTestId('recipe-final')).toHaveAttribute('data-algorithm', 'logistic_regression');
    await expect(page.getByTestId('recipe-final-params')).toContainText('C = 0.5');
    await expect(page.getByTestId('recipe-blocks')).toContainText('5 time blocks');
    await expect(page.getByTestId('recipe-step-params').locator('[data-changed="true"]')).toHaveText(['lower_pct = 2', 'upper_pct = 98']);
    await expect(page.getByTestId('recipe-calibration')).toHaveText('isotonic');
  });

  test('without importances, PCA and hard voting are named as the reason', async ({ page }) => {
    const id = await makeRecipeModel(page, PCA_HARD, { feature_importance: [] });
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('features-empty')).toContainText('PCA replaces the original features');
    await expect(page.getByTestId('features-empty')).toContainText('Hard voting');
    await expect(page.getByTestId('feature-bars')).toHaveCount(0);
    // Hard voting ignores calibration, and the section says so.
    await expect(page.getByTestId('recipe-calibration-ignored')).toBeVisible();
  });

  test('importances with a selection step say only the kept features are listed', async ({ page }) => {
    const id = await makeRecipeModel(page, SINGLE);
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('feature-bars')).toBeVisible();
    await expect(page.getByTestId('features-selection-note')).toContainText('kept by the selection steps');
  });

  test('a recipe model with no recorded recipe says so and retrains like any model', async ({ page }) => {
    const id = await makeRecipeModel(page, null);
    await openModels(page);
    await expect(page.getByTestId('ml-model-algorithm')).toHaveText('Recipe');
    await openModel(page, id);
    await expect(page.getByTestId('recipe-missing-copy')).toBeVisible();
    await expect(page.getByTestId('recipe-summary')).toHaveCount(0);
    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('recipe-retrain-prompt')).toHaveCount(0);
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
  });
});

// ── Retrain ─────────────────────────────────────────────────────────

test.describe('retrain a recipe model', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  });

  test('unchanged saved recipe: goes straight to the Train tab and saves nothing', async ({ page }) => {
    const saved = await api<RecipeDto>(page, 'saveRecipe', SINGLE);
    const id = await makeRecipeModel(page, saved);
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('recipe-saved-status')).toHaveAttribute('data-status', 'same');
    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByTestId('recipe-retrain-prompt')).toHaveCount(0);
    await expectTrainFormUses(page, 'rf_scaled');
    expect((await api<RecipeDto[]>(page, 'listRecipes')).map((r) => r.name)).toEqual(['rf_scaled']);
  });

  test('a saved recipe typed with minimal parameters is unchanged against the copy the trainer stores with defaults filled in', async ({ page }) => {
    const typed = await api<RecipeDto>(page, 'saveRecipe', recipe({
      name: 'typed_min', label: 'Typed', steps: [{ kind: 'standard_scaler', params: {} }, { kind: 'pca', params: { n_components: 0.9 } }],
    }));
    const stored: RecipeDto = {
      ...typed,
      steps: [{ kind: 'standard_scaler', params: { with_mean: true, with_std: true } }, { kind: 'pca', params: { n_components: 0.9, whiten: false } }],
    };
    const id = await makeRecipeModel(page, stored);
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('recipe-saved-status')).toHaveAttribute('data-status', 'same');
    await expect(page.getByTestId('recipe-saved-status')).toContainText('unchanged');
    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('recipe-retrain-prompt')).toHaveCount(0);
    await expectTrainFormUses(page, 'typed_min');
  });

  test('retraining while a clean algorithm builder is open shows the Train form that was filled in, not the builder', async ({ page }) => {
    const saved = await api<RecipeDto>(page, 'saveRecipe', SINGLE);
    const id = await makeRecipeModel(page, saved);
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-algorithm-select').selectOption('recipe:rf_scaled');
    await page.getByTestId('ml-recipe-edit').click();
    await expect(page.getByTestId('ml-recipe-builder')).toBeVisible();
    await page.getByTestId('ml-tab-models').click(); // a clean builder does not block leaving
    await openModel(page, id);
    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByTestId('ml-recipe-builder')).toHaveCount(0);
    await expectTrainFormUses(page, 'rf_scaled');
  });

  test('a recipe edited since training: says so and saves the model copy under a new name before training', async ({ page }) => {
    const original = await api<RecipeDto>(page, 'saveRecipe', SINGLE);
    const id = await makeRecipeModel(page, original);
    await api(page, 'saveRecipe', { ...SINGLE, steps: [], model: { kind: 'single', algorithm: 'extra_trees', params: {} } });
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('recipe-saved-status')).toHaveAttribute('data-status', 'edited');
    await expect(page.getByTestId('recipe-saved-status')).toContainText('edited since');
    // The model still shows its own copy, not the edited recipe.
    await expect(page.getByTestId('recipe-member-name')).toHaveText('Random Forest');

    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('recipe-retrain-prompt')).toHaveAttribute('data-status', 'edited');
    await expect(page.getByTestId('recipe-retrain-message')).toContainText('edited after this model was trained');
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'false');
    await expect(page.getByTestId('recipe-retrain-name')).toHaveValue('rf_scaled_copy');

    await page.getByTestId('recipe-retrain-save').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
    // The copy was saved straight through the API, so the form must pick it up rather than report it missing.
    await expectTrainFormUses(page, 'rf_scaled_copy');
    const list = await api<RecipeDto[]>(page, 'listRecipes');
    expect(list.map((r) => r.name).sort()).toEqual(['rf_scaled', 'rf_scaled_copy']);
    const copy = list.find((r) => r.name === 'rf_scaled_copy');
    expect(copy && recipeFingerprint(copy)).toBe(recipeFingerprint(original));
    // The edited recipe was left alone.
    expect(list.find((r) => r.name === 'rf_scaled')?.steps).toEqual([]);
  });

  test('an edited recipe can also be used as it is now, without saving anything', async ({ page }) => {
    const original = await api<RecipeDto>(page, 'saveRecipe', SINGLE);
    const id = await makeRecipeModel(page, original);
    await api(page, 'saveRecipe', { ...SINGLE, steps: [] });
    await openModels(page);
    await openModel(page, id);
    await page.getByTestId('model-retrain').click();
    await page.getByTestId('recipe-retrain-use-saved').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
    await expectTrainFormUses(page, 'rf_scaled');
    expect((await api<RecipeDto[]>(page, 'listRecipes')).map((r) => r.name)).toEqual(['rf_scaled']);
  });

  test('a deleted recipe: says so, can be cancelled, and a taken suggestion is skipped', async ({ page }) => {
    const original = await api<RecipeDto>(page, 'saveRecipe', SINGLE);
    const id = await makeRecipeModel(page, original);
    await api(page, 'saveRecipe', { ...VOTING, name: 'rf_scaled_copy' });
    await api(page, 'deleteRecipe', 'rf_scaled');
    await openModels(page);
    await openModel(page, id);
    await expect(page.getByTestId('recipe-saved-status')).toHaveAttribute('data-status', 'missing');
    await page.getByTestId('model-retrain').click();
    await expect(page.getByTestId('recipe-retrain-prompt')).toHaveAttribute('data-status', 'missing');
    await expect(page.getByTestId('recipe-retrain-message')).toContainText('has been deleted');
    await expect(page.getByTestId('recipe-retrain-use-saved')).toHaveCount(0);
    await expect(page.getByTestId('recipe-retrain-name')).toHaveValue('rf_scaled_copy2');

    // An existing name is refused instead of overwritten.
    await page.getByTestId('recipe-retrain-name').fill('rf_scaled_copy');
    await expect(page.getByTestId('recipe-retrain-problem')).toContainText('already exists');
    await expect(page.getByTestId('recipe-retrain-save')).toBeDisabled();
    await page.getByTestId('recipe-retrain-name').fill('Bad Name');
    await expect(page.getByTestId('recipe-retrain-problem')).toContainText('lowercase');

    await page.getByTestId('recipe-retrain-cancel').click();
    await expect(page.getByTestId('recipe-retrain-prompt')).toHaveCount(0);
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'false');

    await page.getByTestId('model-retrain').click();
    await page.getByTestId('recipe-retrain-save').click();
    await expect(page.getByTestId('ml-tab-train')).toHaveAttribute('aria-selected', 'true');
    await expectTrainFormUses(page, 'rf_scaled_copy2');
    const list = await api<RecipeDto[]>(page, 'listRecipes');
    const restored = list.find((r) => r.name === 'rf_scaled_copy2');
    expect(restored && recipeFingerprint(restored)).toBe(recipeFingerprint(original));
    expect(list.find((r) => r.name === 'rf_scaled_copy')?.label).toBe('Vote mix');
  });
});

// ── Compare ─────────────────────────────────────────────────────────

test.describe('comparing recipe models', () => {
  test('the configuration diff shows steps, members, final estimator and calibration', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
    const a = await makeRecipeModel(page, SINGLE);
    const b = await makeRecipeModel(page, STACKING);
    const c = await makeBuiltIn(page, {});
    await openModels(page);
    for (const row of await page.getByTestId('ml-model-check').all()) await row.check();
    await page.getByTestId('ml-models-compare').click();
    await expect(page.getByTestId('model-compare')).toBeVisible();

    await expect(page.getByTestId(`compare-algorithm-${a}`)).toHaveText('Scaled forest');
    await expect(page.getByTestId(`compare-recipe-badge-${a}`)).toBeVisible();
    await expect(page.getByTestId(`compare-recipe-badge-${c}`)).toHaveCount(0);

    const cell = (row: string, id: string) => page.getByTestId(`compare-config-${row}-${id}`);
    await expect(cell('recipe-steps', a)).toHaveText('1. Standard scaler (with_std = false)\n2. Keep the best k features (k = 5)');
    await expect(cell('recipe-steps', b)).toHaveText('1. Clip outliers (lower_pct = 2, upper_pct = 98)');
    await expect(cell('recipe-steps', c)).toHaveText('n/a');
    await expect(cell('recipe-steps', a)).toHaveAttribute('data-differs', 'true');
    await expect(cell('recipe-model', a)).toHaveText('Single model');
    await expect(cell('recipe-model', b)).toContainText('Stacking ensemble, 5 blocks');
    await expect(cell('recipe-members', a)).toHaveText('Random Forest (n_estimators = 300)');
    await expect(cell('recipe-members', b)).toHaveText('Random Forest\nExtra Trees (n_estimators = 150)');
    await expect(cell('recipe-final', a)).toHaveText('none');
    await expect(cell('recipe-final', b)).toHaveText('Logistic Regression (C = 0.5)');
    await expect(cell('recipe-calibration', a)).toHaveText('none');
    await expect(cell('recipe-calibration', b)).toHaveText('isotonic');
    await expect(cell('algorithm', a)).toHaveText('Scaled forest');
    await expect(cell('algorithm', c)).toHaveText('Random Forest');
  });

  test('two recipe models with the same steps show them as equal', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
    const a = await makeRecipeModel(page, SINGLE);
    const b = await makeRecipeModel(page, { ...SINGLE, model: { kind: 'single', algorithm: 'random_forest', params: { n_estimators: 50 } } });
    await openModels(page);
    for (const row of await page.getByTestId('ml-model-check').all()) await row.check();
    await page.getByTestId('ml-models-compare').click();
    await expect(page.getByTestId(`compare-config-recipe-steps-${a}`)).toHaveAttribute('data-differs', 'false');
    await expect(page.getByTestId(`compare-config-recipe-members-${a}`)).toHaveAttribute('data-differs', 'true');
    await expect(page.getByTestId(`compare-config-recipe-members-${b}`)).toHaveText('Random Forest (n_estimators = 50)');
    await page.getByTestId('compare-only-diffs').check();
    await expect(page.getByTestId(`compare-config-recipe-steps-${a}`)).toHaveCount(0);
    await expect(page.getByTestId(`compare-config-recipe-members-${a}`)).toBeVisible();
  });

  test('without recipe models the diff has no recipe rows', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
    await makeBuiltIn(page, {});
    await makeBuiltIn(page, {});
    await openModels(page);
    for (const row of await page.getByTestId('ml-model-check').all()) await row.check();
    await page.getByTestId('ml-models-compare').click();
    await expect(page.getByTestId('model-compare')).toBeVisible();
    await expect(page.locator('[data-testid^="compare-config-recipe-"]')).toHaveCount(0);
  });
});

// ── Activity ────────────────────────────────────────────────────────

test.describe('activity labels', () => {
  test('saving and deleting a recipe read as plain actions', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-activity')).toBeVisible({ timeout: 10000 });
    await page.getByTestId('tab-activity').click();
    await expect(page.getByTestId('activity-panel')).toBeVisible();
    await api(page, 'saveRecipe', SINGLE);
    await expect(page.getByTestId('activity-action').first()).toHaveText('Saved algorithm recipe');
    await api(page, 'deleteRecipe', 'rf_scaled');
    await expect(page.getByTestId('activity-action').first()).toHaveText('Deleted algorithm recipe');
    await expect(page.getByTestId('activity-action')).toHaveCount(2);
  });
});
