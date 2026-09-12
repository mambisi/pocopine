# RFC-124: `pine-code` — a native code text editor

| Field | Value |
|---|---|
| **Status** | Implemented; release validation pending |
| **Created** | 2026-09-06 |
| **Proposed crate** | `pine-code` |
| **Component** | `PineCodeEditor` / `<pine-code-editor>` |
| **References** | `pine-richtext`, CodeMirror 6 `state` / `view` / `language` / `commands` |
| **Delivery** | Specification only; APIs and paths below are proposed unless identified as existing |

## 1. Decision and scope

Build a small, reusable code editor in Rust and Pocopine. It should be easy to
embed in a settings form, JSON inspector, query panel, or source preview.
Here, **native** means the text model, editing commands, history, highlighting,
and component lifecycle belong to Rust/Pocopine, compiled to WASM for the
browser. Browser text input uses the platform DOM. This is not a desktop
widget toolkit or a JavaScript CodeMirror wrapper.

Use CodeMirror 6 as a behavioral and architectural reference, and
`pine-richtext` as the local integration reference. Give code its own flat
text model: source text must not acquire rich-text nodes, marks, schema
validation, or Markdown serialization.

The editor uses a **Rust-owned contenteditable view**. It renders logical
lines and syntax-token spans directly inside one editable DOM surface. Rust
owns the document, transactions, selection mapping, reconciliation, and undo
history; the browser provides text input, IME, native caret/selection, and
layout. There must be no visible or hidden textarea or mirrored text overlay.

Keep the first release small by rendering the complete bounded document,
with incremental patches to changed lines. Viewport virtualization and a
large-file text buffer can follow without replacing the input surface.

### Initial release

| Capability | Required behavior |
|---|---|
| Text editing | One selection; insert, delete, replace, cut, copy, paste, native navigation |
| Code presentation | Monospace, optional line numbers, horizontal/vertical scrolling, light/dark tokens |
| Languages | Plain text, JSON, Rust; language selection is explicit |
| Commands | Undo/redo, indent/outdent, preserve indentation on Enter, select all |
| Search | Literal, case-sensitive find next/previous, replace current/all |
| Integration | Initial text, typed handle, compact change notifications, explicit document replacement |
| Input quality | IME, Unicode, touch selection, keyboard access, read-only and disabled states |
| Size | Default maximum 256 KiB normalized UTF-8, 10,000 logical lines, and 32 KiB per logical line |

All size limits apply. They remain subject to the performance gate in §11;
the implementation's measurements and adjustments are recorded in the example
validation report.
Empty text is one line; a final newline adds an empty final line.

### Deferred

Viewport virtualization, multiple cursors, rectangular selection, soft wrapping,
folding, minimaps,
diff/merge views, completion, snippets, automatic bracket insertion, semantic
diagnostics, LSP, formatting, collaboration, file tabs, filesystem access, and
code execution are outside the first release. SQL, JavaScript/TypeScript,
HTML/CSS, Markdown, and mixed `.poco` highlighting can follow independently.
Apps own persistence and Save/Run actions.

## 2. What exists in this repository

The following are existing references inspected for this draft:

| Reference | Reuse or lesson |
|---|---|
| [`pine-richtext` manifest](../crates/pine-richtext/Cargo.toml) | Pure core by default, optional browser view |
| [`state`](../crates/pine-richtext/src/state.rs), [`history`](../crates/pine-richtext/src/history.rs) | State transitions and undo belong to the model, with commands dispatched through one owner |
| [`view/root.rs`](../crates/pine-richtext/src/view/root.rs) | Initial-content seed and compact change event avoid a parent document round trip on every keystroke |
| [`view/input.rs`](../crates/pine-richtext/src/view/input.rs), [`view/selection.rs`](../crates/pine-richtext/src/view/selection.rs) | Browser input and position conversion need dedicated adapters and regression coverage |
| [`view/reconciler.rs`](../crates/pine-richtext/src/view/reconciler.rs), [`selection_observer.rs`](../crates/pine-richtext/src/view/selection_observer.rs) | Incremental DOM reconciliation and selection observation without repainting during selection drag |
| [`richtext` example](../examples/richtext/src/lib.rs) | Parent initializes content and reads live state through an editor handle |
| [`richtext` browser tests](../tests/playwright/richtext.spec.mjs), [`performance tests`](../tests/playwright/richtext.perf.spec.mjs) | Browser interaction and latency deserve explicit fixtures |
| [`pp-if` removal](../crates/pocopine-core/src/directives/if_.rs), [`pp-for` removal](../crates/pocopine-core/src/directives/for_.rs), [`subtree release`](../crates/pocopine-core/src/mount.rs) | Some removal paths detach before cleanup; attached-surface finalization in §5 requires a framework change |
| [`website/build.rs`](../examples/website/build.rs) | Existing Syntect highlighting is build-time HTML generation; it is not an editable browser service |

Reuse patterns and relevant test cases first. Do not make `pine-code` depend
on `pine-richtext`, copy its tree transforms into a text buffer, or extract a
shared editor framework before two implementations demonstrate a useful
common boundary. Existing comments and examples are not sufficient evidence
of behavior; implementation should follow the live dispatch paths and tests.

CodeMirror documents a state/view split, transaction-based updates, position
mapping, UTF-16 offsets, and viewport rendering. We follow its separation of
state and contenteditable view, using checked Rust byte offsets in the core.
Viewport rendering is deferred within the initial document limits.
[CodeMirror system guide](https://codemirror.net/docs/guide/).

The four upstream Git repositories were also cloned and inspected directly;
§12 pins their revisions and maps source files to implementation phases.
`view/src/editorview.ts` creates a contenteditable surface, with document
tiles, height mapping, and a DOM observer supporting it. Use its input,
selection, DOM-reading, and reconciliation boundaries as references for the
Rust view. Browser-specific fixes still need reproduction and tests; the
initial implementation does not need the full tile/height-map machinery.

## 3. Crate and component boundaries

Proposed layout:

```text
crates/pine-code/
  Cargo.toml
  src/
    lib.rs
    text.rs             # text, positions, line lookup, UTF conversion
    change.rs           # validated replacements, mapping, inverse edits
    state.rs            # committed document, selection, revision
    commands.rs         # pure editing operations
    history.rs
    search.rs
    language/           # presentation tokens and legacy standalone tokenizer helpers
    syntax/             # language registry, optional packages, Tree-sitter parser
    client/
      mod.rs
      component.rs
      input.rs          # input/clipboard events and composition state machine
      selection.rs      # DOM points <-> checked model positions
      dom_reader.rs     # browser mutations -> normalized logical text
      observer.rs       # scoped mutation/selection subscriptions
      render.rs         # line views, token spans, gutter
      reconciler.rs     # minimal patches with selection/IME preservation
      handle.rs         # live editor access and subscriptions
      PineCodeEditor.poco
      pine_code.css
  tests/
examples/code-editor/   # future standalone Pocopine example
```

The default crate exposes target-independent core types and requires no DOM,
server runtime, parser service, or JavaScript editor dependency. A `view`
feature enables the browser component. Put browser modules under `client`,
gated once at the crate root by `all(feature = "view", target_arch = "wasm32")`;
put browser dependencies in the corresponding Cargo target table. Client
submodules do not repeat platform gates. The first release has no SSR editor
component; host code can still process the core text types.

Follow the workspace's [client/server module convention](../.agents/skills/client-server-modules/SKILL.md).
Applications register `PineCodeEditor` and declare it in their browser-side
parent's `uses` registry. Do not add the editor to `pine::register_all()` or
make ordinary Pine inputs pull in its code. The example's browser app module
must be target-gated accordingly.

Keep the public names stable as internals evolve. There is no `EditorV1`
type tree and no serialized editor-state format in this release.

## 4. Text, positions, and transactions

### Text representation

`TextDocument` privately owns a UTF-8 `String` and a line-start index. Public
access is read-only. Creating a new state may copy the string and rebuild the
index in O(document length). This is acceptable only within the published
size and latency envelope. A rope is not a prerequisite for this release;
the private representation leaves that choice open.

Use `TextOffset` for a zero-based **UTF-8 byte offset** and `TextRange` for a
half-open `[from, to)` range. Every endpoint must be within the document and
on a Unicode scalar boundary. `Selection` stores anchor/head and preserves
backward selection; a collapsed selection is a caret. One selection is enough
for the initial API.

DOM `Selection`/`Range` offsets inside text nodes use UTF-16; offsets on
element nodes are child indexes. `client/selection.rs` must distinguish those
cases, account for logical line breaks, and traverse token wrappers as
transparent presentation. Convert text-node offsets through checked helpers
in `text.rs`. Reject invalid API positions and never confuse byte offsets,
UTF-16 offsets, and child indexes. The existing rich-text adapter converts to
Unicode scalar positions, so its numeric offsets cannot be reused directly.

Native navigation/deletion uses the browser where reliable, including
grapheme and bidirectional-text behavior. Any command that moves/deletes a
displayed character must use grapheme boundaries, not `+ 1` on bytes. Line
numbers exposed to users are one-based. DOM positions outside this editor
must not overwrite its saved model selection.

Normalize CRLF and bare CR to LF on load and insertion. `text()` returns LF
text; `export_text(LineEnding)` can produce LF or CRLF. A load reports whether
normalization occurred. Mixed original line endings are not preserved; this
is an explicit import contract, not a byte-preserving file editor. Preserve
tabs, spaces, trailing whitespace, final-newline presence, and Unicode text;
do not autoformat or apply Unicode normalization. Binary decoding is app work.

### Atomic editing

`EditorState` holds the document, selection, and `DocumentRevision`.
`Transaction` contains a base revision, a `ChangeSet`, an optional resulting
selection, and an edit origin. All changes in one set address the **same
pre-edit document**, including replace-all and multiline indentation.

The transition must:

1. Check the expected document revision, every range, and size limits before
   publishing any state. Normalize inserted line endings before measuring.
2. Sort disjoint changes; reject overlapping ranges and ambiguous insertions
   at the same position. Commands consolidate adjacent or shared-boundary
   operations into an unambiguous replacement before dispatch.
3. Build the result from unchanged slices and inserted strings in one pass;
   never repeatedly shift offsets against a partially edited document.
4. Validate an explicitly supplied selection in the resulting document, or
   map the current selection through the changes with explicit endpoint bias.
5. Publish the committed document, selection, revision, and history together.
   Attempt DOM reconciliation, then deliver one update with the commit outcome
   and view status. A rendering failure cannot turn a committed edit into a
   rejected transaction; §8 defines the result and notification ordering.

`ChangeSet` supports application, inversion against the original text, and
position mapping. At an insertion, `Before` remains before inserted text and
`After` moves past it. A position inside a replaced range maps to the start or
end of its replacement according to bias. Commands that insert at the caret
explicitly select the end of the inserted text. When a transaction omits the
resulting selection:

- Map a collapsed caret with `After` bias.
- For a nonempty selection, map the lower endpoint with `After` and the upper
  endpoint with `Before`, then restore its original anchor/head direction.
  Inserting text exactly outside either edge must not extend the selection.
- If replacement swallows the selected range and these mapped endpoints cross,
  collapse to the mapped lower endpoint, at the replacement's end. Never
  publish an inverted ordered range.

For example, selecting `bc` in `abcd` gives `[1, 3)`. Inserting `X` at byte 3
produces `abcXd` with `bc` still selected, rather than including `X`. Repeat
that fixture with a backward selection. Callers needing different behavior
supply a checked resulting selection. Empty changes with unchanged selection
are no-ops. The endpoint biases follow `state/src/selection.ts`; the default
caret bias and crossing-range collapse above are explicit Pocopine choices.

Revision increases on each committed text change, including undo/redo, and
on an explicit document load, even if the loaded text is identical. It never
resets during an editor instance's lifetime. Selection/focus/highlight changes
do not advance it. Stale edits return `StaleRevision`; they are not silently
rebased. Revision and position validation also apply to selection-only API
transactions and future async tools. `set_selection(selection, expected_revision)`
checks the revision after flushing pending ordinary input, then checks the
positions. Even an in-bounds range from an older revision returns
`StaleRevision`. Selection-only changes leave the document revision unchanged;
this fence identifies the text the positions address, not the latest caret move.

The change/inverse/mapping contracts use `state/src/change.ts` from the
[pinned upstream source](#12-source-provenance) as their reference.
Operational transformation and CodeMirror's serialized change encoding are
not part of this API.

## 5. Input, composition, and history

The contenteditable element is the only editable text surface. `EditorState`
is authoritative; browser mutations are candidate input to reconcile, not a
second durable document. Every accepted edit must enter the same transaction
pipeline, regardless of whether it began as a key command, paste, DOM mutation,
or API call.

### Input and DOM reconciliation

- Use `beforeinput` and key bindings to dispatch commands for Enter,
  indentation, history, and other edits whose intent and range are known.
  Cancel the corresponding native mutation when the event is cancelable.
  Prevent rich-formatting actions such as browser bold/italic commands.
- In writable mode, let ordinary text input and IME use the browser's editing
  behavior.
  `input` plus a scoped `MutationObserver` collect changed text/child-list
  regions. Coalesce records for the same edit so it commits exactly once.
  Missing target ranges or noncancelable input require a working fallback.
- Resolve the affected region through retained line-view boundaries. Read
  normalized plain text and the resulting DOM selection together, then diff
  against that region in the pre-edit document to build a scalar-aligned
  replacement. Use the known pre-input selection/intent to disambiguate edits
  in repeated text. Expand to adjacent lines for splits/joins; a full-surface
  read is a recovery path when structural mutations invalidate local bounds.
- The DOM reader understands canonical line wrappers and browser-created
  `<div>`, `<p>`, and `<br>` boundaries. It emits LF separators exactly once,
  ignores the renderer's empty-line sentinel, preserves intentional blank
  lines and Unicode whitespace, and treats inline formatting wrappers as
  transparent. Neither `innerText` nor `textContent` alone defines the text
  contract: one depends on layout and the other loses block boundaries.
- Validate and commit the candidate transaction, then reconcile the DOM to
  canonical line/token views. A rejected edit restores committed text and
  selection. Renderer-generated mutations must not become input transactions:
  flush pending user records before a guarded write, suppress only owned
  write records, and resume observation without dropping subsequent input.
- Before an API edit, command, or snapshot read, flush pending ordinary input
  first. Validate expected revisions against that resulting state. Active
  composition follows the separate contract below.

Read native selection changes using the owning document/root's `Selection`
anchor/focus pair. Maintain an index from line views and text nodes to model
positions; update it in the same reconciliation pass as the DOM. Restore
direction with `setBaseAndExtent` or an equivalent Range/extend path. Ordinary
pointer selection must not repaint text or continually restore the selection
while the user drags. Handle boundary positions between tokens, empty lines,
line splits/joins, and detached nodes explicitly. Listeners only act on this
editor's selection; focus moving to an app toolbar retains the last valid one.

### Clipboard and drag/drop

Copy and cut serialize the selected range from the model as `text/plain`;
line numbers and syntax wrappers never enter the clipboard. Cut dispatches
one deletion only after clipboard writing succeeds. Paste inserts plain text
in one transaction and does not mount clipboard HTML, images, or links.
Unsupported rich-formatting mutations are rejected or reduced to safe text
through the DOM reader, then canonicalized.

Plain-text drops map the pointer's DOM position to a checked model offset.
Moving a selection within the editor uses one transaction with source and
destination in the same pre-edit coordinates; dropping inside that selection
is a no-op. External text drops insert text, and modifier-based copy preserves
the source. File drops are app-owned. Test these paths with undo/redo and
multiline selections rather than relying on native rich-HTML insertion.

### Composition state machine

- On `compositionstart`, flush earlier ordinary input, capture committed
  text/selection/revision, and protect the composing DOM range and its line
  ancestors from replacement. Track browser-created text nodes as it evolves.
- While composing, leave those text nodes and native selection under browser
  control. Do not split, merge, replace, or reparent them for highlighting.
  Keep their current presentation and defer token patches on affected lines;
  other lines may receive safe presentation-only updates. Retain the committed
  baseline revision until finalization. Provisional text and DOM selection
  belong to the composition session; do not validate its selection against
  unchanged committed text or publish partial history entries. Do not run
  indentation/search edits.
- Track `isComposing` and actual event order. Finalize from the terminal input
  sequence, reconciling the final value once. Cover browsers where the last
  `input` occurs before or after `compositionend`, plus blur/cancel paths.
  Include missing `compositionend` after dead-key input and a newline arriving
  immediately after composition. The pinned view source handles these cases
  explicitly; reproduce its contenteditable regression scenarios before
  adopting browser-specific delays. Highlight updates, span splits/merges,
  and rapidly consecutive compositions must not cancel or duplicate input.
- Commit the completed composition as one undoable edit. Cancellation that
  restores the baseline creates no edit. Read final text and selection before
  releasing the protected range, then normalize its DOM and apply deferred
  token patches without moving the caret.
- Document replacement, undo/redo, programmatic text edits, selection-only
  dispatch, `set_selection`, find navigation, and `focus`/`reveal_selection`
  return `CompositionActive` until finalization. They do not move native
  selection, restore the saved caret, or queue work with stale positions.
  Searches may inspect committed text without selecting a match.
  `text()`, `selection()`, and `snapshot()` expose committed state; snapshot
  also includes a composing flag. Apps must use that flag when saving or
  computing positions during composition. Apply a prop change to
  read-only/disabled after finalizing an already active writable composition.

### Finalization before detachment

The editor requires a **proposed element-owned pre-detachment callback** in
Pocopine. It must run synchronously, once, while the surface is attached and
before descendant listeners, refs, or scopes are released. The current
`on_unmount`, `on_scope_unmount`, and `on_before_subtree_release` hooks do not
guarantee this: `pp-if` and `pp-for` have paths that remove a node before
calling `release_subtree`. Adding the callback is a C0 framework prerequisite,
not an assumption that the existing hooks already satisfy this contract.

All framework removal paths that can contain the editor must invoke it:
conditional branches, keyed-row removal and bulk clear, dynamic component and
route replacement, and owned mount disposal. With leave transitions, invoke
it immediately before actual removal, not when the transition starts; a
canceled transition must keep the editor live. Nested callbacks must run
before any scope in the departing subtree is released. Capture the surface
and runtime directly when registering; do not resolve child refs during cleanup.

Finalization blocks new commands, cancels scheduled presentation work, and
flushes pending ordinary input. If composition has already ended, drain its
terminal input through the normal commit path once. If it is still active,
capture its observed LF text, baseline revision, and available checked
selection in an `InterruptedComposition` payload, then end the session with
an `Interrupted` outcome. Do not promote an unfinished candidate to committed
text or fabricate a successful IME commit. Preserve the candidate even if it
exceeds document limits, marking its validation error and any unavailable
selection explicitly. An active composition rejected by read-only policy
does not become an application recovery draft.

After final change and composition notifications, invoke `on_finalize` once
with an owned `CodeFinalSnapshot`: the latest committed snapshot (now with
`composing = false`), optional interrupted composition, and any finalization
error. It fires even when text is unchanged. Reads remain available inside
the callback; mutations return `Finalizing`. Then dispose observers,
listeners, subscriptions, scheduled work, and the handle before detaching.
The payload remains readable after disposal, so the app can retain committed
text or offer recovery of the interrupted draft without a later handle read.
Callbacks cannot delay removal or await persistence; the app owns saving.

The attached-surface guarantee covers framework-owned removal. An unexpected
external detachment must preserve the last committed snapshot and report
`UnexpectedDetach` through finalization, marking unavailable draft/selection
data rather than claiming a successful attached drain. Page termination is
not a persistence mechanism.

### One history owner

Rust history is authoritative. Store inverse/forward changes and before/after
selections, not a whole document snapshot per keypress. Adjacent typing or
same-direction deletion may group within 500 ms; time is supplied by the
caller so host tests are deterministic. Selection jumps, blur, paste, line
commands, and API edits close the group. One composition, replace-all, or
multiline indentation operation is one undo step. New edits clear redo.

Route keyboard undo/redo and browser `historyUndo`/`historyRedo` requests to
the same history. A browser history mutation that cannot be canceled must be
reconciled against the last committed state and the Rust undo action, never
recorded as a fresh ordinary edit. Verify interception for keyboard, context
menu, and platform history actions; do not rely on `execCommand` or a second
browser-owned undo stack to implement editor commands.
The upstream hook is in `commands/src/history.ts`, which combines history
state with a view `beforeinput` handler. That source also maps non-history
changes through retained undo entries. Our smaller API avoids that machinery
by permitting only undoable edits or an explicit history-clearing load.

Default retention is 200 groups and 8 MiB of retained history data, evicting
whole oldest groups from the total undo/redo budget. Account for allocated
text and change/selection metadata, including capacity inside a typing group.
If one edit alone exceeds the budget, it still applies but clears history
and reports that it was not retained.

Ordinary API edits participate in history. `load_document` explicitly clears
history and resets selection/scroll; there is no generic “skip history” edit
that can leave old inverse offsets invalid. Undo/redo increment revision and
restore their recorded selection. They do not make a document “saved”; dirty
state and save acknowledgments belong to the application.

## 6. Rendering and accessibility

### DOM ownership and line rendering

Use one component root, one scroller, an optional noneditable gutter, and one
contenteditable element. Only that element contains source text. The renderer
owns its descendants; Pocopine owns the surrounding component and chrome.
Do not mount a component per line or let a parent template rewrite the editor's
DOM subtree.

The proposed rendered shape is:

```html
<div data-pine-code-root>
  <div data-pine-code-scroller>
    <div data-pine-code-gutter aria-hidden="true"></div>
    <div data-pine-code-content contenteditable="true"
         role="textbox" aria-multiline="true" tabindex="0"
         spellcheck="false" autocapitalize="off">
      <div data-pine-code-line><span data-pine-code-token="keyword">let</span> x = 1;</div>
      <div data-pine-code-line><br data-pine-code-empty></div>
    </div>
  </div>
</div>
```

One line wrapper represents one logical line; the final empty wrapper exists
only when the document ends in LF (or when the whole document is empty).
The marked `<br>` keeps an empty line focusable and measurable but contributes
zero model characters. The LF between wrappers belongs to the text model.
Token wrappers have no model length and are never persisted. Create content
with text nodes, not source interpolation into `innerHTML`.

Retain line views across transactions and keep their model offsets current.
An edit inside one line patches that line; a split/join updates the affected
wrappers while retaining unaffected siblings. Syntax changes may invalidate
later lines through syntax context, but must not rebuild the entire surface
by default. Reconcile spans and text nodes within dirty lines, preserving
unchanged nodes where possible. Read selection before patches and restore its
mapped anchor/head afterward only when needed. Selection restoration must not
steal focus from another component. Protect composing nodes as specified in §5.

The default view renders all lines within the declared limits. Gutter entries
track the same line views and heights, share vertical scrolling, and remain
visible during horizontal scrolling. Use a monospace font, `white-space: pre`,
an explicit line height, and CSS `tab-size`; soft wrapping is deferred. Token
styles change color only. Retain native caret/selection drawing and keyboard
navigation; styled span boundaries must not introduce cursor stops or change
bidirectional ordering. Test both logical model positions and visual movement.

Batch DOM writes, then measure geometry in a scheduled layout phase for
reveal/scroll and gutter updates. Resize/font-load observers, mutation and
selection subscriptions, and animation-frame work are scope-owned and canceled
on disposal. Prefix internal hooks with `data-pine-code-*` and use explicit CSS
selectors so required styles do not depend on utility discovery inside token
spans. Parent rerenders and theme changes preserve the editable root's identity.

Highlighting failure or budget exhaustion switches to uncolored text nodes
within the same contenteditable line view. Changes to the composing line wait
until composition finishes. Forced-colors mode uses system text/selection
colors; there is no duplicate rendering layer to hide or align. If the view
cannot safely reconcile, enter `ViewStatus::Failed`, retain the committed
snapshot, and suspend new native edits and DOM-position reads. Deliver the
commit and failure status as specified in §8. An explicit `recover_view()`
rebuilds from the latest committed state; only a successful rebuild of both
DOM and position index restores input. Recovery preserves revision/history,
does not emit another document change, and does not steal focus. During an
active composition, protect its nodes and retain terminal-event observation;
recovery returns `CompositionActive` until that session has settled. Never
read unrelated mutations from a failed view back into the document or discard
committed content to recover rendering.

### Accessibility, editability, and application forms

Set `role="textbox"` and `aria-multiline="true"` on the content surface and
forward `aria-label`, `aria-labelledby`, and `aria-describedby` there. Use
`aria-labelledby` for a visible label. The gutter is `aria-hidden`, excluded
from selection, and cannot receive focus. Keep spellcheck and autocapitalization
off. Syntax spans remain in the accessibility tree as the actual source text;
they have no per-token labels or extra roles.

Read-only retains `contenteditable="true"` and `tabindex="0"`, and sets
`aria-readonly="true"`. This preserves native caret drawing, arrow/Home/End
navigation, and Shift-selection, including grapheme and bidirectional movement.
`tabindex` alone would only make a noneditable surface focusable; it would not
restore a native editing caret. See [CodeMirror's read-only example](https://codemirror.net/examples/readonly/).

Enforce read-only in dispatch and all input paths: reject text edits,
replacements, cut deletion, undo/redo, paste/drop insertion, and formatting.
Cancel mutating `beforeinput` events when possible. For noncancelable browser
mutations, restore committed text and the pre-mutation selection without
changing revision/history or emitting a text change. If an IME starts while
read-only, protect its native nodes until termination but reject the entire
candidate; do not publish its provisional selection or text. The usual
composition guards apply until restoration completes. ARIA does not enforce
this policy, and keeping the surface contenteditable may still summon a
mobile keyboard; test that behavior before claiming mobile support.

Copy, select all, native navigation, find, and revision-checked selection
commands remain available when read-only and not composing. Tab/Shift-Tab
always move focus in read-only mode, including when indentation was configured.
Disabled sets `contenteditable="false"`, `aria-disabled="true"`, and
`tabindex="-1"`, and blocks user interactions plus programmatic selection,
focus, and reveal commands. Enforce pointer-focus suppression as well as Tab
exclusion. An explicit app `load_document` remains available in either state.
Apply state changes during composition according to §5.

Tab and Shift-Tab move focus by default. An opt-in `tab_behavior="indent"`
binds them to indentation and documents Escape followed by Tab/Shift-Tab as
the exit sequence. The escape applies to the next key only; a search panel
may consume the first Escape to close itself. Indentation must remain
available through typed commands regardless of the Tab setting. This follows
the accessibility rationale in [CodeMirror's Tab example](https://codemirror.net/examples/tab/).

A contenteditable element is not a native form control. The first release
does not expose native `name`, `required`, or automatic form-reset behavior.
The application reads `snapshot()` to validate/submit text and invokes
`load_document` for a reset. Submission must wait while `composing` is true;
the app must not save the provisional DOM or accidentally submit stale
committed text. The example demonstrates this explicit form integration.

## 7. Highlighting, commands, and search

### Registered Tree-sitter languages

The browser editor uses Tree-sitter in a dedicated module worker. Rust continues
owning committed text, revisions, history, selection, and native input import.
Highlighting is presentation only. The earlier line tokenizer helpers may remain
available to standalone callers; they are not the browser editor's backend.

Applications enable optional `lang-rust`, `lang-json`, `lang-python`, and
`lang-javascript` Cargo features and register the corresponding `languages::*()`
definitions in a `LanguageRegistry`. A custom `TreeSitterLanguage` supplies an ID,
a grammar `LanguageFn`, a standalone highlights query, and an indentation unit.
The same registry factory runs in the page (metadata) and the worker (parsing).
The component selects by `language` ID. New grammars do not change editing code.
See [the implemented API guide](../crates/pine-code/LANGUAGES.md) for complete
startup, build, registry, and custom-query examples.

The worker imports the same content-hashed application module, then calls an
explicit exported worker entrypoint. Applications mount their DOM in a separate
exported page entrypoint. The library owns worker startup, message callbacks,
Blob URL cleanup, timeouts, and termination before editor disposal completes.
CSP must allow its module and Blob worker bootstrap. Worker failure falls back
to plain presentation; explicit load or language change can retry.

Only one request per editor is in flight. Rapid changes are coalesced into the
newest committed snapshot. The worker derives a scalar-aligned UTF-8 InputEdit
between snapshots and reuses the preceding syntax tree. It caches queries by
language. Every response carries revision, language ID, and generation; loads,
configuration/recovery changes, and further edits make older results ineligible.
Composition may continue while parsing runs, but no returned syntax patches
may touch the composing surface. The next committed edit invalidates old results.

Queries produce semantic color categories such as keyword, string, function,
type, property, and comment. Nested captures override their enclosing capture;
later patterns win at identical ranges. Normalize them into ordered, disjoint,
scalar-aligned ranges per logical line and validate again at the DOM boundary.
Standalone text predicates are supported. Patterns requiring property/local-scope
predicates are omitted and unsupported custom predicates are rejected. Locals,
injection queries, compiler semantic analysis, and LSP remain separate work.

Default presentation limits remain 8,000 spans and 8 KiB per highlighted line.
DOM patches run in 4 ms frame slices; parsing runs off the browser main thread.
An additional 1 MiB parser input ceiling, bounded parser/query progress, 64,000
capture ceiling, and 32 MiB capture-paint-work ceiling prevent unbounded parser
work. Worker startup and request recovery timeouts are 30 and 10 seconds.
These are failure ceilings, not typing-latency targets. Span overflow remains
plain until explicit load or language change to avoid rescanning every keystroke.
Unknown languages, invalid queries/ranges, worker failures, and budgets affect
presentation without rejecting or rolling back committed edits.

### Editing commands

- Enter inserts LF plus the current line's leading whitespace, limited to
  the whitespace before the caret when the caret is inside indentation.
  Automatic brace pairing or syntax-based indentation is deferred.
- Indent/outdent changes selected logical lines in one transaction. A
  selection ending at the next line's start excludes that next line.
- With a collapsed caret and Tab indentation enabled, insert enough spaces
  to reach the next indentation stop, or a literal tab in tab mode. Outdent
  removes at most one leading indentation unit. Do not rewrite existing tabs.
- Preserve selection direction and map its endpoints consistently. Commands
  operate on the live state; parent copies are never the command source.
- Key bindings resolve app overrides before defaults. An unhandled key keeps
  its browser behavior. Ignore command shortcuts during composition and avoid
  interpreting AltGr text input as a command.

### Find and replace

The example includes a small, separate find/replace panel built from Pine
inputs/buttons. The editor exposes pure search and typed selection/replacement
operations; a mandatory toolbar is not part of its layout. No regex,
case-folding, whole-word semantics, or search decorations are required first.

Search is literal and case-sensitive. Empty query returns no matches. Scan
non-overlapping matches; next/previous wraps once. Match positions are byte
ranges tied to the searched revision. Find selects and reveals one result
using `set_selection(selection, searched_revision)` and native selection;
a stale result requires a new search, and composition blocks selection/reveal.
Replace current first verifies the current selection
still matches the query. Replace-all builds one change set from the original
snapshot, validates the final size, and is one undo step. Changing the query
or document invalidates cached matches. Return total count separately from a
bounded list of displayed matches; cap cached match ranges at 10,000. That
cache cap must not limit find navigation or the number replaced. Replace-all
can stream every match into one consolidated replacement, preserving the
unchanged text between matches, without retaining an unbounded range list.

## 8. Pocopine API contract

Minimal proposed template:

```html
<pine-code-editor
  pp-ref="source"
  pp-bind:initial-value="source_text"
  language="rust"
  line-numbers
  aria-label="Rust source">
</pine-code-editor>
```

`initial_value` is read once when mounted, matching the existing rich-text
seed pattern. Subsequent prop changes do not overwrite edits. The first
release deliberately has no two-way whole-string `pp-model` contract: use
the typed handle to replace text and observe committed changes. Theme,
language, line numbers, indentation settings, read-only, and disabled can
change without remounting or resetting history.

Defaults are empty initial text, plain language, line numbers off, two-space
indentation unless supplied by the language, tab display width four, Tab focus
navigation, and read-only/disabled false. An explicit indentation override
survives language changes. `tab_size` is a display width from 1 through 32, distinct
from the indentation unit. Theme defaults inherit the application's color
tokens, with documented light/dark fallbacks. Unknown language identifiers
use plain text and report a configuration status; invalid sizes or indentation
settings return a configuration error. Document/history limits are configured
at mount and cannot be lowered underneath live state or retained history.
Explicit indentation is one tab or 1 through 32 spaces; the empty value uses
the language default.

| Proposed handle operation | Contract |
|---|---|
| `text()` / `snapshot()` | Read committed LF text; snapshot includes revision, selection, composing state, and view status |
| `dispatch(transaction)` | Atomic edit with an expected revision; returns `CommitOutcome` or a rejection before this transaction commits |
| `load_document(text, expected_revision)` | Normalized replacement; clears history, selects offset zero, resets scroll when the view is ready; returns `CommitOutcome` |
| `selection()` | Read the committed selection; use `snapshot()` when its revision/composing flag is needed |
| `set_selection(selection, expected_revision)` | Selection-only transaction; checks revision after input flush, preserves direction, returns `CommitOutcome`; blocked during composition or when disabled |
| `undo()` / `redo()` | Use editor history and return `CommitOutcome`; unavailable during composition or read-only/disabled |
| `focus()` / `reveal_selection()` | Native focus and scroll; blocked during composition, when disabled, or when the view is failed |
| `recover_view()` | Rebuild failed DOM/index from committed state and return view status; preserves text/revision/history; blocked during composition |
| `on_change(callback)` | Scoped subscription to committed changes, including commits whose rendering failed; returns a disposal guard |
| `on_view_status(callback)` | Scoped notification of view failure/recovery, including failures without a document change; returns a disposal guard |
| `on_composition_change(callback)` | Scoped start/end notification; end distinguishes committed, canceled, rejected, and interrupted sessions |
| `on_finalize(callback)` | Scoped, synchronous final notification with owned committed text, optional interrupted draft, and finalization error; delivered once before handle disposal |

Resolve the handle from this component instance's ref, following the existing
rich-text handle pattern. After disposal, operations return `Disposed`; handles
must not silently bind to another editor with the same DOM identifier. During
pre-detachment finalization, reads are allowed and state/view-changing calls
return `Finalizing`, including inside its notifications. Subscription guards
can still be disposed. Finalization's own input drain is an internal operation,
not permission for callbacks to dispatch. Reads during finalization return the
captured committed state without flushing the DOM again; an interrupted draft
must not become an ordinary input edit through a callback's `snapshot()` call.
Its owned payload survives disposal.

`CodeChange` reports origin, before/after revisions, changed ranges and lengths,
current selection, document/selection change flags, history availability and
whether the edit was retained, and the view status for that commit.
It does not export a complete string by default. The caller pulls `text()` on
demand, such as for debounced autosave or preview. Origins distinguish input,
composition, paste, drop, cut, command, undo, redo, API edit, and load.

### Commit outcomes and view failures

`CommitOutcome` contains the resulting revision and `ViewStatus` (`Ready` or
`Failed` with a typed rendering error). It is also returned for a valid no-op,
with the existing revision and no change notification. A returned rejection
means the requested transaction did not commit. Earlier ordinary input flushed
at the API boundary can still have committed independently; report those edits
normally and validate the requested transaction against the resulting revision.

After model validation succeeds, publish state and history atomically, then
attempt reconciliation. On success, the DOM and position index are synchronized
before `on_change`. On failure, retain the new model/history, mark the view
failed, and still deliver exactly one `on_change` with that revision and failed
status. Return `Ok(CommitOutcome)` for that edit. Never return a transaction
error for an edit already committed, silently roll it back, or suppress its
autosave notification because rendering failed. In particular, inserting `X`
then failing to patch the DOM must expose the text containing `X`, advance
revision once, and retain its undo entry normally; recovery cannot insert `X`
a second time.

A changed view status is published before callbacks and notified through
`on_view_status` after any associated `on_change`. Presentation-only failures
and successful recovery use `on_view_status` without a document change.
Plain-mode highlighting fallback is a presentation status, not a failed view.
While failed, `text()` and `snapshot()` read committed state without importing
untrusted DOM mutations. Model edits and loads may still commit subject to the
usual editability/composition rules and report the failed status; commands that
require usable DOM geometry return `ViewUnavailable` before acting. Explicit
recovery renders the latest revision, including any intervening model edits.

Composition-end notifications follow the final commit/rejection/cancellation,
including any view-status notification, even when text is unchanged. Interrupted
sessions follow the finalization ordering in §5. Reads in listeners are safe.
All state/view-changing reentrant calls return `ReentrantDispatch` without
side effects; `Finalizing` takes precedence during teardown. A caller may
schedule a later command against the then-current revision while the editor
remains live. Highlight-only rendering never emits a document change.

Rejections include invalid positions/ranges, overlapping changes, stale
revision, size limit, composition active, read-only, disabled, reentrant
dispatch, finalizing, disposed handle, invalid configuration, and unavailable
view for a DOM-dependent operation. Rendering errors belong to `ViewStatus`,
not transaction rejection. Rejected native input is restored to the last
committed text/selection and announced accessibly; if restoration itself fails,
keep the rejection and independently publish the failed view status. Oversized
initial content fails visibly and remains available to the caller; never
truncate or silently
substitute an editable empty document. Limit checking also covers paste,
drop, normalization, replacement, and undo/redo.

## 9. Relationship to rich text and future growth

The two editors remain siblings. The first release does not embed a second
editor inside `pine-richtext` code-block nodes. That would require a separate
adapter defining focus entry/exit, input/event ownership, text serialization,
position mapping, and one authority for history and collaboration. RFC-113's
node-view lifecycle is a reference for that future integration, not permission
to install two independent undo stacks for the same source text.

Future language services can consume `(revision, text)` and return validated
ranges/edits. Large-file work can add a persistent text buffer, viewport line
rendering, height estimation, and selection across unrendered regions within
the same contenteditable architecture. Preserve checked positions, transactions,
and the public handle. These are extension boundaries, not commitments that
the initial renderer supports those features or needs a general extension
framework now.

## 10. Implementation phases

| Phase | Deliverable | Exit condition |
|---|---|---|
| C0 — Contenteditable input and lifecycle proof | Line/token DOM, DOM reader, selection mapping, scoped observer, composition protection; framework pre-detachment callback | Input commits exactly once; IME survives token changes; undo routes to Rust history; every framework removal path runs finalization while attached, before scope release |
| C1 — Pure core | `pine-code` text, positions, state, changes, history, search, commands | Host behavior/property tests pass; core builds for host and WASM without `view` |
| C2 — Native component | Incremental line reconciliation, scoped handle, clipboard, composition, final snapshots, limits, read-only, view recovery | Browser editing, guarded selection, read-only navigation, app submit/reset, interrupted-composition delivery, disposal, and injected rendering-failure tests pass |
| C3 — Code presentation | Gutter, registered Tree-sitter languages and worker, direct token-span styling, plain-mode fallback, example search panel | Highlight patches preserve selection/IME; light/dark, forced colors, incomplete syntax, font/zoom changes pass |
| C4 — Release gates | Documentation, standalone example, performance fixture, cross-target validation | All §11 gates pass and measured limits are published |

Keep commits aligned to completed behavior. Browser failures in C0 must be
resolved in the contenteditable adapter and captured as regression fixtures
before implementation proceeds. The lifecycle prerequisite must include
removal/transition regressions in `pocopine-core`; do not work around it with
a post-detachment observer or ask every app to drain the editor manually.
Preserve the native Rust view and the user's input-surface constraint
throughout all phases.

## 11. Validation and acceptance

### Core tests

Cover empty documents, trailing newlines, LF/CRLF/CR normalization, tabs,
astral Unicode, combining marks, backward selections, invalid UTF-8/UTF-16
positions, disjoint replacements, overlap rejection, stale revisions, and
size-limit rollback. Property tests must prove apply/invert restores the
original text and valid positions remain valid after mapping. Differential
ASCII cases may compare mapping/inversion with a pinned CodeMirror snapshot;
convert coordinates explicitly for Unicode cases.

Exercise history grouping, selection restoration, redo invalidation, retention
eviction, load/reset behavior, find wrapping, replacement expansion, line
boundary indentation, and malformed tokenizer output. Include a selection
computed at revision R that remains in bounds at R+1: both `set_selection`
and selection-only dispatch must reject it, and a valid selection-only change
must preserve document revision and backward direction. Use arbitrary Unicode
and edit sequences, not only source-code ASCII fixtures.

Use the source/test map in §12 to select upstream regression scenarios.
Include insertions on both selection edges, deletion covering a backward
selection, independent lexical snapshots, state changes across blank lines,
and a tokenizer that fails to advance. Translate the expected behavior into
Rust fixtures with explicit UTF-16/byte conversion; the original tests cannot
run directly against the proposed API. No upstream test suite was executed
as part of this specification review.

### Browser tests and manual input checks

Add `tests/playwright/code-editor.spec.mjs` to the existing browser harness.
The current [`playwright.config.mjs`](../playwright.config.mjs) runs only a
Chromium rich-text project and starts a static server. The code-editor work
must add explicit browser projects and wire its Pocopine CLI-served example;
the existing defaults alone cannot satisfy this matrix.
Test Chromium, Firefox, and WebKit desktop behavior: typing, selection,
clipboard text, drag/drop text, undo/redo, external replacement, focus escape,
form submit/reset, read-only, parent rerenders, and two independent editors.
Repeated mount/unmount must leave no active callbacks, observers, or handles.

Exercise pre-detachment finalization through conditional removal, keyed-row
removal and bulk clear, dynamic/route replacement, and owned mount disposal,
including editors nested inside departing ancestors. Assert `isConnected`
and live scopes inside finalization, notification-before-disposal ordering,
and payload readability afterward. Cover leave-transition completion,
cancellation, pending ordinary input, terminal composition input waiting to
flush, and an active composition with no `compositionend`. Completed input
commits once; interrupted input is delivered separately from committed text,
including oversized candidates and unavailable selection. Verify an unchanged
editor still finalizes once, reads in the callback work, and writes return
`Finalizing`. Repeated callback reads must not import an interrupted draft.
External detachment reports the weaker `UnexpectedDetach`
outcome. Cleanup must also complete when the view has already failed.

Verify DOM-point/model-position round trips at every token/line boundary,
including element child indexes, non-BMP text, empty-line sentinels, and
backward selections. Test native span splits/merges, line splits/joins, trailing
blank lines, rich-text paste reduced to literal source, unsupported formatting,
internal drag moves, and browser replacement/autocorrect input. One user edit
must yield one text commit; renderer mutations and selection drag must not
produce phantom edits. Assert that rendering or retokenization preserves the
caret, active composition nodes, and unaffected line nodes.

Form tests cover app-managed validation, submission, and explicit reset through
the handle, including composition-in-progress. The example retains the owned
final snapshot independently of the handle and offers interrupted text for
explicit recovery rather than treating it as saved content. A code editor must
not create an auxiliary input control to implement those operations.

While composing, call `set_selection`, selection-only dispatch, find navigation,
focus/reveal, and recovery; assert `CompositionActive`, unchanged native nodes
and selection, and no queued movement after finalization. A pending ordinary
input flush that advances revision must make an older selection request stale.
Repeat permitted selection operations in read-only mode and rejected ones in
disabled mode.

In read-only mode, verify a visible native caret, arrow/Home/End and
Shift-selection across tokens, graphemes, and bidirectional text; copy, select
all, find, and Tab focus exit must work. Attempt typing, paste, cut, drop,
formatting, history actions, noncancelable input, and IME. Assert unchanged
committed text/revision/history, restored DOM, and no text-change notification.
Check read-only/disabled transitions during writable composition, and disabled
pointer-focus suppression. Include mobile keyboard behavior in manual checks.

Inject reconciliation and selection-restoration failures after an accepted
edit. Assert a successful commit outcome with failed view status, exactly one
change notification, readable new text inside the callback, and intact revision
and undo history. Inject a presentation-only failure and verify status delivery
without a text change. While failed, stale DOM cannot overwrite snapshots;
model edits remain policy-checked and recovery renders the latest revision
without another commit or duplicate insertion. Repeat with load, undo/redo,
composition completion, and failed rollback of rejected native input. Verify
reentrant writes are rejected, and final snapshots retain committed text when
recovery never succeeds.

Include light/dark/forced-colors checks, visible caret/selection, 100%/125%/200%
zoom, resize/font loading, tabs, final blank lines, long lines, mixed-direction
source, and gutter alignment while scrolling. Copying must never include
line numbers.

Synthetic composition events do not prove platform IME works. Record manual
Japanese or Chinese composition, dead-key accents, emoji, cancellation, and
undo after composition on real desktop browsers. Test Android Chrome and
iOS Safari touch selection/paste/composition before claiming mobile support.
Record a screen-reader check of label, value, selection, and focus exit using
VoiceOver/Safari and NVDA/Firefox or Chrome. Unrun checks remain explicitly
unverified, even if Playwright passes.

### Performance gates

Create a release-build fixture for 10 KiB, 100 KiB, and 256 KiB documents,
including a 10,000-line sample and a single long-line sample. Measure 200 edits
at the beginning/middle/end plus paste, replace-all, undo, and scrolling.
Record hardware, browser version, build flags, warm-up, WASM transfer size,
retained memory, line/token-node count, patched nodes per edit, DOM read range,
and presentation mode used. Distinguish full-document recovery reads from the
ordinary affected-line path.

Proposed gates on the documented reference desktop:

- At 100 KiB: input-to-next-painted-frame p95 at most 32 ms; no editing task
  above 50 ms after warm-up. Measure input, reconciliation, and paint together.
- At 256 KiB or 10,000 lines: p95 at most 50 ms in the documented supported
  presentation mode, with working selection and undo and no lost input.
- Highlighting respects its work budgets and cannot block text commits.
- An ordinary edit in one line retains unaffected line DOM nodes; highlighting
  only changes later lines when syntax context requires it. No per-edit
  whole-surface `innerHTML` replacement or full-DOM serialization is allowed.
- After 100 mount/edit/unmount cycles, listeners/tasks return to baseline and
  retained editor memory shows no continuing growth after collection.

These gates need measurement. If they fail, lower the published document
limit or improve the contenteditable renderer before acceptance. The initial
full-document view must not claim viewport virtualization or unbounded
large-file performance.

### Workspace gates and example

For implementation, run the workspace's required checks before pushing:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --target wasm32-unknown-unknown
cargo build --workspace --target wasm32-unknown-unknown
cargo test --workspace
```

Also exercise `pine-code` with and without `view` on the appropriate targets;
the default workspace build must not be the only check of an opt-in feature.
The proposed example needs Pocopine app metadata, index page, and host server
bin. Follow the [Pocopine CLI convention](../.agents/skills/run-with-pocopine-cli/SKILL.md):

```sh
pocopine dev --path examples/code-editor
pocopine build --path examples/code-editor --release
```

These commands run the implemented example. See
[`examples/code-editor/VALIDATION.md`](../examples/code-editor/VALIDATION.md)
for current measurements, automated checks, and outstanding manual checks.
This documentation-only draft does not claim implementation test results.

## 12. Source provenance

Direct Git access to all four user-supplied repositories succeeded on
2026-09-06. Shallow reference clones are available under ignored
`tmp/CodeMirror/{state,view,language,commands}`. The forge's HTML index returned
HTTP 403, but that does not prevent cloning its Git endpoints. The source
review now uses these direct snapshots; the initial archived GitHub fallback
is no longer the source baseline.

Versions below come from each checked-out `package.json`; the commit IDs are
the exact revisions inspected, not a claim about later HEADs. Each package
has its own version. These four source references are not an npm lockfile or
a tested dependency resolution.

| Package | Git endpoint | Manifest version | Inspected commit |
|---|---|---|---|
| `state` | [state.git](https://code.haverbeke.berlin/codemirror/state.git) | `6.7.4` | `ed280c375b165a9ab895bd942eb0529fbc8f7549` |
| `view` | [view.git](https://code.haverbeke.berlin/codemirror/view.git) | `6.43.11` | `7b45dd4462cbf552e5b54df845dd2051662cd242` |
| `language` | [language.git](https://code.haverbeke.berlin/codemirror/language.git) | `6.12.4` | `89974ce5d39539ce6c5cfea5278443fa9381cbf2` |
| `commands` | [commands.git](https://code.haverbeke.berlin/codemirror/commands.git) | `6.11.0` | `ca6f705256b3dc0745b843580ef66091d0c625c1` |

Source paths below are relative to their package's pinned checkout:

| Package / phase | Source and regression references | Application to this RFC |
|---|---|---|
| `state` / C1 | `src/text.ts`, `src/change.ts`, `src/selection.ts`, `src/transaction.ts`; `test/test-text.ts`, `test/test-change.ts`, `test/test-state.ts`, `test/test-selection.ts` | Text operations, inverse edits, selection boundaries, transactions; retain Pocopine's checked byte offsets and bounded string representation |
| `view` / C0, C2 | `src/editorview.ts`, `src/input.ts`, `src/domobserver.ts`, `src/domreader.ts`, `src/domchange.ts`, `src/docview.ts`; `test/webtest-composition.ts`, `test/webtest-domchange.ts`, `test/webtest-events.ts` | Contenteditable input ordering, DOM/text conversion, selection, composition, reconciliation, history integration, and disposal |
| `language` / C3 | `src/stream-parser.ts`, `src/stringstream.ts`, `src/language.ts`; `test/test-stream-parser.ts` | Independent lexical state, blank lines, tokenizer progress, cached state reuse, bounded work; implement a small Rust tokenizer API |
| `commands` / C1, C2 | `src/history.ts`, `src/commands.ts`; `test/test-history.ts`, `test/test-commands.ts`, `test/webtest-commands.ts` | Undo grouping/isolation, browser undo interception, selection restoration, indentation, and Tab focus behavior |

The dependency manifests reinforce the scope boundary: upstream `language`
uses Lezer, and `commands` integrates with `language` and `view`. Referencing
these packages does not require bringing their JavaScript dependency graph
into Pocopine. In particular, `StreamLanguage` is backed by Lezer upstream;
its tokenizer/state contract informed the retained standalone helpers. The browser
editor now uses registered Tree-sitter grammars and the worker pipeline in §7.

All four checked-out `LICENSE` files identify the MIT license. Any later
translation or copied tests must record the package, commit, and source paths
used and preserve required license/copyright notices. Follow the existing
[`pine-richtext` attribution practice](../crates/pine-richtext/NOTICE.prosemirror.md),
adding `NOTICE.codemirror.md` when reference-derived implementation lands.
The scratch clones remain ignored reference material; do not vendor them into
the workspace's shipped crates.

All design choices and limits above are Pocopine proposals. This RFC does
not promise CodeMirror API compatibility or parity with its complete feature set.

### Measured limit adjustment

The initial Chromium release measurements exceeded the latency gates with
11,635 syntax spans at 100 KiB (40 ms p95) and a single 256 KiB line (80 ms
p95). The implementation therefore uses an 8,000-span presentation budget
and an additional default `DocumentLimits.max_line_bytes = 32 * 1024`.
The document remains bounded at 256 KiB total and 10,000 lines. Oversized
logical lines are rejected atomically with `SizeLimit`, including loads,
paste, joins and replacement; initial oversized content stays visible and
immutable. Custom limits can opt into a larger, unmeasured envelope.
See the example validation report for the rerun and baseline measurements.
