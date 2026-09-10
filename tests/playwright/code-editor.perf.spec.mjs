import { test, expect } from '@playwright/test';
import os from 'node:os';
import fs from 'node:fs/promises';
import { gzipSync, brotliCompressSync } from 'node:zlib';

test.setTimeout(600_000);
// Trace DOM snapshots and screenshots materially distort large-document timing.
test.use({trace: 'off', screenshot: 'off', video: 'off'});
const surface = '#source [data-pine-code-content]';
const snap = page => page.evaluate(() => JSON.parse(window.codeExample.inspect_editor()).snapshot.Ok);
async function command(page, command, value = '', anchor = 0, head = anchor) {
  return page.evaluate(({command, value, anchor, head}) => {
    const state = JSON.parse(window.codeExample.inspect_editor()).snapshot.Ok;
    return JSON.parse(window.codeExample.editor_command(command, value, anchor, head, BigInt(state.revision)));
  }, {command, value, anchor, head});
}
async function settled(page) {
  // Let a new mount or edit schedule its worker request before observing status.
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await page.waitForFunction(() => JSON.parse(window.codeExample.inspect_editor()).snapshot.Ok.presentation_status !== 'Pending');
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
}
const percentile = (values, p) => [...values].sort((a,b) => a-b)[Math.max(0, Math.ceil(values.length * p) - 1)] ?? 0;
const summarize = values => ({p50: percentile(values, .5), p95: percentile(values, .95), max: Math.max(0, ...values)});

// Reproducible ASCII sizes leave a replacement target on each nonempty line.
function sized(bytes) {
  const line = 'let x = 123; // bounded code editor fixture\n';
  return line.repeat(Math.floor(bytes / line.length)) + 'x'.repeat(bytes % line.length);
}

async function bootstrap(page) {
  await page.goto('/');
  await page.locator(surface).waitFor();
  await page.waitForFunction(() => Boolean(window.codeExampleWasm));
  await settled(page);
}

test('release typing, DOM locality, commands, and retained memory at documented limits', async ({page, browser}, info) => {
  test.skip(info.project.name !== 'code-editor-chromium', 'PerformanceEventTiming is measured in Chromium.');
  await bootstrap(page);
  expect((await command(page, 'load', 'x'.repeat(256 * 1024))).Err).toBe('SizeLimit');
  const report = {
    timestamp: new Date().toISOString(), browser: browser.version(),
    machine: {platform: os.platform(), release: os.release(), cpu: os.cpus()[0]?.model, logicalCpus: os.cpus().length, memoryBytes: os.totalmem()},
    build: 'pocopine build --path examples/code-editor --release',
    methodology: '200 native one-character replacements per dataset after 30 warmups; locations rotate beginning/middle/end. beforeinput-to-input measures synchronous processing. beforeinput-to-rAF measures the next pre-paint callback, not actual displayed pixels. Chromium EventTiming includes next paint, rounded to 8 ms, entries below 16 ms are censored; missing entries are conservatively assigned 16 ms. Long tasks overlap the input-to-rAF interval. Browser automation runs serially.',
    datasets: [],
  };
  const pkg = new URL('../../examples/code-editor/pkg/', import.meta.url);
  const indexHtml = await fs.readFile(new URL('index.html', pkg), 'utf8');
  const hash = indexHtml.match(/code_editor_example\.([a-f0-9]+)\.js/)[1];
  for (const name of await fs.readdir(pkg)) if (name === `code_editor_example_bg.${hash}.wasm`) {
    const data = await fs.readFile(new URL(name, pkg));
    report.bundle = {file: name, wasmBytes: data.length, gzipBytes: gzipSync(data).length, brotliBytes: brotliCompressSync(data).length};
  }
  await page.evaluate(selector => {
    const el = document.querySelector(selector);
    window.editPerf = {samples: [], events: [], longTasks: [], pending: null};
    new PerformanceObserver(list => editPerf.events.push(...list.getEntries().map(e => ({start: e.startTime, duration: e.duration, name: e.name})))).observe({type: 'event', durationThreshold: 16});
    new PerformanceObserver(list => editPerf.longTasks.push(...list.getEntries().map(e => ({start: e.startTime, duration: e.duration})))).observe({type: 'longtask'});
    el.addEventListener('beforeinput', () => {
      editPerf.pending = {start: performance.now(), mutations: 0, changedLines: new Set()};
    }, true);
    new MutationObserver(records => {
      if (!editPerf.pending) return;
      editPerf.pending.mutations += records.length;
      for (const record of records) {
        const node = record.target.nodeType === 1 ? record.target : record.target.parentElement;
        const line = node?.closest?.('[data-pine-code-line]');
        if (line) editPerf.pending.changedLines.add(line.dataset.pineCodeLine);
      }
    }).observe(el, {childList: true, characterData: true, subtree: true});
    el.addEventListener('input', () => {
      const sample = editPerf.pending;
      if (!sample) return;
      sample.syncMs = performance.now() - sample.start;
      requestAnimationFrame(() => {
        sample.end = performance.now();
        sample.prePaintMs = sample.end - sample.start;
        sample.changedLines = sample.changedLines.size;
        editPerf.samples.push(sample);
        if (editPerf.pending === sample) editPerf.pending = null;
      });
    });
  }, surface);
  const cdp = await page.context().newCDPSession(page);
  const output = info.outputPath('code-editor-performance.json');
  for (const [name, text] of [
    ['10 KiB', sized(10 * 1024)], ['64 KiB', sized(64 * 1024)], ['100 KiB', sized(100 * 1024)],
    ['256 KiB', sized(256 * 1024)], ['10,000 lines', 'let x = 1;\n'.repeat(9999) + 'x'],
    ['single 32 KiB line', 'x'.repeat(32 * 1024)],
  ]) {
    expect((await command(page, 'load', text)).Ok).toBeTruthy();
    await command(page, 'focus'); await settled(page);
    const first = text.indexOf('x'), middle = text.indexOf('x', Math.floor(text.length / 2)), last = text.lastIndexOf('x');
    const positions = [first, middle, last];
    for (let i = 0; i < 230; i++) {
      const at = positions[i % 3];
      await command(page, 'selection', '', at, at + 1);
      if (i === 30) await page.evaluate(() => { editPerf.samples = []; editPerf.events = []; editPerf.longTasks = []; window.codeExample.clear_editor_probes(); });
      const before = await page.evaluate(() => editPerf.samples.length);
      await page.keyboard.type(i % 2 ? 'x' : 'y');
      await page.waitForFunction(n => editPerf.samples.length > n, before);
    }
    // EventTiming may be delivered at a later rendering opportunity.
    await settled(page);
    await page.waitForTimeout(100);
    const measurements = await page.evaluate(() => {
      const p = editPerf;
      return {samples: p.samples, events: p.events, longTasks: p.longTasks, metrics: JSON.parse(window.codeExample.editor_metrics()).Ok, wasmMemoryBytes: window.codeExampleWasm.memory.buffer.byteLength};
    });
    const durations = measurements.samples.map(sample => {
      const event = measurements.events.filter(e => e.name === 'keydown' && e.start <= sample.start && e.start > sample.start - 100).at(-1);
      return event?.duration ?? 16;
    });
    const longTasks = measurements.longTasks.filter(task => measurements.samples.some(s => task.start < s.end && task.start + task.duration > s.start));
    expect(measurements.samples).toHaveLength(200);
    expect((await snap(page)).text.length).toBe(text.length);
    expect(measurements.metrics.last_read_lines).toBe(1);
    expect(measurements.metrics.history_bytes).toBeLessThanOrEqual(8 * 1024 * 1024);
    const commands = {};
    for (const [operation, value] of [['replace-all-x', 'z'], ['undo', '']]) {
      commands[operation] = await page.evaluate(({operation, value}) => {
        const s = JSON.parse(codeExample.inspect_editor()).snapshot.Ok;
        const start = performance.now();
        const outcome = JSON.parse(codeExample.editor_command(operation, value, 0, 0, BigInt(s.revision)));
        return {durationMs: performance.now() - start, outcome};
      }, {operation, value});
      expect(commands[operation].outcome.Ok).toBeTruthy();
    }
    await command(page, 'selection', '', first, first + 1);
    commands.paste = await page.locator(surface).evaluate(el => {
      const data = new DataTransfer(); data.setData('text/plain', 'P');
      const start = performance.now();
      el.dispatchEvent(new ClipboardEvent('paste', {bubbles: true, cancelable: true, clipboardData: data}));
      return {durationMs: performance.now() - start};
    });
    expect((await snap(page)).text[first]).toBe('P');
    await command(page, 'undo');
    commands.scroll = await page.locator('#source [data-pine-code-root]').evaluate(el => new Promise(resolve => {
      const start = performance.now(); el.scrollTop = el.scrollHeight; el.scrollLeft = el.scrollWidth;
      requestAnimationFrame(() => resolve({nextPrePaintMs: performance.now() - start, top: el.scrollTop, left: el.scrollLeft}));
    }));
    await settled(page);
    await cdp.send('HeapProfiler.collectGarbage');
    report.datasets.push({name, bytes: text.length, lines: text.split('\n').length,
      presentation: (await snap(page)).presentation_status,
      syncMs: summarize(measurements.samples.map(s => s.syncMs)),
      nextPrePaintMs: summarize(measurements.samples.map(s => s.prePaintMs)),
      eventToPaintMsUpperEstimate: summarize(durations), observedKeydowns: measurements.events.filter(e => e.name === 'keydown').length,
      editingLongTasks: longTasks, maxChangedLines: Math.max(...measurements.samples.map(s => s.changedLines)),
      metrics: measurements.metrics, commands, wasmMemoryBytes: measurements.wasmMemoryBytes,
      jsHeap: await cdp.send('Runtime.getHeapUsage'), dom: await cdp.send('Memory.getDOMCounters'),
    });
    await fs.writeFile(output, JSON.stringify(report, null, 2));
  }
  await fs.writeFile(output, JSON.stringify(report, null, 2));
  await info.attach('performance', {path: output, contentType: 'application/json'});
  // Keep measurements even when a performance gate fails.
  for (const result of report.datasets) {
    const gate = result.bytes <= 100 * 1024 ? 32 : 50;
    expect.soft(result.eventToPaintMsUpperEstimate.p95, `${result.name} input-to-paint p95`).toBeLessThanOrEqual(gate);
    if (result.name === '100 KiB') expect.soft(result.editingLongTasks, '100 KiB editing tasks above 50 ms').toHaveLength(0);
  }
});

test('100 mount/edit/unmount cycles dispose listeners, observers, handles, and frame work', async ({page}, info) => {
  test.skip(info.project.name !== 'code-editor-chromium', 'Heap diagnostics use Chromium CDP.');
  await page.addInitScript(() => {
    window.resourceCounts = {listeners: 0, observers: 0, frames: 0};
    const listeners = new WeakMap();
    const originalAdd = EventTarget.prototype.addEventListener;
    const originalRemove = EventTarget.prototype.removeEventListener;
    const tracked = (target, name) => (target === document && name === 'selectionchange') || target?.hasAttribute?.('data-pine-code-content');
    EventTarget.prototype.addEventListener = function(name, callback, options) {
      if (tracked(this, name)) {
        const entries = listeners.get(this) ?? []; const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
        if (!entries.some(e => e.name === name && e.callback === callback && e.capture === capture)) { entries.push({name, callback, capture}); resourceCounts.listeners++; }
        listeners.set(this, entries);
      }
      return originalAdd.call(this, name, callback, options);
    };
    EventTarget.prototype.removeEventListener = function(name, callback, options) {
      const entries = listeners.get(this); const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
      const at = entries?.findIndex(e => e.name === name && e.callback === callback && e.capture === capture) ?? -1;
      if (at >= 0) { entries.splice(at, 1); resourceCounts.listeners--; }
      return originalRemove.call(this, name, callback, options);
    };
    const OriginalObserver = MutationObserver;
    window.MutationObserver = class extends OriginalObserver {
      counted = false;
      observe(target, options) { if (target?.hasAttribute?.('data-pine-code-content') && !this.counted) { resourceCounts.observers++; this.counted = true; } return super.observe(target, options); }
      disconnect() { if (this.counted) { resourceCounts.observers--; this.counted = false; } return super.disconnect(); }
    };
    const raf = requestAnimationFrame, cancel = cancelAnimationFrame, frames = new Set();
    window.requestAnimationFrame = callback => { const id = raf(time => { frames.delete(id); resourceCounts.frames = frames.size; callback(time); }); frames.add(id); resourceCounts.frames = frames.size; return id; };
    window.cancelAnimationFrame = id => { frames.delete(id); resourceCounts.frames = frames.size; cancel(id); };
  });
  await bootstrap(page);
  const cdp = await page.context().newCDPSession(page);
  const sample = async () => {
    await page.evaluate(() => codeExample.clear_editor_probes());
    await settled(page); await cdp.send('HeapProfiler.collectGarbage');
    return {workers: page.workers().length, resources: await page.evaluate(() => resourceCounts), heap: await cdp.send('Runtime.getHeapUsage'), dom: await cdp.send('Memory.getDOMCounters'), wasmBytes: await page.evaluate(() => codeExampleWasm.memory.buffer.byteLength)};
  };
  const points = [];
  for (let cycle = 0; cycle <= 100; cycle++) {
    if (cycle % 20 === 0) points.push({cycle, ...await sample()});
    if (cycle === 100) break;
    await command(page, 'focus'); await page.keyboard.type('x');
    await page.getByRole('button', {name: 'Open / close editor', exact: true}).click();
    const closed = await page.evaluate(() => JSON.parse(codeExample.inspect_editor()));
    expect(closed.snapshot.Err).toBeTruthy();
    expect(closed.finalized.length).toBeGreaterThan(0);
    if (cycle % 20 !== 0) expect(closed.disposed_handles).toBeGreaterThan(0);
    await page.getByRole('button', {name: 'Open / close editor', exact: true}).click();
    await page.locator(surface).waitFor();
  }
  const cycleOutput = info.outputPath('mount-cycles.json');
  await fs.writeFile(cycleOutput, JSON.stringify(points, null, 2) + '\n');
  await info.attach('mount-cycles', {path: cycleOutput, contentType: 'application/json'});
  expect(points.every(point => point.workers === 2)).toBe(true);
  expect(points.at(-1).resources).toEqual(points[1].resources);
  expect(points.at(-1).dom.nodes).toBeLessThanOrEqual(points[1].dom.nodes + 20);
  expect(points.at(-1).heap.usedSize).toBeLessThan(points[1].heap.usedSize + 512 * 1024);
  expect(points.at(-1).wasmBytes).toBeLessThanOrEqual(points[1].wasmBytes + 65536);
});
