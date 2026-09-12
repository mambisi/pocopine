import { expect, test } from '@playwright/test';

const host = 'pine-rich-text-root[runtime="document"]';
const surface = `${host} .pine-rich-text`;
const paragraph = text => ({type: 'paragraph', content: text ? [{type: 'text', text}] : []});
const frame = page => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));

test.beforeEach(async ({page, browser}, info) => {
  expect(browser.browserType().name()).toBe(info.project.use.defaultBrowserType);
  await page.goto('/');
  await expect(page.locator(`${surface} > p`).first()).toBeVisible();
  info.annotations.push({type: 'browser-version', description: browser.version()});
  info.annotations.push({type: 'browser-engine', description: browser.browserType().name()});
  info.annotations.push({type: 'bundle', description: await page.evaluate(() =>
    performance.getEntriesByType('resource').map(entry => new URL(entry.name).pathname)
      .find(path => /\/richtext\.[a-f0-9]+\.js$/.test(path)) ?? 'unknown')});
  await page.evaluate(selector => {
    const editor = document.querySelector(selector);
    const content = editor.querySelector('.pine-rich-text');
    const plain = value => value instanceof Map ? Object.fromEntries([...value].map(([k,v]) => [k,plain(v)]))
      : Array.isArray(value) ? value.map(plain) : value && typeof value === 'object'
        ? Object.fromEntries(Object.entries(value).map(([k,v]) => [k,plain(v)])) : value;
    window.nativeTest = {
      content,
      command(detail) { editor.dispatchEvent(new CustomEvent('pine:richtext:command', {bubbles: true, detail})); },
      snapshot() {
        let result;
        editor.addEventListener('pine:richtext:export-state-result', event => { result = plain(event.detail); }, {once: true});
        editor.dispatchEvent(new CustomEvent('pine:richtext:export-state', {bubbles: true}));
        return result;
      },
      text() { const walk = node => node.text ?? (node.content ?? []).map(walk).join(''); return walk(this.snapshot().doc); },
      caret(node, offset) { content.focus(); getSelection().setBaseAndExtent(node, offset, node, offset); },
      before(node, from, to, data, {cancelable = false, target = true, inputType = 'insertReplacementText', isComposing = false} = {}) {
        const event = new InputEvent('beforeinput', {bubbles: true, cancelable, inputType, data, isComposing});
        if (target) Object.defineProperty(event, 'getTargetRanges', {value: () => [new StaticRange({startContainer: node, startOffset: from, endContainer: node, endOffset: to})]});
        content.dispatchEvent(event);
        return event.defaultPrevented;
      },
      input(inputType = 'insertReplacementText', isComposing = false) {
        content.dispatchEvent(new InputEvent('input', {bubbles: true, inputType, isComposing}));
      },
      composition(kind) { content.dispatchEvent(new CompositionEvent(kind, {bubbles: true})); },
    };
  }, host);
});

async function seed(page, blocks) {
  await page.evaluate(content => nativeTest.command({kind: 'replace_state', doc: {
    doc: {type: 'doc', content}, selection: null, stored_marks: null, plugin_state: {},
  }}), blocks);
  await frame(page);
  await expect(page.locator(`${host} [role="alert"]`)).toHaveCount(0);
}

async function expectText(page, text) {
  await expect.poll(() => page.evaluate(() => nativeTest.text())).toBe(text);
  await expect(page.locator(surface)).toHaveText(text);
}

test('non-cancelable autocomplete commits once, survives rendering, and undoes', async ({page}) => {
  await seed(page, [paragraph('teh')]);
  const before = await page.evaluate(() => {
    const text = nativeTest.content.querySelector('p').firstChild;
    nativeTest.caret(text, 3);
    const prevented = nativeTest.before(text, 0, 3, 'the');
    const pending = nativeTest.text();
    text.nodeValue = 'the';
    nativeTest.caret(text, 3);
    nativeTest.input();
    return {prevented, pending};
  });
  expect(before).toEqual({prevented: false, pending: 'teh'});
  await expectText(page, 'the');
  await page.evaluate(() => nativeTest.command({kind: 'undo'}));
  await expectText(page, 'teh');
  await page.evaluate(() => nativeTest.command({kind: 'redo'}));
  await expectText(page, 'the');
  await page.keyboard.type('!');
  await expectText(page, 'the!');
});

test('autocomplete without beforeinput reads actual Unicode text and final caret', async ({page}) => {
  await seed(page, [paragraph('😀 teh end')]);
  await page.evaluate(() => {
    const text = nativeTest.content.querySelector('p').firstChild;
    text.nodeValue = '😀 the end';
    nativeTest.caret(text, 6);
    nativeTest.input();
  });
  await expectText(page, '😀 the end');
  await page.keyboard.type('!');
  await expectText(page, '😀 the! end');
});

test('replacement without target ranges waits for native text instead of inserting at caret', async ({page}) => {
  await seed(page, [paragraph('hel tail')]);
  const before = await page.evaluate(() => {
    const text = nativeTest.content.querySelector('p').firstChild;
    nativeTest.caret(text, 3);
    const prevented = nativeTest.before(text, 0, 3, 'hello', {cancelable: true, target: false});
    const pending = nativeTest.text();
    text.nodeValue = 'hello tail';
    nativeTest.caret(text, 5);
    nativeTest.input();
    return {prevented, pending};
  });
  expect(before).toEqual({prevented: false, pending: 'hel tail'});
  await expectText(page, 'hello tail');
});

test('cancelable replacement with target ranges stays a model transaction', async ({page}) => {
  await seed(page, [paragraph('teh tail')]);
  const prevented = await page.evaluate(() => {
    const text = nativeTest.content.querySelector('p').firstChild;
    nativeTest.caret(text, 8);
    return nativeTest.before(text, 0, 3, 'the', {cancelable: true});
  });
  expect(prevented).toBe(true);
  await expectText(page, 'the tail');
  await page.keyboard.type('!');
  await expectText(page, 'the! tail');
});

test('autocomplete with an unreadable event payload uses native readback', async ({page}) => {
  await seed(page, [paragraph('hel')]);
  const before = await page.evaluate(() => {
    const text = nativeTest.content.querySelector('p').firstChild;
    nativeTest.caret(text, 3);
    const event = new InputEvent('beforeinput', {bubbles: true, cancelable: true, inputType: 'insertReplacementText', dataTransfer: new DataTransfer()});
    Object.defineProperty(event, 'getTargetRanges', {value: () => [new StaticRange({startContainer: text, startOffset: 0, endContainer: text, endOffset: 3})]});
    nativeTest.content.dispatchEvent(event);
    const pending = nativeTest.text();
    text.nodeValue = 'hello';
    nativeTest.caret(text, 5);
    nativeTest.input();
    return {prevented: event.defaultPrevented, pending};
  });
  expect(before).toEqual({prevented: false, pending: 'hel'});
  await expectText(page, 'hello');
});

for (const finalAfterEnd of [false, true]) {
  test(`composition with final input ${finalAfterEnd ? 'after' : 'before'} compositionend preserves DOM and history`, async ({page}) => {
    await seed(page, [paragraph('ni')]);
    const during = await page.evaluate(finalAfterEnd => {
      const text = nativeTest.content.querySelector('p').firstChild;
      nativeTest.caret(text, 2);
      nativeTest.composition('compositionstart');
      const prevented = nativeTest.before(text, 0, 2, '你', {inputType: 'insertText', cancelable: true, isComposing: true});
      text.nodeValue = '你';
      nativeTest.caret(text, 1);
      nativeTest.input('insertCompositionText', true);
      const pending = nativeTest.text();
      const key = new KeyboardEvent('keydown', {key: 'Enter', keyCode: 229, isComposing: true, bubbles: true, cancelable: true});
      nativeTest.content.dispatchEvent(key);
      if (finalAfterEnd) nativeTest.composition('compositionend');
      text.nodeValue = '你好';
      nativeTest.caret(text, 2);
      nativeTest.input('insertText');
      if (!finalAfterEnd) nativeTest.composition('compositionend');
      return {prevented, pending, keyPrevented: key.defaultPrevented};
    }, finalAfterEnd);
    expect(during).toEqual({prevented: false, pending: 'ni', keyPrevented: false});
    await expectText(page, '你好');
    await page.evaluate(() => nativeTest.command({kind: 'undo'}));
    await expectText(page, 'ni');
    await page.evaluate(() => nativeTest.command({kind: 'redo'}));
    await expectText(page, '你好');
    await page.keyboard.type('!');
    await expectText(page, '你好!');
  });
}

test('composition deletion can empty a paragraph without resurrecting its text', async ({page}) => {
  await seed(page, [paragraph('hello')]);
  await page.evaluate(() => {
    const paragraph = nativeTest.content.querySelector('p');
    nativeTest.caret(paragraph.firstChild, 5);
    nativeTest.composition('compositionstart');
    paragraph.replaceChildren(document.createElement('br'));
    nativeTest.caret(paragraph, 0);
    nativeTest.composition('compositionend');
  });
  await expectText(page, '');
  await page.keyboard.type('next');
  await expectText(page, 'next');
});

test('native correction retains marks, inline breaks, and surrounding text', async ({page}) => {
  await seed(page, [{type: 'paragraph', content: [
    {type: 'text', text: 'a '}, {type: 'text', text: 'teh', marks: [{type: 'strong'}]},
    {type: 'hard_break', leaf: true}, {type: 'text', text: ' tail'},
  ]}]);
  await page.evaluate(() => {
    const text = nativeTest.content.querySelector('strong').firstChild;
    window.savedBreak = nativeTest.content.querySelector('br');
    nativeTest.caret(text, 3);
    nativeTest.before(text, 0, 3, 'their');
    text.nodeValue = 'their';
    nativeTest.caret(text, 5);
    nativeTest.input();
  });
  await expectText(page, 'a their tail');
  await expect(page.locator(`${surface} strong`)).toHaveText('their');
  expect(await page.evaluate(() => savedBreak === nativeTest.content.querySelector('br'))).toBe(true);
  expect(await page.evaluate(() => nativeTest.snapshot().doc.content[0].content)).toEqual([
    {type: 'text', text: 'a '}, {type: 'text', text: 'their', marks: [{type: 'strong'}]},
    {type: 'hard_break', leaf: true}, {type: 'text', text: ' tail'},
  ]);
});

test('a canceled composition resumes a document replacement deferred during composition', async ({page}) => {
  await seed(page, [paragraph('old')]);
  await page.evaluate(() => {
    const text = nativeTest.content.querySelector('p').firstChild;
    nativeTest.caret(text, 3);
    nativeTest.composition('compositionstart');
    nativeTest.command({kind: 'replace_state', doc: {doc: {type: 'doc', content: [{type: 'paragraph', content: [{type: 'text', text: 'external'}]}]}, selection: null, stored_marks: null, plugin_state: {}}});
  });
  await frame(page);
  await expect(page.locator(surface)).toHaveText('old');
  await page.evaluate(() => nativeTest.composition('compositionend'));
  await expectText(page, 'external');
});

test('unrepresentable native structure is repaired instead of remaining out of sync', async ({page}) => {
  await seed(page, [paragraph('saved')]);
  await page.evaluate(() => {
    const paragraph = nativeTest.content.querySelector('p');
    nativeTest.caret(paragraph.firstChild, 5);
    nativeTest.before(paragraph.firstChild, 0, 5, null);
    paragraph.innerHTML = '<div>unsupported structure</div>';
    nativeTest.caret(paragraph.firstChild.firstChild, 21);
    nativeTest.input();
  });
  await expectText(page, 'saved');
});
