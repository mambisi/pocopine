---
title: "Extractors"
description: "Declare what a method needs by type and the framework fills it — lifecycle-context extractors for hooks, FromHandlerArg for event handlers."
---

# Extractors

An extractor lets a method **declare what it needs by type**, and the
framework fills the parameter. There are two surfaces, one per kind of
method:

| Method kind | Parameter source | Trait |
|---|---|---|
| Lifecycle hooks (`on_setup` / `on_mount` / `on_ready` / `on_unmount`) | the lifecycle context | `From<LifecycleContext>` |
| Event handlers (`@event="…"`) | the dispatched event / call args | `FromHandlerArg` |

They don't mix: an event handler **cannot** take a lifecycle extractor
like `Handle<Self>` or `Inject<…>` as a parameter (those reach the handler
body a different way — see [Reaching context from a handler](#reaching-context-from-a-handler)).

## Lifecycle extractors

Add them as parameters after the receiver on any lifecycle hook. The
framework projects each one from the hook's context:

```rust
#[handlers]
impl Menu {
    pub fn on_ready(&self, handle: Handle<Self>, refs: Refs) {
        let trigger = refs.get("trigger").unwrap();
        events::on_scoped(&trigger, events::ev::click, move |_| {
            handle.update(|s| s.open = !s.open);
        });
    }
}
```

### The surface

| Extractor | Gives you |
|---|---|
| `Handle<Self>` | a typed handle to this component (`update` / `with`) — the in-signature form of `this::<Self>()` |
| `ScopeId` · `ParentId` · `ScopePath` | this scope's id, its parent's id, the full chain to the root |
| `El` · `TypedEl<T>` · `HostEl` | the rendered root element (raw, `dyn_into`-cast, or the custom-element host) |
| `Refs` | named `pp-ref` lookups — `.get(name)`, `.get_as::<T>(name)`, `.get_component::<T>(name)` |
| `IsTeleported` · `TeleportHost` · `TagName` | teleport state, the teleport's origin, the registered tag |
| `Doc` · `Win` · `Body` | `web_sys` `Document` / `Window` / `<body>` shortcuts |
| `Parent<T>` · `NearestParent<T>` | typed handle to the immediate / nearest ancestor of type `T` (compound components) |
| `Inject<KEY, T>` | a keyed context value provided by an ancestor (`create_context!`) |
| `Plugin<T>` | an app-level plugin instance |
| `Loader<T>` | route loader data, on a routed component's `on_setup` ([routing guide](../routing/route-guards-and-loaders.md)) |
| `MountEpoch` · `Elapsed` | mount generation counter; a timestamp |

Wrap any of them in `Option<…>` for the **non-panicking** form — you get
`None` instead of a panic when the value isn't there (no parent of that
type, an absent ref cast, a missing inject key).

### Phase validity

Element-dependent extractors need the rendered DOM, so they're only valid
in `on_mount` / `on_ready`. Using one in `on_setup` or `on_unmount` panics
with a message telling you which phases are allowed.

| Always valid | `on_mount` / `on_ready` only |
|---|---|
| `Handle` · `ScopeId` · `ParentId` · `ScopePath` · `Doc` · `Win` · `Body` · `Parent` · `NearestParent` · `Inject` · `Plugin` · `MountEpoch` · `Elapsed` | `El` · `TypedEl` · `HostEl` · `Refs` · `IsTeleported` · `TeleportHost` · `TagName` |

(See [Lifecycle → Borrow rules](./04-lifecycle.md#borrow-rules): resolve
`Refs` / `El` synchronously inside the hook — they don't survive a
`tick::next`.)

### Authoring your own

An extractor is just an `impl From<LifecycleContext>` — no trait to learn,
and the orphan rule works for your own types:

```rust
struct Theme(String);

impl<'a> From<pocopine::LifecycleContext<'a>> for Theme {
    fn from(_ctx: pocopine::LifecycleContext<'a>) -> Self {
        Theme(read_theme_attr())
    }
}
// then: fn on_mount(&mut self, theme: Theme) { … }
```

Implement `From<LifecycleContext> for Option<YourType>` for the fallible
flavour.

## Event-handler arguments

A handler's parameters are filled from what the binding passes, converted
through `FromHandlerArg`.

A **bare** binding passes the event. `@click="on_click"` is rewritten to
`on_click($event)`, so the handler receives the event object:

```rust
pub fn on_click(&mut self, ev: web_sys::MouseEvent) {
    ev.prevent_default();
}
```

An **explicit call** passes whatever you write — scope values, `$event`, or
both, positionally:

```poco
<input @input="rename($event, row.id)" />
```

```rust
pub fn rename(&mut self, ev: web_sys::InputEvent, id: u32) { … }
```

Each argument is converted with `FromHandlerArg::from_handler_arg`. If a
conversion fails (wrong shape, `undefined`), the handler is **skipped** for
that dispatch — no panic.

### Built-in `FromHandlerArg`

- `JsValue` (identity) and `Option<T>` (`None` on `undefined` / `null`)
- scalars: `String`, `bool`, `f64`, `f32`, `i32`, `i64`, `u32`, `u64`, `isize`, `usize`
- events: `Event`, `UiEvent`, `MouseEvent`, `KeyboardEvent`, `InputEvent`,
  `FocusEvent`, `CustomEvent`, `PointerEvent`, `DragEvent`, `WheelEvent`,
  `TouchEvent`, `SubmitEvent`, `ClipboardEvent`

### Reading an emitted payload

A child reports upward with `emit(name, detail)`; the parent listens with
`@name` and takes the `CustomEvent` (a bare binding passes `$event`),
reading the payload off `.detail()`:

```rust
// Child
pub fn complete(&mut self) {
    self.done = true;
    pocopine::emit("todo-completed", self.id);   // detail = u32
}
```

```poco
<!-- Parent -->
<ul @todo-completed="record_completed">
  <todo-item pp-bind:id="item.id" />
</ul>
```

```rust
// Parent
pub fn record_completed(&mut self, ev: web_sys::CustomEvent) {
    let id = ev.detail().as_f64().unwrap_or_default() as u32;
}
```

For a struct payload, deserialize the detail with `serde_wasm_bindgen`:

```rust
let payload: TodoPayload =
    serde_wasm_bindgen::from_value(ev.detail()).unwrap_or_default();
```

`FromHandlerArg` is what converts an *explicit* argument — the values you
write in a call like `@save="save(draft)"` — into a typed parameter.
Implement it for your own type (two lines over `serde_wasm_bindgen`) when
you pass one that way.

### Reaching context from a handler

Event handlers don't take `Inject` / `Parent` / `Handle` parameters — reach
context from the **body** instead, with `inject` (keyed context),
`this::<Self>()` (own handle), or `store::<T>()` (a store). This is how
compound components talk to their root:

```rust
// A Trigger toggling its Root (Pine accordion):
pub fn click(&mut self) {
    let Some(item) = ITEM.inject() else { return };
    if let Some(scope) = Scope::find(item) {
        if let Some(inner) = scope.typed::<PineAccordionItem>() {
            Handle::new(inner, item).update(|s| s.click_trigger());
        }
    }
}
```

The `Inject<KEY, T>` *extractor* covers the same need in a **lifecycle
hook** signature — resolve the handle once in `on_setup` / `on_ready`,
store it, and the handlers use the stored handle.

## Related

- [Lifecycle](./04-lifecycle.md) — the hooks these extractors run in.
- [State management → Child → parent](./02-state.md#3-child-parent) — the
  `emit` / `@event` convention.
- [Route guards & loaders](../routing/route-guards-and-loaders.md) — the
  `Loader<T>` extractor.
