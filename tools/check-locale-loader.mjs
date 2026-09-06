// Run with node tools/check-locale-loader.mjs; no third-party dependencies.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const loader = readFileSync(new URL('../crates/pocopine-cli/assets/locale-loader.js', import.meta.url), 'utf8');
const progress = readFileSync(new URL('../crates/pocopine-cli/assets/loader.js', import.meta.url), 'utf8');
const parents = JSON.parse(readFileSync(new URL('../crates/pocopine-locale/data/cldr/parentLocales.json', import.meta.url))).supplemental.parentLocales.parentLocale;
const supported = ['en', 'fr', 'de', 'zh', 'zh-Hant', 'es-419'];
function fallback(tag) {
  while (tag) {
    if (supported.includes(tag)) return tag;
    tag = parents[tag] || tag.split('-').slice(0, -1).join('-');
  }
  return null;
}
const fallbacks = Object.fromEntries(Object.keys(parents).map(tag => [tag, fallback(tag)]));

function shell({ path = '/', cookie = '', languages = ['en'], routing = 'none', fail = false } = {}) {
  const nodes = [];
  function element(tag) {
    const classes = new Set();
    const node = { tag, style: {}, children: [], classList: {
      add(...values) { values.forEach(v => classes.add(v)); },
      remove(...values) { values.forEach(v => classes.delete(v)); }, contains(v) { return classes.has(v); }
    }, setAttribute() {}, addEventListener() {}, remove() { nodes.splice(nodes.indexOf(node), 1); },
    appendChild(child) { this.children.push(child); }, append(...children) { this.children.push(...children); },
    querySelector() { return this.children[0]; }
    };
    nodes.push(node); return node;
  }
  const manifest = { config: { default: 'en', routing }, catalogs: Object.fromEntries(supported.map(l => [l, '/pkg/locales/' + l + '.json'])) };
  const island = element('script'); island.id = 'pp-locale-manifest';
  island.textContent = JSON.stringify({ manifest, fallbacks });
  const splash = element('div'); splash.id = 'pp-splash';
  const requests = [];
  const context = { document: { cookie, head: element('head'), body: element('body'), documentElement: element('html'),
    createElement: element, getElementById(id) { return nodes.find(n => n.id === id); } },
    location: { pathname: path, reload() {} }, navigator: { languages },
    fetch(url) { requests.push(url); return fail ? Promise.reject(new Error('offline')) : Promise.resolve({ ok: true, arrayBuffer: async () => new ArrayBuffer(4) }); },
    setTimeout() {}, clearTimeout() {}, setInterval() {}, clearInterval() {}, requestAnimationFrame() {}
  };
  context.window = context;
  runInNewContext(loader, context);
  return { context, requests, splash };
}

for (const [options, expected] of [
  [{ languages: ['fr-CA'] }, 'fr'],
  [{ languages: ['es-AR'] }, 'es-419'],
  [{ languages: ['zh-Hant-TW'] }, 'zh-Hant'],
  [{ languages: ['en-u-nu-arab', 'DE'] }, 'de'],
  [{ cookie: 'pocopine_locale=fr', languages: ['de'] }, 'fr'],
  [{ path: '/de/account', cookie: 'pocopine_locale=fr', routing: 'prefix-all' }, 'de'],
  [{ path: '/de/account', languages: ['fr'], routing: 'none' }, 'fr'],
  [{ languages: ['ja'] }, 'en'],
]) {
  const { context, requests } = shell(options);
  assert.equal(context.__pocopineLocale.selected, expected);
  assert.deepEqual(requests, ['/pkg/locales/' + expected + '.json'], 'catalog starts before wasm');
  await context.__pocopineLocale.load(requests[0]);
  assert.equal(requests.length, 1, 'wasm reuses the in-flight preload');
}
const { context, splash } = shell();
runInNewContext(progress, context);
context.__pocopineProgress.ready();
assert.equal(splash.classList.contains('pp-splash--done'), false);
context.__pocopineLocale.markReady();
assert.equal(splash.classList.contains('pp-splash--done'), true);
const failed = shell({ fail: true });
await new Promise(resolve => setImmediate(resolve));
assert.ok(failed.context.document.getElementById('pp-locale-error'));
assert.equal(failed.context.__pocopineLocale.ready, false);
console.log('locale loader selection, parallel request reuse, boot gate and visible failure passed');
