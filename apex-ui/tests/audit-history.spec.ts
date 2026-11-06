import { test, expect, type Page } from '@playwright/test';
import { diffLines, eventFacts, foldContext, groupOrderEvents, latestRestorable, restoreBlockedReason, sourceLabel } from '../src/components/audit/auditUtils';
import type { OrderEventDto, VersionDto } from '../src/lib/types';

/**
 * Immutable audit ledger UI: version history, diff, restore, deleted objects, integrity and order timelines.
 * Web mode: the Rust ledger is replaced by lib/mockLedger.ts (same rules: append-only, identical save = no-op,
 * delete = tombstone, restore = new version, hash-chained records and event streams).
 */

const STRATEGY = 'strategies/my_strategy.py';
const MARKER = '# audit marker line';

type LedgerHooks = {
  tamper: (kind: string, id: string, version: number, mode?: string) => void;
  tamperEvent: (stream: string, seq: number) => void;
  externalEdit: (kind: string, id: string, text: string) => unknown;
  putReference: (kind: string, id: string, text: string, actor?: string, summary?: string) => unknown;
  history: (kind: string, id: string) => VersionDto[];
};
type Mock = { ledger: LedgerHooks } & Record<string, (...a: unknown[]) => unknown>;

const mock = <T = unknown>(page: Page, name: string, ...args: unknown[]) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const) as Promise<T>;

const ledgerHook = <T = unknown>(page: Page, name: keyof LedgerHooks, ...args: unknown[]) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Mock }).__apexMock.ledger[n as keyof LedgerHooks] as (...x: unknown[]) => unknown)(...(a as unknown[])), [name, args] as const) as Promise<T>;

/** Run an API wrapper of lib/tauri.ts in the page (web mode talks to the in-memory mock). */
const api = <T = unknown>(page: Page, fn: string, ...args: unknown[]) =>
  page.evaluate(async ([name, a]) => {
    const path = '/src/lib/tauri.ts';
    const mod = await import(/* @vite-ignore */ path) as Record<string, (...x: unknown[]) => Promise<unknown>>;
    return mod[name as string](...(a as unknown[]));
  }, [fn, args] as const) as Promise<T>;

async function openIde(page: Page) {
  await page.goto('/');
  await page.getByTestId('tab-strategy').click();
  await expect(page.getByTestId('strategy-editor').locator('.monaco-editor')).toBeVisible({ timeout: 15000 });
}

async function typeAndSave(page: Page, text: string) {
  const editor = page.getByTestId('strategy-editor');
  await editor.locator('.view-lines').click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type(`\n${text}`);
  await expect(editor.locator('.view-lines')).toContainText(text);
  await page.getByTestId('save-strategy').click();
  await expect(page.getByTestId('save-confirmation')).toBeVisible();
}

const rows = (page: Page) => page.getByTestId('audit-version-row');

test.describe('Diff and order helpers', () => {
  test('line diff marks added and removed lines and folds unchanged ones', () => {
    const d = diffLines('a\nb\nc\nd', 'a\nB\nc\nd\ne');
    expect(d.ops.filter((o) => o.type === 'del').map((o) => o.text)).toEqual(['b']);
    expect(d.ops.filter((o) => o.type === 'add').map((o) => o.text)).toEqual(['B', 'e']);
    expect(d.added).toBe(2);
    expect(d.removed).toBe(1);
    expect(diffLines('same\ntext', 'same\ntext')).toMatchObject({ added: 0, removed: 0 });
    expect(diffLines('', 'x')).toMatchObject({ added: 1, removed: 0 });
    const long = Array.from({ length: 40 }, (_, i) => `line ${i}`).join('\n');
    const folded = foldContext(diffLines(long, `${long}\nextra`).ops, 3);
    expect(folded.some((r) => r.type === 'fold')).toBe(true);
    expect(folded.filter((r) => r.type === 'add')).toHaveLength(1);
  });

  test('large inputs are capped instead of freezing the page', () => {
    const big = Array.from({ length: 5000 }, (_, i) => `row ${i}`).join('\n');
    const d = diffLines(big, `${big}\nmore`, 2000);
    expect(d.capped).toBe(true);
    expect(d.ops.length).toBeLessThanOrEqual(2000);
  });

  test('restore rules and source labels', () => {
    const v = (version: number, p: Partial<VersionDto>): VersionDto => ({
      seq: version, kind: 'strategy', object_id: 'a.py', version, action: 'update', actor: 'user', ts: '2026-01-01T00:00:00Z', content_hash: 'h'.repeat(64),
      size: 3, retained: true, parent_version: null, restored_from: null, summary: '', hash: 'x', prev_hash: 'y', ...p,
    });
    const list = [v(1, {}), v(2, { retained: false }), v(3, { action: 'delete', content_hash: null })];
    expect(restoreBlockedReason(list[2], list[2])).toContain('delete marker');
    expect(restoreBlockedReason(list[1], list[2])).toContain('write-once');
    expect(restoreBlockedReason(list[0], list[2])).toBeNull();
    expect(restoreBlockedReason(list[0], list[0])).toContain('current');
    expect(latestRestorable(list)?.version).toBe(1);
    expect(sourceLabel({ kind: 'automation', id: 'a', version: 3, hash: null })).toBe('automation@v3');
    expect(sourceLabel({ kind: 'manual', id: '', version: null, hash: null })).toBe('manual');
    const ev = (seq: number, order_id: string, event: string, data: Record<string, unknown> = {}): OrderEventDto => ({
      seq, ts: '2026-01-01T00:00:00Z', order_id, broker_id: 'paper', event, actor: 'user', source: null, data, hash: 'h', prev_hash: 'p',
    });
    const groups = groupOrderEvents([ev(1, 'o1', 'submitted'), ev(2, 'o2', 'submitted'), ev(3, 'o1', 'risk_rejected', { reason: 'too big' })]);
    expect(groups.map((g) => [g.orderId, g.state])).toEqual([['o1', 'rejected'], ['o2', 'submitted']]);
  });
});

test.describe('Order events as the backend writes them', () => {
  // The Rust order stream: submitted, risk_accepted and risk_rejected have an EMPTY order_id and are tied to the
  // order by data.request_id; fills carry delta_qty and avg_price (quantity and price are the order's own).
  const ev = (seq: number, order_id: string, event: string, data: Record<string, unknown> = {}): OrderEventDto => ({
    seq, ts: '2026-01-01T00:00:00Z', order_id, broker_id: 'paper', event, actor: 'user',
    source: { kind: 'strategy', id: 'strategies/mom.py', version: 3, hash: 'h3' }, data, hash: `h${seq}`, prev_hash: 'p',
  });
  const request = { symbol: 'AAPL', side: 'buy', order_type: 'market', quantity: 10, price: null };

  test('submitted and risk events are grouped under the order their request became, not under System events', () => {
    const events = [
      ev(1, '', 'live_armed'),
      ev(2, '', 'submitted', { request_id: 'r1', live: false, confirmed: null, request }),
      ev(3, '', 'risk_accepted', { request_id: 'r1' }),
      ev(4, 'PAPER-1', 'broker_ack', { request_id: 'r1', broker_order_id: 'PAPER-1' }),
      ev(5, 'PAPER-1', 'fill', { status: 'filled', filled_qty: 10, avg_price: 101.5, delta_qty: 10, quantity: 10, price: null }),
      ev(6, '', 'submitted', { request_id: 'r2', live: true, request: { ...request, symbol: 'TCS.NS' } }),
      ev(7, '', 'risk_rejected', { request_id: 'r2', reason: 'Live trading is not armed.', stage: 'live_guard' }),
    ];
    const groups = groupOrderEvents(events);
    const byState = Object.fromEntries(groups.map((g) => [g.state, g]));
    expect(groups).toHaveLength(3);
    // The order: all four events, with symbol, side and quantity from the submitted request.
    const order = groups.find((g) => g.orderId === 'PAPER-1')!;
    expect(order.events.map((e) => e.event)).toEqual(['submitted', 'risk_accepted', 'broker_ack', 'fill']);
    expect([order.symbol, order.side, order.quantity, order.state]).toEqual(['AAPL', 'buy', 10, 'filled']);
    // A request that never became an order is its own rejected card, not System events.
    expect(byState.rejected.orderId).toBe('');
    expect(byState.rejected.events.map((e) => e.event)).toEqual(['submitted', 'risk_rejected']);
    expect(byState.rejected.symbol).toBe('TCS.NS');
    // Only the arming event is a system event.
    expect(byState.system.events.map((e) => e.event)).toEqual(['live_armed']);
  });

  test('a fill shows what it added and the average price, and a confirmed order says so', () => {
    expect(eventFacts(ev(1, 'o', 'fill', { delta_qty: 4, avg_price: 101.5, quantity: 10, price: null }))).toBe('4 @ 101.5');
    expect(eventFacts(ev(2, '', 'submitted', { live: true, confirmed: true, request }))).toContain('confirmed');
    expect(eventFacts(ev(3, '', 'submitted', { live: true, confirmed: false, request }))).not.toContain('confirmed');
  });
});

test.describe('Version history in the Strategy IDE', () => {
  test('lists a version per save, newest first, and an identical save adds none', async ({ page }) => {
    await openIde(page);
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(page.getByTestId('audit-history-note')).toContainText('append-only');
    await expect(rows(page)).toHaveCount(1);
    await expect(rows(page).first().getByTestId('audit-version-number')).toHaveText('v1');
    await page.getByTestId('audit-history-close').click();

    await typeAndSave(page, MARKER);
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(rows(page)).toHaveCount(2);
    await expect(rows(page).first().getByTestId('audit-version-number')).toHaveText('v2');
    await expect(rows(page).first().getByTestId('audit-version-action')).toHaveText('update');
    await expect(rows(page).first().getByTestId('audit-version-actor')).toHaveText('You');
    await expect(rows(page).first().getByTestId('audit-version-summary')).toContainText('my_strategy.py');
    await expect(rows(page).first().getByTestId('audit-version-hash')).toHaveText(/^[0-9a-f]{8}$/);
    await expect(rows(page).nth(1).getByTestId('audit-version-action')).toHaveText('create');
    await page.getByTestId('audit-history-close').click();

    // Saving the same text again must not create a version.
    await page.getByTestId('save-strategy').click();
    await expect(page.getByTestId('save-confirmation')).toBeVisible();
    expect(await ledgerHook<VersionDto[]>(page, 'history', 'strategy', STRATEGY)).toHaveLength(2);
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(rows(page)).toHaveCount(2);
  });

  test('picking two versions shows what changed between them', async ({ page }) => {
    await openIde(page);
    await typeAndSave(page, MARKER);
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(rows(page)).toHaveCount(2);
    await page.getByTestId('audit-version-select-1').check();
    await page.getByTestId('audit-version-select-2').check();
    const diff = page.getByTestId('audit-diff');
    await expect(diff).toBeVisible();
    await expect(page.getByTestId('audit-diff-added')).toHaveText('+1');
    await expect(page.getByTestId('audit-diff-removed')).toHaveText('-0');
    await expect(diff.locator('[data-op="add"]').filter({ hasText: MARKER })).toHaveCount(1);
    await expect(diff.locator('[data-op="del"]')).toHaveCount(0);

    // View content: read-only text of version 1 without the marker.
    await page.getByTestId('audit-version-view-1').click();
    await expect(page.getByTestId('audit-history-content')).toContainText('class MyStrategy');
    await expect(page.getByTestId('audit-history-content')).not.toContainText(MARKER);
  });

  test('restore asks first, adds a new version, keeps the old ones and brings the file content back', async ({ page }) => {
    await openIde(page);
    await typeAndSave(page, MARKER);
    const editor = page.getByTestId('strategy-editor');
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(rows(page)).toHaveCount(2);

    // The current version cannot be restored; an old one can, after a confirmation that says what happens.
    await expect(page.getByTestId('audit-version-restore-2')).toBeDisabled();
    await expect(page.getByTestId('audit-version-restore-reason-2')).toContainText('current');
    await page.getByTestId('audit-version-restore-1').click();
    await expect(page.getByTestId('audit-history-confirm-text')).toContainText('creates a new version');
    await expect(page.getByTestId('audit-history-confirm-text')).toContainText('nothing is overwritten');
    await page.getByTestId('audit-history-confirm-no').click();
    await expect(rows(page)).toHaveCount(2);

    await page.getByTestId('audit-version-restore-1').click();
    await page.getByTestId('audit-history-confirm-yes').click();
    await expect(page.getByTestId('audit-history-message')).toContainText('Restored version 1 as the new version 3');
    await expect(rows(page)).toHaveCount(3);
    const top = rows(page).first();
    await expect(top.getByTestId('audit-version-number')).toHaveText('v3');
    await expect(top.getByTestId('audit-version-action')).toHaveText('restore');
    await expect(top).toContainText('content of version 1');
    // Nothing was erased: the version with the marker is still there and still readable.
    await page.getByTestId('audit-version-view-2').click();
    await expect(page.getByTestId('audit-history-content')).toContainText(MARKER);
    await page.getByTestId('audit-history-close').click();

    // The IDE reloaded the file: the marker is gone from the editor.
    await expect(editor.locator('.view-lines')).not.toContainText(MARKER);
    await expect(editor.locator('.view-lines')).toContainText('Strategy');
  });

  test('a change made outside APEX is shown with the External actor', async ({ page }) => {
    await openIde(page);
    await ledgerHook(page, 'externalEdit', 'strategy', STRATEGY, '# edited in another editor\n');
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(rows(page)).toHaveCount(2);
    const top = rows(page).first();
    await expect(top).toHaveAttribute('data-action', 'external');
    await expect(top.getByTestId('audit-version-actor')).toHaveText('External');
    await expect(top.getByTestId('audit-version-action')).toHaveText('external');
  });

  test('a file written by the Copilot is attributed to the Copilot', async ({ page }) => {
    await openIde(page);
    await mock(page, 'createStrategyAsCopilot', 'by_copilot.py');
    await page.getByTestId('file-by_copilot.py').click();
    await page.getByTestId('audit-history-open-strategy').click();
    await expect(rows(page)).toHaveCount(1);
    await expect(rows(page).first().getByTestId('audit-version-actor')).toHaveText('Copilot');
  });
});

test.describe('Deleting keeps the history', () => {
  async function openAutomations(page: Page) {
    await page.goto('/');
    await mock(page, 'createAutomationAsCopilot', 'Watcher');
    await page.getByTestId('tab-automations').click();
    await expect(page.getByTestId('automation-row')).toHaveCount(1);
  }

  test('delete says the history is kept; the rule is hidden and comes back from Show deleted as a new version', async ({ page }) => {
    await openAutomations(page);
    const del = page.getByTestId('automation-delete');
    await expect(del).toContainText('history is kept');
    await del.click();
    await expect(page.getByTestId('automation-row')).toHaveCount(0);

    await page.getByTestId('audit-show-deleted-automation').click();
    const item = page.getByTestId('audit-deleted-item-automation');
    await expect(item).toHaveCount(1);
    await expect(item).toContainText('auto_1');
    await page.getByTestId('audit-restore-deleted-automation-auto_1').click();
    await expect(page.getByTestId('automation-row')).toHaveCount(1);
    await expect(page.getByTestId('audit-deleted-item-automation')).toHaveCount(0);

    await page.getByTestId('audit-history-open-automation-auto_1').click();
    await expect(rows(page)).toHaveCount(3);
    expect(await rows(page).evaluateAll((els) => els.map((e) => e.getAttribute('data-action')))).toEqual(['restore', 'delete', 'create']);
    await expect(rows(page).last().getByTestId('audit-version-actor')).toHaveText('Copilot');
    await expect(rows(page).first().getByTestId('audit-version-actor')).toHaveText('You');
  });

  test('a deleted object shows a Deleted banner in its history with Restore latest version', async ({ page }) => {
    await openAutomations(page);
    await page.getByTestId('automation-delete').click();
    await page.getByTestId('audit-show-deleted-automation').click();
    await page.getByTestId('audit-history-open-automation-auto_1').click();
    await expect(page.getByTestId('audit-history-deleted-banner')).toContainText('Deleted');
    await expect(page.getByTestId('audit-history-deleted-banner')).toContainText('history is kept');
    // The delete marker has nothing to restore; the reason is shown.
    await expect(page.getByTestId('audit-version-restore-2')).toBeDisabled();
    await expect(page.getByTestId('audit-version-restore-reason-2')).toContainText('delete marker');
    await page.getByTestId('audit-history-restore-latest').click();
    await page.getByTestId('audit-history-confirm-yes').click();
    await expect(page.getByTestId('audit-history-deleted-banner')).toHaveCount(0);
    await page.getByTestId('audit-history-close').click();
    await expect(page.getByTestId('automation-row')).toHaveCount(1);
  });

  test('toggling a rule adds a version instead of editing the old one', async ({ page }) => {
    await openAutomations(page);
    await page.getByTestId('automation-toggle').click();
    await page.getByTestId('audit-history-open-automation-auto_1').click();
    await expect(rows(page)).toHaveCount(2);
    await expect(rows(page).first().getByTestId('audit-version-summary')).toContainText('Paused');
    await page.getByTestId('audit-version-diffprev-2').click();
    await expect(page.getByTestId('audit-diff').locator('[data-op="del"]')).toContainText('"enabled": true');
    await expect(page.getByTestId('audit-diff').locator('[data-op="add"]')).toContainText('"enabled": false');
  });

  test('estimators and recipes: delete wording, Show deleted and restore', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-ml').click();
    await api(page, 'saveEstimatorFile', 'my_est.py', 'def make_estimator(params):\n    return None\n');
    await api(page, 'saveEstimatorFile', 'my_est.py', 'def make_estimator(params):\n    return 1\n');
    expect(await api<VersionDto[]>(page, 'ledgerHistory', 'estimator', 'my_est.py')).toHaveLength(2);
    await page.getByTestId('ml-tab-train').click();
    await page.getByTestId('ml-algorithm-select').selectOption('custom');
    await expect(page.getByTestId('ml-estimator-editor')).toBeVisible();
    const del = page.getByTestId('ml-est-delete-my_est.py');
    await expect(del).toContainText('history is kept');
    await del.click();
    await expect(page.getByTestId('ml-est-delete-confirm-my_est.py').locator('xpath=..')).toContainText('history is kept');
    await page.getByTestId('ml-est-delete-confirm-my_est.py').click();
    await expect(page.getByTestId('ml-est-item-my_est.py')).toHaveCount(0);
    await page.getByTestId('audit-show-deleted-estimator').click();
    await page.getByTestId('audit-restore-deleted-estimator-my_est.py').click();
    await expect(page.getByTestId('ml-est-item-my_est.py')).toBeVisible();
    expect((await api<VersionDto[]>(page, 'ledgerHistory', 'estimator', 'my_est.py')).map((v) => v.action)).toEqual(['create', 'update', 'delete', 'restore']);
  });

  test('a dataset kept only by hash cannot be restored and says why', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-ml').click();
    await expect(page.getByTestId('ml-dataset-builder')).toBeVisible({ timeout: 15000 });
    await api(page, 'buildDataset', {
      name: 'huge_set', symbols: ['AAPL'], timeframe: 'd1', bars: 300, features: ['ret_1'], label: { kind: 'direction', horizon_bars: 5, threshold_pct: 0.5 },
    });
    await api(page, 'buildDataset', {
      name: 'huge_set', symbols: ['AAPL'], timeframe: 'd1', bars: 400, features: ['ret_1'], label: { kind: 'direction', horizon_bars: 5, threshold_pct: 0.5 },
    });
    await page.getByTestId('audit-history-open-dataset-huge_set').click();
    await expect(rows(page)).toHaveCount(2);
    await expect(rows(page).first().getByTestId('audit-version-retained')).toContainText('write-once');
    await expect(page.getByTestId('audit-version-restore-1')).toBeDisabled();
    await expect(page.getByTestId('audit-version-restore-reason-1')).toContainText('write-once');
  });
});

test.describe('Integrity', () => {
  async function openIntegrity(page: Page) {
    await page.goto('/');
    await page.getByTestId('tab-activity').click();
    await page.getByTestId('activity-view-integrity').click();
    await expect(page.getByTestId('ledger-integrity')).toBeVisible();
  }

  test('starts as Not verified; Verify now shows progress and then Verified OK', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('tab-activity').click();
    await expect(page.getByTestId('ledger-badge')).toHaveAttribute('data-state', 'unverified');
    await expect(page.getByTestId('ledger-badge-text')).toHaveText('Not verified');
    await page.getByTestId('activity-view-integrity').click();
    await page.getByTestId('ledger-verify').click();
    await expect(page.getByTestId('ledger-verify-progress')).toBeVisible();
    await expect(page.getByTestId('ledger-verify-summary')).toContainText('Everything checks out');
    await expect(page.getByTestId('ledger-verify-result')).toHaveAttribute('data-ok', 'true');
    await expect(page.getByTestId('ledger-badge').first()).toHaveAttribute('data-state', 'ok');
    await expect(page.getByTestId('ledger-badge-text').first()).toContainText('Verified OK');
    await expect(page.getByTestId('ledger-problem')).toHaveCount(0);
  });

  test('a tampered record is reported with its kind and where', async ({ page }) => {
    await openIntegrity(page);
    await ledgerHook(page, 'tamper', 'strategy', STRATEGY, 1);
    await page.getByTestId('ledger-verify').click();
    await expect(page.getByTestId('ledger-verify-summary')).toContainText('1 problem found');
    const problem = page.getByTestId('ledger-problem').first();
    await expect(problem.getByTestId('ledger-problem-kind')).toHaveText('record_hash');
    await expect(problem.getByTestId('ledger-problem-where')).toContainText(STRATEGY);
    await expect(problem.getByTestId('ledger-problem-detail')).not.toBeEmpty();
    await expect(page.getByTestId('ledger-badge').first()).toHaveAttribute('data-state', 'problem');
    await expect(page.getByTestId('ledger-badge-text').first()).toHaveText('Integrity problem');
  });

  test('a changed stored file and a changed order event are found too', async ({ page }) => {
    await openIntegrity(page);
    await mock(page, 'placeOrderAs', null);
    await ledgerHook(page, 'tamper', 'strategy', STRATEGY, 1, 'blob');
    await ledgerHook(page, 'tamperEvent', 'orders', 1);
    await page.getByTestId('ledger-verify').click();
    const kinds = page.getByTestId('ledger-problem-kind');
    await expect(kinds).toHaveCount(2);
    const texts = await kinds.allTextContents();
    expect(texts.sort()).toEqual(['blob_hash', 'stream_hash']);
  });

  test('head hashes are shown with the limit note and can be copied', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await page.goto('/');
    await mock(page, 'placeOrderAs', null);
    await mock(page, 'recordActivity', 'user', 'system.start', 'system', 'app', 'Something happened');
    await page.getByTestId('tab-activity').click();
    await page.getByTestId('activity-view-integrity').click();
    await expect(page.getByTestId('ledger-limit-note')).toContainText('Copy these somewhere safe');
    await expect(page.getByTestId('ledger-limit-note')).toContainText('rewrite the whole log');
    for (const name of ['ledger', 'activity', 'orders']) {
      const shown = (await page.getByTestId(`ledger-head-${name}`).textContent()) ?? '';
      expect(shown).toMatch(/^[0-9a-f]{64}$/);
      await page.getByTestId(`ledger-copy-${name}`).click();
      await expect(page.getByTestId('ledger-copy-notice')).toContainText('Copied');
      expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(shown);
    }
    // The three chains are separate: their heads differ.
    const heads = await Promise.all(['ledger', 'activity', 'orders'].map((n) => page.getByTestId(`ledger-head-${n}`).textContent()));
    expect(new Set(heads).size).toBe(3);
  });
});

test.describe('Order timeline', () => {
  async function openOrders(page: Page) {
    await page.getByTestId('tab-activity').click();
    await page.getByTestId('activity-view-orders').click();
  }

  test('a paper order is a sequence of events, with what produced it', async ({ page }) => {
    await page.goto('/');
    await mock(page, 'createAutomationAsCopilot', 'Watcher');
    await mock(page, 'placeOrderAs', 'automation:auto_1');
    await openOrders(page);
    const group = page.getByTestId('order-timeline-group');
    await expect(group).toHaveCount(1);
    expect(await group.getByTestId('order-event').evaluateAll((els) => els.map((e) => e.getAttribute('data-event'))))
      .toEqual(['submitted', 'risk_accepted', 'broker_ack', 'fill']);
    await expect(group.getByTestId('order-source-badge')).toHaveText('automation@v1');
    await expect(group.getByTestId('order-final-status')).toHaveText('filled');
    await expect(group.getByTestId('order-event-facts').first()).toContainText('RELIANCE.NS');

    // The source links to that exact version in the history viewer.
    await group.getByTestId('order-source-link').click();
    await expect(page.getByTestId('audit-history')).toHaveAttribute('data-object-id', 'auto_1');
    await expect(page.getByTestId('audit-history-content')).toContainText('"name": "Watcher"');
  });

  test('the source records the exact version, which moves on when the automation changes', async ({ page }) => {
    await page.goto('/');
    await mock(page, 'createAutomationAsCopilot', 'Watcher');
    await api(page, 'setAutomationEnabled', 'auto_1', false);
    await mock(page, 'placeOrderAs', 'automation:auto_1');
    await openOrders(page);
    await expect(page.getByTestId('order-source-badge')).toHaveText('automation@v2');
  });

  test('filters by source, status and order id', async ({ page }) => {
    await page.goto('/');
    await mock(page, 'createAutomationAsCopilot', 'Watcher');
    await mock(page, 'placeOrderAs', 'automation:auto_1');
    await mock(page, 'placeOrderAs', 'copilot:turn-7');
    await mock(page, 'placeOrderAs', null, { order_type: 'Limit', price: 100 });
    await openOrders(page);
    const groups = page.getByTestId('order-timeline-group');
    await expect(groups).toHaveCount(3);
    await expect(page.getByTestId('orders-count')).toHaveText('3 orders');
    await page.getByTestId('orders-filter-source').selectOption('copilot');
    await expect(groups).toHaveCount(1);
    await expect(groups.first().getByTestId('order-source-badge')).toHaveText('Copilot');
    await page.getByTestId('orders-filter-source').selectOption('');
    await page.getByTestId('orders-filter-status').selectOption('open');
    await expect(groups).toHaveCount(1);
    await expect(groups.first().getByTestId('order-source-badge')).toHaveText('manual');
    await page.getByTestId('orders-filter-clear').click();
    await expect(groups).toHaveCount(3);
    const id = (await groups.first().getAttribute('data-order-id')) ?? '';
    await page.getByTestId('orders-filter-id').fill(id);
    await expect(groups).toHaveCount(1);
    await page.getByTestId('orders-filter-broker').selectOption('paper');
    await expect(groups).toHaveCount(1);
  });

  test('a rejected live order is kept as events and arming is recorded', async ({ page }) => {
    await page.goto('/');
    await mock(page, 'placeOrderAs', null, { broker_id: 'zerodha' }).catch(() => undefined);
    await api(page, 'setLiveArmed', true);
    await api(page, 'setLiveArmed', false);
    await openOrders(page);
    const rejected = page.locator('[data-testid="order-timeline-group"][data-state="rejected"]');
    await expect(rejected).toHaveCount(1);
    expect(await rejected.getByTestId('order-event').evaluateAll((els) => els.map((e) => e.getAttribute('data-event')))).toEqual(['submitted', 'risk_rejected']);
    await expect(rejected.getByTestId('order-group-id')).toHaveText('not sent to a broker');
    await expect(rejected.getByTestId('order-event-facts').last()).toContainText('not armed');
    const system = page.locator('[data-testid="order-timeline-group"][data-state="system"]');
    expect(await system.getByTestId('order-event').evaluateAll((els) => els.map((e) => e.getAttribute('data-event')))).toEqual(['live_armed', 'live_disarmed']);
  });

  test('the blotter has an Audit trail button per order that opens that order\'s timeline', async ({ page }) => {
    await page.goto('/');
    const first = await mock<string>(page, 'placeOrderAs', null);
    await mock(page, 'placeOrderAs', 'strategy:strategies/my_strategy.py', { symbol: 'TCS.NS' });
    await page.getByTestId('tab-blotter').click();
    await expect(page.getByTestId('blotter-row')).toHaveCount(2, { timeout: 10000 });
    await page.getByTestId(`blotter-audit-${first}`).click();
    const drawer = page.getByTestId('order-audit-drawer');
    await expect(drawer).toBeVisible();
    await expect(drawer.getByTestId('order-timeline-group')).toHaveCount(1);
    await expect(drawer.getByTestId('order-timeline-group')).toHaveAttribute('data-order-id', first);
    await expect(drawer.getByTestId('order-source-badge')).toHaveText('manual');
    await expect(drawer.getByTestId('order-event')).toHaveCount(4);
    await page.getByTestId('order-audit-close').click();
    await expect(drawer).toHaveCount(0);
  });

  test('cancelling adds events; the earlier ones stay as they were', async ({ page }) => {
    await page.goto('/');
    const id = await mock<string>(page, 'placeOrderAs', null, { order_type: 'Limit', price: 100 });
    const before = await api<{ hash: string }[]>(page, 'listOrderEvents', { orderId: id });
    await page.getByTestId('tab-blotter').click();
    await expect(page.getByTestId(`blotter-cancel-${id}`)).toBeVisible({ timeout: 10000 });
    await page.getByTestId(`blotter-cancel-${id}`).click();
    await expect.poll(async () => (await api<unknown[]>(page, 'listOrderEvents', { orderId: id })).length).toBe(before.length + 2);
    const after = await api<{ hash: string; event: string }[]>(page, 'listOrderEvents', { orderId: id });
    const oldest = after.slice(-before.length).map((e) => e.hash);
    expect(oldest).toEqual(before.map((e) => e.hash));
    expect(after.slice(0, 2).map((e) => e.event)).toEqual(['cancelled', 'cancel_requested']);
  });
});
