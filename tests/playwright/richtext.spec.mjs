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
    const root = document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await expect(taskItem.locator('.pine-task-item-check')).toHaveAttribute(
    'contenteditable',
    'false',
  );
  await expect(taskItem).not.toHaveAttribute('contenteditable', 'false');
  await expect(taskItem.locator('.pine-task-item')).not.toHaveAttribute(
    'contenteditable',
    'false',
  );
  await expect(taskItem.locator('.pine-task-item-content')).not.toHaveAttribute(
    'contenteditable',
    'false',
  );
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
  await expect(page.locator('pine-rich-text-root[runtime="document"]')).toBeVisible();
  await expect
    .poll(() => events.some((event) => event.debug_version === 'pine-richtext@0.1.0:debug-json-v1'))
    .toBe(true);

  const taskItems = page.locator('pine-rich-text-root[runtime="document"] [data-pine-node-type="task_item"]');
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
  // <pine-rich-text-root[runtime="document"]> back to <Editor> is deferred to tick::next.
  // If the toolbar reads self.doc between a keystroke and the flush,
  // its dispatch overwrites the just-typed character. The test types a
  // recognizable marker into the first paragraph, immediately clicks
  // Bold over a selection that contains the marker, and asserts that
  // both the marker AND the strong mark land in the surface.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos]');

  // Place the caret at the end of the first paragraph.
  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
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
    const p = document.querySelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
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
    return document.querySelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]')?.innerHTML ?? '';
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await page.locator('pine-rich-text-root[runtime="document"] p[data-pos]').nth(1).evaluate((paragraph) => {
    paragraph.__pineSmokeParagraphToken = 'preserve-suffix';
  });

  const paragraphCountBefore = await page.locator(
    'pine-rich-text-root[runtime="document"] > p, pine-rich-text-root[runtime="document"] .pine-rich-text > p',
  ).count();

  await page.evaluate(() => {
    const p = document.querySelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    return [...surface.querySelectorAll(':scope > p')].map((el) => el.outerHTML);
  });
  expect(paragraphHTMLs.length).toBeGreaterThanOrEqual(paragraphCountBefore + 1);
  expect(paragraphHTMLs.join('\n')).toContain('SECOND');
  await expect
    .poll(() =>
      page.evaluate(() => {
        const surface =
          document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
          document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await selectFirstTwoTopLevelParagraphs(page);
  // Bulleted list is the 7th toolbar button in the current layout
  // (B / I / { } / H1 / H2 / P / Quote / • List …).
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  const listItems = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^1\. List$/ }).click();

  const orderedItems = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^☑ List$/ }).click();

  const taskList = page.locator('pine-rich-text-root[runtime="document"] ul.task-list').first();
  await expect(taskList.locator('[data-pine-node-type="task_item"]')).toHaveCount(2);
  await expect(taskList.locator('[data-pine-node-type="task_item"]').nth(0)).toHaveAttribute('data-checked', 'false');
  await expect(taskList.locator('[data-pine-node-type="task_item"]').nth(1)).toHaveAttribute('data-checked', 'false');
  await expect(taskList.locator('.pine-task-item-check').nth(0)).toBeVisible();
  await expect(taskList.locator('.pine-task-item-check').nth(1)).toBeVisible();
  await expectTaskItemChromeHasNoTextNodes(taskList.locator('[data-pine-node-type="task_item"]').nth(0));
  await expectTaskItemChromeHasNoTextNodes(taskList.locator('[data-pine-node-type="task_item"]').nth(1));
  const taskTexts = await taskList.locator('[data-pine-node-type="task_item"]').evaluateAll((items) =>
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();

  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    const bullet = [...surface.querySelectorAll(':scope > ul')]
      .find((ul) => !ul.classList.contains('task-list'));
    return {
      topParagraphs: [...surface.querySelectorAll(':scope > p')].map((p) => p.textContent),
      bulletItems: bullet ? [...bullet.children].map((li) => li.textContent.trim()) : [],
      taskItems: [...surface.querySelectorAll(':scope > ul.task-list > [data-pine-node-type="task_item"]')].map(
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
  const taskItems = page.locator('pine-rich-text-root[runtime="document"] [data-pine-node-type="task_item"]');
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
  const taskItems = page.locator('pine-rich-text-root[runtime="document"] [data-pine-node-type="task_item"]');
  await expect(taskItems).toHaveCount(2);
  await taskItems.nth(1).evaluate((item) => {
    item.__pineSmokeHostToken = 'preserve-host';
    item.querySelector('.pine-task-item-check').__pineSmokeCheckToken = 'preserve-check';
  });
  events.length = 0;

  await taskItems.nth(1).locator('.pine-task-item-content p').click();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+ArrowRight' : 'End');
  await page.keyboard.type(' updated');

  // Either `text` (plain-text inline fast path landed in commit 4) or
  // `reconciled` (older structural patch) preserves the surrounding
  // node-view chrome — what matters for this regression test is the
  // chrome assertions below, not which patch class the reconciler
  // picked.
  await expect
    .poll(() => events.findLast((event) => event.event === 'watch.doc')?.payload?.patch)
    .toMatch(/^(text|reconciled)$/);
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos]');

  // Select the same two-paragraph span the closed-variant test uses.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    return [...surface.querySelectorAll(':scope > ul')]
      .map((ul) => ul.outerHTML)
      .filter((html) => !html.includes('class="task-list"'))[0];
  });
  expect(ulHTML).toBeTruthy();
  expect(ulHTML).toMatch(/<ul[^>]*>\s*<li[^>]*>[\s\S]*<\/li>\s*<li[^>]*>[\s\S]*<\/li>\s*<\/ul>/);
  expect(errors).toEqual([]);
});

test('typed TaskListExtension mounts PineTaskItem on native task-item hosts', async ({ page }) => {
  // The named document runtime composes
  // `TaskListExtension::with_typed_node_view::<PineTaskItem>()`. The
  // semantic `TaskItemNode` supplies the native `<li>` host while the
  // component supplies chrome around its compiled owned-content outlet.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] [data-pine-node-type="task_item"]');

  const items = page.locator('pine-rich-text-root[runtime="document"] [data-pine-node-type="task_item"]');
  await expect(items).toHaveCount(2);
  // Every stable native host carries the semantic type and position; the
  // mounted component exposes its editor-owned child outlet without a
  // runtime selector marker.
  for (let i = 0; i < 2; i += 1) {
    await expect(items.nth(i)).toHaveJSProperty('tagName', 'LI');
    await expect(items.nth(i)).toHaveAttribute('data-pos', /\d+/);
    await expect(items.nth(i)).toHaveAttribute('data-pine-node-view', 'typed');
    await expect(items.nth(i).locator('.pine-task-item-content')).toBeVisible();
    await expect(items.nth(i).locator('[pp-owned-content]')).toHaveCount(0);
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();
  // Selection should be inside the new bullet list now. Drop it into
  // the first li explicitly so the conversion test starts from a
  // known cursor.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
  await selectFirstTwoTopLevelParagraphs(page);
  await page.locator('.toolbar button', { hasText: /^• List$/ }).click();
  await page.waitForTimeout(150);

  // Drop cursor into the first new bullet item.
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    const taskLists = [...surface.querySelectorAll(':scope > ul.task-list')];
    return taskLists.map((ul) => ({
      itemCount: ul.querySelectorAll(':scope > [data-pine-node-type="task_item"]').length,
      texts: [...ul.querySelectorAll(':scope > [data-pine-node-type="task_item"]')].map((item) =>
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    const taskList = [...surface.querySelectorAll(':scope > ul.task-list')].find((ul) =>
      ul.textContent.includes('Hello, pine-richtext'),
    );
    const para = taskList.querySelector('[data-pine-node-type="task_item"] p');
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
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
  await page.waitForSelector('pine-rich-text-root[runtime="comment"] p[data-pos="0"]');

  // Doc editor: has the seeded paragraphs + a `ul.task-list`.
  const docHasTaskList = await page.evaluate(() => {
    const doc = document.querySelector('pine-rich-text-root[runtime="document"]');
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
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');
  await page.waitForSelector('pine-rich-text-root[runtime="comment"] p[data-pos="0"]');

  // Snapshot the doc editor's HTML so we can confirm it doesn't change.
  const docHtmlBefore = await page.locator('pine-rich-text-root[runtime="document"]').innerHTML();

  // Dispatch against the doc editor's inner surface — no `comment_submit` command in its runtime.
  await page.evaluate(() => {
    const docSurface = document.querySelector(
      'pine-rich-text-root[runtime="document"] .pine-rich-text',
    );
    docSurface.dispatchEvent(
      new CustomEvent('pine:richtext:command', {
        detail: { kind: 'custom', name: 'comment_submit', args: null },
        bubbles: true,
      }),
    );
  });
  await page.waitForTimeout(120);
  const docHtmlAfter = await page.locator('pine-rich-text-root[runtime="document"]').innerHTML();
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

async function caretAtEndOfFirstParagraph(page) {
  await page.evaluate(() => {
    const p = document.querySelector(
      'pine-rich-text-root[runtime="document"] p[data-pos="0"]',
    );
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

async function caretAtStartOfFreshParagraphAfterFirst(page) {
  // Place caret at end of paragraph 1, press Enter to make a new empty
  // paragraph below it, and leave the caret at the start of that fresh
  // paragraph — the only place where `^# ` / `^* ` / `^> ` markdown
  // shortcuts can fire (the rules require the marker to be the
  // textblock's entire content).
  await caretAtEndOfFirstParagraph(page);
  await page.keyboard.press('Enter');
}

test('typing `--` triggers the em-dash smart-typography rule', async ({ page }) => {
  // Phase 5 C4: end-to-end coverage that
  // `SmartTypographyExtension`'s em-dash rule fires from the demo's
  // beforeinput → run_rules path. The rule pattern is `--$`: typing
  // the second `-` immediately after a `-` rewrites both characters
  // to `—` (U+2014).
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await caretAtEndOfFirstParagraph(page);
  await page.keyboard.type('--');

  const firstParagraphText = await page.evaluate(
    () =>
      document.querySelector(
        'pine-rich-text-root[runtime="document"] p[data-pos="0"]',
      )?.textContent ?? '',
  );
  expect(firstParagraphText).toContain('—');
  expect(firstParagraphText).not.toMatch(/--$/);
  expect(errors).toEqual([]);
});

test('typing `"hello"` triggers smart-quote rules in both directions', async ({ page }) => {
  // Phase 5 C4: open-quote rule fires at the start of a fresh word
  // (`(^|[\s{\[(<'"‘“])"$` matches `"` after whitespace or
  // SOT), close-quote rule fires when the prior char is wordy
  // (`"$` with a word-char lookbehind). Typed `"hello"` should
  // become `“hello”` (left + right double-quotation marks).
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await caretAtEndOfFirstParagraph(page);
  // The end of the seeded paragraph is wordy — to exercise open-quote,
  // type a space first so the buffer immediately before `"` is whitespace.
  await page.keyboard.type(' "hello"');

  const firstParagraphText = await page.evaluate(
    () =>
      document.querySelector(
        'pine-rich-text-root[runtime="document"] p[data-pos="0"]',
      )?.textContent ?? '',
  );
  expect(firstParagraphText).toContain('“hello”');
  expect(firstParagraphText).not.toContain('"hello"');
  expect(errors).toEqual([]);
});

test('typing `# ` at start of an empty paragraph converts it to an H1', async ({ page }) => {
  // Phase 5 C4: `MarkdownShortcutsExtension`'s heading rule
  // (`^(#{1,6})\s$`) converts the parent paragraph to a `heading`
  // node with `level` matching the `#` count. The textblock must be
  // empty before the trigger so the regex's anchor matches.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await caretAtStartOfFreshParagraphAfterFirst(page);
  await page.keyboard.type('# Heading');

  await expect
    .poll(async () =>
      page.evaluate(() => {
        const surface =
          document.querySelector(
            'pine-rich-text-root[runtime="document"] .pine-rich-text',
          ) ?? document.querySelector('pine-rich-text-root[runtime="document"]');
        return [...surface.querySelectorAll(':scope > h1')].map(
          (el) => el.textContent,
        );
      }),
    )
    .toContain('Heading');
  expect(errors).toEqual([]);
});

test('typing `* ` at start of an empty paragraph wraps it in a bullet list', async ({ page }) => {
  // Phase 5 C4: bullet-list rule (`^\s*([-+*])\s$`) wraps the
  // current textblock in `bullet_list > list_item`. The DOM
  // materializes as `<ul><li><p>...`.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await caretAtStartOfFreshParagraphAfterFirst(page);
  await page.keyboard.type('* item one');

  await expect
    .poll(async () =>
      page.evaluate(() => {
        const surface =
          document.querySelector(
            'pine-rich-text-root[runtime="document"] .pine-rich-text',
          ) ?? document.querySelector('pine-rich-text-root[runtime="document"]');
        const bullets = [
          ...surface.querySelectorAll(':scope > ul:not(.task-list)'),
        ];
        return bullets.map((ul) =>
          [...ul.querySelectorAll(':scope > li')].map((li) => li.textContent),
        );
      }),
    )
    .toEqual(expect.arrayContaining([expect.arrayContaining(['item one'])]));
  expect(errors).toEqual([]);
});

test('typing `> ` at start of an empty paragraph wraps it in a blockquote', async ({ page }) => {
  // Phase 5 C4: blockquote rule (`^\s*>\s$`) wraps the current
  // textblock in `blockquote`. The DOM materializes as
  // `<blockquote><p>...`.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await caretAtStartOfFreshParagraphAfterFirst(page);
  await page.keyboard.type('> a quote');

  await expect
    .poll(async () =>
      page.evaluate(() => {
        const surface =
          document.querySelector(
            'pine-rich-text-root[runtime="document"] .pine-rich-text',
          ) ?? document.querySelector('pine-rich-text-root[runtime="document"]');
        return [...surface.querySelectorAll(':scope > blockquote')].map(
          (el) => el.textContent,
        );
      }),
    )
    .toEqual(expect.arrayContaining([expect.stringContaining('a quote')]));
  expect(errors).toEqual([]);
});

test('Export MD button serializes the current doc to markdown', async ({ page }) => {
  // Phase 6 C2: clicking Export MD reads the surface's current doc
  // via the pp-model:doc binding, runs it through
  // `EditorRuntime::markdown_serializer()`, and writes the result
  // into a `<pre data-test="exported-markdown">` block.
  //
  // The default seed has a heading would-be (no, just paragraphs +
  // a task list with one checked + one unchecked item) so the
  // exported markdown must include the task-list lines AND the
  // seeded paragraph text.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await page.locator('[data-test="export-md"]').click();

  const exported = await expect
    .poll(async () =>
      page.locator('[data-test="exported-markdown"]').textContent(),
    )
    .not.toBe('');

  const md = (await page.locator('[data-test="exported-markdown"]').textContent()) ?? '';
  // Seeded paragraph text must round-trip.
  expect(md).toContain('Hello, pine-richtext.');
  // GFM task-list lines from `TaskListExtension::markdown_node_emitters`.
  expect(md).toContain('[x] Schema with task_list / task_item');
  expect(md).toContain('[ ] Click the box to toggle this item');
  expect(errors).toEqual([]);
});

test('Export MD captures live edits made before clicking', async ({ page }) => {
  // After typing into the surface, the pp-model:doc binding
  // propagates the updated doc back to the Editor's `doc` field on
  // the next tick. Click Export MD and verify the typed text is in
  // the exported markdown.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  await caretAtEndOfFirstParagraph(page);
  await page.keyboard.type(' LIVE-EDIT-MARKER');

  await page.locator('[data-test="export-md"]').click();

  await expect
    .poll(async () =>
      page.locator('[data-test="exported-markdown"]').textContent(),
    )
    .toContain('LIVE-EDIT-MARKER');
  expect(errors).toEqual([]);
});

test('Import MD button parses markdown into the surface doc', async ({ page }) => {
  // Phase 6 C3: the import-markdown textarea + button feeds
  // `MarkdownParser` and dispatches `ReplaceState` so the surface
  // swaps its doc. The result must contain the expected block
  // structure (heading + paragraph + bullet list).
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  const md = [
    '# Imported heading',
    '',
    'A paragraph from markdown.',
    '',
    '* one',
    '* two',
    '',
  ].join('\n');

  // Import block is in a `<details>` — open it so the textarea
  // becomes visible to Playwright's fill action.
  await page
    .locator('.import-markdown-wrap')
    .evaluate((d) => d.setAttribute('open', ''));
  await page.locator('[data-test="import-markdown-input"]').fill(md);
  await page.locator('[data-test="import-md"]').click();

  // Heading materializes as <h1> directly under the surface.
  await expect
    .poll(async () =>
      page.evaluate(() => {
        const surface =
          document.querySelector(
            'pine-rich-text-root[runtime="document"] .pine-rich-text',
          ) ?? document.querySelector('pine-rich-text-root[runtime="document"]');
        return [...surface.querySelectorAll(':scope > h1')].map(
          (el) => el.textContent,
        );
      }),
    )
    .toEqual(expect.arrayContaining([expect.stringContaining('Imported heading')]));

  // Paragraph text survives.
  const html = await page
    .locator('pine-rich-text-root[runtime="document"] .pine-rich-text')
    .innerHTML();
  expect(html).toContain('A paragraph from markdown.');

  // Bullet list with two items.
  const bulletItems = await page.evaluate(() => {
    const surface =
      document.querySelector(
        'pine-rich-text-root[runtime="document"] .pine-rich-text',
      ) ?? document.querySelector('pine-rich-text-root[runtime="document"]');
    const ul = surface.querySelector(':scope > ul:not(.task-list)');
    if (!ul) return [];
    return [...ul.querySelectorAll(':scope > li')].map((li) => li.textContent);
  });
  expect(bulletItems).toEqual(expect.arrayContaining(['one', 'two']));

  expect(errors).toEqual([]);
});

test('Import then Export round-trips through model', async ({ page }) => {
  // End-to-end Phase 6: paste markdown into the import box,
  // click Import MD → surface adopts the doc → click Export MD
  // → the result reflects the imported content. Proves the full
  // parse → model → serialize pipeline works in the browser.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  const md = '- [x] task one\n- [ ] task two\n';

  await page
    .locator('.import-markdown-wrap')
    .evaluate((d) => d.setAttribute('open', ''));
  await page.locator('[data-test="import-markdown-input"]').fill(md);
  await page.locator('[data-test="import-md"]').click();

  // Wait for the surface to repaint with the imported task list.
  await expect
    .poll(async () =>
      page.evaluate(() => {
        const surface =
          document.querySelector(
            'pine-rich-text-root[runtime="document"] .pine-rich-text',
          ) ?? document.querySelector('pine-rich-text-root[runtime="document"]');
        return surface.querySelector(':scope > ul.task-list') !== null;
      }),
    )
    .toBe(true);

  await page.locator('[data-test="export-md"]').click();

  await expect
    .poll(async () =>
      page.locator('[data-test="exported-markdown"]').textContent(),
    )
    .toContain('[x] task one');
  const out = await page.locator('[data-test="exported-markdown"]').textContent();
  expect(out).toContain('[ ] task two');
  expect(errors).toEqual([]);
});

test('invalid state replacement fails loudly and preserves the live document', async ({ page }) => {
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.message));

  await page.goto('/');
  const host = page.locator('pine-rich-text-root[runtime="document"]');
  const surface = host.locator('.pine-rich-text');
  await expect(surface.locator(':scope > p').first()).toContainText('Hello, pine-richtext.');

  const textBefore = await surface.textContent();
  await page.evaluate(() => {
    const host = document.querySelector('pine-rich-text-root[runtime="document"]');
    window.__pineLoadErrors = [];
    host.addEventListener('pine:richtext:load-error', (event) => {
      window.__pineLoadErrors.push(event.detail);
    });
    host.dispatchEvent(
      new CustomEvent('pine:richtext:command', {
        bubbles: true,
        detail: {
          kind: 'replace_state',
          doc: {
            doc: { type: 'not_a_registered_node', content: [] },
            selection: null,
            stored_marks: null,
            plugin_state: {},
          },
        },
      }),
    );
  });

  await expect(surface).toHaveAttribute('data-pine-richtext-load-error', 'true');
  const alert = host.locator('.pine-richtext-load-error');
  await expect(alert).toBeVisible();
  await expect(alert).toHaveAttribute('role', 'alert');
  await expect(alert).toContainText('not_a_registered_node');

  expect(await surface.textContent()).toBe(textBefore);
  const details = await page.evaluate(() => window.__pineLoadErrors);
  expect(details).toHaveLength(1);
  expect(details[0].runtime).toBe('document');
  expect(details[0].wire_fingerprint).toMatch(/^[0-9a-f]{64}$/);
  expect(details[0].error).toContain('not_a_registered_node');
  expect(details[0].input.doc.type).toBe('not_a_registered_node');
  expect(pageErrors).toEqual([]);
});

test('Mod+a selects the entire surface (selection-only commit syncs DOM caret)', async ({ page }) => {
  // Regression: select_all sets `Selection::All` without touching
  // the doc, so the reconciler short-circuits as `Unchanged`. The
  // watcher used to skip cursor sync on `Unchanged`, leaving the
  // visible caret pinned at its previous position. Now the watcher
  // tracks the previous selection and syncs the DOM range when the
  // model selection changes even if no DOM mutation happened.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  // Park the caret somewhere inside the surface so the "before"
  // selection is collapsed and clearly not equal to "all".
  await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    surface.focus();
    const firstText = document.createTreeWalker(surface, NodeFilter.SHOW_TEXT).nextNode();
    if (!firstText) throw new Error('no text node to seed caret');
    const range = document.createRange();
    range.setStart(firstText, 0);
    range.setEnd(firstText, 0);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  });

  // Press Mod+a — the keymap binding fires `commands::select_all`,
  // which lands a selection-only transaction. The watcher must
  // notice the selection change and re-issue the DOM range.
  const modifier = process.platform === 'darwin' ? 'Meta' : 'Control';
  await page.keyboard.press(`${modifier}+a`);

  const selectionInfo = await page.evaluate(() => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    const sel = window.getSelection();
    const range = sel.rangeCount ? sel.getRangeAt(0) : null;
    if (!range) return { covered: false };
    const surfaceText = surface.textContent ?? '';
    const selected = range.toString();
    return {
      covered: selected.length > 0 && selected.length >= surfaceText.length - 4,
      selectedLen: selected.length,
      surfaceLen: surfaceText.length,
    };
  });

  expect(selectionInfo.covered).toBe(true);
  expect(errors).toEqual([]);
});

test('pasting multi-block markdown preserves heading and list structure', async ({ page }) => {
  // Regression: the paste handler used to use open_start=open_end=1
  // unconditionally, which dissolved leading headings and trailing
  // lists into the cursor's paragraph. Per-edge heuristic now keeps
  // structural blocks closed; this test exercises the user-reported
  // markdown blob to keep the regression locked.
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await page.waitForSelector('pine-rich-text-root[runtime="document"] p[data-pos="0"]');

  const md = '## What I should *not* do\n\n- Don\'t start in TypeScript\n- Docker-only is the scope\n';

  // Dispatch a real `paste` event with a populated DataTransfer.
  // beforeinput-`insertFromPaste` won't fire under Playwright without
  // a real clipboard write; the paste-event path is what production
  // installs and the only path the handler listens on.
  await page.evaluate((markdown) => {
    const surface =
      document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
      document.querySelector('pine-rich-text-root[runtime="document"]');
    surface.focus();
    const firstText = document.createTreeWalker(surface, NodeFilter.SHOW_TEXT).nextNode();
    if (firstText) {
      const range = document.createRange();
      range.setStart(firstText, 0);
      range.setEnd(firstText, 0);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    }
    const dt = new DataTransfer();
    dt.setData('text/plain', markdown);
    surface.dispatchEvent(
      new ClipboardEvent('paste', {
        clipboardData: dt,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, md);

  // Heading + bullet list must both materialise.
  await expect
    .poll(async () =>
      page.evaluate(() => {
        const surface =
          document.querySelector('pine-rich-text-root[runtime="document"] .pine-rich-text') ??
          document.querySelector('pine-rich-text-root[runtime="document"]');
        return {
          hasHeading: surface.querySelector('h1, h2, h3, h4, h5, h6') !== null,
          hasList: surface.querySelector('ul, ol') !== null,
          headingText:
            surface.querySelector('h2')?.textContent ??
            surface.querySelector('h1, h3, h4, h5, h6')?.textContent ??
            '',
        };
      }),
    )
    .toMatchObject({ hasHeading: true, hasList: true });

  expect(errors).toEqual([]);
});
