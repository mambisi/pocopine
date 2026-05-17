# pine-richtext extensions

`pine_richtext::extension::RichTextExtension` is the single contract a
plugin implements to add functionality to the editor. A `RichTextExtension`
can contribute any combination of:

- **Node specs** — folded into `schema_basic::schema()` in
  registration order.
- **Mark specs** — same.
- **Named commands** — reached by external dispatchers via the
  `pine:richtext:command` CustomEvent's `Custom { name, args }`
  variant.
- **Key bindings** — merged into `view::input::default_keymap()`
  underneath the base 4 (`Backspace`, `Delete`, `Enter`, `Mod-a`).
- **Node views** — custom-element bindings forwarded eagerly into the
  `render::node_views` registry at registration time.
- **State plugins** — for things like history.

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
}
```

Every method has a default that contributes nothing, so a minimal
extension only overrides what it needs.

## Lifecycle

Extensions register **before** `pocopine::App::run()` (i.e. before any
component lifecycle hook reads `schema_basic::schema()`). The first
call to `schema()` seals the registry — subsequent `register(…)` calls
panic with the name of the offending extension. This matches
`App::register::<T>()`'s lifecycle and keeps build-time misorder a
loud failure rather than a silent miss.

```rust
fn main() {
    // 1. Register extensions.
    pine_richtext::extension::register(Box::new(MyExtension));

    // 2. Boot pocopine.
    pocopine::App::new()
        .register::<MyEditor>()
        .register::<pine_richtext::view::PineRichTextRoot>()
        .run();
}
```

## Worked example: shipping the demo's checklist

The demo registers `TaskListExtension::with_node_view::<PineTaskItem>()`
so the reconciler renders `task_item` nodes as the `<pine-task-item>`
custom element. End-to-end:

```rust
use pine_richtext::extension;
use pine_richtext::extensions::TaskListExtension;

#[wasm_bindgen(start)]
pub fn main() {
    extension::register(Box::new(
        TaskListExtension::new().with_node_view::<PineTaskItem>(),
    ));
    App::new()
        .register::<Editor>()
        .register::<PineRichTextRoot>()
        .register::<PineTaskItem>()
        .run();
}
```

The user-registered `TaskListExtension` shares the same `name()`
(`"task_list"`) as the built-in entry returned by
`extensions::default_extensions()`. The schema fold treats name
collisions as **user shadows base**: the user's
`TaskListExtension::with_node_view::<PineTaskItem>()` replaces the
default `TaskListExtension::new()` in fold position, preserving the
schema's node ordering while letting the user contribute extra node
views, commands, or key bindings.

`with_node_view::<C>` is gated behind the `view` cargo feature
because the `pocopine_core::app::Component` trait it uses to derive
`C::NAME` is itself view-only. Without `view`, the lower-level
`with_node_view_tag(tag, content_selector)` accepts raw strings.

## Default extension set

`pine_richtext::extensions::default_extensions()` returns, in fold
order:

| # | Extension | Contributes |
|---|---|---|
| 1 | `CoreNodesExtension` | `doc`, `paragraph`, `blockquote`, `horizontal_rule`, `heading`, `code_block` |
| 2 | `ListsExtension` | `bullet_list`, `ordered_list`, `list_item` + named commands `wrap_in_bullet_list`, `wrap_in_ordered_list` |
| 3 | `TaskListExtension::new()` | `task_list`, `task_item` + named commands `wrap_in_task_list`, `set_task_checked`. **Override this** with `with_node_view::<C>()` to thread a custom element. |
| 4 | `CoreInlineExtension` | `text`, `image`, `hard_break` |
| 5 | `CoreMarksExtension` | marks `link`, `em`, `strong`, `code` |
| 6 | `HistoryExtension` | history plugin + named commands `undo`/`redo` + key bindings `Mod-z`, `Mod-Shift-z` |

## Dispatching extension commands

The `pine:richtext:command` CustomEvent's payload is the
`CommandRequest` enum. The `Custom` variant is the open path
extensions use:

```js
surface.dispatchEvent(new CustomEvent('pine:richtext:command', {
  bubbles: true,
  detail: {
    kind: 'custom',
    name: 'wrap_in_bullet_list',
    args: {},          // forwarded as JSON to the NamedCommand factory
  },
}));
```

Resolution: the surface looks up `name` in the
`extension::registry::named_command` table (user-registered wins;
defaults are the fallback). The factory receives `args` as
`serde_json::Value`; returning `None` means malformed args, and the
command is a silent no-op (same as a non-applicable PM command).

## Process-global semantics (Phase 4 minimal-disruption)

This release ships extensions as a **process-global** registry, on the
same lifecycle as `App::register::<T>()`. Two
`<pine-rich-text-root>` instances on the same page share one schema,
one node-view map, one command table, and one keymap. Per-instance
scoping is a future Phase 4b — meaningful for apps that want a doc
editor and a comment editor on the same page to have different
extension sets.

What this means in practice:

- **Register before `App::run()`**. Late registration panics.
- **Duplicate `name()` is logged and dropped.** First-wins per
  `tracing::warn!` to `target: "pocopine.log"`.
- **User extensions can shadow defaults** when they share a name.
  The schema fold swaps the user's version into the default's
  position. Command lookup checks the user table first then falls
  back to defaults. Keymap is user-first, then defaults fill
  remaining slots.
- **Schema sees collisions as build errors.** Two extensions
  declaring the same node type (without name-shadowing) crashes the
  fold — matching `App::register::<T>()`'s collision behavior.
