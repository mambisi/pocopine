# RFC 031 — `#[prop]` / `#[state]` field roles

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-009-pp-model-components.md`](./rfc-009-pp-model-components.md), [Vue 3 `defineProps` / `defineExpose`](https://vuejs.org/api/sfc-script-setup.html#defineprops-defineemits), [React controlled-vs-uncontrolled patterns](https://react.dev/learn/sharing-state-between-components) |

## 1. Summary

Split `#[component]` struct fields into two declarative roles:

- **`#[prop]`** — intended to flow *in* from parents (HTML
  attributes, `pp-bind`, fallthrough, `pp-model` write path).
- **`#[state]`** — intended to flow *out*. Internal reactive
  state the component owns: DOM-event-driven fields
  (`<img @load>` → `loaded: bool`), compound-derived mirrors
  (`open` in a Trigger mirroring Root), computed-ish fields.

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

Unmarked fields keep the current semantics (parent-settable),
so this is an additive annotation pass, not a breaking change.

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
    /// Parent-provided. Default behavior.
    #[prop]
    pub size: String,

    /// Internal — parent can't write to this via attrs.
    #[state]
    pub hovered: bool,

    /// Unmarked — treated as `#[prop]` for backward compat
    /// (current Pine code). Macro emits a deprecation lint
    /// after one release window so authors can migrate.
    pub unclassified: u32,
}
```

### 4.2 Accepted modifiers

v1 keeps the attributes bare — `#[prop]` and `#[state]` with no
arguments. Future extensions considered:

- `#[prop(required)]` — error if parent doesn't supply.
- `#[prop(default = "…")]` — override Default.
- `#[state(compute = "path_or_fn")]` — derive from other fields
  reactively; see §3's follow-up note.

These come later. v1 ships the bare role annotations only.

### 4.3 `#[component]` attribute option

```rust
#[component(template = "…", strict_roles)]
```

With `strict_roles`, unmarked fields are a compile error. Without
it, unmarked fields default to `#[prop]` for smooth migration.
Pine's own crate flips `strict_roles` on after the migration pass
lands; user apps can opt in at their own pace.

## 5. Behavior

### 5.1 Static attribute application

`walker::apply_static_props` skips fields declared `#[state]`:

```rust
fn apply_static_props(el: &Element, scope: &Scope) {
    // … existing attribute iteration …
    let field = name.replace('-', "_");
    // NEW: ask ComponentState whether the field is prop-writable.
    if !scope.state.borrow().is_prop(&field) {
        continue;
    }
    // … existing assignment …
}
```

`ComponentState::is_prop(&str) -> bool` is macro-generated; it
returns `false` for fields annotated `#[state]`, `true` for
`#[prop]` / unmarked.

### 5.2 `pp-bind` child-prop path

Same rule — writing from parent's `pp-bind:foo="path"` to a
registered component's `foo` prop calls `is_prop("foo")` first.
`#[state]` fields stay opaque to the parent.

### 5.3 Fallthrough attrs (RFC-010)

Current fallthrough skips `pp-*`, `@`, `:`, and attrs matching a
declared field. Adding the `#[state]` rule: fallthrough treats
`#[state]` fields as "declared, don't fall through" exactly like
a `#[prop]` — so `<pine-thing hovered="true">` doesn't end up on
the template root as an HTML attribute either.

### 5.4 `pp-model` child path

`pp-model:<field>="parent_key"` writes `event.detail` to the
parent's `parent_key` and mirrors parent's value into child's
`<field>`. If `<field>` is `#[state]`, the mirror-in leg is
skipped (same rule as §5.1). The event-out leg stays unchanged
— components still fire `pp:update:model` on internal state
changes if they want the two-way binding for that specific
field, and authors can mark a field both `#[state]` *and* emit
from it (the role annotation governs *writes into* the field,
not emits out).

### 5.5 Reactivity & template access

Zero change. Templates read `<span pp-text="loaded">` the same
way whether `loaded` is `#[prop]` or `#[state]`. `watch_field` /
`watch_scope_field` subscribe to any field regardless of role.

### 5.6 Devtools

Devtools' component panel groups fields into two sections — props
above, state below — driven by `ComponentState::field_role(&str)`
(the same macro-generated metadata that powers `is_prop`).

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
    // `#[state]` because the <img>'s events are the authority,
    // not the parent. The handler path:
    //
    //   <img @load="on_load" @error="on_error">
    //
    //   fn on_load(&mut self) { self.loaded = true; ... }
    //
    // is plain reactive state mutation; the annotation only
    // says "parents stay out of this field."
    #[state] pub loaded: bool,
    #[state] pub error: bool,
}
```

No new machinery, no "entity-backed" role — the wrapped element
IS just another source of mutation, like a timer callback or a
network fetch. If, later, a pattern emerges where multiple
components want shared boilerplate for wrapping an element's
native state (common enough across `<img>`, `<video>`, `<audio>`,
`<details>` toggle, form-input `:invalid`), we can land a
`#[mirror]` helper that takes a DOM event name and a setter —
but that's ergonomic sugar for the `#[state]` + handler pattern,
not a third role.

## 7. Implementation sketch

### 7.1 `crates/pocopine-macros/src/lib.rs`

Parse the per-field attributes when building `field_idents` /
`field_names`:

```rust
enum FieldRole { Prop, State }

struct FieldInfo {
    ident: Ident,
    name: String,    // stripped of `r#`
    role: FieldRole, // #[prop] / #[state] / unmarked default
}

fn parse_fields(input: &DeriveInput) -> Vec<FieldInfo> { … }
```

Emit an extra `impl` member:

```rust
fn field_role(&self, key: &str) -> Option<&'static str> {
    match key {
        #(#prop_field_names => Some("prop"),)*
        #(#state_field_names => Some("state"),)*
        _ => None,
    }
}
fn is_prop(&self, key: &str) -> bool {
    matches!(self.field_role(key), Some("prop"))
}
```

Add the `is_prop` method to the `ComponentState` trait.

### 7.2 `crates/pocopine-core/src/walker.rs`

One line in `apply_static_props`, one in the fallthrough skip
list, one in `pp-bind`'s child-prop write:

```rust
if !scope.state.borrow().is_prop(&field) {
    continue;
}
```

### 7.3 Migration pass

Mechanical per Pine compound — read each struct, annotate fields
as `#[prop]` or `#[state]` based on how they're used:

- Set in handlers only → `#[state]`
- Referenced in HTML attrs + author-facing usage → `#[prop]`
- Both (e.g. Dialog's `open` is prop-settable AND self-mutated
  through handlers) → `#[prop]` (parent-writable wins when in
  doubt; the annotation governs the *can-parent-write-it*
  contract).

Expected distribution across the ten Pine compounds:

- Most `open` / `value` / `values` / `checked` / `pressed` →
  `#[prop]` (they're the `pp-model` binding targets).
- Most `*_id` (title_id, description_id, trigger_id, label_id) →
  `#[state]` (derived from scope id in `on_setup`).
- Mirror fields on sub-parts (Trigger's `open`, Item's
  `selected`) → `#[state]` (set by `watch_scope_field`, never
  by parent).
- Avatar's `loaded`, accordion-item's internal mirror of Root's
  `values` membership → `#[state]`.

### 7.4 Lint + deprecation

After RFC lands:

1. Macro accepts both annotated and unannotated fields.
2. In `#[component(template = "…", strict_roles)]` mode, an
   unannotated field is an error — Pine's crate opts in.
3. In the lax default, an unannotated field emits a
   `#[deprecated]`-style compile warning suggesting one of the
   two roles.

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

- **Should `#[state]` fields still be `pub`?** Rust visibility
  and "parent-writable" are orthogonal axes. A `pub` field that
  can't be written from attrs is confusing. Options: require
  `pub(crate)` on `#[state]`, or accept the confusion as
  documentation noise. v1 accepts any visibility; style guide
  recommends `pub` for symmetry with other fields even though
  parents can't write it.
- **Should `#[state]` fields emit `pp:update:model`?** The RFC
  says "governs writes-in, not emits-out" — a `#[state]` field
  can still be a `pp-model` target. Confusing? Let's see if real
  usage produces the pattern before forbidding it.
- **Naming.** `#[state]` overloads a common word (scope state,
  state trait, component state). Alternatives: `#[internal]`,
  `#[private_prop]`, `#[owned]`. None are as clear as `#[state]`;
  sticking with it.

## 10. Rollout

1. Land macro support + `is_prop` / `field_role` in
   `pocopine-macros` + `pocopine-core`.
2. Walker + pp-bind + fallthrough + pp-model honour the role in
   their skip rules.
3. Annotate every Pine compound (one pass across ~10 modules).
4. Flip `strict_roles` on for `crates/pine/`.
5. Document in `docs/` and update RFC-001.
