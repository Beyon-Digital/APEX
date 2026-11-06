import { test, expect, type Locator, type Page } from '@playwright/test';

// The Copilot is an ordinary tile now. These specs run in the default tiled workspace.
test.use({ storageState: { cookies: [], origins: [] } });

type Box = { x: number; y: number; width: number; height: number };

async function box(locator: Locator): Promise<Box> {
  const b = await locator.boundingBox();
  if (!b) throw new Error('element has no bounding box');
  return b;
}

const overlaps = (a: Box, b: Box) =>
  a.x < b.x + b.width - 1 && b.x < a.x + a.width - 1 && a.y < b.y + b.height - 1 && b.y < a.y + a.height - 1;

/** Grid tiles animate into place, so poll until the boxes settle. */
async function expectNoOverlap(page: Page, id: string, others: string[]) {
  await expect
    .poll(async () => {
      const a = await box(page.getByTestId(`tile-${id}`));
      const hits: string[] = [];
      for (const o of others) {
        if (overlaps(a, await box(page.getByTestId(`tile-${o}`)))) hits.push(o);
      }
      return hits;
    }, { message: `${id} must not overlap ${others.join(', ')}` })
    .toEqual([]);
}

async function open(page: Page) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
}

async function openCopilotTile(page: Page) {
  await open(page);
  await page.getByTestId('copilot-launch').click();
  await expect(page.getByTestId('tile-copilot')).toBeVisible();
}

async function say(page: Page, text: string) {
  await page.getByTestId('copilot-input').fill(text);
  await page.getByTestId('copilot-send').click();
}

test.describe('Copilot tile', () => {
  test('the floating dock is gone and nothing floats over the workspace', async ({ page }) => {
    await open(page);
    await expect(page.getByTestId('copilot-dock')).toHaveCount(0);
    await expect(page.getByTestId('copilot-dock-toggle')).toHaveCount(0);
    await expect(page.getByTestId('copilot-dock-panel')).toHaveCount(0);
    // The Copilot is not on screen until it is added as a tile.
    await expect(page.getByTestId('copilot-panel')).toHaveCount(0);
    // No fixed-position element anywhere in the app belongs to the Copilot.
    const floating = await page.evaluate(() =>
      [...document.querySelectorAll('[data-testid^="copilot-"]')].filter((el) => {
        for (let n: Element | null = el; n; n = n.parentElement) {
          if (getComputedStyle(n).position === 'fixed') return true;
        }
        return false;
      }).length,
    );
    expect(floating).toBe(0);
  });

  test('the header button toggles the Copilot tile on and off', async ({ page }) => {
    await open(page);
    const button = page.getByTestId('copilot-launch');
    await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
    await expect(button).toHaveAttribute('aria-pressed', 'false');

    await button.click();
    await expect(page.getByTestId('tile-copilot')).toBeVisible();
    await expect(page.getByTestId('copilot-panel')).toBeVisible();
    await expect(button).toHaveAttribute('aria-pressed', 'true');

    await button.click();
    await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
    await expect(button).toHaveAttribute('aria-pressed', 'false');
  });

  for (const word of [':COPILOT', ':AI']) {
    test(`the command bar word ${word} adds the Copilot tile`, async ({ page }) => {
      await open(page);
      await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
      const input = page.getByTestId('command-bar-input');
      await input.click();
      await input.fill(word);
      await input.press('Enter');
      await expect(page.getByTestId('tile-copilot')).toBeVisible();
      await expect(page.getByTestId('copilot-panel')).toBeVisible();
    });
  }

  test('the tile does not cover the watchlist or any other tile', async ({ page }) => {
    await openCopilotTile(page);
    await expectNoOverlap(page, 'copilot', ['watchlist', 'chart', 'orders', 'positions', 'blotter', 'alerts']);
    // The watchlist keeps its own rows reachable: its panel is not under the Copilot.
    await expect
      .poll(async () => overlaps(await box(page.getByTestId('tile-copilot')), await box(page.getByTestId('watchlist-panel'))))
      .toBe(false);
  });

  test('the tile can be dragged and resized like the others without overlapping', async ({ page }) => {
    await page.setViewportSize({ width: 1500, height: 900 });
    await openCopilotTile(page);
    const tile = page.getByTestId('tile-copilot');
    await tile.scrollIntoViewIfNeeded();
    await page.waitForTimeout(500);
    const before = await box(tile);

    // Resize with the corner handle.
    const handle = tile.locator('.react-resizable-handle-se');
    const h = await box(handle);
    await page.mouse.move(h.x + h.width / 2, h.y + h.height / 2);
    await page.mouse.down();
    await page.mouse.move(h.x + h.width / 2 + 160, h.y + h.height / 2 + 4, { steps: 8 });
    await page.mouse.up();
    await expect.poll(async () => (await box(tile)).width).toBeGreaterThan(before.width + 60);

    // Drag by the title bar to the left edge.
    const bar = await box(tile.locator('.tile-drag'));
    const start = await box(tile);
    await page.mouse.move(bar.x + 30, bar.y + bar.height / 2);
    await page.mouse.down();
    await page.mouse.move(bar.x - 400, bar.y + bar.height / 2 + 6, { steps: 12 });
    await page.mouse.up();
    await expect.poll(async () => (await box(tile)).x).toBeLessThan(start.x - 100);

    await expectNoOverlap(page, 'copilot', ['watchlist', 'chart', 'orders', 'positions', 'blotter', 'alerts']);
  });

  test('the Build preset shows chart, strategy IDE and Copilot together', async ({ page }) => {
    await open(page);
    await page.getByTestId('tiles-preset-build').click();
    for (const id of ['chart', 'strategy', 'copilot']) {
      await expect(page.getByTestId(`tile-${id}`)).toBeVisible();
    }
    await expect(page.getByTestId('tile-watchlist')).toHaveCount(0);
    await expectNoOverlap(page, 'chart', ['strategy', 'copilot']);
    await expectNoOverlap(page, 'strategy', ['copilot']);
    // Existing presets still work.
    await page.getByTestId('tiles-preset-trading').click();
    await expect(page.getByTestId('tile-watchlist')).toBeVisible();
    await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
  });

  test('closing the tile keeps the conversation', async ({ page }) => {
    await openCopilotTile(page);
    await say(page, 'remember me');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await page.getByTestId('tile-close-copilot').click();
    await expect(page.getByTestId('tile-copilot')).toHaveCount(0);
    await page.getByTestId('copilot-launch').click();
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['remember me']);
  });
});

test.describe('Copilot chat UI', () => {
  test.beforeEach(async ({ page }) => {
    await openCopilotTile(page);
  });

  test('renders markdown: headings, lists, table, code fence with Copy, safe links', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await say(page, 'MARKDOWN please');
    const msg = page.getByTestId('copilot-assistant-msg');
    await expect(msg.getByRole('heading', { name: 'Plan' })).toBeVisible();
    await expect(msg.locator('strong')).toHaveText('bold');
    await expect(msg.locator('em')).toHaveText('italic');
    await expect(msg.getByTestId('copilot-md-code')).toHaveText('inline');
    await expect(msg.locator('ul li')).toHaveText(['first item', 'second item']);
    await expect(msg.locator('ol li')).toHaveText(['one', 'two']);
    await expect(msg.getByTestId('copilot-md-table').locator('th')).toHaveText(['a', 'b']);
    await expect(msg.getByTestId('copilot-md-table').locator('td')).toHaveText(['1', '2']);

    const block = msg.getByTestId('copilot-code-block');
    await expect(block).toContainText('print("hi")');
    await block.getByTestId('copilot-code-copy').click();
    await expect(block.getByTestId('copilot-code-copy')).toHaveText('Copied');
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('print("hi")');

    // http(s) links open safely; a javascript: link is just text.
    const links = msg.locator('a');
    await expect(links).toHaveCount(1);
    await expect(links.first()).toHaveAttribute('href', 'https://example.com/docs');
    await expect(links.first()).toHaveAttribute('target', '_blank');
    await expect(links.first()).toHaveAttribute('rel', 'noopener noreferrer');
    await expect(msg).toContainText('bad');
    expect(await msg.locator('[href^="javascript"]').count()).toBe(0);

    // Whole-message copy.
    await page.getByTestId('copilot-copy-msg').click();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toContain('# Plan');
  });

  test('HTML in model text is shown as text and never executed', async ({ page }) => {
    await say(page, 'RAWHTML');
    const msg = page.getByTestId('copilot-assistant-msg');
    await expect(msg).toContainText('<img src=x onerror="window.__xss=1">');
    await expect(msg).toContainText('<script>window.__xss=1</script>');
    await expect(msg.locator('img, script, b')).toHaveCount(0);
    await expect(page.locator('#rawhtml-b')).toHaveCount(0);
    await page.waitForTimeout(300);
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss)).toBeUndefined();
  });

  test('the composer sends on Enter, adds a line on Shift+Enter and grows then caps', async ({ page }) => {
    const input = page.getByTestId('copilot-input');
    expect(await input.evaluate((el) => el.tagName)).toBe('TEXTAREA');
    const oneLine = (await box(input)).height;

    await input.fill('line one');
    await input.press('Shift+Enter');
    await input.pressSequentially('line two');
    await expect(page.getByTestId('copilot-user-msg')).toHaveCount(0);
    await expect(input).toHaveValue('line one\nline two');
    expect((await box(input)).height).toBeGreaterThan(oneLine + 10);

    await input.fill(Array.from({ length: 15 }, (_, i) => `row ${i}`).join('\n'));
    const capped = (await box(input)).height;
    expect(capped).toBeLessThanOrEqual(142);
    expect(capped).toBeGreaterThan(oneLine + 60);

    await input.fill('line one\nline two');
    await input.press('Enter');
    await expect(page.getByTestId('copilot-user-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-user-msg')).toContainText('line two');
    await expect(input).toHaveValue('');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
  });

  test('sending is blocked while a reply is pending', async ({ page }) => {
    await say(page, 'SLOWREPLY first');
    await expect(page.getByTestId('copilot-thinking')).toBeVisible();
    await page.getByTestId('copilot-input').fill('second');
    await expect(page.getByTestId('copilot-send')).toBeDisabled();
    await page.getByTestId('copilot-input').press('Enter');
    await expect(page.getByTestId('copilot-user-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-send')).toBeEnabled();
  });

  test('tool calls are a collapsible group with status and an Open button for created things', async ({ page }) => {
    await say(page, 'MAKESTRATEGY moving average');
    const trace = page.getByTestId('copilot-tool-trace');
    await expect(trace).toBeVisible();
    await expect(page.getByTestId('copilot-tools-toggle')).toContainText('Used 1 tool');
    await expect(page.getByTestId('copilot-tool-call')).toHaveCount(0);
    await page.getByTestId('copilot-tools-toggle').click();
    const call = page.getByTestId('copilot-tool-call');
    await expect(call).toHaveCount(1);
    await expect(call).toHaveAttribute('data-ok', 'true');
    await expect(call).toContainText('Write file');
    await expect(call).toContainText('strategies/copilot_strategy.py');
    await page.getByTestId('copilot-tools-toggle').click();
    await expect(page.getByTestId('copilot-tool-call')).toHaveCount(0);
    // The created file stays reachable while the group is collapsed.
    await expect(page.getByTestId('copilot-open-artifact')).toHaveAttribute('data-kind', 'strategy');
  });

  test('Open on a created strategy shows it in the Strategy IDE', async ({ page }) => {
    await expect(page.getByTestId('tile-strategy')).toHaveCount(0);
    await say(page, 'MAKESTRATEGY');
    await page.getByTestId('copilot-open-artifact').click();
    await expect(page.getByTestId('tile-strategy')).toBeVisible();
    await expect(page.getByTestId('file-copilot_strategy.py')).toHaveAttribute('aria-current', 'true');
    await expect(page.getByTestId('tile-strategy')).toContainText('strategies/copilot_strategy.py');
  });

  test('Open on a model or automation switches to the panel that owns it', async ({ page }) => {
    // Drive the same code path the Open button uses.
    await page.evaluate(async () => {
      const nav = await import('/src/stores/navStore.ts');
      nav.openItem('automation', 'auto_1');
    });
    await expect(page.getByTestId('tile-automations')).toBeVisible();
    await page.evaluate(async () => {
      const nav = await import('/src/stores/navStore.ts');
      nav.openItem('model', 'any_model');
    });
    await expect(page.getByTestId('tile-ml')).toBeVisible();
  });

  test('a failed reply offers Retry, which re-asks without duplicating the question', async ({ page }) => {
    await say(page, 'FAILREPLY check');
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mock request failed');
    await expect(page.getByTestId('copilot-copy-msg')).toHaveCount(0);
    await page.getByTestId('copilot-retry').click();
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=agent: FAILREPLY check');
    await expect(page.getByTestId('copilot-user-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-retry')).toHaveCount(0);
  });

  test('suggestion chips fill the empty state and send when clicked', async ({ page }) => {
    await expect(page.getByTestId('copilot-empty')).toBeVisible();
    expect(await page.getByTestId('copilot-suggestion').count()).toBeGreaterThan(3);
    await page.getByTestId('copilot-suggestion').first().click();
    await expect(page.getByTestId('copilot-empty')).toHaveCount(0);
    await expect(page.getByTestId('copilot-user-msg')).toHaveCount(1);
  });

  test('a new reply does not pull the view down when scrolled up; Jump to latest does', async ({ page }) => {
    await page.setViewportSize({ width: 1400, height: 700 });
    for (let i = 0; i < 3; i += 1) {
      await say(page, `MARKDOWN round ${i}`);
      await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(i + 1);
    }
    const scroller = page.getByTestId('copilot-scroll');
    expect(await scroller.evaluate((el) => el.scrollHeight > el.clientHeight + 100)).toBe(true);
    await expect(page.getByTestId('copilot-jump-latest')).toHaveCount(0);

    await say(page, 'SLOWREPLY last');
    await expect(page.getByTestId('copilot-thinking')).toBeVisible();
    await scroller.evaluate((el) => { el.scrollTop = 0; });
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(4);
    expect(await scroller.evaluate((el) => el.scrollTop)).toBeLessThan(5);
    await expect(page.getByTestId('copilot-jump-latest')).toBeVisible();

    await page.getByTestId('copilot-jump-latest').click();
    await expect.poll(() => scroller.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThan(10);
    await expect(page.getByTestId('copilot-jump-latest')).toHaveCount(0);
  });

  test('the mode switch and history stay in the tile header', async ({ page }) => {
    await expect(page.getByTestId('copilot-mode-agent')).toHaveAttribute('aria-pressed', 'true');
    await page.getByTestId('copilot-mode-review').click();
    await expect(page.getByTestId('copilot-mode-review')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('copilot-mode-desc')).toContainText('Asks before each change');
    await say(page, 'hello');
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=review: hello');
    await page.getByTestId('copilot-sessions-toggle').click();
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
    await page.getByTestId('copilot-new-chat').click();
    await expect(page.getByTestId('copilot-empty')).toBeVisible();
  });
});
