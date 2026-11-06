import { test, expect, type Page } from '@playwright/test';

// The web mock echoes the mode it receives ("[mock] mode=agent: <message>").
// "SLOWREPLY" in a message delays the mock 800 ms; "LIVEORDER" makes it pause once
// for approval (outside ask mode) and answer on the replay.

async function trackIpc(page: Page) {
  await page.addInitScript(() => {
    (window as unknown as { __ipcLog: unknown[] }).__ipcLog = [];
  });
}

async function chatCalls(page: Page) {
  return page.evaluate(() =>
    (window as unknown as { __ipcLog: { cmd: string; args: Record<string, unknown> }[] }).__ipcLog
      .filter((c) => c.cmd === 'copilot_chat')
      .map((c) => c.args),
  );
}

async function openCopilot(page: Page) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  await page.getByTestId('copilot-launch').click();
  await expect(page.getByTestId('copilot-panel')).toBeVisible();
}

async function say(page: Page, text: string) {
  await page.getByTestId('copilot-input').fill(text);
  await page.getByTestId('copilot-send').click();
}

async function openSessions(page: Page) {
  if (!(await page.getByTestId('copilot-sessions').isVisible())) {
    await page.getByTestId('copilot-sessions-toggle').click();
  }
  await expect(page.getByTestId('copilot-sessions')).toBeVisible();
}

const item = (page: Page, title: string) =>
  page.getByTestId('copilot-session-item').filter({ has: page.getByText(title, { exact: true }) });

test.describe('Copilot modes', () => {
  test('defaults to Agent and sends it with the request', async ({ page }) => {
    await trackIpc(page);
    await openCopilot(page);
    await expect(page.getByTestId('copilot-mode-agent')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('copilot-mode-ask')).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByTestId('copilot-mode-desc')).toContainText('live-broker actions still ask you');

    await say(page, 'hello');
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=agent: hello');
    expect((await chatCalls(page)).map((a) => a.mode)).toEqual(['agent']);
  });

  test('a mode change persists across reload and is sent on the next message', async ({ page }) => {
    await trackIpc(page);
    await openCopilot(page);
    await page.getByTestId('copilot-mode-ask').click();
    await expect(page.getByTestId('copilot-mode-ask')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('copilot-mode-desc')).toContainText('Read-only');

    await page.reload();
    await page.waitForLoadState('networkidle');
    await page.getByTestId('copilot-launch').click();
    await expect(page.getByTestId('copilot-mode-ask')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('copilot-mode-agent')).toHaveAttribute('aria-pressed', 'false');

    await say(page, 'after reload');
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=ask: after reload');
    expect((await chatCalls(page)).map((a) => a.mode)).toEqual(['ask']);
  });

  test('suggestions send in the selected mode', async ({ page }) => {
    await openCopilot(page);
    await page.getByTestId('copilot-mode-review').click();
    await page.getByTestId('copilot-suggestion').first().click();
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=review:');
  });

  test('the approval card still works in agent mode and the replay keeps the mode and turn', async ({ page }) => {
    await trackIpc(page);
    await openCopilot(page);
    await say(page, 'LIVEORDER buy 1');
    await expect(page.getByTestId('copilot-approval-card')).toBeVisible();
    await expect(page.getByTestId('copilot-approval-line')).toContainText('LIVE');
    await page.getByTestId('copilot-approve').click();
    await expect(page.getByTestId('copilot-assistant-msg').last()).toContainText('mode=agent: LIVEORDER buy 1');
    await expect(page.getByTestId('copilot-approval-card')).toContainText('Approved');

    const calls = await chatCalls(page);
    expect(calls).toHaveLength(2);
    expect(calls.map((c) => c.mode)).toEqual(['agent', 'agent']);
    expect(calls[1].turnId).toBe(calls[0].turnId);
    // The replay's history excludes the paused turn.
    expect(calls[1].history).toEqual([]);
  });

  test('ask mode never asks for approval in the mock', async ({ page }) => {
    await openCopilot(page);
    await page.getByTestId('copilot-mode-ask').click();
    await say(page, 'LIVEORDER buy 1');
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=ask');
    await expect(page.getByTestId('copilot-approval-card')).toHaveCount(0);
  });
});

test.describe('Copilot sessions', () => {
  test('new chat starts a separate session; each keeps its own messages', async ({ page }) => {
    await trackIpc(page);
    await openCopilot(page);
    await say(page, 'first topic');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);

    await page.getByTestId('copilot-new-chat').click();
    await expect(page.getByTestId('copilot-empty')).toBeVisible();
    await expect(page.getByTestId('copilot-user-msg')).toHaveCount(0);
    await say(page, 'second topic');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);

    await openSessions(page);
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(2);
    await expect(item(page, 'second topic')).toHaveAttribute('aria-current', 'true');
    await expect(item(page, 'first topic')).not.toHaveAttribute('aria-current', 'true');

    await item(page, 'first topic').getByTestId('copilot-session-open').click();
    await expect(page.getByTestId('copilot-sessions')).toHaveCount(0);
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['first topic']);
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('first topic');
    await expect(page.getByTestId('copilot-panel')).not.toContainText('second topic');

    // The history sent to the backend comes from the session in view only.
    await say(page, 'follow up');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(2);
    const calls = await chatCalls(page);
    expect(calls[calls.length - 1].history).toEqual([
      { role: 'user', content: 'first topic' },
      { role: 'assistant', content: '[mock] mode=agent: first topic' },
    ]);

    await openSessions(page);
    await item(page, 'second topic').getByTestId('copilot-session-open').click();
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['second topic']);
  });

  test('new chat does not stack empty sessions', async ({ page }) => {
    await openCopilot(page);
    await page.getByTestId('copilot-new-chat').click();
    await page.getByTestId('copilot-new-chat').click();
    await openSessions(page);
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
    await expect(page.getByTestId('copilot-session-item')).toContainText('New chat');
  });

  test('sessions and the active one survive a reload', async ({ page }) => {
    await openCopilot(page);
    await say(page, 'keep me');
    await page.getByTestId('copilot-new-chat').click();
    await say(page, 'and me');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);

    await page.reload();
    await page.waitForLoadState('networkidle');
    await page.getByTestId('copilot-launch').click();
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['and me']);
    await openSessions(page);
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(2);
    await item(page, 'keep me').getByTestId('copilot-session-open').click();
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['keep me']);
  });

  test('rename changes the title and keeps it after more messages', async ({ page }) => {
    await openCopilot(page);
    await say(page, 'original title');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await openSessions(page);
    await item(page, 'original title').getByTestId('copilot-session-rename').click();
    await page.getByTestId('copilot-session-rename-input').fill('Renamed chat');
    await page.getByTestId('copilot-session-rename-input').press('Enter');
    await expect(item(page, 'Renamed chat')).toHaveCount(1);
    await expect(page.getByTestId('copilot-session-title')).toHaveText('Renamed chat');

    await say(page, 'another message');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(2);
    await expect(page.getByTestId('copilot-session-title')).toHaveText('Renamed chat');

    await page.reload();
    await page.waitForLoadState('networkidle');
    await page.getByTestId('copilot-launch').click();
    await expect(page.getByTestId('copilot-session-title')).toHaveText('Renamed chat');
  });

  test('delete asks first, and deleting the active chat activates another', async ({ page }) => {
    await openCopilot(page);
    await say(page, 'chat one');
    await page.getByTestId('copilot-new-chat').click();
    await say(page, 'chat two');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);

    await openSessions(page);
    // Cancelling keeps it.
    await item(page, 'chat two').getByTestId('copilot-session-delete').click();
    await item(page, 'chat two').getByTestId('copilot-session-delete-cancel').click();
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(2);

    // Deleting the active one falls back to the other.
    await item(page, 'chat two').getByTestId('copilot-session-delete').click();
    await item(page, 'chat two').getByTestId('copilot-session-delete-confirm').click();
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
    await expect(item(page, 'chat one')).toHaveAttribute('aria-current', 'true');
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['chat one']);

    // Deleting the last one leaves a fresh empty chat.
    await item(page, 'chat one').getByTestId('copilot-session-delete').click();
    await item(page, 'chat one').getByTestId('copilot-session-delete-confirm').click();
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
    await expect(page.getByTestId('copilot-session-item')).toContainText('New chat');
    await expect(page.getByTestId('copilot-empty')).toBeVisible();
  });

  test('deleting a non-active chat leaves the active one alone', async ({ page }) => {
    await openCopilot(page);
    await say(page, 'old one');
    await page.getByTestId('copilot-new-chat').click();
    await say(page, 'current one');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await openSessions(page);
    await item(page, 'old one').getByTestId('copilot-session-delete').click();
    await item(page, 'old one').getByTestId('copilot-session-delete-confirm').click();
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
    await expect(item(page, 'current one')).toHaveAttribute('aria-current', 'true');
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['current one']);
  });

  test('legacy single history becomes one session and the old key is removed', async ({ page }) => {
    await page.goto('/');
    await page.evaluate(() => {
      localStorage.setItem(
        'apex.copilot.history',
        JSON.stringify([
          { id: 1, role: 'user', content: 'legacy question about RELIANCE' },
          { id: 2, role: 'assistant', content: 'legacy answer' },
        ]),
      );
    });
    await page.reload();
    await page.waitForLoadState('networkidle');
    await page.getByTestId('copilot-launch').click();
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['legacy question about RELIANCE']);
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveText(['legacy answer']);
    await openSessions(page);
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
    await expect(page.getByTestId('copilot-session-item')).toContainText('legacy question about RELIANCE');
    expect(await page.evaluate(() => localStorage.getItem('apex.copilot.history'))).toBeNull();

    // A second reload must not migrate again or duplicate.
    await page.reload();
    await page.waitForLoadState('networkidle');
    await page.getByTestId('copilot-launch').click();
    await openSessions(page);
    await expect(page.getByTestId('copilot-session-item')).toHaveCount(1);
  });

  test('a slow reply lands in the session that asked, not the one in view', async ({ page }) => {
    await openCopilot(page);
    await say(page, 'SLOWREPLY alpha');
    await expect(page.getByTestId('copilot-thinking')).toBeVisible();

    // Start another chat while the first request is still in flight.
    await page.getByTestId('copilot-new-chat').click();
    await expect(page.getByTestId('copilot-empty')).toBeVisible();
    await expect(page.getByTestId('copilot-thinking')).toHaveCount(0);
    await say(page, 'beta');
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('mode=agent: beta');

    // Stay on the second chat until the slow reply has landed somewhere (two
    // assistant messages stored in total), so it cannot be masked by switching early.
    await expect
      .poll(() =>
        page.evaluate(() => {
          const raw = localStorage.getItem('apex.copilot.sessions.v1');
          const data = JSON.parse(raw ?? '{"sessions":[]}') as { sessions: { entries: { role: string }[] }[] };
          return data.sessions.reduce((n, s) => n + s.entries.filter((e) => e.role === 'assistant').length, 0);
        }),
      )
      .toBe(2);
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-assistant-msg')).not.toContainText('SLOWREPLY');

    await openSessions(page);
    await item(page, 'SLOWREPLY alpha').getByTestId('copilot-session-open').click();
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-assistant-msg')).toContainText('SLOWREPLY alpha');
    await expect(page.getByTestId('copilot-thinking')).toHaveCount(0);

    await openSessions(page);
    await item(page, 'beta').getByTestId('copilot-session-open').click();
    await expect(page.getByTestId('copilot-user-msg')).toHaveText(['beta']);
    await expect(page.getByTestId('copilot-assistant-msg')).toHaveCount(1);
    await expect(page.getByTestId('copilot-assistant-msg')).not.toContainText('SLOWREPLY');
  });

  test('the history list opens and closes from the header', async ({ page }) => {
    await openCopilot(page);
    await page.getByTestId('copilot-sessions-toggle').click();
    await expect(page.getByTestId('copilot-sessions')).toBeVisible();
    await page.getByTestId('copilot-sessions-toggle').click();
    await expect(page.getByTestId('copilot-sessions')).toHaveCount(0);
  });
});
