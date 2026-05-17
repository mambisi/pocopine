import { expect, test } from '@playwright/test';

function collectRichTextDebug(page) {
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
    if (marker !== 'pine-richtext:json') return;

    try {
      const raw = await args[1].jsonValue();
      events.push(JSON.parse(raw));
    } catch {
      // Ignore unrelated console logs that happen to match the arity.
    }
  });
  return events;
}

async function selectText(page, needle) {
  await page.evaluate((text) => {
    const root = document.querySelector('pine-rich-text-root');
    if (!root) throw new Error('missing richtext root');

    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      const start = node.textContent.indexOf(text);
      if (start >= 0) {
        const range = document.createRange();
        range.setStart(node, start);
        range.setEnd(node, start + text.length);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        return;
      }
      node = walker.nextNode();
    }
    throw new Error(`could not find text: ${text}`);
  }, needle);
}

async function selectFirstTwoTopLevelParagraphs(page) {
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root .pine-rich-text') ??
      document.querySelector('pine-rich-text-root');
    const paragraphs = [...surface.querySelectorAll(':scope > p')];
    const p1 = paragraphs[0];
    const p2 = paragraphs[1];
    if (!p1 || !p2) throw new Error('need at least two paragraphs');
    const t1 = p1.firstChild;
    const t2 = p2.firstChild;
    const range = document.createRange();
    range.setStart(t1, Math.max(0, t1.textContent.length - 3));
    range.setEnd(t2, Math.min(t2.textContent.length, 3));
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });
}

async function expectTaskItemInline(taskItem) {
  const rects = await taskItem.evaluate((item) => {
    const check = item.querySelector('.pine-task-item-check');
    const paragraph = item.querySelector('.pine-task-item-content p');
    if (!check || !paragraph) throw new Error('missing task item chrome');
    const checkboxRect = check.getBoundingClientRect();
    const paragraphRect = paragraph.getBoundingClientRect();
    return {
      checkbox: {
        x: checkboxRect.x,
        y: checkboxRect.y,
        width: checkboxRect.width,
        height: checkboxRect.height,
      },
      paragraph: {
        x: paragraphRect.x,
        y: paragraphRect.y,
        width: paragraphRect.width,
        height: paragraphRect.height,
      },
    };
  });

  expect(rects.checkbox.x).toBeLessThan(rects.paragraph.x);
  expect(Math.abs(rects.checkbox.y - rects.paragraph.y)).toBeLessThan(8);
}

async function expectTaskItemChromeHasNoTextNodes(taskItem) {
  const textNodes = await taskItem.evaluate((item) => {
    const root = item.querySelector('.pine-task-item');
    if (!root) throw new Error('missing task item root');
    return {
      host: [...item.childNodes]
        .filter((node) => node.nodeType === Node.TEXT_NODE)
        .map((node) => node.textContent),
      root: [...root.childNodes]
        .filter((node) => node.nodeType === Node.TEXT_NODE)
        .map((node) => node.textContent),
    };
  });

  expect(textNodes).toEqual({ host: [], root: [] });
}

async function expectTaskItemContentEditableBoundary(taskItem) {
  await expect(taskItem.locator('.pine-task-item')).toHaveAttribute('contenteditable', 'false');
  await expect(taskItem.locator('.pine-task-item-content')).toHaveAttribute('contenteditable', 'true');
}

async function expectTaskItemsTight(taskItems) {
  const gap = await taskItems.evaluateAll((items) => {
    if (items.length < 2) throw new Error('missing task items');
    const first = items[0].getBoundingClientRect();
    const second = items[1].getBoundingClientRect();
    return second.y - first.bottom;
  });

  expect(gap).toBeLessThan(8);
}

test('materializes task item checkboxes and toggles checked state', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const events = collectRichTextDebug(page);

  await page.goto('/');
  await expect(page.locator('pine-rich-text-root')).toBeVisible();
  await expect
    .poll(() => events.some((event) => event.debug_version === 'pine-richtext@0.1.0:debug-json-v1'))
    .toBe(true);

  const taskItems = page.locator('pine-rich-text-root pine-task-item');
  await expect(taskItems).toHaveCount(2);
  await expect(taskItems.nth(0)).toHaveAttribute('data-checked', 'true');
  await expect(taskItems.nth(1)).toHaveAttribute('data-checked', 'false');
  await expect(taskItems.nth(0).locator('.pine-task-item-check')).toBeVisible();
  await expect(taskItems.nth(1).locator('.pine-task-item-check')).toBeVisible();
  await expectTaskItemInline(taskItems.nth(0));
  await expectTaskItemInline(taskItems.nth(1));
  await expectTaskItemChromeHasNoTextNodes(taskItems.nth(0));
  await expectTaskItemChromeHasNoTextNodes(taskItems.nth(1));
  await expectTaskItemContentEditableBoundary(taskItems.nth(0));
  await expectTaskItemContentEditableBoundary(taskItems.nth(1));
  await expectTaskItemsTight(taskItems);

  events.length = 0;
  await taskItems.nth(1).evaluate((item) => {
    item.__pineSmokeHostToken = 'preserve-host';
    item.querySelector('.pine-task-item-check').__pineSmokeCheckToken = 'preserve-check';
  });
  await taskItems.nth(1).locator('.pine-task-item-check').click();
  await expect
    .poll(() => events.findLast((event) => event.event === 'watch.doc')?.payload?.patch)
    .toBe('node_attrs');
  await expect(taskItems.nth(1)).toHaveAttribute('data-checked', 'true');
  await expect
    .poll(() =>
      taskItems.nth(1).evaluate((item) => ({
        host: item.__pineSmokeHostToken,
        check: item.querySelector('.pine-task-item-check')?.__pineSmokeCheckToken,
      })),
    )
    .toEqual({ host: 'preserve-host', check: 'preserve-check' });
  await expect(taskItems.nth(1).locator('.pine-task-item-check')).toBeVisible();
  await expectTaskItemInline(taskItems.nth(1));
  await expectTaskItemChromeHasNoTextNodes(taskItems.nth(1));
  await expectTaskItemContentEditableBoundary(taskItems.nth(1));
  await expectTaskItemsTight(taskItems);

  events.length = 0;
  await taskItems.nth(1).locator('.pine-task-item-check').click();
  await expect
    .poll(() => events.findLast((event) => event.event === 'watch.doc')?.payload?.patch)
    .toBe('node_attrs');
  await expect(taskItems.nth(1)).toHaveAttribute('data-checked', 'false');
  await expect
    .poll(() =>
      taskItems.nth(1).evaluate((item) => ({
        host: item.__pineSmokeHostToken,
        check: item.querySelector('.pine-task-item-check')?.__pineSmokeCheckToken,
      })),
    )
    .toEqual({ host: 'preserve-host', check: 'preserve-check' });
  await expect(taskItems.nth(1).locator('.pine-task-item-check')).toBeVisible();
  await expectTaskItemInline(taskItems.nth(1));
  await expectTaskItemChromeHasNoTextNodes(taskItems.nth(1));
  await expectTaskItemContentEditableBoundary(taskItems.nth(1));
  await expectTaskItemsTight(taskItems);
  expect(errors).toEqual([]);
});

test('typed text survives an immediate format toolbar click', async ({ page }) => {
  // Repro for the parent-doc-staleness bug: pp-model propagation from
  // <pine-rich-text-root> back to <Editor> is deferred to tick::next.
  // If the toolbar reads self.doc between a keystroke and the flush,
  // its dispatch overwrites the just-typed character. The test types a
  // recognizable marker into the first paragraph, immediately clicks
  // Bold over a selection that contains the marker, and asserts that
  // both the marker AND the strong mark land in the surface.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root p[data-pos]');

  // Place the caret at the end of the first paragraph.
  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root p[data-pos="0"]');
    const text = p.firstChild;
    const range = document.createRange();
    range.setStart(text, text.textContent.length);
    range.setEnd(text, text.textContent.length);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    p.focus?.();
  });

  await page.keyboard.type('XYZ');
  // Select the just-typed "XYZ" range so toggle_mark has a non-empty
  // selection to operate on.
  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root p[data-pos="0"]');
    const text = p.firstChild;
    const range = document.createRange();
    const len = text.textContent.length;
    range.setStart(text, len - 3);
    range.setEnd(text, len);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });
  // Bold is the first toolbar button.
  await page.locator('.toolbar button').nth(0).click();

  // After the toolbar click, "XYZ" must still be in the doc and must
  // carry a <strong> wrapper (or otherwise be marked as strong).
  const firstParagraphHTML = await page.evaluate(() => {
    return document.querySelector('pine-rich-text-root p[data-pos="0"]')?.innerHTML ?? '';
  });
  expect(firstParagraphHTML).toContain('XYZ');
  expect(firstParagraphHTML).toMatch(/<strong>[^<]*XYZ/);
  expect(errors).toEqual([]);
});

test('Enter at end of paragraph creates a visible new paragraph', async ({ page }) => {
  // Repro for the "Enter makes the new line disappear until I press
  // Enter again" bug. Place the caret at the end of paragraph 1, press
  // Enter once, then assert: doc has two top-level paragraphs AND the
  // DOM has two `<p data-pos>` elements visible.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root p[data-pos="0"]');

  await page.locator('pine-rich-text-root p[data-pos]').nth(1).evaluate((paragraph) => {
    paragraph.__pineSmokeParagraphToken = 'preserve-suffix';
  });

  const paragraphCountBefore = await page.locator(
    'pine-rich-text-root > p, pine-rich-text-root .pine-rich-text > p',
  ).count();

  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root p[data-pos="0"]');
    const text = p.firstChild;
    const range = document.createRange();
    range.setStart(text, text.textContent.length);
    range.setEnd(text, text.textContent.length);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  await page.keyboard.press('Enter');
  // Type a marker into the new paragraph to make it visually identifiable.
  await page.keyboard.type('SECOND');

  const paragraphHTMLs = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root .pine-rich-text') ??
      document.querySelector('pine-rich-text-root');
    return [...surface.querySelectorAll(':scope > p')].map((el) => el.outerHTML);
  });
  expect(paragraphHTMLs.length).toBeGreaterThanOrEqual(paragraphCountBefore + 1);
  expect(paragraphHTMLs.join('\n')).toContain('SECOND');
  await expect
    .poll(() =>
      page.evaluate(() => {
        const surface =
          document.querySelector('pine-rich-text-root .pine-rich-text') ??
          document.querySelector('pine-rich-text-root');
        return [...surface.querySelectorAll(':scope > p')]
          .find((paragraph) =>
            paragraph.textContent.includes('Select some text and use the toolbar:'),
          )
          ?.__pineSmokeParagraphToken;
      }),
    )
    .toBe('preserve-suffix');
  expect(errors).toEqual([]);
});

test('toolbar bullet list creates one list item per selected paragraph', async ({ page }) => {
  // Toolbar list buttons use the list-specific command, not generic
  // wrap_in. Select the END of paragraph 1 through the START of
  // paragraph 2, click • List, and assert that each paragraph becomes
  // its own <li>.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root p[data-pos="0"]');

  await selectFirstTwoTopLevelParagraphs(page);
  // Bulleted list is the 7th toolbar button in the current layout
  // (B / I / { } / H1 / H2 / P / Quote / • List …).
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  const listItems = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root .pine-rich-text') ??
      document.querySelector('pine-rich-text-root');
    const list = [...surface.querySelectorAll(':scope > ul')]
      .find((ul) => !ul.classList.contains('task-list'));
    if (!list) return null;
    return [...list.children].map((item) => ({
      tag: item.tagName,
      paragraphCount: item.querySelectorAll(':scope > p').length,
      text: item.textContent,
    }));
  });
  expect(listItems).toEqual([
    { tag: 'LI', paragraphCount: 1, text: 'Hello, pine-richtext.' },
    {
      tag: 'LI',
      paragraphCount: 1,
      text: expect.stringContaining('Select some text and use the toolbar:'),
    },
  ]);
  expect(errors).toEqual([]);
});

test('toolbar ordered and checklist lists also create one item per selected paragraph', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^1\. List$/ }).click();

  const orderedItems = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root .pine-rich-text') ??
      document.querySelector('pine-rich-text-root');
    const list = surface.querySelector(':scope > ol');
    if (!list) return null;
    return {
      start: list.getAttribute('start'),
      items: [...list.children].map((item) => ({
        tag: item.tagName,
        paragraphCount: item.querySelectorAll(':scope > p').length,
        text: item.textContent,
      })),
    };
  });
  expect(orderedItems).toEqual({
    start: null,
    items: [
      { tag: 'LI', paragraphCount: 1, text: 'Hello, pine-richtext.' },
      {
        tag: 'LI',
        paragraphCount: 1,
        text: expect.stringContaining('Select some text and use the toolbar:'),
      },
    ],
  });

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^☑ List$/ }).click();

  const taskList = page.locator('pine-rich-text-root ul.task-list').first();
  await expect(taskList.locator('pine-task-item')).toHaveCount(2);
  await expect(taskList.locator('pine-task-item').nth(0)).toHaveAttribute('data-checked', 'false');
  await expect(taskList.locator('pine-task-item').nth(1)).toHaveAttribute('data-checked', 'false');
  await expect(taskList.locator('.pine-task-item-check').nth(0)).toBeVisible();
  await expect(taskList.locator('.pine-task-item-check').nth(1)).toBeVisible();
  await expectTaskItemChromeHasNoTextNodes(taskList.locator('pine-task-item').nth(0));
  await expectTaskItemChromeHasNoTextNodes(taskList.locator('pine-task-item').nth(1));
  const taskTexts = await taskList.locator('pine-task-item').evaluateAll((items) =>
    items.map((item) => item.textContent.trim()),
  );
  expect(taskTexts[0]).toBe('Hello, pine-richtext.');
  expect(taskTexts[1]).toContain('Select some text and use the toolbar:');
  expect(errors).toEqual([]);
});

test('italic mark toggle reconciles one subtree and preserves task checkboxes', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const events = collectRichTextDebug(page);

  await page.goto('/');
  const taskItems = page.locator('pine-rich-text-root pine-task-item');
  await expect(taskItems).toHaveCount(2);
  await expect(taskItems.nth(0).locator('.pine-task-item-check')).toBeVisible();
  await taskItems.nth(0).locator('.pine-task-item-check').evaluate((check) => {
    check.__pineSmokeCheckToken = 'preserve-check';
  });
  events.length = 0;

  await selectText(page, 'headings');
  await page.locator('.toolbar button').nth(1).click();

  await expect
    .poll(() => events.findLast((event) => event.event === 'watch.doc')?.payload?.patch)
    .toBe('reconciled');
  await expect
    .poll(() =>
      taskItems
        .nth(0)
        .locator('.pine-task-item-check')
        .evaluate((check) => check.__pineSmokeCheckToken),
    )
    .toBe('preserve-check');
  await expect(taskItems).toHaveCount(2);
  await expect(taskItems.nth(0).locator('.pine-task-item-check')).toBeVisible();
  await expect(taskItems.nth(1).locator('.pine-task-item-check')).toBeVisible();
  expect(errors).toEqual([]);
});

test('typing inside a task item preserves node-view chrome', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const events = collectRichTextDebug(page);

  await page.goto('/');
  const taskItems = page.locator('pine-rich-text-root pine-task-item');
  await expect(taskItems).toHaveCount(2);
  await taskItems.nth(1).evaluate((item) => {
    item.__pineSmokeHostToken = 'preserve-host';
    item.querySelector('.pine-task-item-check').__pineSmokeCheckToken = 'preserve-check';
  });
  events.length = 0;

  await taskItems.nth(1).locator('.pine-task-item-content p').click();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+ArrowRight' : 'End');
  await page.keyboard.type(' updated');

  await expect
    .poll(() => events.findLast((event) => event.event === 'watch.doc')?.payload?.patch)
    .toBe('reconciled');
  await expect(taskItems.nth(1).locator('.pine-task-item-content p')).toContainText(
    'Click the box to toggle this item updated',
  );
  await expect
    .poll(() =>
      taskItems.nth(1).evaluate((item) => ({
        host: item.__pineSmokeHostToken,
        check: item.querySelector('.pine-task-item-check')?.__pineSmokeCheckToken,
      })),
    )
    .toEqual({ host: 'preserve-host', check: 'preserve-check' });
  await expectTaskItemInline(taskItems.nth(1));
  await expectTaskItemChromeHasNoTextNodes(taskItems.nth(1));
  await expectTaskItemContentEditableBoundary(taskItems.nth(1));
  await expectTaskItemsTight(taskItems);
  expect(errors).toEqual([]);
});
