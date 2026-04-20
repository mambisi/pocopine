# RFC 031 — `#[prop]` / `#[state]` field roles

| Field | Value |
|---|---|
| **Status** | Implemented (breaking) |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-009-pp-model-components.md`](./rfc-009-pp-model-components.md), [Vue 3 `defineProps` / `defineExpose`](https://vuejs.org/api/sfc-script-setup.html#defineprops-defineemits), [React controlled-vs-uncontrolled patterns](https://react.dev/learn/sharing-state-between-components) |

## 1. Summary

Split `#[component]` struct fields into two declarative roles:

- **`#[prop]`** — intended to flow *in* from parents (HTML
  attributes, `pp-bind`, fallthrough, `pp-model` mirror-in).
- **state (the default — unmarked)** — intended to flow *out*.
  Internal reactive state the component owns: DOM-event-driven
  fields (`<img @load>` → `loaded: bool`), compound-derived
  mirrors (`open` in a Trigger mirroring Root), derived IDs,
  computed-ish fields.

`#[prop]` is the explicit "parent contract" marker — same
philosophy as `pub` vs private in Rust: you annotate what leaks
outward, not what stays internal. Compounds usually have two or
three props per sub-part and five-plus state fields (all the
mirror values), so unmarked-is-state also drops annotation
noise where it matters least.

The mechanism stays identical — both are entries in the same
scope state map, both trigger reactivity the same way — but the
role declaration lets the framework:

1. **Skip `#[state]` fields in `apply_static_props`** — so a
   parent accidentally writing `<pine-avatar-image loaded="true">`
   can't clobber the child's own derived state.
2. **Skip `#[state]` fields in `pp-bind` child-prop writes** —
   same reason, one directive up.
3. **Expose role in devtools** — shows a separate "internal state"
   column alongside the props table.
4. **Document intent at the callsite** — a reader scanning the
   struct can tell "this value comes from a parent" vs "this
   value is computed here" without following every handler.

Unmarked fields default to state. No `#[state]` marker — one
attribute does the work, less noise. Enforcement is at the
directive level (runtime `is_prop` gate), not compile time: an
unannotated field that a parent tries to write simply has its
write silently dropped, as if the attribute didn't exist. That
keeps the mental model tight (one attribute, one meaning) and
matches Rust's `pub` vs private default.

```rust
#[component(template = "PineAvatarImage.poco")]
pub struct PineAvatarImage {
    // Props — parent sets via HTML attrs or pp-bind.
    #[prop] pub src: String,
    #[prop] pub alt: String,

    // Internal state — driven by the <img>'s own load/error
    // events via the on_load / on_error handlers. A parent writing
    // `<pine-avatar-image loaded="true">` would be a bug; the
    // annotation makes the macro drop that attr on the floor
    // instead of fighting the event-driven reconciliation.
    #[state] pub loaded: bool,
    #[state] pub error: bool,
}
```

## 2. The counterargument, acknowledged

> "State and props have no distinction — it's still state of the
> component."

True at the implementation level. Both are reactive entries in
the same scope state. Both can be read from templates the same
way. Both trigger re-renders through the same path. The macro
already flattens them into one blob.

This RFC doesn't claim the mechanism *differs*. It claims the
**intent differs and should be machine-readable**.

Analogues:

- **React**: props are intentionally read-only (dev-time
  `Object.freeze` in dev); state is mutable. Same memory, different
  write rules. Most bugs in this space come from blurring the
  line (e.g. derived-state duplicated into state, then drifting
  out of sync with props).
- **Vue 3**: props is a reactive proxy separate from `ref()` /
  `reactive()` state — same reactivity substrate, different read
  rules (`readonly` on the props proxy).
- **Solid / Svelte**: both are reactive signals, props are a
  restricted-read projection.

The pattern is consistent across ecosystems: the runtime uses
one reactive system, but props vs state gets different **policies**.
RFC-031 puts that policy into pocopine.

## 3. Non-goals

- **Readonly enforcement on `#[prop]` in Rust.** Rust borrow
  checking already makes this easy — a `#[prop]` field is just
  a regular `pub field: T`. Authors can write `self.prop = …`
  in handlers if they want; we can't stop them at the type
  system layer without contorting the scope proxy shape. The
  contract is *directional*: `#[prop]` expects parent writes;
  `#[state]` expects self-writes. Violations are author-visible
  in handler code, not runtime errors.
- **Serialization changes.** Serde still sees every field. The
  `#[prop]` / `#[state]` attributes are macro-level hints, not
  runtime flags on the value.
- **Computed state.** Follow-up RFC (`#[computed]`) — derived,
  read-only, auto-tracked. Out of scope here.
- **Required / optional / defaulted props.** Vue 3 has
  `defineProps({ foo: { type: String, required: true } })`.
  Useful, but separate — a `#[prop(required)]` attribute would
  layer on top of this RFC, not replace it.

## 4. Surface

### 4.1 Attribute syntax

```rust
#[component(template = "…")]
pub struct Thing {
    /// Parent-provided. Explicit — flows in from HTML attrs,
    /// `pp-bind:size`, or `pp-model:size`.
    #[prop]
    pub size: String,

    /// Internal — parent writes are silently dropped. No marker
    /// required (unmarked = state).
}
```

### 4.2 Accepted modifiers

v1 keeps the attribute bare — `#[prop]` with no arguments.
Future extensions considered:

- `#[prop(required)]` — error if parent doesn't supply.
- `#[prop(default = "…")]` — override Default.

These come later. v1 ships the bare marker only.

### 4.3 Default = state

No opt-out, no opt-in. An unmarked field is state. An annotated
`#[prop]` field is a prop. That's the full surface.

## 5. Behavior

### 5.1 Static attribute application

`walker::apply_static_props` skips non-prop fields:

```rust
fn apply_static_props(el: &Element, scope: &Scope) {
    // … existing attribute iteration …
    let field = name.replace('-', "_");
    // NEW — only `#[prop]` fields accept writes from HTML attrs.
    if !scope.state.borrow().is_prop(&field) {
        continue;
    }
    // … existing assignment …
}
```

`ComponentState::is_prop(&str) -> bool` is macro-generated; it
returns `true` only for fields annotated `#[prop]`. Unknown
keys and state fields return `false`.

### 5.2 `pp-bind` child-prop path

Same rule — writing from parent's `pp-bind:foo="path"` to a
registered component's `foo` prop calls `is_prop("foo")` first.
State fields stay opaque to the parent. The gate sits at the
pp-bind directive (not the child's proxy set trap), because the
child's OWN writes (internal handlers, `self.field = …`,
`Handle::update(…)`) also route through the same proxy and
must always land.

### 5.3 Fallthrough attrs (RFC-010)

Unchanged — fallthrough still skips any attribute matching a
declared field name regardless of role. State fields are
declared (they're `keys()` entries), so they don't fall through
either; the author simply gets no effect from writing them.

### 5.4 `pp-model` child path

`pp-model:<field>="parent_key"` writes `event.detail` to the
parent's `parent_key` and mirrors parent's value into child's
`<field>`. If `<field>` is state (unmarked), the mirror-in leg
is skipped (same gate as §5.1). The event-out leg stays
unchanged — state fields can still emit `pp:update:model`; the
role annotation governs *writes into* the field, not emits
out.

### 5.5 Reactivity & template access

Zero change. Templates read `<span pp-text="loaded">` the same
way whether `loaded` is `#[prop]` or state. `watch_field` /
`watch_scope_field` subscribe to any field regardless of role.

### 5.6 Devtools

Devtools' component panel groups fields into two sections — props
above, state below — driven by the same macro-generated
`is_prop` method. Follow-up work.

## 6. "State from a special entity" — §7 expansion

Raised by the original discussion: some fields are driven by a
*wrapped* element's own native state, not by pocopine's reactive
system. `<pine-avatar-image>` wraps an `<img>` whose load/error
events are the source of truth for `loaded` / `error`.

This RFC treats that as the *textbook `#[state]` case* — not a
separate role. The distinction "parent-writable or not" already
captures it:

```rust
#[component(template = "PineAvatarImage.poco")]
pub struct PineAvatarImage {
    #[prop] pub src: String,
    #[prop] pub alt: String,
    // Unmarked (= state) because the <img>'s events are the
    // authority, not the parent. The handler path:
    //
    //   <img @load="on_load" @error="on_error">
    //
    //   fn on_load(&mut self) { self.loaded = true; ... }
    //
    // is plain reactive state mutation; omitting `#[prop]`
    // keeps parents out of the field.
    pub loaded: bool,
    pub error: bool,
}
```

No new machinery, no "entity-backed" role — the wrapped element
IS just another source of mutation, like a timer callback or a
network fetch. If, later, a pattern emerges where multiple
components want shared boilerplate for wrapping an element's
native state (common enough across `<img>`, `<video>`, `<audio>`,
`<details>` toggle, form-input `:invalid`), we can land a
`#[mirror]` helper that takes a DOM event name and a setter —
but that's ergonomic sugar for unmarked-state + handler, not a
third role.

## 7. Implementation sketch

### 7.1 `crates/pocopine-macros/src/lib.rs`

Parse the per-field attributes when building `field_idents` /
`field_names`: consume `#[prop]`, record which fields carried it,
leave unmarked fields alone. The macro strips the `#[prop]`
attribute from the emitted struct so rustc doesn't see an
unknown attribute.

Emit an extra `ComponentState` member:

```rust
fn is_prop(&self, key: &str) -> bool {
    matches!(key, #(#prop_field_names)|*)
}
```

When there are zero prop fields, the arm is `false` (with a
`let _ = key;` to silence the unused-var lint).

The `is_prop` method also lives on the `ComponentState` trait
with a default `false` impl, so non-component `ComponentState`
impls (if any appear) don't break.

### 7.2 `crates/pocopine-core/src/walker.rs` + directives

Three sites gate on `is_prop`:

1. `walker::apply_static_props` — static HTML-attr writes.
2. `directives::bind::run` — parent→child `pp-bind:<prop>` path.
   Uses the new `walker::child_component_scope(el)` helper to
   get the child's scope id + proxy, so we can query
   `is_prop` without going through the proxy (which would track
   a dependency we don't want).
3. `directives::model::run_component` — parent→child mirror-in
   leg of `pp-model:<field>`.

The proxy's set trap itself stays ungated — child components
write to their own state through the same proxy during handlers,
and gating there would break internal writes.

### 7.3 Migration pass

Mechanical per Pine compound — read each struct, mark
parent-writable fields `#[prop]`:

- Set in handlers only → leave unmarked (state).
- Referenced in HTML attrs or `pp-model:<field>` in demos/tests
  → `#[prop]`.
- Both (e.g. Dialog's `open` is prop-settable AND self-mutated
  through handlers) → `#[prop]` (parent-writable wins when in
  doubt; the annotation governs the *can-parent-write-it*
  contract).

Actual distribution across the Pine compounds:

- Most `open` / `value` / `values` / `checked` / `pressed` →
  `#[prop]` (they're the `pp-model` binding targets).
- Most `*_id` (title_id, description_id, trigger_id, label_id),
  `anchor` → unmarked state (derived from scope id in `on_setup`).
- Mirror fields on sub-parts (Trigger's `open`, Item's
  `selected`) → unmarked state (set by `watch_scope_field`,
  never by parent).
- Avatar's `loaded` → unmarked state (driven by `<img @load>`).

### 7.4 No compile error

Unannotated fields are valid (= state). The breakage is
behavioural — a parent trying to write a previously-writable
field now has its write silently dropped. Authors verify by
running their test suite; there's no macro-level check that
flags "you probably wanted to annotate this."

## 8. Alternatives considered

### 8.1 Two structs

```rust
struct ThingProps { pub size: String }
struct ThingState { pub hovered: bool }
#[component(props = ThingProps, state = ThingState, …)]
struct Thing { /* generated */ }
```

Heavy ceremony. The scope proxy would need to fuse the two into
a unified reactive blob anyway. Rejected for author-facing
overhead outweighing the conceptual clarity.

### 8.2 Marker types

```rust
pub struct Thing {
    pub size: Prop<String>,
    pub hovered: State<bool>,
}
```

Breaks the zero-cost principle — every field read becomes `.0`
or `.get()`. Serde derivation needs extra plumbing. Rejected.

### 8.3 Name convention (`_hovered`, `hovered_`)

Ugly. No compile-time enforcement. Rejected — attributes exist
for a reason.

### 8.4 Status quo (do nothing)

Keep mixing, trust convention. Fine for a two-person project;
breaks down as a library grows or is consumed. Pine's compounds
already have ~40 fields that could be confusing; Pine itself
needs the clarity and so will downstream consumers authoring
their own components.

## 9. Open questions

- **Should state fields still be `pub`?** Rust visibility and
  "parent-writable" are orthogonal axes. A `pub` field that
  can't be written from attrs is mildly confusing but symmetric
  with the prop fields. v1 accepts any visibility; the field
  still needs to be `pub` to participate in reactivity /
  template bindings.
- **Should state fields emit `pp:update:model`?** The RFC says
  "governs writes-in, not emits-out" — an unmarked field can
  still be a `pp-model` target from outside. Useful when the
  child reports internal changes (e.g. Avatar's `loaded` could
  notify the parent when the image finishes loading) without
  being overwritten by the parent.

## 10. Rollout — breaking, single-shot

1. Land macro support + `is_prop` in `pocopine-macros` +
   `pocopine-core`. Unmarked fields keep compiling (now state).
2. Walker's `apply_static_props` + `pp-bind` child-prop + `pp-model`
   mirror-in honour the role in their skip rules.
3. Migrate every `#[component]` struct in the workspace in the
   same commit series — Pine (all compounds), every example,
   every test fixture — adding `#[prop]` to fields that were
   parent-writable.
4. Update RFC-001 and docs.

No deprecation window. The breakage is behavioural: a parent's
attempt to write a newly-unmarked field is now silently dropped,
so the migration is "find the prop fields, annotate them, run
tests."
