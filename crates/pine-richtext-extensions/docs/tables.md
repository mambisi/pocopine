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
- the component owns contextual overlay handles, resize/reorder feedback, and
  other chrome;
- a resize mutates DOM only while the pointer is down, then commits exactly
  one typed transaction on pointer-up;
- a row/column drag likewise previews locally and commits one semantic move on
  pointer-up; cancellation and same-position drops create no history entry;
- pointer cancel restores model geometry without dispatching;
- cell, row, column, and whole-table selections all become
  `Selection::Cells`, never a fake linear text range;
- the cloned `NodeViewHandle<TableNode>` is the opaque host-generation token.
  A gesture also records its start position, so a removed, replaced, or moved
  table cannot receive a stale commit.

Text editing keeps normal pointer behavior. Hold Shift, Control, or Command
while dragging across cells to create a rectangular cell selection. Hover or
focus the table to reveal compact Pine six-dot grip handles outside the row
edge and above each column: click one to select the table, row, or column, and
drag a body-row or column handle to reorder it. Selecting one row or column also
reveals two small previous/next buttons below the table, providing a click and
keyboard alternative to dragging. The canonical header row remains selectable
but pinned. The controls use a comfortable hit target around a much smaller
glyph, consume no permanent top or left gutter, remain keyboard-focusable, and
use `aria-pressed`; cells use `aria-selected`. A pointer press outside the
table dismisses the painted selection, pressed handles, and reorder controls.
The editor retains the semantic cell rectangle until another editor selection
replaces it, so pointer-preserving commands in an external toolbar still act
on the intended cells.

Semantic reordering is available through `move_row(source, target)` and
`move_column(source, target)`, and through the same `move_row` / `move_column`
named commands with explicit `{ "source": ..., "target": ... }` arguments.
The target is the final index after the move. Header row zero stays pinned;
body-row moves cannot cross into it. Column moves reorder every row together
with `TableAttrs::column_widths`. View-dispatched moves keep the moved row or
column selected so the contextual controls support repeated keyboard/click
moves; direct document commands keep a text selection in the moved item while
preserving its orthogonal cell coordinate. Invalid and same-index moves produce
no transaction.

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
- `[data-state="moving"]`
- `[data-resize-axis="column" | "row"]`
- `[data-move-axis="column" | "row"]`
- `[data-move-source="true"]`
- `[data-move-target="before" | "after"]`
- `[data-selection="none" | "cells" | "row" | "column" | "table"]`

Applications can override:

- `--pine-richtext-table-border`
- `--pine-richtext-table-header-background`
- `--pine-richtext-table-selected-background`
- `--pine-richtext-table-selected-outline`
- `--pine-richtext-table-handle-size`
- `--pine-richtext-table-handle-glyph-inline-size`
- `--pine-richtext-table-handle-glyph-block-size`
- `--pine-richtext-table-handle-gap`
- `--pine-richtext-table-handle-radius`
- `--pine-richtext-table-handle-color`
- `--pine-richtext-table-handle-background`
- `--pine-richtext-table-handle-hover-color`
- `--pine-richtext-table-handle-hover-background`
- `--pine-richtext-table-handle-selected-color`
- `--pine-richtext-table-handle-selected-background`
- `--pine-richtext-table-handle-selected-border`
- `--pine-richtext-table-handle-selected-shadow`
- `--pine-richtext-table-handle-focus`
- `--pine-richtext-table-handle-idle-opacity`
- `--pine-richtext-table-handle-transition`
- `--pine-richtext-table-move-indicator`
- `--pine-richtext-table-move-indicator-size`
- `--pine-richtext-table-move-source-opacity`
- `--pine-richtext-table-cell-padding-block`
- `--pine-richtext-table-cell-padding-inline`
- `--pine-richtext-table-min-column-width`
- `--pine-richtext-table-min-row-height`
- `--pine-richtext-table-radius`
- `--pine-richtext-table-selector-background`
- `--pine-richtext-table-selector-color`

Column widths and row heights are semantic numeric attrs. Colors, borders,
spacing, fonts, handles, and selected-state treatment remain application CSS.
