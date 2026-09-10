import { defineConfig, devices } from '@playwright/test';

const browserChannel = process.env.PLAYWRIGHT_BROWSER_CHANNEL
  ?? (process.env.CI ? undefined : 'chrome');
const richtextSmokePort = process.env.RICHTEXT_SMOKE_PORT ?? '5245';
const richtextSmokeUrl = `http://127.0.0.1:${richtextSmokePort}`;
// Which example directory the static server serves. Defaults to the richtext
// smoke; set PLAYWRIGHT_SERVE_DIR=examples/<other> to run another example's spec.
const serveDir = process.env.PLAYWRIGHT_SERVE_DIR ?? 'examples/richtext';
const codeEditor = process.env.PLAYWRIGHT_EXAMPLE === 'code-editor';
const codePort = process.env.CODE_EDITOR_PORT ?? '3044';
const codeUrl = `http://127.0.0.1:${codePort}`;

export default defineConfig({
  testDir: './tests/playwright',
  ...(codeEditor ? { testMatch: /code-editor.*\.spec\.mjs/ } : { testIgnore: /code-editor.*\.spec\.mjs/ }),
  timeout: 30_000,
  expect: {
    timeout: 5_000,
  },
  use: {
    baseURL: codeEditor ? codeUrl : richtextSmokeUrl,
    browserName: 'chromium',
    ...(!codeEditor && browserChannel ? { channel: browserChannel } : {}),
    headless: true,
    trace: 'retain-on-failure',
    viewport: { width: 1280, height: 900 },
  },
  projects: codeEditor ? [
    { name: 'code-editor-chromium', use: { ...devices['Desktop Chrome'] } },
    { name: 'code-editor-firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'code-editor-webkit', use: { ...devices['Desktop Safari'] } },
  ] : [
    {
      name: 'richtext-chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: codeEditor ? {
    command: `pocopine run --path examples/code-editor --port ${codePort}${process.env.CODE_EDITOR_RELEASE === '1' ? ' --release' : ''}`,
    url: codeUrl,
    reuseExistingServer: !process.env.CI,
    timeout: 240_000,
  } : {
    command: `python3 -m http.server ${richtextSmokePort} --bind 127.0.0.1 --directory ${serveDir}`,
    url: richtextSmokeUrl,
    reuseExistingServer: !process.env.CI,
    timeout: 10_000,
  },
});
