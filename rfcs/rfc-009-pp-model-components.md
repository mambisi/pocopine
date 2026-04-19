# RFC 009 — `pp-model` on components

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md) §5.5 (slots / props), [`rfc-008-event-handler-args.md`](./rfc-008-event-handler-args.md) (required for the child-side dispatch) |

## 1. Summary

Let `pp-model="field"` work across a custom-component boundary, not
just on native inputs. Parent binds a scope field; child component
exposes a `model` prop and emits `pp:update:model` when the value
should change. Headless-UI-style contract — no magic, two
visible seams.

```html
<!-- parent -->
<pine-input pp-model="email"></pine-input>
```

```rust
#[component]
pub struct PineInput {
    pub model: String,
    // ...
}

#[handlers]
impl PineInput {
    pub fn on_input(&mut self, ev: InputEvent) {
        if let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) {
            let value = el.value();
            self.model = value.clone();
            crate::dispatch_event("pp:update:model", &JsValue::from_str(&value));
        }
    }
}
```

## 2. Motivation

Today `pp-model` works on `<input>` / `<textarea>` / `<select>` via
`addEventListener("input", ...)` + direct `value` reads. Nothing
flows through a component. Pine can't ship `<pine-input>`,
`<pine-slider>`, `<pine-select>` without cross-component two-way
binding — every form input blocks on this.

The contract needs to be **explicit on both sides**:

* The parent doesn't need to know the child exists — it writes
  `pp-model="email"` and trusts the directive.
* The child doesn't need to know the parent's field name — it has
  a `model` field and emits an event when it changes.

## 3. Non-goals

* **Custom model names per binding** (Vue's
  `v-model:open="isOpen"`). v0 uses a single hard-coded `model`
  field. Supporting other names means propagating the prop name
  through the directive + the emitted event; worth a follow-up once
  Pine needs a `<pine-combobox v-model:open>` pattern.
* **Modifiers** (`pp-model.lazy`, `.number`, `.trim`) on component
  targets. The child component owns the transformation; if it wants
  a numeric model, its internal `on_input` converts.
* **Bidirectional proxies.** We don't wire the child's `set`
  trap to push back to the parent automatically — too magical, and
  it'd silently double-trigger on every field change. Explicit
  event dispatch stays.
* **Validation / async updates.** The child can choose to dispatch
  conditionally (debounce, schema check) — not a directive concern.
* **`pp-model` on stores.** Stores are addressed through
  `$store.*`; this RFC covers component-to-component only.

## 4. Surface

### 4.1 Parent side

```html
<pine-input pp-model="email"></pine-input>
```

Exactly the same attribute syntax as today. The directive detects
that the host is a registered component tag and switches to the
component path; otherwise falls through to the existing native-input
behaviour.

### 4.2 Child side

A component participating in `pp-model` declares:

* a `pub model: T` field (`T: Serialize + Deserialize + Default`),
* an event dispatch of `pp:update:model` whose `detail` is the new
  value, emitted whenever the user action should update the bound
  field.

That's the entire contract. Naming is the convention; the runtime
doesn't introspect method names or annotations.

## 5. Semantics

### 5.1 Mount-time setup

When `pp-model`'s directive runs on a host element that
[`is_registered`](../crates/pocopine-core/src/templates.rs) returns
true for (i.e. a component tag):

1. **Prop binding (parent → child).** Register an effect that
   reads `parent[field]` and writes to the child's proxy's
   `model`. Same mechanism as `pp-bind:model="field"` — reuses the
   `child_component_proxy` helper.
2. **Event listener (child → parent).** Add a listener on the host
   element for `pp:update:model`. On fire, take
   `ev.detail`, convert via `FromHandlerArg`-like path (this RFC
   depends on RFC-008's trait for the conversion code), and write
   to `parent[field]`.

### 5.2 Unmount

Standard element removal releases the effect + listener as usual
(observer + `release_subtree`). No custom teardown needed.

### 5.3 Reentrancy

A cycle is possible if the child naively emits `pp:update:model`
every time its `model` field changes, including the write the
parent just pushed. The convention resolves this: the child only
emits when **user action** causes the change (an `on_input` /
`on_change` handler), not from a plain set. Same pattern as Vue's
`v-model` — authors don't wire the set-trap to `$emit`.

### 5.4 Native-input short-circuit

The existing native path (input/textarea/select) stays untouched:
if the host isn't a registered component, `pp-model` uses the
direct `value` / `checked` / `input event` wiring. No behaviour
change for `<input pp-model="foo">`.

## 6. Examples

### 6.1 Pine text input

```html
<!-- PineInput.poco -->
<input
  class="pine-input"
  pp-bind:value="model"
  pp-on:input="on_input"
  pp-ref="el"
/>
```

```rust
#[component(style = "pine-input.css")]
pub struct PineInput {
    pub model: String,
}

#[handlers]
impl PineInput {
    pub fn on_input(&mut self, ev: InputEvent) {
        let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            else { return };
        let v = el.value();
        self.model = v.clone();
        pocopine::dispatch_event("pp:update:model", &JsValue::from_str(&v));
    }
}
```

Parent usage:

```html
<pine-input pp-model="name"></pine-input>
```

### 6.2 Pine checkbox

```html
<!-- PineCheckbox.poco -->
<label class="pine-checkbox">
  <input type="checkbox" pp-bind:checked="model" pp-on:change="on_change" />
  <slot></slot>
</label>
```

```rust
#[component]
pub struct PineCheckbox { pub model: bool }

#[handlers]
impl PineCheckbox {
    pub fn on_change(&mut self, ev: Event) {
        let checked = ev.target()
            .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            .map(|el| el.checked())
            .unwrap_or(false);
        self.model = checked;
        pocopine::dispatch_event("pp:update:model", &JsValue::from_bool(checked));
    }
}
```

### 6.3 Composing two levels

```html
<pine-field label="Email">
  <pine-input pp-model="email"></pine-input>
</pine-field>
```

Nothing special: `<pine-field>` renders its slot verbatim, the
`<pine-input>` inside binds against the parent's `email` field.

## 7. Implementation

### 7.1 `dispatch_event` helper

Add a re-export in `pocopine::prelude`:

```rust
pub fn dispatch_event(name: &str, detail: &JsValue) {
    // Fire a bubbling CustomEvent from the component's host element
    // (pulled via scope::current_el, set during every directive run).
    // ...
}
```

Essentially the same implementation as the `$dispatch` magic, but
callable from Rust. `$dispatch` in templates already fires the
right event; Rust handlers need the programmatic counterpart.

### 7.2 `pp-model` directive branch

Augment `directives/model.rs`:

```rust
if child_component_proxy(call.el).is_some() {
    run_component_path(call);
} else {
    run_native_path(call);  // existing
}
```

`run_component_path`:

1. **Parent → child effect.** Reuse `pp-bind`-style code: read
   `resolve_path(parent_proxy, &field)` → write to child's `model`.
2. **Child → parent listener.** `el.add_event_listener_with_callback(
   "pp:update:model", cb)`. In the callback:
   * `let detail = ev.detail();`
   * `let cur = resolve_path(&parent_proxy, &field);`
   * Deserialize `detail` into the same shape as `cur` — via
     `serde_wasm_bindgen::from_value` if `cur` came from a
     Serialize-ing field — and write with `path::write_path`.

Field path semantics follow the existing native `pp-model`
(same `write_path` util as `$store.foo.bar`).

### 7.3 Relationship to RFC-008

This RFC depends on RFC-008 shipping first. The child's event
handler (`on_input(ev: InputEvent)`) requires typed arg support;
without it the Pine component can't observe the event that drives
the model update.

## 8. Edge cases

* **Missing `model` field on the child.** Writing to a field the
  child's state doesn't know returns `UNDEFINED` (macro-generated
  `set` match's default branch). No crash — the binding is a
  silent no-op. A debug-mode warning is a follow-up.
* **Multiple `pp-model` on the same child.** Disallowed — the
  contract names a single `model` field. The directive uses the
  last-written value (standard attribute behaviour); flagged via
  a future lint.
* **Child emits without changing its own `model` first.** Works
  — the parent will take the detail at face value. The child
  can choose to skip updating its own state (e.g. "fire & forget
  custom value" flows).
* **Cycle via `set` trap.** Avoided by convention: the child
  writes `self.model = …` inside its handler, the parent then
  writes back through `pp-model`. That second write triggers the
  child's set trap, which by convention does NOT re-emit.
  Authors who want automatic emission should write it manually —
  and suffer the cycle unless they de-dup.

## 9. Alternatives considered

* **Magic auto-binding via the child's `set` trap.** Child's set
  trap automatically emits the update event. Zero boilerplate for
  component authors, but easy to get into infinite echo loops.
  Rejected.
* **Callback prop pattern** (`pp-bind:on-change="set_field"`).
  React-style controlled components. Works but needs the parent
  to name the callback, pulling the "one canonical way" feel
  toward the React end. Less ergonomic for simple bindings.
* **`pp-model:prop="field"` for non-"model" names.** Adds enough
  surface that it's worth its own RFC; deferred.
* **Let the directive peek at a `#[model]` macro attribute** on
  the child's struct field. More explicit but more ceremony —
  every component has to annotate. The name-by-convention approach
  wins on ergonomics.

## 10. Out of scope (future)

* Multi-model components (`pp-model:open`, `pp-model:selected`).
* `pp-model` modifiers (`.lazy`, `.number`, `.trim`).
* Store-aware `pp-model` (`$store.cart.items.length`). Already
  supported via `pp-model` in native-input mode; no change here.
* Debug-mode warnings on missing `model` field / duplicate bindings.
