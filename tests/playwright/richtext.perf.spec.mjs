// Browser-side perf harness for pine-richtext.
//
// Each test loads the demo with a `?bench=...` URL param (which seeds
// the surface with a large doc), drives synthetic user input, and
// aggregates the `pine-richtext:perf` console events emitted by
// `debug-perf="true"` on the surface. Results are written to
// `target/perf/<scenario>.json` so before/after numbers are easy to
// diff in CI logs.
//
// The aim is not strict pass/fail — that's what `parity_commands.rs`
// is for. The aim is to surface where time goes as the doc grows, so
// the next code change can be judged by the right metric. Each test
// asserts a generous wall-clock budget so a 10x regression breaks the
// build; the rich data lives in the written JSON.

import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

const PERF_OUTPUT_DIR = resolve(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'target',
  'perf',
);

/** Buffer every `pine-richtext:perf` console event on the page. */
function collectPerfEvents(page) {
  const events = [];
  page.on('console', async (message) => {
    if (message.type() !== 'log') return;
    const args = message.args();
    if (args.length < 2) return;
    let marker;
    try {
      marker = await args[0].jsonValue();
    } catch {
      return;
    }
    if (marker !== 'pine-richtext:perf') return;
    try {
      const raw = await args[1].jsonValue();
      events.push(JSON.parse(raw));
    } catch {
      // Ignore unrelated logs sharing the arity.
    }
  });
  return events;
}

/** Place the caret at the end of the very first paragraph. */
async function caretAtEndOfFirstParagraph(page) {
  await page.evaluate(() => {
    const p = document.querySelector(
      'pine-rich-text-root:not([runtime]) p[data-pos="0"]',
    );
    if (!p?.firstChild) throw new Error('first paragraph missing');
    const text = p.firstChild;
    const range = document.createRange();
    range.setStart(text, text.textContent.length);
    range.setEnd(text, text.textContent.length);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    p.focus?.();
  });
}

function summarize(values) {
  if (values.length === 0) {
    return { count: 0, sum_ms: 0, mean_ms: 0, p50_ms: 0, p95_ms: 0, p99_ms: 0, max_ms: 0 };
  }
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
  const sum = sorted.reduce((acc, v) => acc + v, 0);
  return {
    count: sorted.length,
    sum_ms: round2(sum),
    mean_ms: round2(sum / sorted.length),
    p50_ms: round2(at(0.5)),
    p95_ms: round2(at(0.95)),
    p99_ms: round2(at(0.99)),
    max_ms: round2(sorted[sorted.length - 1]),
  };
}

function round2(n) {
  return Math.round(n * 100) / 100;
}

/** Group events by `event`, pull a numeric field from the payload, and summarize. */
function summarizeBy(events, field) {
  const groups = new Map();
  for (const evt of events) {
    const val = evt?.payload?.[field];
    if (typeof val !== 'number') continue;
    if (!groups.has(evt.event)) groups.set(evt.event, []);
    groups.get(evt.event).push(val);
  }
  const out = {};
  for (const [name, values] of groups) {
    out[name] = summarize(values);
  }
  return out;
}

function writePerfReport(scenario, payload) {
  mkdirSync(PERF_OUTPUT_DIR, { recursive: true });
  const path = resolve(PERF_OUTPUT_DIR, `${scenario}.json`);
  writeFileSync(path, JSON.stringify(payload, null, 2));
  return path;
}

async function bootBenchPage(page, query) {
  const events = collectPerfEvents(page);
  await page.goto(`/?${query}`);
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
  return events;
}

test.describe('pine-richtext typing latency', () => {
  for (const preset of ['small', 'medium', 'large']) {
    test(`type 200 chars at end of first paragraph in ${preset} doc`, async ({ page }) => {
      const errors = [];
      page.on('pageerror', (error) => errors.push(error.message));

      const events = await bootBenchPage(page, `bench=${preset}`);
      await caretAtEndOfFirstParagraph(page);
      events.length = 0;

      const sample = 'the quick brown fox jumps over the lazy dog ';
      const typed = sample.repeat(5).slice(0, 200);

      const started = Date.now();
      await page.keyboard.type(typed, { delay: 0 });
      // Let the last frame's microtasks settle.
      await page.waitForTimeout(50);
      const wallMs = Date.now() - started;

      // After-dispatch is the most user-facing field (covers the
      // commit + DOM flush). before-dispatch shows command time
      // alone. total_ms covers ops without a dispatch.
      const after = summarizeBy(events, 'total_after_dispatch_ms');
      const before = summarizeBy(events, 'total_before_dispatch_ms');
      const total = summarizeBy(events, 'total_ms');
      const commit = summarizeBy(
        events.filter((e) => e.event === 'dispatch.commit'),
        'total_ms',
      );

      const report = {
        scenario: `type-200-${preset}`,
        chars_typed: typed.length,
        wall_clock: {
          total_ms: wallMs,
          per_char_ms: round2(wallMs / typed.length),
        },
        events_captured: events.length,
        per_event: {
          total_after_dispatch_ms: after,
          total_before_dispatch_ms: before,
          total_ms: total,
        },
        dispatch_commit: commit,
      };
      const path = writePerfReport(report.scenario, report);
      console.log(`[perf] wrote ${path}`);

      // Sanity budget — typing 200 chars in a 500-paragraph doc should
      // still finish under 30 seconds. If it doesn't, something is
      // very wrong; that's what we're hunting.
      expect(wallMs).toBeLessThan(30_000);
      expect(events.length).toBeGreaterThan(0);
      expect(errors).toEqual([]);
    });
  }

  test('Enter chain at end of first paragraph in large doc', async ({ page }, testInfo) => {
    // Enter at the end of a paragraph is the user-flow that exercises
    // split_block + content matching + reconciler structural patch.
    // The structural-write path still scales poorly in a 500-paragraph
    // doc — each Enter currently runs ~400 ms. The test budget is
    // intentionally loose so the spec still reports a number; the
    // ratchet lives in the JSON snapshot, not the assertion.
    testInfo.setTimeout(120_000);
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));

    const events = await bootBenchPage(page, 'bench=large');
    await caretAtEndOfFirstParagraph(page);
    events.length = 0;

    const presses = 30;
    const started = Date.now();
    for (let i = 0; i < presses; i += 1) {
      await page.keyboard.press('Enter');
    }
    await page.waitForTimeout(50);
    const wallMs = Date.now() - started;

    const report = {
      scenario: `enter-${presses}-large`,
      presses,
      wall_clock: {
        total_ms: wallMs,
        per_press_ms: round2(wallMs / presses),
      },
      events_captured: events.length,
      per_event: {
        total_after_dispatch_ms: summarizeBy(events, 'total_after_dispatch_ms'),
        total_before_dispatch_ms: summarizeBy(events, 'total_before_dispatch_ms'),
        total_ms: summarizeBy(events, 'total_ms'),
      },
    };
    const path = writePerfReport(report.scenario, report);
    console.log(`[perf] wrote ${path}`);

    expect(wallMs).toBeLessThan(60_000);
    expect(errors).toEqual([]);
  });

  test('type 200 chars in leading paragraph of 500-task-item doc', async ({ page }, testInfo) => {
    // Custom-element scenario: the doc contains 500 `<pine-task-item>`
    // node-views below a leading paragraph. Typing happens in the
    // leading paragraph (where caret-placement is straightforward),
    // but the reconciler still has to skip the entire task-list
    // subtree on every commit — so this isolates "doc with many
    // custom elements" cost from "typing inside a custom element"
    // cost (a separate scenario below).
    testInfo.setTimeout(60_000);
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));

    const events = await bootBenchPage(page, 'bench=tasks');
    await caretAtEndOfFirstParagraph(page);
    events.length = 0;

    const sample = 'the quick brown fox jumps over the lazy dog ';
    const typed = sample.repeat(5).slice(0, 200);

    const started = Date.now();
    await page.keyboard.type(typed, { delay: 0 });
    await page.waitForTimeout(50);
    const wallMs = Date.now() - started;

    const report = {
      scenario: 'type-200-tasks',
      chars_typed: typed.length,
      wall_clock: { total_ms: wallMs, per_char_ms: round2(wallMs / typed.length) },
      events_captured: events.length,
      per_event: {
        total_after_dispatch_ms: summarizeBy(events, 'total_after_dispatch_ms'),
        total_before_dispatch_ms: summarizeBy(events, 'total_before_dispatch_ms'),
        total_ms: summarizeBy(events, 'total_ms'),
      },
    };
    const path = writePerfReport(report.scenario, report);
    console.log(`[perf] wrote ${path}`);
    expect(wallMs).toBeLessThan(30_000);
    expect(errors).toEqual([]);
  });

  test('toggle 30 task-item checkboxes in 500-task-item doc', async ({ page }, testInfo) => {
    // The toggle path dispatches `SetNodeAttr { attr: "checked" }`
    // and the reconciler should land on the `NodeAttrs` outcome —
    // attr-only patches don't rebuild content or move the cursor.
    // If this path regresses (e.g. by promoting to a full
    // reconcile), the per-press budget explodes.
    testInfo.setTimeout(60_000);
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));

    const events = await bootBenchPage(page, 'bench=tasks');
    events.length = 0;

    const presses = 30;
    const started = Date.now();
    for (let i = 0; i < presses; i += 1) {
      // Click the i-th checkbox. Selector matches the node-view chrome.
      await page
        .locator('pine-rich-text-root:not([runtime]) pine-task-item .pine-task-item-check')
        .nth(i)
        .click();
    }
    await page.waitForTimeout(50);
    const wallMs = Date.now() - started;

    const report = {
      scenario: `toggle-${presses}-tasks`,
      presses,
      wall_clock: { total_ms: wallMs, per_press_ms: round2(wallMs / presses) },
      events_captured: events.length,
      per_event: {
        total_after_dispatch_ms: summarizeBy(events, 'total_after_dispatch_ms'),
        total_before_dispatch_ms: summarizeBy(events, 'total_before_dispatch_ms'),
        total_ms: summarizeBy(events, 'total_ms'),
      },
    };
    const path = writePerfReport(report.scenario, report);
    console.log(`[perf] wrote ${path}`);
    expect(wallMs).toBeLessThan(45_000);
    expect(errors).toEqual([]);
  });

  test('Backspace chain at end of first paragraph in large doc', async ({ page }) => {
    // Backspace is the other heavy structural-edit path; it runs
    // delete_backward_char_transaction or falls into the
    // command-chain dispatch.
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));

    const events = await bootBenchPage(page, 'bench=large');
    await caretAtEndOfFirstParagraph(page);
    // Type some chars first so backspace has something to chew on.
    await page.keyboard.type('XXXXXXXXXXXXXXXXXXXX', { delay: 0 });
    await page.waitForTimeout(20);
    events.length = 0;

    const started = Date.now();
    for (let i = 0; i < 20; i += 1) {
      await page.keyboard.press('Backspace');
    }
    await page.waitForTimeout(50);
    const wallMs = Date.now() - started;

    const report = {
      scenario: 'backspace-20-large',
      presses: 20,
      wall_clock: {
        total_ms: wallMs,
        per_press_ms: round2(wallMs / 20),
      },
      events_captured: events.length,
      per_event: {
        total_after_dispatch_ms: summarizeBy(events, 'total_after_dispatch_ms'),
        total_before_dispatch_ms: summarizeBy(events, 'total_before_dispatch_ms'),
        total_ms: summarizeBy(events, 'total_ms'),
      },
    };
    const path = writePerfReport(report.scenario, report);
    console.log(`[perf] wrote ${path}`);

    expect(wallMs).toBeLessThan(15_000);
    expect(errors).toEqual([]);
  });
});
