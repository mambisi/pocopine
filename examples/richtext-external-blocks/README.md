# Pine rich text — typed external blocks

An isolated, interactive reference page for the RFC-113 typed node-view stack.
The document contains versioned task items, tables, and inline tags; all UI is
mounted through typed runtime contributions and compile-time-owned content
outlets. No component tag, content selector, document position, or HTML string
is authored by the application.

## Launch

From the workspace root:

```bash
cargo run -p pocopine-cli -- dev --path examples/richtext-external-blocks
```

The static dev server defaults to `http://localhost:5243` and prints the final
URL; pass `--port <PORT>` when needed. A build-only check uses the same project
path:

```bash
cargo run -p pocopine-cli -- build --path examples/richtext-external-blocks
```

## What to try

- Toggle a task checkbox, then use its local **Delete** control. That component
  handler dispatches the transaction that removes its own semantic node, which
  proves callback scheduling reaches a safe point before clean unmount. Use
  **Unmount tasks** / **Restore document** to watch all lifecycle counts change.
- Drag table cell edges for column/row resize. Use the table's **A/B/C**,
  **1/2/3**, and **Table** controls for column, row, and whole-table selection.
  Hold Shift, Control, or Command while dragging across cells for a rectangular
  cell selection.
- Select text to open the BubbleMenu. Search commands or run the stale-result
  proof; a superseded provider token is rejected while the current result keeps
  its mapped selection bookmark.
- Insert a tag, then use ArrowLeft/ArrowRight to enter and leave the chip.
  Backspace/Delete and the visible remove affordance delete it as one atom.
- Use Undo/Redo and compare the live semantic JSON with the intentionally
  loss-aware Markdown output.

## Browser smoke

Build the stable, unhashed smoke artifact and run the focused spec through the
workspace script:

```bash
npm run test:richtext-external-blocks
```

The equivalent explicit commands are:

```bash
wasm-pack build examples/richtext-external-blocks --target web --out-dir pkg
PLAYWRIGHT_SERVE_DIR=examples/richtext-external-blocks \
  npx playwright test richtext-external-blocks.spec.mjs
```

The smoke covers typed native hosts, self-deleting component handlers without a
re-entrant `RefCell` panic, task lifecycle updates, BubbleMenu stale search
rejection, tag insertion/history, table resize and every table selection mode,
and both output formats.
