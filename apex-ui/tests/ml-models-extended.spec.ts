import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import {
  HOLDOUT_GAP_PTS, classMeaning, holdoutVerdict, labelText, modelVerdict, shortingContext,
} from '../src/components/ml/modelUtils';
import type { HoldoutDto, ModelDetailDto } from '../src/lib/types';

const holdout = (over: Partial<HoldoutDto> = {}): HoldoutDto => ({
  rows: 96, accuracy: 0.58, baseline_accuracy: 0.5,
  trading: { strategy_return_pct: 2.1, buy_hold_return_pct: 1.4, hit_rate: 0.55, trades: 11, max_drawdown_pct: -2.2, sharpe: 0.6 },
  ...over,
});
const metrics = (accuracy: number) => ({ accuracy, f1: accuracy - 0.03, precision: accuracy, recall: accuracy });
const trading = (over: Partial<NonNullable<ModelDetailDto['trading']>> = {}) => ({
  strategy_return_pct: 8, buy_hold_return_pct: 5, hit_rate: 0.55, trades: 60, max_drawdown_pct: -4, sharpe: 1.2, ...over,
});

async function make(page: Page, patch: Partial<ModelDetailDto> = {}): Promise<string> {
  return page.evaluate((p) => (window as unknown as { __apexMock: { trainModelCustom: (x: unknown) => string } }).__apexMock.trainModelCustom(p), patch);
}
async function openFirst(page: Page) {
  await page.getByTestId('tab-ml').click();
  await page.getByTestId('ml-tab-models').click();
  await page.getByTestId('ml-model-row').first().click();
  await expect(page.getByTestId('model-detail')).toBeVisible();
}

test.describe('hold-out verdict logic', () => {
  test('threshold is documented as 5 points and exactly 5 counts as overfit', () => {
    expect(HOLDOUT_GAP_PTS).toBe(5);
    expect(holdoutVerdict({ metrics: metrics(0.6), holdout: holdout({ accuracy: 0.56 }) }).id).toBe('held');
    expect(holdoutVerdict({ metrics: metrics(0.6), holdout: holdout({ accuracy: 0.55 }) }).id).toBe('overfit');
    expect(holdoutVerdict({ metrics: metrics(0.6), holdout: holdout({ accuracy: 0.4 }) }).tone).toBe('bad');
  });

  test('a hold-out better than cross-validation held up; one below the majority class has no edge', () => {
    expect(holdoutVerdict({ metrics: metrics(0.6), holdout: holdout({ accuracy: 0.7 }) }).id).toBe('held');
    const v = holdoutVerdict({ metrics: metrics(0.5), holdout: holdout({ accuracy: 0.49, baseline_accuracy: 0.6 }) });
    expect(v.id).toBe('no-edge');
    expect(v.tone).toBe('warn');
  });

  test('no hold-out says so and points at the Train tab', () => {
    const v = holdoutVerdict({ metrics: metrics(0.6), holdout: null });
    expect(v.id).toBe('none');
    expect(v.text).toContain('No hold-out was set');
    expect(v.text).toContain('Train tab');
  });

  test('without a cross-validation accuracy nothing is claimed', () => {
    expect(holdoutVerdict({ metrics: {}, holdout: holdout() }).id).toBe('unknown');
  });
});

test.describe('shorting context and verdict', () => {
  const model = { model_id: 'm1', trade_mode: 'long_short' as const };
  const fold = { fold: 1, train_rows: 200, test_rows: 60, accuracy: 0.6, f1: 0.6, precision: 0.6, recall: 0.6, baseline_accuracy: 0.5, trade_return_pct: 1, hit_rate: 0.5, trades: 12 };
  const base = { metrics: metrics(0.6), cv_folds: [fold], rows: 500, trade_mode: 'long_short' as const };

  test('long/flat models and models without automations are never limited', () => {
    expect(shortingContext({ model_id: 'm1', trade_mode: 'long_flat' }, [{ name: 'a', model_id: 'm1', allow_short: false, broker_id: 'paper' }], []).limited).toBe(false);
    expect(shortingContext(model, [], []).limited).toBe(false);
  });

  test('an automation without allow_short, or on a broker that cannot short, limits it', () => {
    const brokers = [{ broker_id: 'paper', can_short: true }, { broker_id: 'zerodha', can_short: false }];
    expect(shortingContext(model, [{ name: 'a', model_id: 'm1', allow_short: false, broker_id: 'paper' }], brokers).reasons[0]).toContain('long only');
    expect(shortingContext(model, [{ name: 'b', model_id: 'm1', allow_short: true, broker_id: 'zerodha' }], brokers).reasons[0]).toContain('zerodha, which cannot short');
    expect(shortingContext(model, [{ name: 'c', model_id: 'm1', allow_short: true, broker_id: 'paper' }], brokers).limited).toBe(false);
    expect(shortingContext(model, [{ name: 'd', model_id: 'other', allow_short: false, broker_id: 'paper' }], brokers).limited).toBe(false);
  });

  test('the verdict does not call a long/short return a success when the automations cannot short', () => {
    const t = trading({ strategy_return_pct: 20, buy_hold_return_pct: 5 });
    const free = modelVerdict({ ...base, trading: t });
    expect(free.tone).toBe('good');
    expect(free.text).toContain('ahead of buy and hold');
    const limited = modelVerdict({ ...base, trading: t }, { limited: true, reasons: ['x'] });
    expect(limited.tone).toBe('warn');
    expect(limited.text).not.toContain('ahead of buy and hold');
    expect(limited.text).toContain('do not rely on it');
  });

  test('class meaning follows the label kind, and asymmetric thresholds show as up/down', () => {
    expect(classMeaning({ horizon_bars: 5, kind: 'direction', threshold_pct: 0 })).toContain('Class 0 sells short');
    expect(classMeaning({ horizon_bars: 5, kind: 'return_threshold', threshold_pct: 1 })).toContain('Class -1 sells short');
    expect(labelText({ horizon_bars: 5, kind: 'return_threshold', threshold_pct: 1 })).toBe('Move beyond ±1% after 5 bars');
    expect(labelText({ horizon_bars: 5, kind: 'return_threshold', threshold_pct: 1, up_threshold_pct: 1.5, down_threshold_pct: 0.5 })).toBe('Up beyond +1.5% / down beyond -0.5% after 5 bars');
  });
});

test.describe('model detail: new fields', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
  });

  test('hold-out card: held up, likely overfit, and not set', async ({ page }) => {
    await make(page, { display_name: 'Held', metrics: metrics(0.6), holdout: holdout({ accuracy: 0.58, rows: 96 }) });
    await openFirst(page);
    await expect(page.getByTestId('model-holdout')).toHaveAttribute('data-verdict', 'held');
    await expect(page.getByTestId('holdout-rows')).toHaveText('96');
    await expect(page.getByTestId('holdout-accuracy')).toHaveText('58.0%');
    await expect(page.getByTestId('holdout-baseline')).toContainText('baseline 50.0%');
    await expect(page.getByTestId('holdout-trading')).toHaveText('+2.1%');
    await expect(page.getByTestId('model-holdout-verdict')).toContainText('Held up');
  });

  test('hold-out much worse than cross-validation is flagged as overfit', async ({ page }) => {
    await make(page, { display_name: 'Overfit', metrics: metrics(0.7), holdout: holdout({ accuracy: 0.52 }) });
    await openFirst(page);
    await expect(page.getByTestId('model-holdout')).toHaveAttribute('data-verdict', 'overfit');
    await expect(page.getByTestId('model-holdout-verdict')).toContainText('likely overfit');
    await expect(page.getByTestId('model-holdout-verdict')).toHaveAttribute('data-tone', 'bad');
  });

  test('no hold-out says so and hints at the Train tab', async ({ page }) => {
    await make(page, { display_name: 'None', holdout: null });
    await openFirst(page);
    await expect(page.getByTestId('model-holdout')).toHaveAttribute('data-verdict', 'none');
    await expect(page.getByTestId('model-holdout-verdict')).toContainText('No hold-out was set');
    await expect(page.getByTestId('holdout-rows')).toHaveCount(0);
  });

  test('trading rule sits next to the return with an explanatory tooltip', async ({ page }) => {
    await make(page, { display_name: 'Rule', trade_mode: 'long_short' });
    await openFirst(page);
    const rule = page.getByTestId('model-trading-rule');
    await expect(rule).toHaveText('Trading rule: Long / short');
    await expect(rule).toHaveAttribute('title', /sell short/);
    await expect(page.getByTestId('config-trade-mode')).toHaveText('Long / short');
  });

  test('a long/flat model shows the long/flat rule and no shorting note', async ({ page }) => {
    await make(page, { display_name: 'Flat', trade_mode: 'long_flat' });
    await openFirst(page);
    await expect(page.getByTestId('model-trading-rule')).toHaveText('Trading rule: Long / flat');
    await expect(page.getByTestId('model-trading-rule')).toHaveAttribute('title', /No short positions/);
    await expect(page.getByTestId('model-shorting-note')).toHaveCount(0);
  });

  test('a long/short model used by an automation that cannot short gets a note and no success claim', async ({ page }) => {
    const id = await make(page, { display_name: 'LS', trade_mode: 'long_short', metrics: metrics(0.6), trading: trading({ strategy_return_pct: 20 }) });
    await page.evaluate((m) => (window as unknown as { __apexMock: { createAutomationAsCopilot: (n: string, m: string) => string } }).__apexMock.createAutomationAsCopilot('Long only bot', m), id);
    await openFirst(page);
    await expect(page.getByTestId('model-shorting-note')).toContainText('Long only bot is long only');
    await expect(page.getByTestId('model-verdict')).toContainText('do not rely on it');
    await expect(page.getByTestId('model-verdict')).not.toContainText('ahead of buy and hold');
    await expect(page.getByTestId('model-verdict')).toHaveAttribute('data-tone', 'warn');
  });

  test('a long/short model with no automation using it has no note and keeps the normal verdict', async ({ page }) => {
    await make(page, { display_name: 'LS free', trade_mode: 'long_short', metrics: metrics(0.6), trading: trading({ strategy_return_pct: 20 }) });
    await openFirst(page);
    await expect(page.getByTestId('model-shorting-note')).toHaveCount(0);
    await expect(page.getByTestId('model-verdict')).toContainText('ahead of buy and hold');
  });

  test('custom features, context symbols, fee, estimator, random state and asymmetric label thresholds', async ({ page }) => {
    await make(page, {
      display_name: 'Rich', algorithm: 'custom', estimator_file: 'my_tree', fee_pct: 0.1, hyperparams: { random_state: 42 },
      custom_features: [{ name: 'mom_gap', expr: 'sma(close, 5) / sma(close, 20) - 1' }, { name: 'idx_ret', expr: 'ret(other("^NSEI","close"), 3)' }],
      context_symbols: ['^NSEI'], label: { horizon_bars: 5, kind: 'return_threshold', threshold_pct: 1, up_threshold_pct: 1.5, down_threshold_pct: 0.5 },
    });
    await openFirst(page);
    await expect(page.getByTestId('config-custom-features')).toBeVisible();
    await expect(page.getByTestId('custom-feature-expr-mom_gap')).toHaveText('sma(close, 5) / sma(close, 20) - 1');
    await expect(page.getByTestId('custom-feature-expr-mom_gap')).toHaveCSS('font-family', /mono/i);
    await expect(page.getByTestId('custom-feature-idx_ret')).toBeVisible();
    await expect(page.getByTestId('config-context-symbols')).toHaveText('^NSEI');
    await expect(page.getByTestId('config-fee')).toContainText('0.1%');
    await expect(page.getByTestId('config-estimator')).toHaveText('my_tree');
    await expect(page.getByTestId('config-estimator-warning')).toContainText('Custom code');
    await expect(page.getByTestId('config-estimator-warning')).toContainText('saved with this model');
    await expect(page.getByTestId('config-random-state')).toHaveText('42');
    await expect(page.getByTestId('config-label')).toHaveText('Up beyond +1.5% / down beyond -0.5% after 5 bars');
  });

  test('the copy button puts the formula on the clipboard', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await make(page, { display_name: 'Copy', custom_features: [{ name: 'mom_gap', expr: 'close / sma(close, 20)' }] });
    await openFirst(page);
    await page.getByTestId('custom-feature-copy-mom_gap').click();
    await expect(page.getByTestId('custom-feature-copy-mom_gap')).toHaveText('Copied');
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('close / sma(close, 20)');
  });

  test('a plain model has no custom feature, context or estimator sections', async ({ page }) => {
    await make(page, { display_name: 'Plain' });
    await openFirst(page);
    await expect(page.getByTestId('config-custom-features')).toHaveCount(0);
    await expect(page.getByTestId('config-context-symbols')).toHaveCount(0);
    await expect(page.getByTestId('config-estimator')).toHaveCount(0);
    await expect(page.getByTestId('config-fee')).toHaveText('none');
    await expect(page.getByTestId('config-random-state')).toContainText('default');
  });
});

test.describe('model compare: new fields', () => {
  test('diffs trade mode, hold-out, fee, estimator and custom features; best hold-out result is highlighted', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-ml')).toBeVisible({ timeout: 10000 });
    const a = await make(page, { display_name: 'A', trade_mode: 'long_flat', fee_pct: 0, holdout: holdout({ accuracy: 0.55, trading: { ...holdout().trading!, strategy_return_pct: 1 } }) });
    const b = await make(page, {
      display_name: 'B', trade_mode: 'long_short', fee_pct: 0.2, estimator_file: 'my_tree', holdout: holdout({ accuracy: 0.6, trading: { ...holdout().trading!, strategy_return_pct: 4 } }),
      custom_features: [{ name: 'g', expr: 'ret(close, 2)' }],
    });
    const c = await make(page, { display_name: 'C', holdout: null });
    await page.getByTestId('tab-ml').click();
    await page.getByTestId('ml-tab-models').click();
    const checks = page.getByTestId('ml-model-check');
    await checks.nth(0).check();
    await checks.nth(1).check();
    await checks.nth(2).check();
    await page.getByTestId('ml-models-compare').click();
    await expect(page.getByTestId('model-compare')).toBeVisible();

    for (const row of ['trade-mode', 'holdout', 'fee', 'estimator', 'custom-features']) {
      await expect(page.getByTestId(`compare-config-${row}-${a}`)).toHaveAttribute('data-differs', 'true');
    }
    await expect(page.getByTestId(`compare-config-trade-mode-${b}`)).toHaveText('Long / short');
    await expect(page.getByTestId(`compare-config-trade-mode-${a}`)).toHaveText('Long / flat');
    await expect(page.getByTestId(`compare-config-holdout-${c}`)).toHaveText('none');
    await expect(page.getByTestId(`compare-config-estimator-${b}`)).toHaveText('my_tree');
    await expect(page.getByTestId(`compare-config-custom-features-${b}`)).toContainText('g = ret(close, 2)');
    await expect(page.getByTestId(`compare-config-context-symbols-${a}`)).toHaveAttribute('data-differs', 'false');

    await expect(page.getByTestId(`compare-cell-holdout-accuracy-${b}`)).toHaveAttribute('data-best', 'true');
    await expect(page.getByTestId(`compare-cell-holdout-accuracy-${a}`)).toHaveAttribute('data-best', 'false');
    await expect(page.getByTestId(`compare-cell-holdout-return-${b}`)).toHaveAttribute('data-best', 'true');
    await expect(page.getByTestId(`compare-cell-holdout-return-${b}`)).toHaveText('+4.0%');
    await expect(page.getByTestId(`compare-cell-holdout-return-${c}`)).toHaveText('n/a');
  });
});
