import { expect, test } from '@playwright/test';

test('strategy code opens and remains editable without a CDN', async ({ page }) => {
  const blockedScripts: string[] = [];
  await page.route('https://**/*', async (route) => {
    if (route.request().resourceType() === 'script') blockedScripts.push(route.request().url());
    await route.abort();
  });
  await page.goto('/');
  await page.getByTestId('tab-strategy').click();

  // The wrapper alone can be visible even if Monaco is stuck on "Loading...".
  const editor = page.getByTestId('strategy-editor');
  await expect(editor.locator('.monaco-editor')).toBeVisible({ timeout: 15000 });
  await expect(editor.locator('.view-lines')).toContainText('Strategy');
  await editor.locator('.view-lines').click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type('\n# local editor verification');
  await expect(editor.locator('.view-lines')).toContainText('local editor verification');
  await page.getByTestId('save-strategy').click();
  await expect(page.getByTestId('save-confirmation')).toBeVisible();
  expect(blockedScripts).toEqual([]);
});

test('save shortcut targets the selected file after switching strategies', async ({ page }) => {
  await page.goto('/');
  await page.getByTestId('tab-strategy').click();
  const editor = page.getByTestId('strategy-editor');
  await expect(editor.locator('.monaco-editor')).toBeVisible({ timeout: 15000 });
  await page.getByTestId('strategy-new-file').click();
  await page.getByTestId('file-name-input').fill('shortcut_second.py');
  await page.getByTestId('confirm-create-file').click();
  await expect(page.getByTestId('file-shortcut_second.py')).toBeVisible();
  await editor.locator('.view-lines').click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type('\n# second file shortcut marker');
  await expect(editor.locator('.view-lines')).toContainText('second file shortcut marker');
  await expect(page.getByTestId('command-bar-input')).not.toBeFocused();
  await page.keyboard.press('ControlOrMeta+s');
  await expect(page.getByTestId('save-confirmation')).toBeVisible();
  await page.getByTestId('file-my_strategy.py').click();
  await expect(editor.locator('.view-lines')).not.toContainText('second file shortcut marker');
  await page.getByTestId('file-shortcut_second.py').click();
  await expect(editor.locator('.view-lines')).toContainText('second file shortcut marker');
});
