import { expect, test, type Page } from '@playwright/test';

// The default config seeds focus mode (see playwright.config.ts), which is where the side panel lives.

async function open(page: Page) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
}

const side = (page: Page) => page.getByTestId('copilot-side-panel');
const toggle = (page: Page) => page.getByTestId('copilot-launch');

test.describe('Copilot in focus mode: a docked side panel', () => {
  test('the button opens and hides it, and it sits beside the work area instead of covering it', async ({ page }) => {
    await open(page);
    await expect(side(page)).toHaveCount(0);
    await expect(toggle(page)).toHaveAttribute('aria-pressed', 'false');
    const before = await page.getByTestId('workspace-panel').boundingBox();

    await toggle(page).click();
    await expect(side(page)).toBeVisible();
    await expect(toggle(page)).toHaveAttribute('aria-pressed', 'true');
    // Full Copilot: header, history, new chat and the mode switch are all there.
    for (const id of ['copilot-session-title', 'copilot-sessions-toggle', 'copilot-new-chat', 'copilot-mode-agent', 'copilot-input']) {
      await expect(side(page).getByTestId(id)).toBeVisible();
    }

    // The work area gave up room; the two do not overlap.
    const after = await page.getByTestId('workspace-panel').boundingBox();
    const dock = await side(page).boundingBox();
    expect(after!.width).toBeLessThan(before!.width - 100);
    expect(after!.x + after!.width).toBeLessThanOrEqual(dock!.x + 1);

    await toggle(page).click();
    await expect(side(page)).toHaveCount(0);
    const restored = await page.getByTestId('workspace-panel').boundingBox();
    expect(Math.abs(restored!.width - before!.width)).toBeLessThan(2);
  });

  test('the close button inside the panel hides it', async ({ page }) => {
    await open(page);
    await toggle(page).click();
    await page.getByTestId('copilot-side-close').click();
    await expect(side(page)).toHaveCount(0);
    await expect(toggle(page)).toHaveAttribute('aria-pressed', 'false');
  });

  test('it never replaces the panel you are working in', async ({ page }) => {
    await open(page);
    await page.getByTestId('tab-news').click();
    await expect(page.getByTestId('news-panel')).toBeVisible();
    await toggle(page).click();
    await expect(side(page)).toBeVisible();
    await expect(page.getByTestId('news-panel')).toBeVisible();
  });

  test('Ctrl+Shift+A toggles it, including while typing in an input', async ({ page }) => {
    await open(page);
    await page.keyboard.press('Control+Shift+A');
    await expect(side(page)).toBeVisible();
    await page.getByTestId('copilot-input').click();
    await page.keyboard.type('draft');
    await page.keyboard.press('Control+Shift+A');
    await expect(side(page)).toHaveCount(0);
    await page.keyboard.press('Control+Shift+A');
    await expect(page.getByTestId('copilot-input')).toHaveValue('');
  });

  test('the open state and width survive a reload', async ({ page }) => {
    await open(page);
    await toggle(page).click();
    const handle = page.getByTestId('copilot-side-resize');
    await handle.focus();
    const start = (await side(page).boundingBox())!.width;
    await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('ArrowLeft');
    await expect.poll(async () => (await side(page).boundingBox())!.width).toBeGreaterThan(start + 40);
    const widened = (await side(page).boundingBox())!.width;

    await page.reload();
    await page.waitForLoadState('networkidle');
    await expect(side(page)).toBeVisible();
    expect(Math.abs((await side(page).boundingBox())!.width - widened)).toBeLessThan(2);
  });

  test('dragging the edge resizes it within limits', async ({ page }) => {
    await open(page);
    await toggle(page).click();
    const box = (await page.getByTestId('copilot-side-resize').boundingBox())!;
    const start = (await side(page).boundingBox())!.width;
    await page.mouse.move(box.x + box.width / 2, box.y + 200);
    await page.mouse.down();
    await page.mouse.move(box.x - 150, box.y + 200, { steps: 5 });
    await page.mouse.up();
    expect((await side(page).boundingBox())!.width).toBeGreaterThan(start + 100);

    // Far past the limit stops at the maximum.
    const box2 = (await page.getByTestId('copilot-side-resize').boundingBox())!;
    await page.mouse.move(box2.x + 1, box2.y + 200);
    await page.mouse.down();
    await page.mouse.move(0, box2.y + 200, { steps: 5 });
    await page.mouse.up();
    expect((await side(page).boundingBox())!.width).toBeLessThanOrEqual(900);
  });

  test('a reply that arrives while it is hidden is still in the conversation when it is shown again', async ({ page }) => {
    await open(page);
    await toggle(page).click();
    await page.getByTestId('copilot-input').fill('SLOWREPLY hello');
    await page.getByTestId('copilot-send').click();
    await toggle(page).click();
    await expect(side(page)).toHaveCount(0);
    await page.waitForTimeout(1200);
    await toggle(page).click();
    await expect(page.getByTestId('copilot-assistant-msg').last()).toContainText('hello');
  });

  for (const word of [':COPILOT', ':AI']) {
    test(`the command bar word ${word} opens the side panel in focus mode, never a centre tab`, async ({ page }) => {
      await open(page);
      await page.getByTestId('tab-news').click();
      const input = page.getByTestId('command-bar-input');
      await input.click();
      await input.fill(word);
      await input.press('Enter');
      await expect(side(page)).toBeVisible();
      await expect(page.getByTestId('news-panel')).toBeVisible();
      // Asking again keeps it open (the word shows it; it does not toggle).
      await input.click();
      await input.fill(word);
      await input.press('Enter');
      await expect(side(page)).toBeVisible();
    });
  }
});

test.describe('Copilot in tiles mode', () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test('the same button toggles the tile, and Ctrl+Shift+A does too', async ({ page }) => {
    await open(page);
    await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
    await page.keyboard.press('Control+Shift+A');
    await expect(page.getByTestId('tile-copilot')).toBeVisible();
    await expect(side(page)).toHaveCount(0);
    await page.keyboard.press('Control+Shift+A');
    await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
  });
});
