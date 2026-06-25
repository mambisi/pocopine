import { expect, test } from '@playwright/test';

// Serve with: PLAYWRIGHT_SERVE_DIR=examples/sync-external-e2e (see playwright.config.mjs).
// Proves QueryClient::apply_external_changes in a REAL browser: a realtime-style
// delta routes into the live view AND survives a reload, served from IndexedDB.

const rows = (page) => page.evaluate(() => JSON.parse(window.view_rows()));

test('apply_external_changes routes into the view and survives an offline reload', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#app')).toHaveText('ready');

  // Start with a clean IndexedDB so a prior run can't leak in.
  await page.evaluate(
    () => new Promise((resolve) => {
      const req = indexedDB.deleteDatabase('sync_external_e2e');
      req.onsuccess = req.onerror = req.onblocked = () => resolve();
    }),
  );
  await page.reload();
  await expect(page.locator('#app')).toHaveText('ready');
  expect(await rows(page)).toHaveLength(0);

  // Inject a row as if a WebSocket delivered it (no server, no mutation).
  await page.evaluate(() => window.inject('n1', 'delivered over WS'));
  await expect.poll(async () => (await rows(page)).length).toBe(1);
  expect((await rows(page))[0]).toMatchObject({ id: 'n1', body: 'delivered over WS' });

  // Reload (there is no server → /open + /pull fail): the row must still be here,
  // hydrated from the durable IndexedDB cache. This is the offline-survival guarantee.
  await page.reload();
  await expect(page.locator('#app')).toHaveText('ready');
  await expect.poll(async () => (await rows(page)).length, { timeout: 10_000 }).toBe(1);
  expect((await rows(page))[0]).toMatchObject({ id: 'n1', body: 'delivered over WS' });
});
