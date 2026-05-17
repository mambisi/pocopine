# RFC 079 - `pine-richtext` TablesExtension

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-17 |
| **Builds on** | Phase 6 C4 ([pluggable markdown contract](../docs/codex-reviews/phase-6-c4.md)) |
| **Related** | [`prosemirror-tables`](https://github.com/ProseMirror/prosemirror-tables) (reference, not port) |

## 1. Summary

Add a `TablesExtension` to `pine-richtext` covering GFM-shaped tables:
schema (`table`, `table_row`, `table_header_cell`, `table_cell`),
markdown serialize + parse via the Phase 6 C4 pluggable contract,
DOM rendering as `<table>` / `<thead>` / `<tbody>` / `<tr>` /
`<th>` / `<td>`, and a small command surface for cell navigation,
row/column insertion, and table deletion.

The extension is opt-in. Runtimes that don't include it get
zero table overhead (pipe-table markdown imports as plain
paragraphs per the Phase 6 C4 `ENABLE_TABLES` gate).

## 2. Problem Statement

Today `pine-richtext`'s schema covers `prosemirror-schema-basic`
(doc, paragraph, blockquote, heading, code_block, horizontal_rule,
image, hard_break) plus `prosemirror-schema-list` (bullet_list,
ordered_list, list_item) plus our `task_list` extension.

Three real use cases push for tables:

- Documentation editors (RFC bodies, internal docs, API references)
  use tables for parameter / response shapes constantly. Today a
  doc author who pastes a pipe-table markdown source gets four
  loose paragraphs.
- Comment threads benefit from tables less frequently but the
  Markdown→HTML round-trip path for user-generated content
  must not silently drop table syntax.
- The Phase 6 C4 markdown contract added `ENABLE_TABLES` plumbing
  but has no consumer. Shipping `TablesExtension` validates that
  the contract is sufficient for a real 4-node block shape.

## 3. Goals

- Schema node types: `table`, `table_row`, `table_header_cell`,
  `table_cell`. Content expressions mirror GFM table semantics
  (one header row, one or more body rows; cells contain inline
  content only — no nested blocks for v1).
- Markdown export via `markdown_node_emitters()`: emit
  `Tag::Table(alignments)` / `Tag::TableHead` / `Tag::TableRow` /
  `Tag::TableCell` events; `pulldown-cmark-to-cmark` renders
  the GFM `|...|...|` shape.
- Markdown import via `markdown_parse_rules()`: declarative
  `ParseMapping::Block` rules for each of the four shapes. The
  runtime auto-enables `pulldown_cmark::Options::ENABLE_TABLES`
  because the Table rule is registered (Phase 6 C4 gate).
- DOM rendering: standard `<table><thead><tr><th>…</th></tr></thead><tbody><tr><td>…</td></tr></tbody></table>`.
- Per-column alignment via a `table` attr (`alignments:
  Vec<Option<Alignment>>`) consumed by both the markdown emitter
  and the DOM renderer.
- Commands: `insert_table { rows, cols }`, `insert_row_above`,
  `insert_row_below`, `insert_column_left`, `insert_column_right`,
  `delete_row`, `delete_column`, `delete_table`, and the
  cell-navigation `Tab` / `Shift-Tab` / arrow-key bindings.
- Key bindings: `Tab` advances cell, `Shift-Tab` retreats, arrow
  keys at cell boundaries move to the adjacent cell (when one
  exists).
- Round-trip idempotence: `parse(serialize(doc))` is identical
  to `doc` for all GFM-supported table shapes.

## 4. Non-goals

- Merged cells (`rowspan` / `colspan`). GFM doesn't support them
  and CommonMark-flavored markdown can't round-trip them.
- Column-resize drag handles (the v1 of upstream `prosemirror-tables`
  has these; they're a UX feature on top of the schema, not a
  schema concern). May ship as a follow-up extension.
- Spreadsheet-style cell formulas, sorting, filtering. Out of
  scope for a markdown-shaped table.
- Nested blocks inside cells (paragraphs containing lists,
  blockquotes-in-cells). GFM tables allow only inline content
  per cell. If we ever want richer cell content, change the
  content expression in a follow-up RFC — but at that point
  markdown round-trip is lossy.
- HTML-only attributes (`<col>`, `<colgroup>`, table captions
  via `<caption>`). The markdown round-trip can't represent
  them.
- Migration from non-`TablesExtension`-aware runtimes. Apps
  building a doc editor without the extension and later
  enabling it get the same docs — there's no schema upgrade
  step needed because `table*` types weren't valid before, so
  nothing's there to migrate.

## 5. Schema

```rust
NodeSpec::new("table")
    .group("block")
    .content("table_header_row table_body_row+")  // OR just "table_row+"; see §5.3
    .defining()
    .attr("alignments", json!([]))  // Vec<Option<"left" | "right" | "center">>

NodeSpec::new("table_row")
    .content("(table_header_cell | table_cell)+")

NodeSpec::new("table_header_cell")
    .content("inline*")
    .marks(MarkPolicy::All)
    .defining()

NodeSpec::new("table_cell")
    .content("inline*")
    .marks(MarkPolicy::All)
    .defining()
```

### 5.1. Distinguishing header rows

GFM has exactly one header row (the row before the `|---|---|`
separator) followed by zero or more body rows. Two ways to
encode this in the schema:

**Option A** — distinct `table_header_row` / `table_body_row`
types. Stricter content match; rules out a doc with two header
rows or zero header rows.

**Option B** — single `table_row` type; the cell type
(`table_header_cell` vs `table_cell`) discriminates. Looser
schema but matches PM's `prosemirror-tables` shape.

Recommendation: **Option B** for parity with upstream + simpler
schema. The single-header-row invariant is enforced by the
markdown parser at import time (the first row's cells become
`table_header_cell`s, all subsequent rows' cells become
`table_cell`s).

### 5.2. Alignment attr

Per-column alignment is a `table`-level attribute, NOT a per-cell
attribute:

```rust
attrs: { "alignments": ["left", null, "right"] }
```

`null` means default (left). Stored on the table node so adding
a cell to a column doesn't require updating every cell. The
markdown emitter consumes this for the `|---:|:---:|:---|` line;
the DOM renderer adds `style="text-align:..."` per cell based on
its column index.

### 5.3. Empty-cell handling

GFM allows empty cells. Schema content `inline*` (not `inline+`)
permits them. The markdown parser creates an empty paragraph
when a cell has no content text; the renderer emits `<td></td>`.

## 6. Markdown emit / parse via the C4 contract

### 6.1. `markdown_node_emitters()`

```rust
fn markdown_node_emitters(&self) -> Vec<(String, NodeEmitter)> {
    use pulldown_cmark::{Event, Tag, TagEnd, Alignment};
    vec![
        ("table".into(), Arc::new(|node, _parent, _index, sink| {
            let alignments = node.attrs()
                .get("alignments")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().map(parse_alignment).collect())
                .unwrap_or_default();
            sink.push(Event::Start(Tag::Table(alignments)));
            sink.render_content(node);  // emits TableHead + TableRows
            sink.push(Event::End(TagEnd::Table));
        })),
        ("table_row".into(), Arc::new(|node, parent, index, sink| {
            // First row's cells are header cells → emit Tag::TableHead frame.
            let is_header_row = index == 0
                && node.content().iter().all(|c| c.type_name() == "table_header_cell");
            if is_header_row {
                sink.push(Event::Start(Tag::TableHead));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::TableHead));
            } else {
                sink.push(Event::Start(Tag::TableRow));
                sink.render_content(node);
                sink.push(Event::End(TagEnd::TableRow));
            }
        })),
        ("table_header_cell".into(), Arc::new(cell_emitter)),
        ("table_cell".into(), Arc::new(cell_emitter)),
    ]
}

fn cell_emitter(node, _parent, _index, sink) {
    sink.push(Event::Start(Tag::TableCell));
    emit_inline_content(node, sink);
    sink.push(Event::End(TagEnd::TableCell));
}
```

### 6.2. `markdown_parse_rules()`

```rust
fn markdown_parse_rules(&self) -> Vec<MarkdownParseRule> {
    vec![
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::Table),
            maps_to: ParseMapping::Block {
                node_type: "table".into(),
                get_attrs: Some(Arc::new(|event| {
                    let mut attrs = Attrs::new();
                    if let Event::Start(Tag::Table(alignments)) = event {
                        let v: Vec<Value> = alignments.iter().map(alignment_to_json).collect();
                        attrs.insert("alignments".into(), json!(v));
                    }
                    attrs
                })),
            },
        },
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::TableHead),
            maps_to: ParseMapping::Custom(/* wraps next TableRow as header */),
        },
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::TableRow),
            maps_to: ParseMapping::Block {
                node_type: "table_row".into(),
                get_attrs: None,
            },
        },
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::TableCell),
            maps_to: ParseMapping::Block {
                // Header vs body cell decision lives in TableHead
                // logic — see §6.3.
                node_type: "table_cell".into(),
                get_attrs: None,
            },
        },
    ]
}
```

### 6.3. Header-row detection on import

Pulldown-cmark emits `Tag::TableHead` around the header row's
cells, then `Tag::TableRow` around each body row. We need each
cell inside `TableHead` to become a `table_header_cell` instead
of `table_cell`. Two options:

- **A**: a `Custom` rule for `TableHead` that flips a walker
  flag, then `TableCell`'s declarative rule reads the flag. Adds
  `ParseSink::set_table_cell_is_header(bool)`-style API (or a
  generic "context flag" API).
- **B**: declarative `Block` rule for `TableCell` consults the
  surrounding stack to detect whether the enclosing `TableRow`
  is inside a `TableHead`. Requires new `ParseSink` API to
  inspect the stack.

Option A is symmetric with the existing GFM task-list pattern
(`ParseSink::flag_enclosing_list_as_task`) and probably the
right call. We'd add a small generic mechanism:

```rust
impl ParseSink<'_> {
    /// Set a typed flag on the closest enclosing block builder
    /// of the given type name. The flag is read by descendants
    /// at finalization time.
    pub fn flag_enclosing_block(&mut self, type_name: &str, key: &str, value: Value);

    /// Read a flag set by an enclosing block. Returns None if no
    /// matching builder is open or the flag isn't set.
    pub fn read_enclosing_block_flag(&self, type_name: &str, key: &str) -> Option<Value>;
}
```

The TableHead `Custom` rule writes `("in_header", true)` on the
enclosing `table`; the TableCell `Custom` rule reads it and
chooses `table_header_cell` vs `table_cell`. After TableHead
closes, it clears the flag.

This generalizes beyond tables — any extension that needs
"contextual children" gets a clean primitive.

## 7. Commands

```rust
pub const COMMANDS: &[(&str, NamedCommand)] = &[
    ("insert_table", insert_table_command()),       // args: { rows, cols, with_header }
    ("insert_row_above", row_command(Direction::Above)),
    ("insert_row_below", row_command(Direction::Below)),
    ("insert_column_left", column_command(Direction::Left)),
    ("insert_column_right", column_command(Direction::Right)),
    ("delete_row", delete_row_command()),
    ("delete_column", delete_column_command()),
    ("delete_table", delete_table_command()),
];
```

All commands operate on the table containing the current cursor.
`insert_row_*` clones the column count from the existing
`table.attrs.alignments`; `insert_column_*` extends every row
+ updates `alignments`. `delete_row` collapses the row; if it's
the only header row, the next body row promotes to header
(matches GFM's implicit "first row is header" rule).

## 8. Key bindings

| Combo | Command |
|---|---|
| `Tab` | Move to next cell. If at last cell, append a new row (mirrors PM behavior). |
| `Shift-Tab` | Move to previous cell. No-op at first cell. |
| `Enter` inside a cell | Default: insert a `hard_break` (cells are inline-only; no paragraph split). |

The `Tab` "append row at end" behavior is the canonical PM-tables
UX. Without it tabbing out of the last cell loses the user's
position and they have to click somewhere new. With it, the
user can keep typing forever.

## 9. DOM rendering

Renderer maps:

```
table → <table>
table_row (containing only table_header_cell) → wrap in <thead><tr>...</tr></thead>
table_row (containing table_cells) → <tr> inside <tbody>
table_header_cell → <th>
table_cell → <td>
```

Per-cell `style="text-align:..."` derived from the table's
`alignments` attr indexed by cell position.

The renderer needs to group consecutive body rows into a single
`<tbody>` wrapper — the reconciler walks `table` children and
emits `<thead>` for the first all-header-cell row, then a single
`<tbody>` containing all subsequent rows.

## 10. Selection inside tables

Cells are textblocks. Standard selection rules apply:

- A text selection inside a cell behaves like any inline
  selection (drag-to-extend, double-click selects word, etc.).
- A selection spanning two cells is allowed but operations
  treat each cell independently. Replace-selection on a
  cross-cell text range deletes BOTH partial cell contents and
  inserts the replacement into the first.
- A `Selection::Node` on a `table_cell` is allowed (selects the
  whole cell). Used by `delete_column` / `delete_row` after the
  command runs to position the cursor on the adjacent cell.

`NodeSelection` on a `table` itself is permitted (matches PM)
so `delete_table` can run on it.

## 11. Phasing

| Phase | Scope |
|---|---|
| **T1** | Schema + DOM rendering + markdown emit/parse + the basic 4-node + `insert_table` command. Round-trip tests prove parse∘serialize is idempotent for hand-built tables. |
| **T2** | Cell navigation (`Tab` / `Shift-Tab`), `insert_row_*` / `insert_column_*` / `delete_*` commands. Demo wires a toolbar dropdown for "Insert Table". |
| **T3** | Per-column alignment via toolbar / context menu. Selection edge cases (cross-cell, table-level). |
| **T4** | Playwright coverage: paste pipe-table markdown via the import box, type into cells, tab navigation, delete row/column, export back. |

Each phase ends with a Codex review per the existing pattern.

## 12. Risks

| Risk | Mitigation |
|---|---|
| Header-row detection via `Custom` parse rule requires `ParseSink::flag_enclosing_block` / `read_enclosing_block_flag` — new API surface on a Phase 6 C4 type | Land the API in T1 alongside the table extension; document the pattern in `docs/extensions.md` so future extensions (callouts, definition lists) can reuse it. |
| Reconciler-level `<thead>` / `<tbody>` grouping is a new rendering pattern (every other node-type emits a single HTML element per model node) | Implement as a one-off in the table emitter, not as a generic renderer feature. The reconciler stays node-type-agnostic; the table renderer wraps its output. |
| `Tab`-at-last-cell appends a row, but the user might want literal Tab indentation | Tab indent inside a cell is rare in markdown; trade-off favors PM-tables UX. If real complaints surface, add a Shift-modifier opt-in for literal Tab. |
| Tables-aware markdown serialization emits `Vec<Alignment>` which pulldown-cmark-to-cmark serializes as `|---:|:---:|:---|`; pulldown-cmark's `Alignment` enum has 4 variants (`None`, `Left`, `Center`, `Right`) but our schema uses 3 (left/center/right) plus null | Map `null` → `Alignment::None`. Reverse mapping treats `None` as `null` in JSON. |
| Schema content match `(table_header_cell \| table_cell)+` is permissive — could allow a row with mixed header + body cells, which doesn't round-trip through markdown | Document the invariant (first row's cells are headers; subsequent rows' cells are body); enforce in the markdown parser. Don't enforce at the schema level — the looser shape preserves Option B's simplicity. |

## 13. Out-of-scope drafts

- **Column-resize drag handles.** Defer to a follow-up RFC. The
  PM upstream version uses a third-party plugin layer; we'd
  follow the same pattern.
- **Table CSV import / export.** A standalone extension that
  consumes / produces CSV via the same `markdown_node_emitters`
  / `markdown_parse_rules` shape but targeting a different
  serializer. Different code path; different RFC.
- **Spreadsheet formulas.** Out of scope.
