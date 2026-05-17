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
    const root = document.querySelector('pine-rich-text-root:not([runtime])');
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
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
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
  await expect(page.locator('pine-rich-text-root:not([runtime])')).toBeVisible();
  await expect
    .poll(() => events.some((event) => event.debug_version === 'pine-richtext@0.1.0:debug-json-v1'))
    .toBe(true);

  const taskItems = page.locator('pine-rich-text-root:not([runtime]) pine-task-item');
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
  // <pine-rich-text-root:not([runtime])> back to <Editor> is deferred to tick::next.
  // If the toolbar reads self.doc between a keystroke and the flush,
  // its dispatch overwrites the just-typed character. The test types a
  // recognizable marker into the first paragraph, immediately clicks
  // Bold over a selection that contains the marker, and asserts that
  // both the marker AND the strong mark land in the surface.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos]');

  // Place the caret at the end of the first paragraph.
  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
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
    const p = document.querySelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
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
    return document.querySelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]')?.innerHTML ?? '';
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
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');

  await page.locator('pine-rich-text-root:not([runtime]) p[data-pos]').nth(1).evaluate((paragraph) => {
    paragraph.__pineSmokeParagraphToken = 'preserve-suffix';
  });

  const paragraphCountBefore = await page.locator(
    'pine-rich-text-root:not([runtime]) > p, pine-rich-text-root:not([runtime]) .pine-rich-text > p',
  ).count();

  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
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
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    return [...surface.querySelectorAll(':scope > p')].map((el) => el.outerHTML);
  });
  expect(paragraphHTMLs.length).toBeGreaterThanOrEqual(paragraphCountBefore + 1);
  expect(paragraphHTMLs.join('\n')).toContain('SECOND');
  await expect
    .poll(() =>
      page.evaluate(() => {
        const surface =
          document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
          document.querySelector('pine-rich-text-root:not([runtime])');
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
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');

  await selectFirstTwoTopLevelParagraphs(page);
  // Bulleted list is the 7th toolbar button in the current layout
  // (B / I / { } / H1 / H2 / P / Quote / • List …).
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  const listItems = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
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
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^1\. List$/ }).click();

  const orderedItems = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
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
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^☑ List$/ }).click();

  const taskList = page.locator('pine-rich-text-root:not([runtime]) ul.task-list').first();
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

test('toolbar paragraph converts selected list items back into paragraphs', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const items = [...surface.querySelectorAll(':scope > ul:not(.task-list) > li')];
    if (items.length < 2) throw new Error('expected two bullet items');
    const first = items[0].querySelector('p').firstChild;
    const second = items[1].querySelector('p').firstChild;
    const range = document.createRange();
    range.setStart(first, 0);
    range.setEnd(second, Math.min(second.textContent.length, 3));
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  await page.locator('.toolbar button', { hasText: /^P$/ }).click();

  const result = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    return {
      topParagraphs: [...surface.querySelectorAll(':scope > p')].map((p) => p.textContent),
      bulletLists: [...surface.querySelectorAll(':scope > ul')]
        .filter((ul) => !ul.classList.contains('task-list'))
        .map((ul) => [...ul.children].map((li) => li.textContent.trim())),
    };
  });

  expect(result.topParagraphs[0]).toBe('Hello, pine-richtext.');
  expect(result.topParagraphs[1]).toContain('Select some text and use the toolbar:');
  expect(result.bulletLists).toEqual([]);
  expect(errors).toEqual([]);
});

test('Backspace at the first bullet unwraps that item without deleting the rest of the list', async ({
  page,
}) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const paragraph = surface.querySelector(':scope > ul:not(.task-list) > li:first-child p');
    if (!paragraph?.firstChild) throw new Error('missing first bullet paragraph');
    const range = document.createRange();
    range.setStart(paragraph.firstChild, 0);
    range.collapse(true);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  await page.keyboard.press('Backspace');

  const result = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const bullet = [...surface.querySelectorAll(':scope > ul')]
      .find((ul) => !ul.classList.contains('task-list'));
    return {
      topParagraphs: [...surface.querySelectorAll(':scope > p')].map((p) => p.textContent),
      bulletItems: bullet ? [...bullet.children].map((li) => li.textContent.trim()) : [],
      taskItems: [...surface.querySelectorAll(':scope > ul.task-list > pine-task-item')].map(
        (item) => item.textContent.trim(),
      ),
    };
  });

  expect(result.topParagraphs[0]).toBe('Hello, pine-richtext.');
  expect(result.bulletItems).toHaveLength(1);
  expect(result.bulletItems[0]).toContain('Select some text and use the toolbar:');
  expect(result.taskItems).toHaveLength(2);
  expect(errors).toEqual([]);
});

test('italic mark toggle reconciles one subtree and preserves task checkboxes', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const events = collectRichTextDebug(page);

  await page.goto('/');
  const taskItems = page.locator('pine-rich-text-root:not([runtime]) pine-task-item');
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
  const taskItems = page.locator('pine-rich-text-root:not([runtime]) pine-task-item');
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

test('extension-contributed `Custom` command reaches the surface', async ({ page }) => {
  // Phase 4: the open `pine:richtext:command` `{ kind: "custom", name,
  // args }` shape resolves through the registry, so an
  // extension-contributed `wrap_in_bullet_list` produces the same DOM
  // as the closed `kind: "wrap_in_list", ...` variant did before.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos]');

  // Select the same two-paragraph span the closed-variant test uses.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const paragraphs = [...surface.querySelectorAll(':scope > p')];
    const p1 = paragraphs[0];
    const p2 = paragraphs[1];
    if (!p1 || !p2) throw new Error('need at least two paragraphs');
    const range = document.createRange();
    range.setStart(p1.firstChild, 0);
    range.setEnd(p2.firstChild, p2.firstChild.textContent.length);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  // Dispatch the open-variant CustomEvent on the surface element.
  // Use `bubbles: true` and `composed: true` so the listener
  // registered on the surface fires regardless of whether the test
  // dispatches on the surface itself or a descendant.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const event = new CustomEvent('pine:richtext:command', {
      bubbles: true,
      composed: true,
      detail: {
        kind: 'custom',
        name: 'wrap_in_bullet_list',
        args: {},
      },
    });
    surface.dispatchEvent(event);
  });
  await page.waitForTimeout(150);

  const ulHTML = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    return [...surface.querySelectorAll(':scope > ul')]
      .map((ul) => ul.outerHTML)
      .filter((html) => !html.includes('class="task-list"'))[0];
  });
  expect(ulHTML).toBeTruthy();
  expect(ulHTML).toMatch(/<ul[^>]*>\s*<li[^>]*>[\s\S]*<\/li>\s*<li[^>]*>[\s\S]*<\/li>\s*<\/ul>/);
  expect(errors).toEqual([]);
});

test('TaskListExtension::with_node_view renders custom task-item element', async ({ page }) => {
  // Phase 4 C4: the demo registers `TaskListExtension::with_node_view::<PineTaskItem>()`,
  // which forwards the `pine-task-item` tag + `data-pine-richtext-content`
  // selector eagerly into the render::node_views registry. The
  // reconciler picks that up when rendering `task_item` nodes.
  // Asserting the seed doc's custom-element layout closes the
  // extension contract.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) pine-task-item');

  const items = page.locator('pine-rich-text-root:not([runtime]) pine-task-item');
  await expect(items).toHaveCount(2);
  // Every custom-element host must carry data-pos (so the reconciler
  // and selection bridge can find them) and the inner content host
  // (so the reconciler knows where to render inline content).
  for (let i = 0; i < 2; i += 1) {
    await expect(items.nth(i)).toHaveAttribute('data-pos', /\d+/);
    await expect(items.nth(i).locator('[data-pine-richtext-content]')).toBeVisible();
  }
  expect(errors).toEqual([]);
});

test('clicking ordered list inside a bullet list converts in place without freezing', async ({ page }) => {
  // Pre-fix this clicked-button took ~4 seconds because wrap_in_list
  // pushed find_wrapping's BFS to depth 6 trying to find a way to wrap
  // already-inside-a-list selection. The new fast path detects the
  // ancestor list and swaps its type via replace_with.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');

  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();
  // Selection should be inside the new bullet list now. Drop it into
  // the first li explicitly so the conversion test starts from a
  // known cursor.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const li = surface.querySelector(':scope > ul > li');
    const p = li.querySelector('p');
    const range = document.createRange();
    range.setStart(p.firstChild, 0);
    range.setEnd(p.firstChild, 2);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  const started = Date.now();
  await page.locator('.toolbar button', { hasText: /^1\. List$/ }).click({ timeout: 1500 });
  const elapsed = Date.now() - started;
  expect(elapsed).toBeLessThan(1500);

  const result = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const list = [...surface.querySelectorAll(':scope > ol, :scope > ul')]
      .find((el) => !el.classList.contains('task-list'));
    return {
      tag: list?.tagName,
      childCount: list?.children.length,
      texts: list ? [...list.children].map((li) => li.textContent.trim()) : null,
    };
  });
  expect(result.tag).toBe('OL');
  expect(result.childCount).toBe(2);
  expect(result.texts?.[0]).toContain('Hello, pine-richtext.');
  expect(result.texts?.[1]).toContain('Select some text');
  expect(errors).toEqual([]);
});

test('bullet → task → bullet round-trips through the conversion contract', async ({ page }) => {
  // Bullet and task lists have different *item* types (`list_item` vs
  // `task_item`), so the conversion contract has to rebuild each
  // item with the target type — not just swap the wrapper. The
  // pre-conversion-contract code couldn't see task_list as a
  // "list-shaped" ancestor when the target was bullet_list (item
  // type mismatch), and walked the slow BFS again. This regression
  // test asserts the round-trip works AND stays under 1500ms each
  // way.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();
  await page.waitForTimeout(150);

  // Drop cursor into the first new bullet item.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const li = surface.querySelector(':scope > ul > li');
    const p = li.querySelector('p');
    const range = document.createRange();
    range.setStart(p.firstChild, 0);
    range.setEnd(p.firstChild, 2);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  // bullet → task
  let started = Date.now();
  await page.locator('.toolbar button', { hasText: /^☑ List$/ }).click({ timeout: 1500 });
  expect(Date.now() - started).toBeLessThan(1500);

  const afterTask = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const taskLists = [...surface.querySelectorAll(':scope > ul.task-list')];
    return taskLists.map((ul) => ({
      itemCount: ul.querySelectorAll(':scope > pine-task-item').length,
      texts: [...ul.querySelectorAll(':scope > pine-task-item')].map((item) =>
        item.textContent.trim(),
      ),
    }));
  });
  // First task-list is from the seed doc; second should be the newly
  // converted one with paragraph 1+2.
  expect(afterTask.length).toBe(2);
  const newTask = afterTask.find((u) => u.texts.some((t) => t.includes('Hello, pine-richtext')));
  expect(newTask).toBeTruthy();
  expect(newTask.itemCount).toBe(2);

  // Cursor inside the freshly-converted task list.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const taskList = [...surface.querySelectorAll(':scope > ul.task-list')].find((ul) =>
      ul.textContent.includes('Hello, pine-richtext'),
    );
    const para = taskList.querySelector('pine-task-item p');
    const range = document.createRange();
    range.setStart(para.firstChild, 0);
    range.setEnd(para.firstChild, 2);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  // task → bullet
  started = Date.now();
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click({ timeout: 1500 });
  expect(Date.now() - started).toBeLessThan(1500);

  const afterBullet = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root:not([runtime]) .pine-rich-text') ??
      document.querySelector('pine-rich-text-root:not([runtime])');
    const bullet = [...surface.querySelectorAll(':scope > ul')].find(
      (ul) => !ul.classList.contains('task-list'),
    );
    return bullet
      ? {
          itemCount: bullet.querySelectorAll(':scope > li').length,
          texts: [...bullet.querySelectorAll(':scope > li')].map((li) => li.textContent.trim()),
        }
      : null;
  });
  expect(afterBullet).toBeTruthy();
  expect(afterBullet.itemCount).toBe(2);
  expect(afterBullet.texts[0]).toContain('Hello, pine-richtext.');
  expect(errors).toEqual([]);
});

test('two editors on one page carry different schemas (Phase 4b C4)', async ({ page }) => {
  // The demo now hosts two `<pine-rich-text-root>` mounts:
  //   1. Default runtime — kitchen-sink: paragraphs + lists + task list + headings + history
  //   2. `runtime="comment"` — minimal: only paragraph + basic marks, no headings, no lists,
  //      no task items, no history.
  // This test asserts the two surfaces materialize different initial docs AND that
  // commands forbidden by the comment runtime's schema (e.g. wrap_in_task_list) are silent
  // no-ops there — proving per-instance schema scoping end-to-end.
  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
  await page.waitForSelector('pine-rich-text-root[runtime="comment"] p[data-pos="0"]');

  // Doc editor: has the seeded paragraphs + a `ul.task-list`.
  const docHasTaskList = await page.evaluate(() => {
    const doc = document.querySelector('pine-rich-text-root:not([runtime])');
    return !!doc.querySelector('ul.task-list');
  });
  expect(docHasTaskList).toBe(true);

  // Comment editor: only ONE paragraph, no lists / headings / task items.
  const commentShape = await page.evaluate(() => {
    const comment = document.querySelector('pine-rich-text-root[runtime="comment"]');
    return {
      paragraphCount: comment.querySelectorAll('p').length,
      hasTaskList: !!comment.querySelector('ul.task-list'),
      hasBulletList: !!comment.querySelector('ul:not(.task-list)'),
      hasHeading: !!comment.querySelector('h1, h2, h3, h4, h5, h6'),
    };
  });
  expect(commentShape.paragraphCount).toBe(1);
  expect(commentShape.hasTaskList).toBe(false);
  expect(commentShape.hasBulletList).toBe(false);
  expect(commentShape.hasHeading).toBe(false);

  // Now dispatch unsupported commands directly at the comment surface. The
  // comment runtime's schema only knows `doc`/`paragraph`/`text` + marks, so
  // `wrap_in_task_list`, `wrap_in_blockquote`, `set_block_type{heading}` all
  // fail at schema lookup and the DOM stays unchanged. The listener lives on
  // the INNER `.pine-rich-text` div (not the custom element wrapper), so
  // target that directly.
  const commentHtmlBefore = await page
    .locator('pine-rich-text-root[runtime="comment"] .pine-rich-text')
    .innerHTML();
  await page.evaluate(() => {
    const commentSurface = document.querySelector(
      'pine-rich-text-root[runtime="comment"] .pine-rich-text',
    );
    const forbidden = [
      { kind: 'wrap_in_list', list_type: 'task_list', item_type: 'task_item', attrs: {} },
      { kind: 'wrap_in_list', list_type: 'bullet_list', item_type: 'list_item', attrs: {} },
      { kind: 'wrap_in', node_type: 'blockquote', attrs: {} },
      { kind: 'set_block_type', node_type: 'heading', attrs: { level: 1 } },
      { kind: 'set_block_type', node_type: 'code_block', attrs: {} },
    ];
    for (const detail of forbidden) {
      commentSurface.dispatchEvent(
        new CustomEvent('pine:richtext:command', { detail, bubbles: true }),
      );
    }
  });
  await page.waitForTimeout(150);
  const commentHtmlAfter = await page
    .locator('pine-rich-text-root[runtime="comment"] .pine-rich-text')
    .innerHTML();
  expect(commentHtmlAfter).toBe(commentHtmlBefore);
});

test('runtime-scoped custom command only fires on its own editor (Phase 4b C4)', async ({
  page,
}) => {
  // The demo's `CommentRuntimeExtension` contributes a single named command,
  // `comment_submit`, that inserts `✓submitted` into the doc. The extension is
  // only registered on the comment runtime — so the same
  // `{ kind: "custom", name: "comment_submit" }` event fires against the comment
  // editor (inserts text) but is a silent no-op against the doc editor (no such
  // named command in its runtime's table). Proves per-instance command scoping.
  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root:not([runtime]) p[data-pos="0"]');
  await page.waitForSelector('pine-rich-text-root[runtime="comment"] p[data-pos="0"]');

  // Snapshot the doc editor's HTML so we can confirm it doesn't change.
  const docHtmlBefore = await page.locator('pine-rich-text-root:not([runtime])').innerHTML();

  // Dispatch against the doc editor's inner surface — no `comment_submit` command in its runtime.
  await page.evaluate(() => {
    const docSurface = document.querySelector(
      'pine-rich-text-root:not([runtime]) .pine-rich-text',
    );
    docSurface.dispatchEvent(
      new CustomEvent('pine:richtext:command', {
        detail: { kind: 'custom', name: 'comment_submit', args: null },
        bubbles: true,
      }),
    );
  });
  await page.waitForTimeout(120);
  const docHtmlAfter = await page.locator('pine-rich-text-root:not([runtime])').innerHTML();
  expect(docHtmlAfter).toBe(docHtmlBefore);

  // Dispatch against the comment editor's inner surface — its runtime has the
  // command, so the sentinel string should land in the comment's paragraph.
  // The command uses `tr.insert_text` which operates on the live selection, so
  // we first place a caret inside the empty paragraph.
  await page.evaluate(() => {
    const commentSurface = document.querySelector(
      'pine-rich-text-root[runtime="comment"] .pine-rich-text',
    );
    const para = commentSurface.querySelector('p');
    const range = document.createRange();
    range.setStart(para, 0);
    range.setEnd(para, 0);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    commentSurface.dispatchEvent(
      new CustomEvent('pine:richtext:command', {
        detail: { kind: 'custom', name: 'comment_submit', args: null },
        bubbles: true,
      }),
    );
  });
  await page.waitForTimeout(120);
  const commentText = await page
    .locator('pine-rich-text-root[runtime="comment"]')
    .textContent();
  expect(commentText).toContain('✓submitted');
});
