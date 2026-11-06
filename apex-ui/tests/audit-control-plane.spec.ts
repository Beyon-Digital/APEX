import { test, expect, type Page } from '@playwright/test';
import { KIND_LABEL, restoreBlockedReason } from '../src/components/audit/auditUtils';
import { CONTROL_GROUPS, actionLabel, inControlGroup, maskSecrets } from '../src/components/activity/activityUtils';
import { redactConfig } from '../src/lib/mockLedger';
import type { VersionContentDto, VersionDto } from '../src/lib/types';

/**
 * Control-plane audit UI: the settings file and alert rules are versioned objects, and every change
 * to limits, credentials, the vault, brokers and the halt shows up in the Activity panel.
 * Web mode: lib/mockLedger.ts and lib/tauri.ts replace the Rust ledger and commands.
 */

const CANARY = 'canary-pw-7Zq9XkL2mN4pR8';
const SETTINGS_ID = 'config/apex.toml';
const VIEW_ONLY = 'Settings history is view-only: secrets are redacted in the history and cannot be restored';

/** Run an API wrapper of lib/tauri.ts in the page (web mode talks to the in-memory mock). */
const api = <T = unknown>(page: Page, fn: string, ...args: unknown[]) =>
  page.evaluate(async ([name, a]) => {
    const path = '/src/lib/tauri.ts';
    const mod = await import(/* @vite-ignore */ path) as Record<string, (...x: unknown[]) => Promise<unknown>>;
    return mod[name as string](...(a as unknown[]));
  }, [fn, args] as const) as Promise<T>;

/** Like api, but returns the error message instead of throwing. */
const apiError = (page: Page, fn: string, ...args: unknown[]) =>
  page.evaluate(async ([name, a]) => {
    const path = '/src/lib/tauri.ts';
    const mod = await import(/* @vite-ignore */ path) as Record<string, (...x: unknown[]) => Promise<unknown>>;
    try { await mod[name as string](...(a as unknown[])); return null; } catch (e) { return e instanceof Error ? e.message : String(e); }
  }, [fn, args] as const) as Promise<string | null>;

const mockHook = (page: Page, name: string, ...args: unknown[]) =>
  page.evaluate(([n, a]) => ((window as unknown as { __apexMock: Record<string, (...x: unknown[]) => unknown> }).__apexMock[n as string])(...(a as unknown[])), [name, args] as const);

async function openSettingsGeneral(page: Page) {
  await page.getByTestId('open-settings').click();
  await expect(page.getByTestId('settings-panel')).toBeVisible();
}

/** Change the daily loss limit and the Postgres URL (which carries the canary password) and save. */
async function saveLimitAndPostgres(page: Page, limit: string, withPostgres: boolean) {
  await openSettingsGeneral(page);
  await page.getByTestId('settings-tab-trading').click();
  await page.getByTestId('settings-max-daily-loss').fill(limit);
  if (withPostgres) {
    await page.getByTestId('settings-tab-general').click();
    await page.getByTestId('runtime-settings-section').locator('summary').click();
    await page.getByTestId('settings-postgres-url').fill(`postgresql://apex:${CANARY}@db.internal:5432/market`);
  }
  await page.getByTestId('save-app-settings').click();
  await expect(page.getByTestId('settings-success')).toBeVisible();
}

test.describe('Helpers', () => {
  test('every versioned kind has a label, including settings and alert', () => {
    expect(KIND_LABEL.settings).toBe('Settings');
    expect(KIND_LABEL.alert).toBe('Alert');
    const kinds = ['strategy', 'estimator', 'recipe', 'dataset', 'model', 'automation', 'notebook', 'settings', 'alert'] as const;
    for (const k of kinds) expect(KIND_LABEL[k]).toBeTruthy();
  });

  test('restore is blocked for every version of the settings file, with the reason', () => {
    const v = { seq: 1, kind: 'settings', object_id: SETTINGS_ID, version: 1, action: 'update', actor: 'user', ts: '', content_hash: 'ab', size: 1, retained: true, parent_version: null, restored_from: null, summary: '', hash: '', prev_hash: '' } as VersionDto;
    expect(restoreBlockedReason(v, { ...v, version: 2 })).toBe(VIEW_ONLY);
    expect(restoreBlockedReason({ ...v, kind: 'alert' }, { ...v, kind: 'alert', version: 2 })).toBeNull();
  });

  test('redactConfig hides URL passwords and secret-named values and keeps everything else byte for byte', () => {
    const text = [
      '# apex config',
      '[storage]',
      `postgres_url = "postgresql://apex:${CANARY}@db.internal:5432/market" # prod`,
      'pool_size = 4',
      'api_key = "sk-live-123"',
      "  Secret_Token='abc'  ",
      'key_material = abc123',
      'max_daily_loss = 50000',
      'note = "password handling is described in the docs"',
      '',
    ].join('\n');
    const out = redactConfig(text);
    expect(out).not.toContain(CANARY);
    expect(out).not.toContain('sk-live-123');
    expect(out).toContain('postgresql://apex:<redacted>@db.internal:5432/market" # prod');
    expect(out).toContain('api_key = "<redacted>"');
    expect(out).toContain('  Secret_Token="<redacted>"  ');
    expect(out).toContain('key_material = "<redacted>"');
    // Everything that is not a secret is unchanged.
    for (const keep of ['# apex config', '[storage]', 'pool_size = 4', 'max_daily_loss = 50000', 'note = "password handling is described in the docs"']) expect(out).toContain(keep);
    expect(out.split('\n')).toHaveLength(text.split('\n').length);
    expect(redactConfig(out)).toBe(out);
  });

  test('maskSecrets hides URL passwords, secret-named values, JWTs and token-like strings but not hashes or ids', () => {
    expect(maskSecrets('connect postgres://u:hunter2@host/db failed')).toBe('connect postgres://u:<masked>@host/db failed');
    expect(maskSecrets('postgres://u:<redacted>@host/db api_key = "<redacted>"')).toBe('postgres://u:<redacted>@host/db api_key = "<redacted>"');
    expect(maskSecrets('{"api_key": "abc", "ok": 1}')).toBe('{"api_key": "<masked>", "ok": 1}');
    expect(maskSecrets('password=hunter2&x=1')).toBe('password=<masked>&x=1');
    expect(maskSecrets('Authorization: Bearer abcdef123456')).toContain('Bearer <masked>');
    expect(maskSecrets('eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.sig')).toBe('<masked>');
    expect(maskSecrets('token A1b2C3d4E5f6G7h8I9j0K1l2M3n4 rejected')).toBe('token <masked> rejected');
    const hash = 'a'.repeat(64);
    expect(maskSecrets(`hash ${hash}`)).toBe(`hash ${hash}`);
    expect(maskSecrets('Trained model rf_20260101_run on strategies/my_strategy.py')).toBe('Trained model rf_20260101_run on strategies/my_strategy.py');
    // Ordinary CamelCase names with a digit are not tokens; what the backend writes for a secret setting is kept.
    expect(maskSecrets('Saved strategy strategies/MomentumBreakoutStrategy2.py')).toBe('Saved strategy strategies/MomentumBreakoutStrategy2.py');
    expect(maskSecrets('Trained model RandomForest_nifty50_daily_2026 on nifty')).toBe('Trained model RandomForest_nifty50_daily_2026 on nifty');
    expect(maskSecrets('llm.providers[a].api_key: changed')).toBe('llm.providers[a].api_key: changed');
    expect(maskSecrets('api_key: sk-live-abc123')).toBe('api_key: <masked>');
    // Random-looking tokens are still masked.
    expect(maskSecrets('leaked kP9xQ2mZ7vL4nR8tW3yB6cD1fG5h here')).toBe('leaked <masked> here');
  });

  test('action labels and the Control groups cover the control plane and nothing else', () => {
    expect(actionLabel('settings.save')).toBe('Changed settings');
    expect(actionLabel('risk.reset_halt')).toBe('Reset trading halt');
    expect(actionLabel('vault.unlock')).toBe('Unlocked vault');
    expect(actionLabel('dataset.fetch')).toBe('Fetched market data');
    // The names the backend writes (apex-tauri/src/commands/*.rs), not guesses.
    const labels: [string, string][] = [
      ['alert.create', 'Created alert'], ['alert.delete', 'Deleted alert'], ['alert.restore', 'Restored alert'], ['broker.session', 'Set broker session'],
      ['broker.logout', 'Logged out of broker'], ['adapter.import_env', 'Imported adapter credentials from the environment'],
      ['settings.external_edit', 'Settings file edited outside APEX'], ['system.kill_switch', 'Kill switch'],
    ];
    for (const [action, label] of labels) expect(actionLabel(action)).toBe(label);
    expect(CONTROL_GROUPS.map((g) => g.id)).toEqual(['settings', 'risk', 'vault', 'adapters', 'brokers', 'alerts']);
    for (const a of ['settings.save', 'settings.external_edit', 'risk.reset_halt', 'adapter.save', 'adapter.import_env', 'vault.lock', 'broker.session', 'broker.logout', 'chatgpt.sign_in', 'live.arm', 'system.kill_switch', 'alert.create', 'alert.delete', 'alert.restore']) {
      expect(inControlGroup(a, 'control'), a).toBe(true);
    }
    for (const a of ['strategy.save', 'model.train', 'dataset.fetch', 'recipe.save', 'system.other']) expect(inControlGroup(a, 'control')).toBe(false);
    expect(inControlGroup('alert.delete', 'alerts')).toBe(true);
    expect(inControlGroup('system.kill_switch', 'brokers')).toBe(true);
    expect(inControlGroup('vault.lock', 'vault')).toBe(true);
    expect(inControlGroup('vault.lock', 'risk')).toBe(false);
  });
});

test.describe('Settings history', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('open-settings')).toBeVisible({ timeout: 10000 });
  });

  test('shows the diff of a limit change, never the password, and cannot restore', async ({ page }) => {
    await saveLimitAndPostgres(page, '60000', true);
    await page.getByTestId('settings-tab-general').click();
    await page.getByTestId('settings-history-open').click();

    const history = page.getByTestId('audit-history');
    await expect(history).toBeVisible();
    await expect(history).toHaveAttribute('data-kind', 'settings');
    await expect(page.getByTestId('audit-history-title')).toHaveText(SETTINGS_ID);
    await expect(page.getByTestId('audit-history-redaction-note')).toContainText('kept as written');
    const rows = page.getByTestId('audit-version-row');
    await expect(rows).toHaveCount(2);

    // Restore is disabled on every version, with the visible reason.
    for (const n of [1, 2]) {
      await expect(page.getByTestId(`audit-version-restore-${n}`)).toBeDisabled();
      await expect(page.getByTestId(`audit-version-restore-reason-${n}`)).toHaveText(VIEW_ONLY);
    }

    // Diff version 1 -> 2: the limit line changed, the URL line shows <redacted>.
    await page.getByTestId('audit-version-diffprev-2').click();
    const diff = page.getByTestId('audit-history-diff-section');
    await expect(diff).toContainText('max_daily_loss = 50000');
    await expect(diff).toContainText('max_daily_loss = 60000');
    await expect(diff).toContainText('postgresql://apex:<redacted>@db.internal:5432/market');

    // Viewing the content is read-only text.
    await page.getByTestId('audit-version-view-2').click();
    await expect(page.getByTestId('audit-history-content')).toContainText('max_daily_loss = 60000');

    // The canary appears nowhere in the open history, nor in what the ledger returns.
    expect(await history.innerText()).not.toContain(CANARY);
    for (const n of [1, 2]) {
      const content = await api<VersionContentDto>(page, 'ledgerVersion', 'settings', SETTINGS_ID, n);
      expect(content.text ?? '').not.toContain(CANARY);
    }
    expect(JSON.stringify(await api(page, 'ledgerHistory', 'settings', SETTINGS_ID))).not.toContain(CANARY);
  });

  test('the backend refuses to restore a settings version', async ({ page }) => {
    await saveLimitAndPostgres(page, '61000', false);
    expect(await apiError(page, 'ledgerRestore', 'settings', SETTINGS_ID, 1)).toBe('settings history is view-only; redacted values cannot be restored');
    const history = await api<VersionDto[]>(page, 'ledgerHistory', 'settings', SETTINGS_ID);
    expect(history.map((v) => v.version)).toEqual([1, 2]);
  });

  test('saving settings records who changed the limit, without the secret', async ({ page }) => {
    await saveLimitAndPostgres(page, '62000', true);
    await page.getByTestId('settings-close-x').click();
    await page.getByTestId('tab-activity').click();
    const row = page.getByTestId('activity-row').first();
    await expect(row.getByTestId('activity-action')).toHaveText('Changed settings');
    await expect(row.getByTestId('activity-actor')).toHaveText('You');
    // As the backend writes it: the summary names the keys, the values are in the details.
    await expect(row.getByTestId('activity-summary')).toHaveText('Settings saved: changed risk.max_daily_loss, storage.postgres_url');
    await row.getByTestId('activity-row-toggle').click();
    const text = await page.getByTestId('activity-panel').innerText();
    expect(text).toContain('risk.max_daily_loss: 50000 -> 62000');
    expect(text).not.toContain(CANARY);
    expect(text).toContain('postgresql://apex:<redacted>@db.internal:5432/market');
  });
});

test.describe('Alert history', () => {
  test('create, history, delete, show deleted, restore', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('create-alert-button').click();
    await page.getByTestId('alert-symbol-input').fill('BTCUSD');
    await page.getByTestId('alert-value-input').fill('100000');
    await page.getByTestId('alert-save-button').click();
    const item = page.getByTestId('alert-item');
    await expect(item).toHaveCount(1);
    await expect(item).toContainText('BTCUSD price above 100000');
    const ruleId = (await api<{ id: string }[]>(page, 'getAlertRules'))[0].id;

    // History of the live alert: one create version, restorable content, current version cannot be restored.
    await page.getByTestId(`alert-history-${ruleId}`).click();
    await expect(page.getByTestId('audit-history')).toHaveAttribute('data-kind', 'alert');
    await expect(page.getByTestId('audit-version-row')).toHaveCount(1);
    await expect(page.getByTestId('audit-version-action')).toHaveText('create');
    await page.getByTestId('audit-version-view-1').click();
    await expect(page.getByTestId('audit-history-content')).toContainText('"threshold": 100000');
    await page.getByTestId('audit-history-close').click();

    // Honest wording on the delete button.
    const del = page.getByTestId(`alert-remove-${ruleId}`);
    await expect(del).toHaveText('Delete');
    await expect(del).toHaveAttribute('title', /history is kept.*restore/i);
    await del.click();
    await expect(item).toHaveCount(0);

    // Show deleted lists it; its history has the delete marker; restoring brings the alert back as a new version.
    await page.getByTestId('audit-show-deleted-alert').click();
    const deleted = page.getByTestId('audit-deleted-item-alert');
    await expect(deleted).toHaveCount(1);
    await expect(deleted).toHaveAttribute('data-object-id', ruleId);
    await page.getByTestId(`audit-history-open-alert-${ruleId}`).click();
    await expect(page.getByTestId('audit-history-deleted-banner')).toBeVisible();
    await expect(page.getByTestId('audit-version-row')).toHaveCount(2);
    await page.getByTestId('audit-history-restore-latest').click();
    await page.getByTestId('audit-history-confirm-yes').click();
    await expect(page.getByTestId('audit-history-message')).toContainText('Restored version 1 as the new version 3');
    await page.getByTestId('audit-history-close').click();

    await expect(item).toHaveCount(1);
    await expect(item).toContainText('BTCUSD price above 100000');
    await expect(deleted).toHaveCount(0);
    const history = await api<VersionDto[]>(page, 'ledgerHistory', 'alert', ruleId);
    expect(history.map((v) => v.action)).toEqual(['create', 'delete', 'restore']);

    // The Activity panel has all three control changes.
    await page.getByTestId('tab-activity').click();
    await expect(page.getByTestId('activity-action').allTextContents()).resolves.toEqual(['Restored alert', 'Deleted alert', 'Created alert']);
  });
});

test.describe('Alert console follows changes made elsewhere', () => {
  test('an alert removed or added by someone else (Copilot) appears without reopening the panel', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('create-alert-button').click();
    await page.getByTestId('alert-symbol-input').fill('BTCUSD');
    await page.getByTestId('alert-value-input').fill('100000');
    await page.getByTestId('alert-save-button').click();
    const item = page.getByTestId('alert-item');
    await expect(item).toHaveCount(1);
    const ruleId = (await api<{ id: string }[]>(page, 'getAlertRules'))[0].id;

    // Removed behind the panel's back: the row goes away.
    expect(await api(page, 'removeAlert', ruleId)).toBe(true);
    await expect(item).toHaveCount(0);

    // Added behind the panel's back: the row shows up.
    await api(page, 'addAlert', 'alert_from_copilot', JSON.stringify({ PriceAbove: { symbol: 'AAPL', threshold: 300 } }));
    await expect(item).toHaveCount(1);
    await expect(item).toContainText('AAPL');
  });
});

test.describe('Activity: control plane', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('tab-activity')).toBeVisible({ timeout: 10000 });
  });

  test('the Control filter lists only control-plane entries', async ({ page }) => {
    await mockHook(page, 'createStrategyAsCopilot', 'alpha.py');
    await mockHook(page, 'recordActivity', 'user', 'model.train', 'model', 'm1', 'Trained random_forest on nifty');
    await api(page, 'addAlert', 'alert_x', JSON.stringify({ PriceAbove: { symbol: 'AAPL', threshold: 300 } }));
    await api(page, 'vaultLock');
    await api(page, 'resetHalt');
    await api(page, 'setBrokerSession', 'zerodha', 'tok-abc');
    await api(page, 'setLiveArmed', true);
    await api(page, 'deleteAdapterConfig', 'binance');
    await page.getByTestId('tab-activity').click();
    const rows = page.getByTestId('activity-row');
    await expect(rows).toHaveCount(8);

    await page.getByTestId('activity-filter-group').selectOption('control');
    await expect(page.getByTestId('activity-count')).toHaveText('6 of 8 entries');
    const actions = await page.getByTestId('activity-action').allTextContents();
    expect(actions.sort()).toEqual(['Armed live trading', 'Created alert', 'Deleted adapter credentials', 'Locked vault', 'Reset trading halt', 'Set broker session'].sort());
    for (const kind of ['strategy', 'model']) await expect(page.locator(`[data-testid="activity-row"][data-kind="${kind}"]`)).toHaveCount(0);

    // A single group narrows it further.
    await page.getByTestId('activity-filter-group').selectOption('vault');
    await expect(rows).toHaveCount(1);
    await expect(rows.first().getByTestId('activity-action')).toHaveText('Locked vault');
    await page.getByTestId('activity-filter-group').selectOption('risk');
    await expect(rows.first().getByTestId('activity-action')).toHaveText('Reset trading halt');

    // Clear resets the group filter too.
    await page.getByTestId('activity-clear-filters').click();
    await expect(rows).toHaveCount(8);
  });

  test('an edit made outside APEX shows an actor label and can be filtered', async ({ page }) => {
    await mockHook(page, 'recordActivity', 'external', 'settings.external_edit', 'system', 'config/apex.toml', 'Settings file changed outside APEX: risk.max_daily_loss');
    await mockHook(page, 'recordActivity', 'user', 'model.train', 'model', 'm1', 'Trained random_forest on nifty');
    await page.getByTestId('tab-activity').click();
    const external = page.locator('[data-testid="activity-row"][data-actor="external"]');
    await expect(external).toHaveCount(1);
    await expect(external.getByTestId('activity-actor')).toHaveText('Outside APEX');
    await expect(external.getByTestId('activity-actor')).not.toHaveClass(/undefined/);
    await expect(external.getByTestId('activity-action')).toHaveText('Settings file edited outside APEX');
    await page.getByTestId('activity-filter-actor').selectOption('external');
    await expect(page.getByTestId('activity-row')).toHaveCount(1);
    await page.getByTestId('activity-filter-group').selectOption('control');
    await expect(page.getByTestId('activity-row')).toHaveCount(1);
  });

  test('a failed unlock is visibly distinct and never shows the passphrase', async ({ page }) => {
    await api(page, 'vaultLock');
    await mockHook(page, 'failNextVaultUnlock');
    expect(await apiError(page, 'vaultUnlock', 'hunter2-passphrase-canary')).toBe('Wrong passphrase');
    await api(page, 'vaultUnlock', 'right-one');

    await page.getByTestId('tab-activity').click();
    const rows = page.getByTestId('activity-row');
    await expect(rows).toHaveCount(3);
    const failed = page.locator('[data-testid="activity-row"][data-ok="false"]');
    await expect(failed).toHaveCount(1);
    await expect(failed.getByTestId('activity-status')).toHaveText('failed');
    await expect(failed.getByTestId('activity-failed-badge')).toHaveText('failed attempt');
    await expect(failed.getByTestId('activity-action')).toHaveText('Unlocked vault');
    await expect(failed.getByTestId('activity-summary')).toContainText('Failed to unlock');
    // The successful unlock and the lock have no failure marks.
    await expect(page.getByTestId('activity-failed-badge')).toHaveCount(1);
    await expect(page.locator('[data-testid="activity-row"][data-ok="true"]')).toHaveCount(2);

    await page.getByTestId('activity-filter-status').selectOption('failed');
    await expect(rows).toHaveCount(1);
    await failed.getByTestId('activity-row-toggle').click();
    expect(await page.getByTestId('activity-panel').innerText()).not.toContain('hunter2-passphrase-canary');
  });

  test('details never show secret-looking values', async ({ page }) => {
    const token = 'A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
    await mockHook(page, 'recordActivity', 'user', 'adapter.save', 'system', 'zerodha', `Saved zerodha (token ${token})`, false,
      `connect postgres://apex:${CANARY}@db.internal/market failed\n{"api_key": "${token}", "attempt": 2}`);
    await page.getByTestId('tab-activity').click();
    const row = page.getByTestId('activity-row').first();
    await row.getByTestId('activity-row-toggle').click();
    const text = await page.getByTestId('activity-panel').innerText();
    expect(text).not.toContain(CANARY);
    expect(text).not.toContain(token);
    expect(text).toContain('<masked>');
    expect(text).toContain('"attempt": 2');
  });
});
