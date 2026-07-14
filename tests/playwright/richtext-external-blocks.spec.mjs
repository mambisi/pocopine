import { expect, test } from '@playwright/test';

async function selectText(page, needle) {
  await page.evaluate((text) => {
    const editor = document.querySelector('#external-blocks-editor');
    editor.querySelector('.pine-rich-text')?.focus();
    const walker = document.createTreeWalker(editor, NodeFilter.SHOW_TEXT);
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
        document.dispatchEvent(new Event('selectionchange'));
        return;
      }
      node = walker.nextNode();
    }
    throw new Error(`missing text: ${text}`);
  }, needle);
}

async function openMoreTools(page) {
  const tools = page.locator('.demo-more-tools');
  if (await tools.getAttribute('open') === null) {
    await tools.locator(':scope > summary').click();
  }
  await expect(tools).toHaveAttribute('open', '');
  return tools;
}

async function visibleTableRows(tableView) {
  return tableView.locator('.pine-richtext-table-row').evaluateAll((rows) =>
    rows.map((row) =>
      [...row.querySelectorAll(':scope > [data-pine-table-cell="true"]')].map((cell) =>
        cell.textContent.trim(),
      ),
    ),
  );
}

async function semanticTable(page) {
  const document = JSON.parse(
    await page.locator('[data-test="semantic-json"]').textContent(),
  );
  const pending = [document];
  while (pending.length > 0) {
    const node = pending.shift();
    if (node?.type === 'table') {
      return node;
    }
    if (Array.isArray(node?.content)) {
      pending.push(...node.content);
    }
  }
  throw new Error('semantic output has no table node');
}

async function dragTableHandle(page, source, target) {
  await source.hover();
  const sourceBox = await source.boundingBox();
  const targetBox = await target.boundingBox();
  if (!sourceBox || !targetBox) {
    throw new Error('table drag handle has no layout box');
  }
  await page.mouse.move(
    sourceBox.x + sourceBox.width / 2,
    sourceBox.y + sourceBox.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(
    targetBox.x + targetBox.width / 2,
    targetBox.y + targetBox.height / 2,
    { steps: 8 },
  );
  await page.mouse.up();
}

async function columnHandleCenterDelta(tableView, index) {
  const handle = tableView.locator(
    `.pine-richtext-table-column-selector[data-column="${index}"]`,
  );
  const header = tableView
    .locator('.pine-richtext-table-row')
    .first()
    .locator(':scope > [data-pine-table-cell="true"]')
    .nth(index);
  const [handleBox, headerBox] = await Promise.all([
    handle.boundingBox(),
    header.boundingBox(),
  ]);
  if (!handleBox || !headerBox) {
    throw new Error('table header or handle has no layout box');
  }
  return Math.abs(
    handleBox.x + handleBox.width / 2 - (headerBox.x + headerBox.width / 2),
  );
}

async function placeCaretInText(page, needle, offset) {
  await page.evaluate(({ text, caretOffset }) => {
    const editor = document.querySelector('#external-blocks-editor .pine-rich-text');
    const walker = document.createTreeWalker(editor, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      const start = node.textContent.indexOf(text);
      if (start >= 0) {
        const range = document.createRange();
        range.setStart(node, start + caretOffset);
        range.collapse(true);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        editor.focus();
        document.dispatchEvent(new Event('selectionchange'));
        return;
      }
      node = walker.nextNode();
    }
    throw new Error(`missing text: ${text}`);
  }, { text: needle, caretOffset: offset });
}

async function caretSnapshot(page) {
  return page.evaluate(() => {
    const selection = window.getSelection();
    const node = selection.anchorNode;
    const parent = node?.parentElement;
    return {
      text: node?.textContent ?? '',
      offset: selection.anchorOffset,
      nodeType: parent?.closest('[data-pine-node-type]')?.getAttribute('data-pine-node-type'),
      inTableCell: parent?.closest('[data-pine-table-cell="true"]') != null,
    };
  });
}

async function editorSelection(page) {
  return page.evaluate(() => {
    const surface = document.querySelector('#external-blocks-editor .pine-rich-text');
    const host = surface.parentElement;
    let state = null;
    host.addEventListener(
      'pine:richtext:export-state-result',
      (event) => {
        state = event.detail;
      },
      { once: true },
    );
    host.dispatchEvent(new CustomEvent('pine:richtext:export-state', { bubbles: true }));
    const plain = (value) => {
      if (value instanceof Map) {
        return Object.fromEntries([...value].map(([key, entry]) => [key, plain(entry)]));
      }
      if (Array.isArray(value)) return value.map(plain);
      if (value && typeof value === 'object') {
        return Object.fromEntries(
          Object.entries(value).map(([key, entry]) => [key, plain(entry)]),
        );
      }
      return value;
    };
    state = plain(state);
    return {
      model: state.selection,
      domCollapsed: window.getSelection().isCollapsed,
    };
  });
}

async function captureStableEditorDom(page) {
  await page.evaluate(() => {
    const surface = document.querySelector('#external-blocks-editor .pine-rich-text');
    const views = [...surface.querySelectorAll('[data-pine-node-view-id]')];
    const probe = {
      editorPanel: document.querySelector('[data-test="editor"]'),
      editorHost: document.querySelector('#external-blocks-editor'),
      surface,
      topLevel: [...surface.children],
      views,
      viewIds: views.map((view) => view.getAttribute('data-pine-node-view-id')),
      removedViews: [],
    };
    new MutationObserver((records) => {
      for (const record of records) {
        for (const removed of record.removedNodes) {
          for (const [index, view] of probe.views.entries()) {
            if (removed === view || (removed instanceof Element && removed.contains(view))) {
              probe.removedViews.push(index);
            }
          }
        }
      }
    }).observe(surface, { childList: true, subtree: true });
    window.__pineStableEditorDom = probe;
  });
}

async function expectStableEditorDom(page) {
  expect(
    await page.evaluate(() => {
      const probe = window.__pineStableEditorDom;
      const surface = document.querySelector('#external-blocks-editor .pine-rich-text');
      const views = [...surface.querySelectorAll('[data-pine-node-view-id]')];
      return {
        editorPanel:
          document.querySelector('[data-test="editor"]') === probe.editorPanel,
        editorHost:
          document.querySelector('#external-blocks-editor') === probe.editorHost,
        surface: surface === probe.surface,
        topLevel:
          surface.children.length === probe.topLevel.length
          && probe.topLevel.every((node, index) => surface.children[index] === node),
        views:
          views.length === probe.views.length
          && probe.views.every((node, index) => views[index] === node),
        viewIds: views.map((view) => view.getAttribute('data-pine-node-view-id')),
        initialViewIds: probe.viewIds,
        removedViews: probe.removedViews,
      };
    }),
  ).toEqual({
    editorPanel: true,
    editorHost: true,
    surface: true,
    topLevel: true,
    views: true,
    viewIds: await page.evaluate(() => window.__pineStableEditorDom.viewIds),
    initialViewIds: await page.evaluate(() => window.__pineStableEditorDom.viewIds),
    removedViews: [],
  });
}

test('contextual table handles reorder body rows and columns', async ({ page }) => {
  await page.goto('/');
  const editorSurface = page.locator('#external-blocks-editor .pine-rich-text');
  await expect(editorSurface).toHaveAttribute('data-pine-richtext-ready', 'true');

  const tableView = page.locator('.pine-richtext-table-view');
  const reorderActions = tableView.locator('.pine-richtext-table-reorder-actions');
  const bodyRowHandle = tableView.locator(
    '.pine-richtext-table-row-selector[data-row="1"]',
  );
  await expect(tableView).toHaveAttribute('data-selection', 'none');
  await expect(bodyRowHandle).toHaveCSS('opacity', '0');

  await tableView.hover();
  await expect
    .poll(() =>
      bodyRowHandle.evaluate((node) => {
        const style = getComputedStyle(node);
        const opacity = Number(style.opacity);
        const idleOpacity = Number(
          style.getPropertyValue('--pine-richtext-table-handle-idle-opacity'),
        );
        return Math.abs(opacity - idleOpacity);
      }),
    )
    .toBeLessThan(0.01);
  const idleOpacity = Number(
    await bodyRowHandle.evaluate((node) => getComputedStyle(node).opacity),
  );
  expect(idleOpacity).toBeGreaterThan(0);
  expect(idleOpacity).toBeLessThan(0.6);
  await expect(bodyRowHandle).toHaveCSS('pointer-events', 'auto');
  await expect(
    bodyRowHandle.locator('.pine-richtext-table-handle-glyph > svg'),
  ).toHaveCount(1);
  await expect(
    bodyRowHandle.locator('.pine-richtext-table-handle-glyph > svg > path'),
  ).toHaveCount(6);
  await expect(bodyRowHandle).toHaveCSS('box-shadow', 'none');
  await expect(
    tableView.locator('.pine-richtext-table-row-selector[data-row="0"]'),
  ).not.toHaveAttribute('data-draggable', 'true');
  await expect(bodyRowHandle).toHaveAttribute('data-draggable', 'true');

  const idleHandleBox = await bodyRowHandle.boundingBox();
  if (!idleHandleBox) {
    throw new Error('idle table row handle has no layout box');
  }
  const selectedCells = tableView.locator(
    '[data-pine-table-cell="true"][data-selected="true"]',
  );
  await bodyRowHandle.click();
  await expect(tableView).toHaveAttribute('data-selection', 'row');
  await expect(bodyRowHandle).toHaveAttribute('aria-pressed', 'true');
  await expect(bodyRowHandle).toHaveCSS('opacity', '1');
  await expect.poll(
    () => bodyRowHandle.evaluate((node) => getComputedStyle(node).boxShadow),
  ).not.toBe('none');
  await expect(selectedCells).toHaveCount(3);
  await expect(
    tableView.locator('.pine-richtext-table-row-selector[data-row="2"]'),
  ).toHaveCSS('box-shadow', 'none');
  const raisedHandleBox = await bodyRowHandle.boundingBox();
  if (!raisedHandleBox) {
    throw new Error('selected table row handle has no layout box');
  }
  for (const coordinate of ['x', 'y', 'width', 'height']) {
    expect(Math.abs(raisedHandleBox[coordinate] - idleHandleBox[coordinate])).toBeLessThan(0.1);
  }

  // The Cells commit clears the old DOM range. Let selection observers run
  // before proving that a later, real browser caret dismisses the table chrome.
  await page.evaluate(
    () => new Promise((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );
  await expect(tableView).toHaveAttribute('data-selection', 'row');
  await editorSurface.locator(':scope > p').first().click();
  await expect(tableView).toHaveAttribute('data-selection', 'none');
  await expect(bodyRowHandle).toHaveAttribute('aria-pressed', 'false');
  await expect(bodyRowHandle).toHaveCSS('box-shadow', 'none');
  await expect(selectedCells).toHaveCount(0);
  await expect(reorderActions).toBeHidden();
  await expect(page.locator('[data-test="selection-status"]')).toHaveText(/^caret at /);

  // A page-level click has no replacement editor caret. The table still
  // dismisses its painted selection and controls when editor focus leaves.
  await tableView.hover();
  await bodyRowHandle.click();
  await expect(tableView).toHaveAttribute('data-selection', 'row');
  await page.locator('.demo-document-header h1').click();
  await expect(tableView).toHaveAttribute('data-selection', 'none');
  await expect(bodyRowHandle).toHaveAttribute('aria-pressed', 'false');
  await expect(selectedCells).toHaveCount(0);
  await expect(reorderActions).toBeHidden();

  // Page-level dismissal is paint-only so pointer-preserving editor toolbar
  // commands can still act on the semantic cell rectangle.
  const dismissedRow = tableView.locator('.pine-richtext-table-row').nth(1);
  await page.locator('[data-test="bold"]').click();
  await expect(dismissedRow.locator('strong')).toHaveCount(3);
  await expect(tableView).toHaveAttribute('data-selection', 'none');
  await page.locator('[data-test="bold"]').click();
  await expect(dismissedRow.locator('strong')).toHaveCount(0);

  await tableView.hover();
  await bodyRowHandle.click();
  await expect(tableView).toHaveAttribute('data-selection', 'row');

  await expect.poll(() => columnHandleCenterDelta(tableView, 0)).toBeLessThan(1.5);
  await page.setViewportSize({ width: 720, height: 900 });
  await tableView.scrollIntoViewIfNeeded();
  await tableView.hover();
  await expect.poll(() => columnHandleCenterDelta(tableView, 0)).toBeLessThan(1.5);

  const beforeRowMove = await visibleTableRows(tableView);
  expect(beforeRowMove).toHaveLength(3);
  await dragTableHandle(
    page,
    bodyRowHandle,
    tableView.locator('.pine-richtext-table-row-selector[data-row="2"]'),
  );
  const afterRowMove = [beforeRowMove[0], beforeRowMove[2], beforeRowMove[1]];
  await expect.poll(() => visibleTableRows(tableView)).toEqual(afterRowMove);
  await expect(
    tableView.locator('.pine-richtext-table-row').first().locator(
      ':scope > .pine-richtext-table-header-cell',
    ),
  ).toHaveCount(beforeRowMove[0].length);

  const beforeColumnMove = await visibleTableRows(tableView);
  await dragTableHandle(
    page,
    tableView.locator('.pine-richtext-table-column-selector[data-column="0"]'),
    tableView.locator('.pine-richtext-table-column-selector[data-column="2"]'),
  );
  const afterColumnMove = beforeColumnMove.map((row) => [row[1], row[2], row[0]]);
  await expect.poll(() => visibleTableRows(tableView)).toEqual(afterColumnMove);

  const selectedColumn = tableView.locator(
    '.pine-richtext-table-column-selector[data-column="1"]',
  );
  await selectedColumn.click();
  await expect(reorderActions).toBeVisible();
  await reorderActions
    .getByRole('button', { name: 'Move selected column to its previous position' })
    .click();
  const afterClickMove = afterColumnMove.map((row) => [row[1], row[0], row[2]]);
  await expect.poll(() => visibleTableRows(tableView)).toEqual(afterClickMove);
  await expect(reorderActions).toBeVisible();
  const moveColumnForward = reorderActions.getByRole('button', {
    name: 'Move selected column to its next position',
  });
  await moveColumnForward.focus();
  await moveColumnForward.press('Enter');
  await expect.poll(() => visibleTableRows(tableView)).toEqual(afterColumnMove);
  await expect(reorderActions).toBeVisible();
  await expect(moveColumnForward).toBeFocused();

  await tableView.locator('.pine-richtext-table-row-selector[data-row="1"]').click();
  const moveRowDown = reorderActions.getByRole('button', {
    name: 'Move selected row down',
  });
  await moveRowDown.focus();
  await moveRowDown.press('Enter');
  const afterKeyboardMove = [afterColumnMove[0], afterColumnMove[2], afterColumnMove[1]];
  await expect.poll(() => visibleTableRows(tableView)).toEqual(afterKeyboardMove);
  await expect(reorderActions).toBeVisible();
  await expect(
    reorderActions.getByRole('button', { name: 'Move selected row up' }),
  ).toBeFocused();
});

test('Backspace expands a backward cross-block selection through the whole table', async ({
  page,
}) => {
  await page.goto('/');
  const editorSurface = page.locator('#external-blocks-editor .pine-rich-text');
  await expect(editorSurface).toHaveAttribute('data-pine-richtext-ready', 'true');

  const domSelection = await page.evaluate(() => {
    const surface = document.querySelector('#external-blocks-editor .pine-rich-text');
    const findText = (root, needle) => {
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      let node = walker.nextNode();
      while (node) {
        if (node.textContent.includes(needle)) return node;
        node = walker.nextNode();
      }
      throw new Error(`missing text: ${needle}`);
    };

    const focusNode = findText(surface.children[0], 'are real inline atoms');
    const cells = [...surface.querySelectorAll(
      '[data-pine-node-type="table"] [data-pine-table-cell="true"]',
    )];
    const anchorNode = findText(cells.at(-1), 'Editable');
    const focusOffset = focusNode.textContent.indexOf('around them.');
    const anchorOffset = anchorNode.textContent.indexOf('Editable') + 'Editable'.length;

    surface.focus();
    const selection = window.getSelection();
    selection.setBaseAndExtent(anchorNode, anchorOffset, focusNode, focusOffset);
    document.dispatchEvent(new Event('selectionchange'));
    return {
      anchorAtEnd: selection.anchorOffset === selection.anchorNode.textContent.length,
      focusBeforePhrase: selection.focusNode.textContent
        .slice(selection.focusOffset)
        .startsWith('around them.'),
      collapsed: selection.isCollapsed,
    };
  });

  expect(domSelection).toEqual({
    anchorAtEnd: true,
    focusBeforePhrase: true,
    collapsed: false,
  });
  await expect.poll(async () => {
    const model = (await editorSelection(page)).model;
    return model.type === 'text' && model.anchor > model.head;
  }).toBe(true);
  const beforeDelete = await editorSelection(page);
  expect(beforeDelete.model.anchor).toBeGreaterThan(beforeDelete.model.head);
  const collapsePos = beforeDelete.model.head;

  await page.keyboard.press('Backspace');

  await expect(page.locator('.pine-rich-text > [data-pine-node-type="table"]')).toHaveCount(0);
  await expect(editorSurface.locator('ul.task-list')).toHaveCount(0);
  await expect(editorSurface).not.toContainText('Select this sentence to open the BubbleMenu');
  await expect(editorSurface).not.toContainText('Table playground');
  await expect(editorSurface).not.toContainText('around them.');
  await expect(editorSurface).toContainText(
    'Typed external blocks keep semantic data in the document and UI state in components.',
  );
  await expect(editorSurface).toContainText(
    'Every edit updates the portable output in the developer inspector.',
  );

  const semantic = JSON.parse(
    await page.locator('[data-test="semantic-json"]').textContent(),
  );
  expect(semantic.content.map((node) => node.type)).toEqual([
    'paragraph',
    'paragraph',
  ]);
  expect(semantic.content[0].content.at(-1)).toEqual({
    type: 'text',
    text: ' are real inline atoms — use ArrowLeft/ArrowRight and Delete ',
  });
  expect(semantic.content[1].content[0].text).toBe(
    'Every edit updates the portable output in the developer inspector. ',
  );
  expect(await editorSelection(page)).toEqual({
    model: {
      type: 'text',
      anchor: collapsePos,
      head: collapsePos,
    },
    domCollapsed: true,
  });

  await page.locator('[data-test="undo"]').click();
  await expect(editorSurface.locator('ul.task-list')).toHaveCount(1);
  await expect(editorSurface.locator('li[data-pine-node-type="task_item"]')).toHaveCount(2);
  await expect(page.locator('.pine-rich-text > [data-pine-node-type="table"]')).toHaveCount(1);
  await expect(editorSurface).toContainText('around them.');
  await expect(editorSurface).toContainText('Table playground');
});

test('typed external-block showcase exercises lifecycle, search, tables, tags, and outputs', async ({
  page,
}) => {
  const errors = [];
  const panicLogs = [];
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => {
    if (
      message.type() === 'error'
      && /RefCell already (?:mutably )?borrowed|component callback frames|RuntimeError: unreachable/.test(
        message.text(),
      )
    ) {
      panicLogs.push(message.text());
    }
  });

  await page.goto('/');
  const editorSurface = page.locator('#external-blocks-editor .pine-rich-text');
  await expect(page.locator('[data-test="editor"]')).toBeVisible();
  await expect(editorSurface).toHaveAttribute('data-pine-richtext-ready', 'true');
  await expect(page.locator('[data-test="status"]')).toContainText(
    'selection observers are live',
  );
  const taskViews = page.locator('ul.task-list > li[data-pine-node-type="task_item"]');
  const tableHosts = page.locator('.pine-rich-text > [data-pine-node-type="table"]');
  const tagViews = page.locator(
    '.pine-rich-text [data-pine-node-type="tag"]:not(.pine-richtext-tag)',
  );
  await expect(taskViews).toHaveCount(2);
  await expect(taskViews.first()).toHaveJSProperty(
    'tagName',
    'LI',
  );
  await expect(tableHosts).toHaveCount(1);
  await expect(page.locator('.pine-richtext-table-row')).toHaveCount(3);
  await expect(tagViews).toHaveCount(3);
  await captureStableEditorDom(page);

  const mixedText =
    'Typed external blocks keep semantic data in the document and UI state in components. ';

  // An inline component host must not gain formatting whitespace around its
  // mounted visual root. A whitespace-only direct text child becomes an
  // anonymous editable boundary in Chromium: it separates the following text
  // onto another visual line and ArrowDown may skip the rest of the paragraph.
  expect(
    await page.evaluate(() => {
      const paragraph = document.querySelector('#external-blocks-editor .pine-rich-text p');
      const host = paragraph?.querySelector(':scope > [data-pine-node-type="tag"]');
      const nextText = host?.nextSibling;
      const hostRect = host?.getClientRects()[0];
      let nextTextRect = null;
      if (nextText?.nodeType === Node.TEXT_NODE) {
        const range = document.createRange();
        range.selectNodeContents(nextText);
        nextTextRect = range.getClientRects()[0] ?? null;
      }
      return {
        directElements: host?.children.length,
        whitespaceTextChildren: host
          ? [...host.childNodes].filter(
            (node) => node.nodeType === Node.TEXT_NODE && node.textContent.trim() === '',
          ).length
          : null,
        display: host ? getComputedStyle(host).display : null,
        continuesOnSameLine:
          hostRect && nextTextRect
            ? Math.abs(hostRect.top - nextTextRect.top) < 2
            : false,
      };
    }),
  ).toEqual({
    directElements: 1,
    whitespaceTextChildren: 0,
    display: 'inline',
    continuesOnSameLine: true,
  });

  // Keep native visual-line movement inside the mixed-inline paragraph. The
  // non-editable chip between text runs must not make Chromium jump directly
  // to the last editable paragraph in the document.
  await placeCaretInText(page, mixedText, 10);
  const verticalStart = await page.evaluate(() => {
    const selection = window.getSelection();
    const range = selection.getRangeAt(0).cloneRange();
    const rect = range.getBoundingClientRect();
    return { top: rect.top, left: rect.left };
  });
  await page.keyboard.press('ArrowDown');
  const verticalDown = await page.evaluate(() => {
    const paragraph = document.querySelector('#external-blocks-editor .pine-rich-text p');
    const selection = window.getSelection();
    const range = selection.getRangeAt(0).cloneRange();
    const rect = range.getBoundingClientRect();
    return {
      collapsed: selection.isCollapsed,
      inFirstParagraph: paragraph?.contains(selection.anchorNode) ?? false,
      top: rect.top,
      left: rect.left,
    };
  });
  expect(verticalDown.collapsed).toBe(true);
  expect(verticalDown.inFirstParagraph).toBe(true);
  expect(verticalDown.top).toBeGreaterThan(verticalStart.top);

  await page.keyboard.press('ArrowUp');
  const verticalUp = await page.evaluate(() => {
    const paragraph = document.querySelector('#external-blocks-editor .pine-rich-text p');
    const selection = window.getSelection();
    const range = selection.getRangeAt(0).cloneRange();
    const rect = range.getBoundingClientRect();
    return {
      collapsed: selection.isCollapsed,
      inFirstParagraph: paragraph?.contains(selection.anchorNode) ?? false,
      top: rect.top,
    };
  });
  expect(verticalUp.collapsed).toBe(true);
  expect(verticalUp.inFirstParagraph).toBe(true);
  expect(verticalUp.top).toBeLessThan(verticalDown.top);

  const semanticBeforeTyping = await page.locator('[data-test="semantic-json"]').textContent();
  await editorSurface.locator('p').nth(1).click();
  await page.keyboard.press('End');
  await page.keyboard.type(' TYPE_PROBE');
  await expect(editorSurface).toContainText('TYPE_PROBE');
  await expect(page.locator('[data-test="semantic-json"]')).toContainText('TYPE_PROBE');
  await expect.poll(() => page.locator('[data-test="semantic-json"]').textContent()).not.toBe(
    semanticBeforeTyping,
  );
  expect(errors).toEqual([]);
  expect(panicLogs).toEqual([]);
  await expectStableEditorDom(page);

  // Mixed inline text must be patched beside retained typed atoms. Exercise
  // both plain and marked text because marked text is wrapped in element DOM.
  await placeCaretInText(page, mixedText, 24);
  await page.keyboard.type('abc');
  for (let index = 0; index < 3; index += 1) await page.keyboard.press('Backspace');
  await page.keyboard.press('Control+b');
  await page.keyboard.type('abc');
  for (let index = 0; index < 3; index += 1) await page.keyboard.press('Backspace');
  await page.keyboard.press('Control+b');
  await expectStableEditorDom(page);

  // Arrow navigation across a selectable inline atom has three states:
  // caret before -> node selection -> caret after (and symmetrically back).
  const architectureTag = page.locator(
    '.pine-richtext-tag[data-tag-id="architecture"]',
  );
  await placeCaretInText(page, mixedText, mixedText.length);
  await page.keyboard.press('ArrowRight');
  const selectedForward = await editorSelection(page);
  expect(selectedForward.model.type).toBe('node');
  expect(selectedForward.domCollapsed).toBe(false);
  await expect(architectureTag).toHaveAttribute('data-selection', 'node');

  await page.keyboard.press('ArrowRight');
  expect(await editorSelection(page)).toEqual({
    model: {
      type: 'text',
      anchor: selectedForward.model.anchor + 1,
      head: selectedForward.model.anchor + 1,
    },
    domCollapsed: true,
  });
  await expect(architectureTag).toHaveAttribute('data-selection', 'outside');

  await page.keyboard.press('ArrowLeft');
  expect((await editorSelection(page)).model).toEqual(selectedForward.model);
  await expect(architectureTag).toHaveAttribute('data-selection', 'node');

  await page.keyboard.press('ArrowLeft');
  expect(await editorSelection(page)).toEqual({
    model: {
      type: 'text',
      anchor: selectedForward.model.anchor,
      head: selectedForward.model.anchor,
    },
    domCollapsed: true,
  });
  await expect(architectureTag).toHaveAttribute('data-selection', 'outside');
  await expectStableEditorDom(page);

  // Leave the cached model selection in the ordinary paragraph, then move
  // only the DOM caret into each owned-content outlet. Backspace/Delete must
  // map the browser target range instead of restoring that stale caret.
  const taskText = 'Task attrs update through a typed NodeViewHandle';
  const taskOffset = 12;
  await placeCaretInText(page, taskText, taskOffset);
  await page.keyboard.press('Backspace');
  const taskCaret = await caretSnapshot(page);
  expect(taskCaret).toEqual({
    text: `${taskText.slice(0, taskOffset - 1)}${taskText.slice(taskOffset)}`,
    offset: taskOffset - 1,
    nodeType: 'task_item',
    inTableCell: false,
  });
  await expect(page.locator('[data-test="semantic-json"]')).toContainText(taskCaret.text);

  const tableText = 'Live attrs + lifecycle';
  const tableOffset = 12;
  const tableViewIdBeforeDelete = await tableHosts.getAttribute('data-pine-node-view-id');
  await placeCaretInText(page, tableText, tableOffset);
  await page.keyboard.press('Delete');
  const tableCaret = await caretSnapshot(page);
  expect(tableCaret).toEqual({
    text: `${tableText.slice(0, tableOffset)}${tableText.slice(tableOffset + 1)}`,
    offset: tableOffset,
    nodeType: 'table',
    inTableCell: true,
  });
  await expect(page.locator('[data-test="semantic-json"]')).toContainText(tableCaret.text);
  await expect(tableHosts).toHaveAttribute('data-pine-node-view-id', tableViewIdBeforeDelete);

  // Browser-native boundary deletion must never mutate component-owned DOM
  // behind Pine's model. An unsupported delete is a stable no-op, while table
  // cells are isolating boundaries rather than joinable text blocks.
  const semanticBeforeTaskBoundary = await page
    .locator('[data-test="semantic-json"]')
    .textContent();
  await placeCaretInText(page, taskCaret.text, 0);
  await page.keyboard.press('Backspace');
  expect(await caretSnapshot(page)).toEqual({
    text: taskCaret.text,
    offset: 0,
    nodeType: 'task_item',
    inTableCell: false,
  });
  await expect(page.locator('[data-test="semantic-json"]')).toHaveText(
    semanticBeforeTaskBoundary,
  );

  const semanticBeforeTableBoundary = await page
    .locator('[data-test="semantic-json"]')
    .textContent();
  await placeCaretInText(page, tableCaret.text, 0);
  await page.keyboard.press('Backspace');
  expect(await caretSnapshot(page)).toEqual({
    text: tableCaret.text,
    offset: 0,
    nodeType: 'table',
    inTableCell: true,
  });

  const precedingCellText = 'Task item';
  await placeCaretInText(page, precedingCellText, precedingCellText.length);
  await page.keyboard.press('Delete');
  expect(await caretSnapshot(page)).toEqual({
    text: precedingCellText,
    offset: precedingCellText.length,
    nodeType: 'table',
    inTableCell: true,
  });
  await expect(page.locator('[data-test="semantic-json"]')).toHaveText(
    semanticBeforeTableBoundary,
  );
  await expect(page.locator('.pine-richtext-table-row')).toHaveCount(3);
  await expect(
    page.locator('.pine-richtext-table-row').nth(1).locator('[data-pine-table-cell="true"]'),
  ).toHaveCount(3);
  await expect(tableHosts).toHaveAttribute('data-pine-node-view-id', tableViewIdBeforeDelete);
  await expectStableEditorDom(page);
  expect(errors).toEqual([]);
  expect(panicLogs).toEqual([]);

  // Deleting the first character in a component-owned list item must project
  // the collapsed caret into the surviving text node, not an unpainted parent
  // element boundary.
  const secondTaskText = 'Unmount and restore this list to inspect lifecycle counts';
  await placeCaretInText(page, secondTaskText, 0);
  await page.keyboard.press('Delete');
  expect(
    await page.evaluate(() => {
      const selection = window.getSelection();
      return {
        nodeType: selection.anchorNode?.nodeType,
        text: selection.anchorNode?.textContent,
        offset: selection.anchorOffset,
      };
    }),
  ).toEqual({
    nodeType: 3,
    text: secondTaskText.slice(1),
    offset: 0,
  });

  // Deleting a keyboard-selected chip must collapse to a caret at the former
  // atom boundary. It must not leave a stale Node selection on shifted text.
  await placeCaretInText(page, mixedText, mixedText.length);
  await page.keyboard.press('ArrowRight');
  const selectedForDelete = await editorSelection(page);
  expect(selectedForDelete.model.type).toBe('node');
  await page.keyboard.press('Delete');
  await expect(architectureTag).toHaveCount(0);
  expect(await editorSelection(page)).toEqual({
    model: {
      type: 'text',
      anchor: selectedForDelete.model.anchor,
      head: selectedForDelete.model.anchor,
    },
    domCollapsed: true,
  });
  await expect(page.locator('.pine-richtext-tag[data-selection="node"]')).toHaveCount(0);
  await page.locator('[data-test="undo"]').click();
  await expect(architectureTag).toHaveCount(1);
  await expect(page.locator('[data-test="semantic-json"]')).toContainText('"id": "architecture"');
  await page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );

  const tableView = page.locator('.pine-richtext-table-view');
  const selectedCells = tableView.locator('[data-pine-table-cell="true"][data-selected="true"]');

  const semanticJson = page.locator('[data-test="semantic-json"]');
  const firstCell = tableView.locator('[data-pine-table-cell="true"]').first();
  // Let Playwright finish any viewport movement before sampling geometry.
  // A raw scrollIntoViewIfNeeded followed by boundingBox can observe an
  // intermediate frame while the document is still settling around the
  // sticky command bar.
  await firstCell.hover();
  const initialCellBox = await firstCell.boundingBox();
  if (!initialCellBox) {
    throw new Error('first table cell has no layout box');
  }

  const beforeColumnResize = await semanticJson.textContent();
  await page.mouse.move(
    initialCellBox.x + initialCellBox.width - 2,
    initialCellBox.y + initialCellBox.height / 2,
  );
  await page.mouse.down();
  await expect(tableView).toHaveAttribute('data-state', 'resizing');
  await page.mouse.move(
    initialCellBox.x + initialCellBox.width + 24,
    initialCellBox.y + initialCellBox.height / 2,
  );
  await page.mouse.up();
  const expectedColumnWidth = Math.round(initialCellBox.width + 26);
  await expect
    .poll(async () => (await semanticTable(page)).attrs.column_widths[0])
    .toBe(expectedColumnWidth);
  expect(await semanticJson.textContent()).not.toBe(beforeColumnResize);
  await expect(tableView).toHaveAttribute('data-state', 'ready');

  const resizedCellBox = await firstCell.boundingBox();
  if (!resizedCellBox) {
    throw new Error('resized table cell has no layout box');
  }
  const beforeRowResize = await semanticJson.textContent();
  await page.mouse.move(
    resizedCellBox.x + resizedCellBox.width / 2,
    resizedCellBox.y + resizedCellBox.height - 2,
  );
  await page.mouse.down();
  await expect(tableView).toHaveAttribute('data-state', 'resizing');
  await page.mouse.move(
    resizedCellBox.x + resizedCellBox.width / 2,
    resizedCellBox.y + resizedCellBox.height + 12,
  );
  await page.mouse.up();
  const expectedRowHeight = Math.round(resizedCellBox.height + 14);
  await expect
    .poll(async () => (await semanticTable(page)).content[0].attrs.height)
    .toBe(expectedRowHeight);
  expect(await semanticJson.textContent()).not.toBe(beforeRowResize);
  await expect(tableView).toHaveAttribute('data-state', 'ready');

  // The selectors are intentionally dormant when the table is neither
  // hovered nor selected; mirror the user's reveal gesture before clicking.
  await tableView.hover();
  await tableView.locator('.pine-richtext-table-column-selector[data-column="1"]').click();
  await expect(tableView).toHaveAttribute('data-selection', 'column');
  await expect(selectedCells).toHaveCount(3);

  await tableView.locator('.pine-richtext-table-row-selector[data-row="1"]').click();
  await expect(tableView).toHaveAttribute('data-selection', 'row');
  await expect(selectedCells).toHaveCount(3);

  await tableView.locator('.pine-richtext-table-select-table').click();
  await expect(tableView).toHaveAttribute('data-selection', 'table');
  await expect(selectedCells).toHaveCount(9);

  await firstCell.click({ modifiers: ['Shift'] });
  await expect(tableView).toHaveAttribute('data-selection', 'cells');
  await expect(selectedCells).toHaveCount(1);

  const moreTools = await openMoreTools(page);
  await page.keyboard.press('Escape');
  await expect(moreTools).not.toHaveAttribute('open', '');
  await expect(page.locator('[data-test="more-tools"]')).toBeFocused();
  await openMoreTools(page);
  await page.locator('[data-test="table-width"]').click();
  await expect(page.locator('[data-test="semantic-json"]')).toContainText('260');
  await expect(moreTools).not.toHaveAttribute('open', '');
  await openMoreTools(page);
  await page.locator('[data-test="table-height"]').click();
  await expect(page.locator('[data-test="semantic-json"]')).toContainText('58');

  const syncsBefore = Number(await page.locator('[data-test="task-syncs"]').textContent());
  await page.locator('.showcase-task-item__check').nth(1).click();
  await expect
    .poll(async () => Number(await page.locator('[data-test="task-syncs"]').textContent()))
    .toBeGreaterThan(syncsBefore);

  const unmountsBeforeSelfDelete = Number(
    await page.locator('[data-test="task-unmounts"]').textContent(),
  );
  await page.locator('[data-test="delete-task-self"]').first().click();
  await expect(taskViews).toHaveCount(1);
  await expect
    .poll(async () => Number(await page.locator('[data-test="task-unmounts"]').textContent()))
    .toBeGreaterThan(unmountsBeforeSelfDelete);
  expect(errors).toEqual([]);
  expect(panicLogs).toEqual([]);

  await selectText(page, 'open the BubbleMenu');
  await expect(page.locator('[data-test="bubble-menu"]')).toHaveAttribute('data-state', 'open');
  await openMoreTools(page);
  await page.locator('[data-test="stale-search"]').click();
  await expect(page.locator('[data-test="search-status"]')).toContainText('Rejected stale');

  const tagsBefore = await tagViews.count();
  await page.locator('[data-test="insert-tag"]').click();
  await expect(tagViews).toHaveCount(tagsBefore + 1);
  await expect(
    tableView.locator('.pine-richtext-tag[data-tag-id="typed-api"]'),
  ).toHaveCount(0);
  await page.locator('[data-test="undo"]').click();
  await expect(tagViews).toHaveCount(tagsBefore);
  await page.locator('[data-test="redo"]').click();
  await expect(tagViews).toHaveCount(tagsBefore + 1);

  await openMoreTools(page);
  await page.locator('[data-test="unmount-tasks"]').click();
  await expect(taskViews).toHaveCount(0);
  await expect
    .poll(async () => Number(await page.locator('[data-test="task-unmounts"]').textContent()))
    .toBeGreaterThanOrEqual(2);
  await openMoreTools(page);
  await page.locator('[data-test="restore-demo"]').click();
  await expect(taskViews).toHaveCount(2);

  const inspector = page.locator('.demo-inspector');
  await inspector.locator(':scope > summary').click();
  await expect(inspector).toHaveAttribute('open', '');
  const markdownOutput = page.locator('[data-test="markdown"]');
  await expect(markdownOutput).toBeVisible();
  await expect(markdownOutput).toContainText('|');
  await inspector.getByRole('button', { name: 'JSON' }).click();
  await expect(page.locator('[data-test="semantic-json"]')).toBeVisible();
  await expect(page.locator('[data-test="semantic-json"]')).toContainText('"type": "table"');
  await expect(page.locator('[data-test="semantic-json"]')).toContainText('"version": 1');
  await inspector.getByRole('button', { name: 'Markdown' }).click();
  await expect(markdownOutput).toBeVisible();
  expect(errors).toEqual([]);
  expect(panicLogs).toEqual([]);
});
