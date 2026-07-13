# Typed table view

Enable `table-view` and register the same extension through the model + view
builder lane:

```rust
use pine_richtext::runtime::RuntimeBuilder;
use pine_richtext_extensions::tables::TablesExtension;

let runtime = RuntimeBuilder::new()
    .with_view(TablesExtension)
    .try_build()?;
```

`TablesExtension` contributes the typed `table`, `table_row`,
`table_header_cell`, and `table_cell` model nodes. Its browser view pairs only
the outer `TableNode` with `PineRichTextTable`. Rows and cells are typed
`NodeDomSpec` contributions compiled to `<tr>`, `<th>`, and `<td>` by Pine's
validated structural renderer. The editor owns their editable descendants
under the component's one
`<tbody pp-owned-content>` outlet.

The split is deliberate:

- the model persists rectangular cell content, alignment, column widths, and
  row heights;
- the component owns selectors, resize feedback, and other chrome;
- a resize mutates DOM only while the pointer is down, then commits exactly
  one typed transaction on pointer-up;
- pointer cancel restores model geometry without dispatching;
- cell, row, column, and whole-table selections all become
  `Selection::Cells`, never a fake linear text range;
- the cloned `NodeViewHandle<TableNode>` is the opaque host-generation token.
  A gesture also records its start position, so a removed, replaced, or moved
  table cannot receive a stale commit.

Text editing keeps normal pointer behavior. Hold Shift, Control, or Command
while dragging across cells to create a rectangular cell selection. The
toolbar exposes keyboard-focusable buttons for whole-table, row, and column
selection. Every selector uses `aria-pressed`; cells use `aria-selected`.

## Styling hooks

The shipped stylesheet uses stable classes and data states rather than
persisting CSS in the document:

- `.pine-richtext-table-view`
- `.pine-richtext-table`
- `.pine-richtext-table-row`
- `.pine-richtext-table-cell`
- `.pine-richtext-table-header-cell`
- `[data-selected="true"]`
- `[data-state="resizing"]`
- `[data-resize-axis="column" | "row"]`
- `[data-selection="none" | "cells" | "row" | "column" | "table"]`

Applications can override:

- `--pine-richtext-table-border`
- `--pine-richtext-table-header-background`
- `--pine-richtext-table-selected-background`
- `--pine-richtext-table-selected-outline`
- `--pine-richtext-table-handle-size`
- `--pine-richtext-table-handle-gap`
- `--pine-richtext-table-cell-padding-block`
- `--pine-richtext-table-cell-padding-inline`
- `--pine-richtext-table-min-column-width`
- `--pine-richtext-table-min-row-height`
- `--pine-richtext-table-radius`
- `--pine-richtext-table-selector-background`
- `--pine-richtext-table-selector-color`

Column widths and row heights are semantic numeric attrs. Colors, borders,
spacing, fonts, handles, and selected-state treatment remain application CSS.
