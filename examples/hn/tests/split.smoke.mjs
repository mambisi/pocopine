// Browser smoke for HN's split build.
//
// Exercises the actual startup path: cold home, click into a story, hard
// reload on /item/:id, browser back/forward, and a cold 404. Fails on any
// console error or page error.
//
// Prereqs:
//   1. cargo build --release -p hn  (server binary at workspace target/release/server)
//   2. cargo run -p pocopine-cli --release -- build --release --split --path examples/hn
//   3. cd examples/hn/tests && npm install
//
// Run with:
//   node split.smoke.mjs
//
// Use POCOPINE_SMOKE_CHANNEL=chrome|chromium|firefox to pick a browser
// channel; default is system Chrome.

import { chromium, firefox } from 'playwright';
import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const HN_DIR = resolve(HERE, '..');
const REPO = resolve(HN_DIR, '..', '..');
const SERVER_BIN = resolve(REPO, 'target', 'release', 'server');
const SPLIT_DIR = resolve(HN_DIR, 'pkg', '.pocopine-split');
const PORT = Number(process.env.POCOPINE_SMOKE_PORT ?? '3001');
const BASE = `http://localhost:${PORT}`;
const CHANNEL = process.env.POCOPINE_SMOKE_CHANNEL ?? 'chrome';

function bail(msg) {
  console.error(`smoke: ${msg}`);
  process.exit(2);
}

if (!existsSync(SERVER_BIN)) {
  bail(`server binary missing at ${SERVER_BIN}; run \`cargo build --release -p hn\` first`);
}
if (!existsSync(resolve(SPLIT_DIR, 'shell.wasm'))) {
  bail(`shell.wasm missing at ${SPLIT_DIR}; run \`pocopine build --release --split --path examples/hn\` first`);
}
for (const f of ['hn_route_home.js', 'hn_route_story.js', 'hn_route_not_found.js']) {
  const modulePath = resolve(HN_DIR, 'pkg', f);
  if (!existsSync(modulePath)) {
    bail(`${f} missing at ${resolve(HN_DIR, 'pkg')}; run \`pocopine build --release --split --path examples/hn\` first`);
  }
  const source = readFileSync(modulePath, 'utf8');
  for (const match of source.matchAll(/\/pkg\/\.pocopine-split\/([^"']+\.wasm)/g)) {
    const chunk = match[1];
    if (!existsSync(resolve(SPLIT_DIR, chunk))) {
      bail(`${f} references missing split chunk ${chunk}`);
    }
  }
}

const consoleErrors = [];
const pageErrors = [];
const networkFails = [];
const failedAssertions = [];

function expect(cond, msg) {
  if (!cond) {
    failedAssertions.push(msg);
    console.error(`  ✗ ${msg}`);
  } else {
    console.log(`  ✓ ${msg}`);
  }
}

async function waitForReady(maxMs = 4000) {
  const deadline = Date.now() + maxMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(BASE + '/');
      if (res.ok) return;
    } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  throw new Error(`server never came up on ${BASE}`);
}

const server = spawn(SERVER_BIN, [], {
  stdio: ['ignore', 'pipe', 'pipe'],
  env: { ...process.env, RUST_LOG: 'warn' },
});
server.stdout.on('data', d => process.stderr.write('[srv] ' + d));
server.stderr.on('data', d => process.stderr.write('[srv-err] ' + d));

let exitCode = 0;
try {
  await waitForReady();

  const launcher = CHANNEL === 'firefox' ? firefox : chromium;
  const launchOpts = CHANNEL === 'firefox' ? { headless: true } : { channel: CHANNEL, headless: true };
  const browser = await launcher.launch(launchOpts);
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  page.on('console', msg => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('pageerror', err => pageErrors.push(err.message));
  page.on('requestfailed', req => {
    const url = req.url();
    if (!url.startsWith(BASE)) return; // ignore third-party (Firebase, etc.)
    // Aborted in-flight requests (navigation cancellation, resource hint
    // dropped, Promise.race losers) aren't failures of the split system —
    // ignore those. Only count hard failures: network errors, DNS, etc.
    const reason = req.failure()?.errorText ?? '';
    if (reason.includes('ABORTED') || reason.includes('CANCELED')) return;
    networkFails.push(`${url} (${reason})`);
  });

  console.log('T1: cold home load');
  await page.goto(`${BASE}/`, { waitUntil: 'networkidle', timeout: 15000 });
  // Stories arrive via fetch + render; wait briefly.
  await page.waitForSelector('a[href^="/item/"]', { timeout: 10000 });
  const storyCount = await page.locator('a[href^="/item/"]').count();
  expect(storyCount > 0, `home rendered story links (${storyCount} found)`);

  console.log('T2: click into a story (client navigation)');
  const firstHref = await page.locator('a[href^="/item/"]').first().getAttribute('href');
  await page.locator('a[href^="/item/"]').first().click();
  await page.waitForURL(`**${firstHref}`, { timeout: 5000 });
  expect(page.url().endsWith(firstHref), `client-navigated to ${firstHref}`);

  console.log('T3: hard reload on /item/:id');
  await page.reload({ waitUntil: 'networkidle', timeout: 15000 });
  await page.waitForTimeout(500);
  const bodyTextLen = (await page.locator('body').textContent())?.length ?? 0;
  expect(bodyTextLen > 200, `story page rendered after reload (${bodyTextLen} chars)`);

  console.log('T4: browser back to home');
  await page.goBack({ waitUntil: 'networkidle', timeout: 5000 });
  await page.waitForTimeout(300);
  expect(page.url() === `${BASE}/`, `back navigated to ${BASE}/`);
  await page.waitForSelector('a[href^="/item/"]', { timeout: 5000 });
  expect((await page.locator('a[href^="/item/"]').count()) > 0, 'home re-rendered after back');

  console.log('T5: browser forward to story');
  await page.goForward({ waitUntil: 'networkidle', timeout: 5000 });
  await page.waitForTimeout(300);
  expect(page.url().endsWith(firstHref), `forward navigated back to ${firstHref}`);

  console.log('T6: cold 404 (deep link to nonexistent path)');
  await page.goto(`${BASE}/this/path/does/not/exist`, { waitUntil: 'networkidle', timeout: 10000 });
  await page.waitForTimeout(300);
  // not_found route should still mount and render *something* without erroring.
  const notFoundText = (await page.locator('body').textContent())?.length ?? 0;
  expect(notFoundText > 10, `404 path rendered the not_found route (${notFoundText} chars)`);

  await browser.close();
} catch (e) {
  console.error('smoke run threw:', e.message);
  console.error(e.stack);
  exitCode = 1;
} finally {
  server.kill();
  // give it a moment to actually die
  await new Promise(r => setTimeout(r, 100));
}

console.log('---');
console.log(`console errors:  ${consoleErrors.length}`);
for (const e of consoleErrors) console.log(`  console.error: ${e}`);
console.log(`page errors:     ${pageErrors.length}`);
for (const e of pageErrors) console.log(`  pageerror: ${e}`);
console.log(`network fails:   ${networkFails.length}`);
for (const u of networkFails) console.log(`  failed: ${u}`);
console.log(`assertions failed: ${failedAssertions.length}`);

if (consoleErrors.length || pageErrors.length || failedAssertions.length || networkFails.length) {
  exitCode = 1;
}
process.exit(exitCode);
