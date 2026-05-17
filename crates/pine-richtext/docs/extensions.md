# pine-richtext extensions

`pine_richtext::extension::RichTextExtension` is the single contract a
plugin implements to add functionality to the editor. As of Phase 4b,
extensions are composed into a per-mount **`EditorRuntime`** — two
`<pine-rich-text-root>` instances on the same page can carry
different runtimes with different schemas, command tables, keymap
factories, plugin sets, and node-view tag bindings.

A `RichTextExtension` can contribute any combination of:

- **Node specs** — folded into the runtime's `Schema` in registration
  order.
- **Mark specs** — same.
- **Named commands** — reached via the `pine:richtext:command`
  CustomEvent's `Custom { name, args }` variant. Per-runtime: the
  same name resolves to different commands in different runtimes.
- **Key bindings** — merged into `view::input::default_keymap()`
  underneath the base 4 (`Backspace`, `Delete`, `Enter`, `Mod-a`).
  Per-runtime.
- **Node-view tags** — custom-element bindings folded into
  `runtime.lookup_node_view`. The renderer and reconciler consult
  this per render; two runtimes can map the same `node_type` to
  different tags. The tags themselves must be globally registered
  with the browser via `pocopine::App::register::<C>()`.
- **State plugins** — for things like history. Per-runtime: each
  surface gets its own plugin state.
- **List-item types** — e.g. `&["task_item"]`. Consulted by the
  list-conversion fast path in `commands::wrap_in_list`.

## Trait shape

```rust
use pine_richtext::extension::{
    ExtensionNodeView, KeyBindings, NamedCommand, RichTextExtension,
};
use pine_richtext::model::{MarkSpec, NodeSpec};
use pine_richtext::state::Plugin;

pub trait RichTextExtension: 'static + Send + Sync {
    fn name(&self) -> &str;
    fn nodes(&self) -> Vec<NodeSpec> { Vec::new() }
    fn marks(&self) -> Vec<MarkSpec> { Vec::new() }
    fn key_bindings(&self) -> KeyBindings { Vec::new() }
    fn commands(&self) -> Vec<(String, NamedCommand)> { Vec::new() }
    fn node_views(&self) -> Vec<ExtensionNodeView> { Vec::new() }
    fn plugins(&self) -> Vec<Plugin> { Vec::new() }
    fn list_item_types(&self) -> &'static [&'static str] { &[] }
}
```

Every method has a default that contributes nothing, so a minimal
extension only overrides what it needs.

## Building a runtime

Use `pine_richtext::runtime::RuntimeBuilder` to compose a runtime
from an explicit chain of extensions. The builder folds them in
registration order, deduplicating by `name()` first-wins:

```rust
use pine_richtext::extensions::{
    CoreInlineExtension, CoreMarksExtension, CoreNodesExtension,
    HistoryExtension, ListsExtension, TaskListExtension,
};
use pine_richtext::runtime::{self, RuntimeBuilder};

let doc_runtime = RuntimeBuilder::new()
    .name("doc")
    .with(CoreNodesExtension)
    .with(ListsExtension)
    .with(TaskListExtension::new())
    .with(CoreInlineExtension)
    .with(CoreMarksExtension)
    .with(HistoryExtension)
    .build();
runtime::registry::register("doc", doc_runtime);
```

Or skip the defaults entirely for a minimal editor (e.g. a comment
box that only accepts paragraphs + marks):

```rust
let comment_runtime = RuntimeBuilder::new()
    .name("comment")
    .without_defaults()
    .with(MyCommentSchemaExtension)  // only doc + paragraph + text
    .with(CoreMarksExtension)
    .build();
runtime::registry::register("comment", comment_runtime);
```

## Mounting

Mark up the custom element with a `runtime` attribute naming the
registered runtime:

```html
<pine-rich-text-root runtime="comment"></pine-rich-text-root>
```

A surface without a `runtime` attribute resolves to the **default
runtime** — folded lazily on first mount from
`default_extensions()` plus any extensions registered via the legacy
`extension::registry::register` path (see appendix). Unknown
runtime names also fall back to the default with a warning.

## Per-instance vs. process-global

The runtime is per *instance* of `<pine-rich-text-root>` — every
mount that resolves to the same name shares an `Arc<EditorRuntime>`.
Per-mount state (the doc, the selection, the history stack) is held
on the component, not on the runtime. So a page with 50 comment
editors all `runtime="comment"` shares one schema fold + one
plugin instance vec, but each editor has its own document and own
history stack.

Once any surface is mounted, the runtime registry is **sealed** —
further `register("name", ...)` calls and legacy
`extension::register` calls panic with the same message. This
matches the registration lifecycle for `pocopine::App::register::<T>()`.

## Worked example: the demo

The demo at `examples/richtext` hosts two editors on one page:

```rust
#[wasm_bindgen(start)]
pub fn main() {
    // The default runtime — kitchen sink.
    extension::register(Box::new(
        TaskListExtension::new().with_node_view::<PineTaskItem>(),
    ));

    // The comment runtime — paragraphs + marks only, plus a demo-
    // local `comment_submit` named command.
    let comment = RuntimeBuilder::new()
        .name("comment")
        .without_defaults()
        .with(CommentSchemaExtension)
        .with(CoreMarksExtension)
        .with(CommentRuntimeExtension)
        .build();
    runtime::registry::register("comment", comment);

    App::new()
        .register::<Editor>()
        .register::<PineRichTextRoot>()
        .register::<PineTaskItem>()
        .run();
}
```

The template uses both:

```html
<pine-rich-text-root pp-bind:initial-doc="initial_doc"></pine-rich-text-root>
<pine-rich-text-root runtime="comment"></pine-rich-text-root>
```

A `pine:richtext:command` `{ kind: "custom", name: "comment_submit" }`
dispatched against the comment editor inserts the sentinel
`✓submitted`; the same event against the doc editor is a silent no-
op because its runtime has no `comment_submit` in its command
table.

## Node-view component contract

Pine-richtext's node views are pocopine's analogue of Tiptap's
React NodeView. A custom element registered with the browser via
`App::register::<C>()` owns wrapper chrome (drag handles, checkboxes,
embed controls); pine keeps owning the inline content rendered
inside.

```rust
use pine_richtext::extension::{ExtensionNodeView, RichTextExtension};
use pine_richtext::model::NodeSpec;

pub struct MyTaskListExt;

impl RichTextExtension for MyTaskListExt {
    fn name(&self) -> &str { "my-task-list" }

    fn nodes(&self) -> Vec<NodeSpec> { /* task_list, task_item, ... */ vec![] }

    fn node_views(&self) -> Vec<ExtensionNodeView> {
        vec![ExtensionNodeView {
            node_type: "task_item".into(),
            tag: "my-task-item".into(),
            content_selector: Some("[data-content]".into()),
        }]
    }
}
```

The `tag` value (`my-task-item`) must be the `NAME` of a pocopine
`Component` registered via `App::register::<C>()` — that's a
browser-level `customElements.define` call, which is process-global.
But the `node_type → tag` mapping in `node_views()` is per-runtime,
so a different runtime can map `task_item` to a different component
tag — provided that other tag is also registered with the browser.

## Backward compatibility

### Legacy `extension::registry`

The `extension::registry` module survives as a soft-deprecated thin
adapter shim over `runtime::registry::default`:

- `extension::registry::register(Box::new(MyExt))` writes into the
  **default** runtime's extension list. Marked `#[deprecated]`;
  apps targeting per-instance scoping should use
  `RuntimeBuilder::new().with(MyExt).build()` +
  `runtime::registry::register(name, rt)` instead. The legacy path
  remains the **only way** to customize the default runtime (there
  is no `runtime::registry::set_default` API today).
- `extension::registry::named_command`,
  `extension::registry::merged_keymap_factories`,
  `extension::registry::is_list_item_type`,
  `extension::registry::list_item_type_names`, and
  `extension::registry::merged_plugins` are read-through adapters
  that delegate to `runtime::registry::default()`.
- `extension::registry::registered()` reads the legacy `EXTENSIONS`
  Vec directly (the default runtime reads it during its lazy init).
- `extension::registry::mark_schema_realized()` flips the legacy
  one-way seal. The seal is also flipped automatically by every
  `runtime::registry::resolve` call so the panic contract on
  late `extension::register` holds regardless of which runtime path
  the app uses.

### Removed: the `render::node_views` global

The Phase 4 process-global node-view registry in
`crate::render::node_views` is gone. Apps that previously called
`crate::render::node_views::register(...)` must move their
contributions to a `RichTextExtension::node_views()` method (or use
the `TaskListExtension::with_node_view::<C>()` builder for the
common task-item case).

The `NodeViewSpec` data type still lives in
`render::node_views::NodeViewSpec` and is re-exported from
`runtime::NodeViewSpec` for backward compatibility with apps
importing the type.
