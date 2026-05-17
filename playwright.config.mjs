import { defineConfig, devices } from '@playwright/test';

const browserChannel = process.env.PLAYWRIGHT_BROWSER_CHANNEL
  ?? (process.env.CI ? undefined : 'chrome');
const richtextSmokePort = process.env.RICHTEXT_SMOKE_PORT ?? '5245';
const richtextSmokeUrl = `http://127.0.0.1:${richtextSmokePort}`;

export default defineConfig({
  testDir: './tests/playwright',
  timeout: 30_000,
  expect: {
    timeout: 5_000,
  },
  use: {
    baseURL: richtextSmokeUrl,
    browserName: 'chromium',
    ...(browserChannel ? { channel: browserChannel } : {}),
    headless: true,
    trace: 'retain-on-failure',
    viewport: { width: 1280, height: 900 },
  },
  projects: [
    {
      name: 'richtext-chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: {
    command: `python3 -m http.server ${richtextSmokePort} --bind 127.0.0.1 --directory examples/richtext`,
    url: richtextSmokeUrl,
    reuseExistingServer: !process.env.CI,
    timeout: 10_000,
  },
});
