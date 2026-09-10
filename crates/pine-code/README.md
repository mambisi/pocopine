# pine-code

A bounded native code editor for Pocopine. Rust owns the flat text document,
revisions, selection and undo history. A `contenteditable` surface receives
browser input and renders direct syntax spans; the browser draws the caret
and native selection. The default crate has no browser dependency.

Enable the component with `pine-code = { workspace = true, features = ["view"] }`
and register `pine_code::client::PineCodeEditor` on the browser App.

```html
<pine-code-editor pp-ref="source" initial-value="fn main() {}"
  language="rust" line-numbers aria-label="Source code">
</pine-code-editor>
```

The bubbling `pine:code:ready` event means the handle is available. Resolve it
from the component element using `CodeEditorHandle::from_element(&element)`.
A handle belongs to that mounted instance; after removal, every operation
returns `Disposed`. Remounting creates a new handle.

## Document ownership

`initial-value` is read once after parent bindings settle. Subsequent bound
seed changes do not overwrite edits. Use `load_document(text, revision)` to
replace the document, normalize line endings, reset selection/scroll/history,
and advance the revision even when the text is identical. Limits are mount-time
configuration. Dynamic props include language, theme, line numbers, read-only,
disabled and indentation options.

The model stores LF text, preserves tabs and final newlines, and uses checked
UTF-8 byte offsets. Browser UTF-16 positions are converted at the view boundary.
A `Transaction` contains a base revision and disjoint changes in the original
document's coordinates. Stale revisions and invalid scalar boundaries are
rejected before applying the transaction.

```rust
use pine_code::{Change, ChangeSet, EditOrigin, Editor};

let mut editor = Editor::new("", Default::default(), Default::default())?;
let changes = ChangeSet::new(editor.state().document(), vec![
    Change::replace(0, 0, "let answer = 42;\n"),
])?;
let transaction = editor.state().transaction(changes, EditOrigin::Api);
let change = editor.dispatch(transaction, 0)?;
# Ok::<(), pine_code::CodeError>(())
```

Keep the `Subscription` returned by `on_change`, `on_finalize`,
`on_composition_change`, `on_view_status`, or `on_presentation_status` alive
for the desired lifetime. Dropping it unsubscribes. Change notifications
carry metadata; read `text()` when the application needs a complete string.
Callbacks can read state but synchronous mutation returns `ReentrantDispatch`.
Defer parent updates using Pocopine's `defer_update` where appropriate.

## Input, history and lifecycle

Typing uses browser input, then imports the affected logical line into one
Rust transaction. Structural edits use a full DOM reader. Enter, indentation,
clipboard, drop, undo and redo route through checked commands. Pasted/copied
content is plain text. Literal search and replacement operate on committed
text; the result cache holds 10,000 ranges, while counting, navigation and
replace-all include all matches.

During composition, the browser owns its candidate DOM. No token or selection
patches touch it. Reads return committed text with `snapshot.composing = true`;
applications should use that flag when saving. Text/selection commands,
focus, find navigation and recovery return `CompositionActive`. Terminal
input commits one undo group. Read-only/disabled prop changes wait for an
already active writable composition to end.

`on_finalize` runs before framework detachment and before any departing scope
is released. Its owned `CodeFinalSnapshot` contains committed text and an
optional interrupted composition draft, including validation errors. Preserve
or offer recovery of that draft explicitly; it is not automatically committed.
Callback reads remain valid; writes return `Finalizing`. The editor then
releases listeners, observers and subscriptions. Framework keep-alive hides
cached components without disposing them. Direct external DOM removal can
only report `UnexpectedDetach` when explicit framework cleanup later runs.

Undo stores forward/inverse edits and selections. Adjacent typing/deletion
may group within 500 ms; paste, composition, commands and selection jumps
close groups. Defaults are 200 groups and 8 MiB retained history. Oversized
history entries still commit, with `history_retained = false`.

## Rendering and configuration

An accepted edit returns `Ok(CommitOutcome)` even if the DOM update fails.
Check `view_status`; do not retry that edit. `on_change` fires once with the
accepted revision and failed status. `recover_view()` reconstructs the view
from the latest committed model without another document event.

- Languages: `plain` (default), `json`, `rust`. Rust supports nested comments,
  raw strings, character literals and lifetimes; JSON supports incomplete
  strings and numeric forms. These are line tokenizers, not compiler parsers.
- Themes: `auto`, `light`, `dark`; forced colors use system colors. Syntax
  styles change color only, preserving text metrics.
- `tab-size`: 1–32, default 4. `indent`: empty for the language default,
  1–32 spaces, or one tab. Rust defaults to four spaces; other modes use two.
- `tab-behavior="focus"` is the default. Opt into `indent`; Escape followed
  by Tab exits that mode. Read-only always permits Tab navigation.
- Read-only preserves `contenteditable` for native navigation and copying,
  while rejecting/restoring mutations. Disabled removes keyboard and pointer
  focus. Provide `aria-label` or `aria-labelledby`; accessible naming and
  descriptions are forwarded to the actual textbox.
- Defaults: 256 KiB normalized text, 10,000 lines, 32 KiB per logical line. Invalid initial documents
  remain visible and immutable with an error. Runtime limits reject edits
  atomically; they never truncate content.
- Highlighting defaults: 8,000 spans, 8 KiB per line, 4 ms work slices.
  Unsupported languages and budget overflow fall back to plain text. After a
  span-budget overflow, plain presentation persists until a load or language
  change starts a fresh pass, avoiding repeated scans on each keystroke.

The renderer mounts all lines and scrolls without wrapping. It does not
provide virtualization, multiple cursors, semantic parsing, completion, LSP,
collaborative editing, or rich-text document semantics. See the
[validation report](../../examples/code-editor/VALIDATION.md) for measured
limits and remaining manual browser checks.

## Development

```sh
cargo test -p pine-code
cargo check -p pine-code --target wasm32-unknown-unknown
cargo clippy -p pine-code --features view --all-targets --target wasm32-unknown-unknown -- -D warnings
wasm-pack test --firefox --headless crates/pine-code --features view
pocopine dev --path examples/code-editor
```

The [example](../../examples/code-editor) demonstrates save/reset, independent
editors, literal find/replace and interrupted-draft recovery.
[CodeMirror reference attribution](NOTICE.codemirror.md) records pinned
upstream sources and their MIT notice.
