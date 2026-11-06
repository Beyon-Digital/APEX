import { defineConfig, devices } from '@playwright/test';

/**
 * See https://playwright.dev/docs/test-configuration.
 */
// Parallel agents/shells can run their own dev server: PW_PORT=1431 PW_OUTPUT=/tmp/pw-1431
const PORT = Number(process.env.PW_PORT ?? 1420);
const ORIGIN = `http://localhost:${PORT}`;

export default defineConfig({
  testDir: './tests',
  outputDir: process.env.PW_OUTPUT ?? 'test-results',
  /* Run tests in files in parallel */
  fullyParallel: true,
  /* Fail the build on CI if you accidentally left test.only in the source code. */
  forbidOnly: !!process.env.CI,
  /* Retry on CI only */
  retries: process.env.CI ? 2 : 0,
  /* Opt out of parallel tests on CI. */
  workers: process.env.CI ? 1 : undefined,
  /* Reporter to use. See https://playwright.dev/docs/test-reporters */
  reporter: process.env.PW_REPORTER ? [[process.env.PW_REPORTER]] : 'html',
  /* Shared settings for all the projects below. See https://playwright.dev/docs/api/class-testoptions. */
  use: {
    /* Base URL to use in actions like `await page.goto('/')`. */
    baseURL: ORIGIN,
    /* Collect trace when retrying the failed test. See https://playwright.dev/docs/trace-viewer */
    trace: 'on-first-retry',
    /* The app defaults to the tiled workspace. Most specs were written for the
       one-panel-at-a-time focus view, so seed it; tiles.spec.ts overrides this. */
    storageState: {
      cookies: [],
      origins: [
        {
          origin: ORIGIN,
          localStorage: [
            { name: 'workspace-storage', value: JSON.stringify({ state: { viewMode: 'focus' }, version: 0 }) },
          ],
        },
      ],
    },
  },

  /* Configure projects for major browsers */
  projects: [
    {
      name: 'chromium',
      // PW_CHANNEL=chrome runs against an installed Google Chrome instead of
      // Playwright's downloaded Chromium.
      use: { ...devices['Desktop Chrome'], ...(process.env.PW_CHANNEL ? { channel: process.env.PW_CHANNEL } : {}) },
    },
  ],

  /* Run your local dev server before starting the tests */
  webServer: {
    command: `bun run dev --host 127.0.0.1 --port ${PORT}`,
    url: ORIGIN,
    reuseExistingServer: true,
    timeout: 120000,
  },
});
