import { test, expect } from '@playwright/test';

const content = '#source [data-pine-code-content]';
const seed = 'fn main() {\n    println!("Hello, 🦀");\n}\n';
const inspect = page => page.evaluate(() => JSON.parse(window.codeExample.inspect_editor()));
const snapshot = async page => (await inspect(page)).snapshot.Ok;
async function command(page, name, value = '', anchor = 0, head = anchor, expected) {
  expected ??= (await snapshot(page)).revision;
  return page.evaluate(({name, value, anchor, head, expected}) => JSON.parse(window.codeExample.editor_command(name, value, anchor, head, BigInt(expected))), {name, value, anchor, head, expected});
}
async function load(page, text) {
  expect((await command(page, 'load', text)).Ok).toBeTruthy();
  await command(page, 'focus');
}
async function frame(page) { await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))); }
const documentChanges = state => state.changes.filter(change => change.document_changed);

test.beforeEach(async ({page}) => {
  await page.goto('/');
  await page.locator('#source [data-pine-code-instance]').waitFor();
  await page.waitForFunction(() => Boolean(window.codeExample));
});

test('bound seed, independent editors, and accessible content surface', async ({page}) => {
  expect((await snapshot(page)).text).toBe(seed);
  await expect(page.getByRole('textbox', {name: 'Source', exact: true})).toHaveAttribute('aria-multiline', 'true');
  await page.locator('#second [data-pine-code-content]').click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type('!');
  expect((await snapshot(page)).text).toBe(seed);
  expect(documentChanges(await inspect(page))).toHaveLength(0);
  await page.getByRole('button', {name: 'Toggle read-only', exact: true}).click();
  expect((await snapshot(page)).text).toBe(seed);
});

test('native typing commits once per character and grouped undo uses Rust history', async ({page}) => {
  await load(page, '');
  const baseline = documentChanges(await inspect(page)).length;
  await page.keyboard.type('hello');
  expect((await snapshot(page)).text).toBe('hello');
  expect(documentChanges(await inspect(page)).length - baseline).toBe(5);
  await page.keyboard.press('ControlOrMeta+z');
  expect((await snapshot(page)).text).toBe('');
  await page.keyboard.press('ControlOrMeta+Shift+z');
  expect((await snapshot(page)).text).toBe('hello');
  await page.keyboard.press('Backspace');
  expect((await snapshot(page)).text).toBe('hell');
});

test('Enter and native joins preserve blank lines and the final newline', async ({page}) => {
  await load(page, '  a\n\n');
  await command(page, 'selection', '', 3);
  await page.keyboard.press('Enter');
  await page.keyboard.type('b');
  expect((await snapshot(page)).text).toBe('  a\n  b\n\n');
  await command(page, 'selection', '', 4);
  await page.keyboard.press('Backspace');
  expect((await snapshot(page)).text).toBe('  a  b\n\n');
  expect(await page.locator(`${content} > [data-pine-code-line]`).count()).toBe(3);
});

test('Unicode positions and backwards native selection survive syntax spans', async ({page}) => {
  await load(page, 'let 🦀 = "é";\n\n');
  await frame(page);
  expect(await page.locator(`${content} [data-token]`).count()).toBeGreaterThan(0);
  const text = (await snapshot(page)).text;
  let bytes = 0;
  for (const ch of text) {
    await command(page, 'selection', '', bytes);
    const native = await page.evaluate(selector => { const s = getSelection(); return { inside: document.querySelector(selector).contains(s.anchorNode), collapsed: s.isCollapsed }; }, content);
    expect(native).toEqual({inside: true, collapsed: true});
    expect((await snapshot(page)).selection.anchor).toBe(bytes);
    bytes += new TextEncoder().encode(ch).length;
  }
  await command(page, 'selection', '', 8, 4);
  expect(await page.evaluate(() => getSelection().toString())).toBe('🦀');
  expect((await snapshot(page)).selection).toEqual({anchor: 8, head: 4});
  await command(page, 'theme', 'dark');
  await frame(page);
  expect((await snapshot(page)).selection).toEqual({anchor: 8, head: 4});
});

test('plain clipboard paste normalizes line endings and copies no gutter', async ({page}) => {
  await load(page, '');
  await page.locator(content).evaluate(el => {
    const data = new DataTransfer(); data.setData('text/plain', '<b>literal</b>\r\n\t🦀\r'); data.setData('text/html', '<b>rich</b>');
    el.dispatchEvent(new ClipboardEvent('paste', {bubbles: true, cancelable: true, clipboardData: data}));
  });
  expect((await snapshot(page)).text).toBe('<b>literal</b>\n\t🦀\n');
  await expect(page.locator(`${content} b`)).toHaveCount(0);
  await page.keyboard.press('ControlOrMeta+a');
  const copied = await page.locator(content).evaluate(el => { const data = new DataTransfer(); el.dispatchEvent(new ClipboardEvent('copy', {bubbles: true, cancelable: true, clipboardData: data})); return data.getData('text/plain'); });
  expect(copied).toBe('<b>literal</b>\n\t🦀\n');
});

test('read-only retains native navigation and restores noncancelable mutations', async ({page}) => {
  await load(page, 'let value = "🦀";');
  await page.getByRole('button', {name: 'Toggle read-only', exact: true}).click();
  await expect(page.locator(content)).toHaveAttribute('aria-readonly', 'true');
  await expect(page.locator(content)).toHaveAttribute('contenteditable', 'true');
  await command(page, 'focus');
  const before = await snapshot(page);
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.press('Shift+ArrowLeft');
  expect((await snapshot(page)).selection.anchor).not.toBe((await snapshot(page)).selection.head);
  await page.keyboard.type('BAD'); await page.keyboard.press('Backspace'); await page.keyboard.press('ControlOrMeta+z');
  await page.locator(content).evaluate(el => { el.firstElementChild.textContent = 'noncancelable'; el.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText', data: 'x'})); });
  expect((await snapshot(page)).text).toBe(before.text);
  expect((await snapshot(page)).revision).toBe(before.revision);
  await expect(page.locator(content)).toHaveText(before.text);
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', {name: 'Save source', exact: true})).toBeFocused();
});

test('disabled rejects programmatic focus and pointer focus', async ({page}) => {
  await page.getByRole('button', {name: 'Toggle disabled', exact: true}).click();
  await expect(page.locator(content)).toHaveAttribute('contenteditable', 'false');
  expect((await command(page, 'focus')).Err).toBe('Disabled');
  await page.locator(content).click({force: true});
  await expect(page.locator(content)).not.toBeFocused();
});

test('Tab indentation is opt-in and Escape then Tab exits', async ({page}) => {
  await load(page, 'a');
  await command(page, 'tab-behavior', 'indent');
  await command(page, 'selection', '', 1);
  await page.keyboard.press('Tab');
  expect((await snapshot(page)).text).toBe('a   ');
  await page.keyboard.press('Escape'); await page.keyboard.press('Tab');
  await expect(page.getByRole('button', {name: 'Save source', exact: true})).toBeFocused();
});

test('find and replace use the live model, and replace all is one undo step', async ({page}) => {
  await load(page, 'apple apple');
  await page.getByLabel('Find', {exact: true}).fill('apple');
  await page.getByLabel('Replace', {exact: true}).fill('pear');
  await page.getByRole('button', {name: 'Next', exact: true}).click();
  expect((await snapshot(page)).selection).toEqual({anchor: 0, head: 5});
  await page.getByRole('button', {name: 'Replace all', exact: true}).click();
  expect((await snapshot(page)).text).toBe('pear pear');
  await command(page, 'undo');
  expect((await snapshot(page)).text).toBe('apple apple');
});

test('composition guards selection, save, focus, find, and recovery until terminal input', async ({page}) => {
  await load(page, 'a');
  await page.locator(content).evaluate(el => { el.dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true})); el.firstElementChild.textContent = 'aに'; });
  expect((await snapshot(page)).text).toBe('a');
  for (const action of ['selection', 'focus', 'find', 'recover']) expect((await command(page, action, 'a')).Err).toBe('CompositionActive');
  await page.getByRole('button', {name: 'Save source', exact: true}).click();
  await expect(page.locator('[data-demo-status]')).toHaveText('Finish composing before saving.');
  await page.locator(content).evaluate(el => { el.dispatchEvent(new CompositionEvent('compositionend', {bubbles: true, data: '日本'})); el.firstElementChild.textContent = 'a日本'; el.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText', data: '日本'})); });
  await frame(page);
  expect((await snapshot(page)).text).toBe('a日本');
  await command(page, 'undo'); expect((await snapshot(page)).text).toBe('a');
});

test('pending ordinary input finalizes while attached and old handles dispose', async ({page}) => {
  await load(page, 'old');
  await page.locator(content).evaluate(el => { el.firstElementChild.textContent = 'pending'; [...document.querySelectorAll('button')].find(button => button.textContent.includes('Open / close')).click(); });
  const state = await inspect(page);
  expect(state.finalized).toHaveLength(1);
  expect(state.finalized[0].committed.text).toBe('pending');
  expect(state.finalized[0].error).toBeNull();
  expect(state.disposed_handles).toBe(1);
  await page.getByRole('button', {name: 'Open / close editor', exact: true}).click();
  await page.locator('#source [data-pine-code-instance]').waitFor();
  expect((await snapshot(page)).text).toBe(seed);
  expect((await inspect(page)).disposed_handles).toBe(1);
});

test('interrupted composition is delivered separately, including an oversized draft', async ({page}) => {
  await load(page, 'saved');
  await page.locator(content).evaluate(el => { el.dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true})); el.firstElementChild.textContent = 'x'.repeat(270_000); [...document.querySelectorAll('button')].find(button => button.textContent.includes('Open / close')).click(); });
  const state = await inspect(page);
  expect(state.finalized[0].committed.text).toBe('saved');
  expect(state.finalized[0].committed.composing).toBe(false);
  expect(state.finalized[0].interrupted.text.length).toBe(270_000);
  expect(state.finalized[0].interrupted.error).toBe('SizeLimit');
  expect(state.disposed_handles).toBe(1);
});

test('failed rendering preserves accepted text and recovery never duplicates it', async ({page}) => {
  await load(page, 'a');
  await page.locator(content).evaluate(el => { window.removedSurface = el; el.remove(); });
  const outcome = await command(page, 'insert', 'X');
  expect(outcome.Ok.view_status.Failed).toBeTruthy();
  expect((await snapshot(page)).text).toBe('Xa');
  await page.evaluate(() => { window.removedSurface.textContent = 'untrusted'; });
  expect((await snapshot(page)).text).toBe('Xa');
  expect((await command(page, 'recover')).Ok.view_status).toBe('Ready');
  expect((await snapshot(page)).text).toBe('Xa');
  await command(page, 'undo'); expect((await snapshot(page)).text).toBe('a');
});

test('highlight fallback retains editable text and theme changes preserve nodes', async ({page}) => {
  await load(page, 'let x = 1;\nlet y = 2;'); await frame(page);
  await page.locator(content).evaluate(el => { window.retainedLine = el.children[1]; });
  await command(page, 'selection', '', 9); await page.keyboard.type('0'); await frame(page);
  expect(await page.locator(content).evaluate(el => el.children[1] === window.retainedLine)).toBe(true);
  await command(page, 'language', 'unknown'); await frame(page);
  expect((await snapshot(page)).presentation_status.PlainFallback.UnknownLanguage).toBe('unknown');
  await expect(page.locator(`${content} [data-token]`)).toHaveCount(0);
  await load(page, 'x'.repeat(9000)); await command(page, 'language', 'rust'); await frame(page);
  expect((await snapshot(page)).presentation_status.PlainFallback).toBe('LineBudget');
  await page.keyboard.press('ControlOrMeta+End'); await page.keyboard.type('!');
  expect((await snapshot(page)).text.length).toBe(9001);
});

test('repeated text groups at the actual caret and noncancelable native history is discarded', async ({page}) => {
  await load(page, 'aaaa');
  await command(page, 'selection', '', 2);
  await page.keyboard.type('aaa');
  await page.locator(content).evaluate(el => {
    el.dispatchEvent(new InputEvent('beforeinput', {bubbles: true, inputType: 'historyUndo'}));
    el.firstElementChild.textContent = 'browser history is not authoritative';
    el.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'historyUndo'}));
  });
  expect((await snapshot(page)).text).toBe('aaaa');
  expect((await snapshot(page)).selection).toEqual({anchor: 2, head: 2});
  await command(page, 'redo');
  expect((await snapshot(page)).text).toBe('aaaaaaa');
});

test('terminal composition followed immediately by Enter creates separate undo groups', async ({page}) => {
  await load(page, 'a');
  await page.locator(content).evaluate(el => {
    el.dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true}));
    el.firstElementChild.textContent = 'aé';
    getSelection().setPosition(el.firstElementChild.firstChild, 2);
    el.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText', data: 'é', isComposing: false}));
    el.dispatchEvent(new KeyboardEvent('keydown', {bubbles: true, cancelable: true, key: 'Enter'}));
  });
  await frame(page);
  expect((await snapshot(page)).text).toBe('aé\n');
  await command(page, 'undo'); expect((await snapshot(page)).text).toBe('aé');
  await command(page, 'undo'); expect((await snapshot(page)).text).toBe('a');
});

test('unsupported formatting is flattened without changing text or revision', async ({page}) => {
  await load(page, 'abc');
  const before = await snapshot(page);
  await page.locator(content).evaluate(el => {
    el.firstElementChild.innerHTML = '<b>abc</b>';
    el.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'formatBold'}));
  });
  expect((await snapshot(page)).text).toBe('abc');
  expect((await snapshot(page)).revision).toBe(before.revision);
  await expect(page.locator(`${content} b`)).toHaveCount(0);
});

test('external plaintext drops and internal moves use checked coordinates and one undo step', async ({page}) => {
  await load(page, 'abc def');
  await command(page, 'language', 'plain'); await frame(page);
  await command(page, 'selection', '', 0, 3);
  const moved = await page.locator(content).evaluate(el => {
    const data = new DataTransfer();
    el.dispatchEvent(new DragEvent('dragstart', {bubbles: true, cancelable: true, dataTransfer: data}));
    const r = document.createRange(); r.setStart(el.firstElementChild.firstChild, el.firstElementChild.firstChild.textContent.length); r.collapse(true);
    const rect = r.getBoundingClientRect();
    el.dispatchEvent(new DragEvent('drop', {bubbles: true, cancelable: true, dataTransfer: data, clientX: Math.round(rect.x), clientY: Math.round(rect.y + rect.height / 2)}));
    return data.getData('text/plain');
  });
  expect(moved).toBe('abc');
  expect((await snapshot(page)).text).toBe(' defabc');
  await command(page, 'undo'); expect((await snapshot(page)).text).toBe('abc def');
  await page.locator(content).evaluate(el => {
    const data = new DataTransfer(); data.setData('text/plain', '<b>\r\n'); data.setData('text/html', '<b>formatted</b>');
    const r = document.createRange(); r.setStart(el.firstElementChild.firstChild, 0); r.collapse(true);
    const rect = r.getBoundingClientRect();
    el.dispatchEvent(new DragEvent('drop', {bubbles: true, cancelable: true, dataTransfer: data, clientX: Math.round(rect.x), clientY: Math.round(rect.y + rect.height / 2)}));
  });
  expect((await snapshot(page)).text).toBe('<b>\nabc def');
  await expect(page.locator(`${content} b`)).toHaveCount(0);
});

test('same-text load advances the revision and reports the lifecycle boundary', async ({page}) => {
  await load(page, 'same');
  const before = await inspect(page);
  await load(page, 'same');
  const after = await inspect(page);
  expect(after.snapshot.Ok.revision).toBe(before.snapshot.Ok.revision + 1);
  expect(after.changes.filter(c => c.origin === 'Load')).toHaveLength(before.changes.filter(c => c.origin === 'Load').length + 1);
});

test('accessible names update and selection survives font, zoom, and forced colors', async ({page}) => {
  await load(page, 'let answer = 42;\n');
  await command(page, 'selection', '', 16, 4);
  await page.locator('#source').evaluate(el => { el.removeAttribute('aria-labelledby'); el.setAttribute('aria-label', 'Renamed source'); });
  await expect(page.getByRole('textbox', {name: 'Renamed source', exact: true})).toBeVisible();
  await page.locator('#source').evaluate(el => { el.style.zoom = '1.25'; el.style.setProperty('--font-mono', 'monospace'); });
  await command(page, 'theme', 'dark'); await frame(page);
  expect((await snapshot(page)).selection).toEqual({anchor: 16, head: 4});
  expect(await page.evaluate(() => getSelection().toString())).toBe('answer = 42;');
  await page.emulateMedia({forcedColors: 'active'}); await frame(page);
  expect((await snapshot(page)).selection).toEqual({anchor: 16, head: 4});
  const metrics = await page.locator(content).evaluate(el => {
    const token = el.querySelector('[data-token]');
    const text = getComputedStyle(el), style = getComputedStyle(token);
    return {font: style.fontFamily === text.fontFamily, size: style.fontSize === text.fontSize, lineHeight: style.lineHeight === text.lineHeight};
  });
  expect(metrics).toEqual({font: true, size: true, lineHeight: true});
});

test('read-only composition cannot become an interrupted recovery draft', async ({page}) => {
  await load(page, 'saved');
  await page.getByRole('button', {name: 'Toggle read-only', exact: true}).click();
  await page.locator(content).evaluate(el => {
    el.dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true}));
    el.firstElementChild.textContent = 'rejected draft';
    [...document.querySelectorAll('button')].find(b => b.textContent.includes('Open / close')).click();
  });
  const final = (await inspect(page)).finalized[0];
  expect(final.committed.text).toBe('saved');
  expect(final.committed.composing).toBe(false);
  expect(final.interrupted).toBeNull();
});

test('line limits reject native growth and joins atomically', async ({page}) => {
  const long = 'x'.repeat(32 * 1024);
  await load(page, long);
  await command(page, 'selection', '', long.length);
  const before = await snapshot(page);
  await page.keyboard.type('!');
  expect((await snapshot(page)).text).toBe(long);
  expect((await snapshot(page)).revision).toBe(before.revision);
  const split = 'x'.repeat(20_000) + '\n' + 'y'.repeat(20_000);
  await load(page, split);
  await command(page, 'selection', '', 20_001);
  await page.keyboard.press('Backspace');
  expect((await snapshot(page)).text).toBe(split);
  expect(await page.locator(`${content} > [data-pine-code-line]`).count()).toBe(2);
  expect((await command(page, 'load', 'z'.repeat(32 * 1024 + 1))).Err).toBe('SizeLimit');
  expect((await snapshot(page)).text).toBe(split);
});

test('typing after a keyword repairs native span boundaries even when token ranges stay equal', async ({page}) => {
  await load(page, '');
  for (const ch of 'let fu') { await page.keyboard.type(ch); await frame(page); }
  expect((await snapshot(page)).text).toBe('let fu');
  await expect(page.locator(`${content} [data-token=keyword]`)).toHaveText('let');
  expect((await snapshot(page)).selection).toEqual({anchor: 6, head: 6});
  await page.keyboard.type('n = "hello";'); await frame(page);
  expect((await snapshot(page)).text).toBe('let fun = "hello";');
  await expect(page.locator(`${content} [data-token=keyword]`)).toHaveText('let');
  await expect(page.locator(`${content} [data-token=string]`)).toHaveText('"hello"');
});
