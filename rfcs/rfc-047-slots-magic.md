# RFC 047 — `$slots` magic + `Children::has_slot` slot-presence probes

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 011](./rfc-011-scoped-slots.md), [RFC 018](./rfc-018-id-magic.md), [RFC 032](./rfc-032-lifecycle-element-param.md), [RFC 046](./rfc-046-children-extractor.md) |

## 1. Summary

Add two parity surfaces for "did the user provide this slot?":

1. **Template-side (`$slots`)** — a new `$`-prefixed magic
   alongside `$el`, `$refs`, `$store`, `$route`, `$id`, `$event`,
   `$dispatch`. `$slots.footer` returns a truthy value when the
   user passed `<template pp-slot="footer">`. Mirrors Vue's
   `$slots.footer` and Svelte's `$$slots.footer`.

2. **Handler-side (`Children::has_slot` + `slots::has`)** —
   the same probe reachable from lifecycle hooks via the
   RFC-046 extractor and from any scope-aware code via a
   module-level function. Narrower audience than `$slots`; load-
   bearing for animations and other Rust-driven decisions that
   key off slot presence.

```html
<!-- PineDialog.poco — wrapper and footer vanish when the user
     didn't pass a <template pp-slot="footer">. -->
<div class="pine-dialog">
  <div class="pine-dialog-body"><slot/></div>

  <template pp-if="$slots.footer">
    <footer class="pine-dialog-footer"
            pp-transition="slide-up-fade">
      <slot name="footer"/>
    </footer>
  </template>
</div>
```

```rust
// Same answer, from a handler. Useful when the decision drives
// state or animation timing rather than markup.
pub fn on_mount(&mut self, children: Children) {
    if children.has_slot("footer") {
        self.animate_footer_in = true;
    }
}
```

## 2. Motivation

### 2.1 The use case

Primitives that vary behaviour or markup based on whether a
named slot was filled:

* **Structural omission.** Dialog without a footer: don't
  render the `<footer>` wrapper at all (margins, accessibility,
  grid rows all change). CSS `:empty` can hide; it can't
  *remove* wrapper markup.
* **Animation gating.** A collapsible region that slides in
  only when user content is present. Knowing the content is
  there is a prerequisite for committing to the enter animation
  — otherwise you animate an empty box, or you skip the
  animation and the content pops in cold.
* **Conditional icons / actions.** Accordion header that
  renders a default chevron unless the user provided a
  `pp-slot="icon"` — at which point the primitive's own chevron
  must step aside. `<slot>` defaults cover the common case; the
  presence probe is for when *the primitive's other markup*
  needs to react.
* **ARIA / labelling.** A primitive that attaches
  `aria-labelledby` to a user-provided label slot but falls
  back to an inline string otherwise.

### 2.2 What we have today

pocopine ships `Slots(Vec<String>)` (RFC 032 §4.3 Tier 4,
`crates/pocopine-core/src/lifecycle.rs:357-363`) — a list of
captured slot names, handler-side only. No template equivalent.

That's a double ergonomic miss:

* **No template answer.** Authors who want "hide the footer
  wrapper when the footer slot is empty" must plumb a boolean
  through `self` — set it in `on_mount`, bind it to
  `pp-show` / `pp-if`. The template-side expression `$slots.footer`
  that Vue and Svelte authors write in one character costs
  three lines and a state field in pocopine today.
* **Names-only on the handler side.** `Slots(Vec<String>)`
  returns a vec of names. A single-key lookup is a `.iter().any(|n|
  n == "footer")` dance with an allocation — or an unused API
  that authors skip in favour of DOM walks. RFC 046's §2.2
  table shows every Pine primitive avoiding it.

### 2.3 Why both surfaces

Vue and Svelte both ship only the template form (`$slots` /
`$$slots`) — their handler-side reflection is either
`useSlots()` (Vue) or props-based snippets (Svelte 5), not
framework-native presence probes.

pocopine has a reason to ship both:

* **Templates** get `$slots` for the idiomatic "hide this
  element unless the slot is filled" case. Matches the
  `$`-magic pattern already established in `magics.rs:27-42`.
* **Handlers** get `Children::has_slot` (hook-time) and
  `slots::has` (scope-lifetime) for animation gating, state
  branching, and anything that needs to fire a side effect,
  not just conditionally render.

### 2.4 Prior art

| Framework | Template form | Handler form |
|---|---|---|
| **Vue 3** | `v-if="$slots.footer"` | `useSlots().footer` (VNode render fn) |
| **Svelte 4** | `{#if $$slots.footer}` | `$$slots.footer` (bool) |
| **Svelte 5** | `{#if children}` on `children` snippet prop | `let { children } = $props()` |
| **React** | `{footer && <footer>{footer}</footer>}` | `props.footer` prop check |
| **Solid** | `<Show when={props.children}>` | `props.children` presence |

Every framework exposes slot/children presence as a
*boolean-ish* value in the template. This RFC brings pocopine
in line.

## 3. Non-goals

* **Not exposing slot *content*.** `$slots.footer` is truthy /
  falsy, not an iterable of nodes or a render function. The
  `<slot name="footer"/>` element inside the component template
  is still the only way to actually *render* slot content. This
  RFC adds presence probes; it doesn't add
  `$slots.footer.children` or render-function access.
* **Not reactive mid-life.** Slot maps are populated once at
  mount (RFC 011 §5.1). `$slots.footer` doesn't change during
  the scope's lifetime; if a new `<template pp-slot="footer">`
  is added to the DOM after mount, the slot map won't see it.
  Pocopine's capture model is mount-once; that constraint is
  inherited here.
* **Not typed slot content.** Compile-time typed slots
  (`SlotProps<T>`) is the open question from RFC 011 §10, still
  deferred.
* **Not a replacement for `<slot>` defaults.** RFC 011 §5.5
  fallback content continues to work — `<slot
  name="icon"><svg>default</svg></slot>` renders the default
  when the user didn't pass `pp-slot="icon"`. `$slots.icon` is
  for deciding whether the *surrounding primitive markup*
  adapts, which is a different question than what renders
  inside the `<slot>` itself.
* **Not scoped-slot probing.** RFC 011 §4.2 scoped slots bind a
  `pp-let` identifier; `$slots.foo` answers presence only, not
  "what `:prop`s did the component expose." That's a template
  *inside* the slot's own `pp-let` scope — not this RFC's
  problem.

## 4. Design

### 4.1 `$slots` template magic

Add `$slots` to the existing magic resolver at
`crates/pocopine-core/src/magics.rs:27-42`. Returns a JS object
whose keys are the names the user filled.

```rust
// magics.rs
pub fn resolve(key: &str, scope_id: ScopeId) -> JsValue {
    match key {
        "$el" => /* existing */,
        "$refs" => /* existing */,
        // ...
        "$slots" => crate::slots::as_object(scope_id),
        _ => JsValue::UNDEFINED,
    }
}
```

`slots::as_object(scope_id)` builds a plain JS object of
`{ <name>: true }` entries, one per key in `by_name` for that
scope. Properties that weren't filled read back as `undefined`,
which is falsy in the expression evaluator — exactly the truthy
/ falsy semantics `pp-if` / `pp-show` / `v-if`-style reads want.

```js
// Shape returned by $slots at runtime (conceptual).
{ header: true, default: true, footer: true }
```

**Contract for `default`.** `$slots.default` is truthy iff the
user passed *any* non-`pp-slot` children to the component tag.
The walker's slot capture (`crates/pocopine-core/src/slots.rs:45-50`
`put()` skips empty stores; `walker.rs:665-730` populates
`"default"` only when there's non-template unnamed content)
already gives this behaviour — no walker change required.

### 4.2 `Children::has_slot` and `slots::has`

RFC 046 originally proposed `Children::has_slot(name)` on the
new extractor; that method moves here. Combined surface:

```rust
// pocopine-core/src/slots.rs
//
// Public, scope-lifetime lookup. Usable from any code that has
// a ScopeId — event handlers, async callbacks, store
// observers. Does not require a LifecycleContext in scope.
pub fn has(scope_id: ScopeId, name: &str) -> bool {
    STORES.with(|s| {
        s.borrow()
            .get(&scope_id)
            .map(|store| store.by_name.contains_key(name))
            .unwrap_or(false)
    })
}
```

```rust
// pocopine-core/src/lifecycle.rs — on the RFC-046 Children<'a>.
impl<'a> Children<'a> {
    /// `true` if the user provided a `<template pp-slot="name">`
    /// child on the component tag. Reads the captured slot map;
    /// does not walk the DOM. `name = "default"` is truthy iff
    /// the user passed any unnamed children.
    pub fn has_slot(&self, name: &str) -> bool {
        crate::slots::has(self.scope_id, name)
    }
}
```

`Children::has_slot` is sugar — a hook-local wrapper over the
module-level `slots::has`. Authors choose based on where they
are:

| Caller | Prefers |
|---|---|
| Template expression | `$slots.footer` |
| Lifecycle hook with `Children` in scope | `children.has_slot("footer")` |
| Event handler, async task, observer | `pocopine::slots::has(scope_id, "footer")` |

### 4.3 Worked examples

**Dialog — omit footer wrapper entirely when empty.**

```html
<!-- PineDialog.poco -->
<div class="pine-dialog">
  <header class="pine-dialog-header"><slot name="header"/></header>
  <div class="pine-dialog-body"><slot/></div>

  <template pp-if="$slots.footer">
    <footer class="pine-dialog-footer"><slot name="footer"/></footer>
  </template>
</div>
```

One expression, no handler plumbing.

**Animated reveal — commit to the slide-in only when content exists.**

```html
<!-- PineCollapsible.poco -->
<div>
  <button @click="toggle">{{ label }}</button>

  <template pp-if="open && $slots.panel">
    <section pp-transition="fade-slide-down">
      <slot name="panel"/>
    </section>
  </template>
</div>
```

```rust
// PineCollapsible handler — disable the enter-animation path
// entirely when the user didn't provide the panel, so the
// transition system doesn't schedule a no-op tick.
#[handlers]
impl PineCollapsible {
    pub fn on_mount(&mut self, children: Children) {
        self.has_panel_animation = children.has_slot("panel");
    }
}
```

Both surfaces agree because both hit the same slot map.

**Combobox — show a Clear button only when user supplied one.**

```html
<template pp-if="$slots.clear">
  <span class="pine-combobox-clear-wrap">
    <slot name="clear"/>
  </span>
</template>
```

## 5. Implementation

### 5.1 `slots::as_object` + `slots::has`

Both land in `crates/pocopine-core/src/slots.rs` next to the
existing `names_for` and `lookup`:

```rust
/// Build a JS object reflecting the user-provided slots for
/// `scope_id`. Keys are slot names; values are `true`.
/// Resolves to an empty object when the scope has no slot
/// store (no slot fills) — callers reading missing keys still
/// get `undefined` (falsy) as intended.
pub fn as_object(scope_id: ScopeId) -> JsValue {
    let obj = js_sys::Object::new();
    STORES.with(|s| {
        if let Some(store) = s.borrow().get(&scope_id) {
            for name in store.by_name.keys() {
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str(name),
                    &JsValue::TRUE,
                );
            }
        }
    });
    obj.into()
}

/// One-key lookup, mirror of `slots::has` reads from
/// `$slots.<name>` at the expression-evaluator level.
pub fn has(scope_id: ScopeId, name: &str) -> bool {
    STORES.with(|s| {
        s.borrow()
            .get(&scope_id)
            .is_some_and(|store| store.by_name.contains_key(name))
    })
}
```

### 5.2 `magics.rs` addition

One match arm added at `magics.rs:27-42`:

```rust
"$slots" => crate::slots::as_object(scope_id),
```

No other walker / evaluator changes. The expression evaluator
already treats unknown property reads as `undefined` (RFC 012);
`$slots.footer` when `footer` isn't set → `undefined` → falsy
in `pp-if` / `pp-show` / boolean coercion contexts.

### 5.3 `Children::has_slot`

Method on the RFC-046 `Children<'a>` struct. One-line delegate
to `slots::has(self.scope_id, name)`. Lifetime story is
exactly what RFC 046 §4.1 already pins: hook-local struct, but
the method reads from scope-lifetime state, so it's safe for
the duration of the hook.

### 5.4 Tests

* **`slots::has`** — unit tests for present-key, missing-key,
  missing-scope, "default" bucket present vs absent.
* **`slots::as_object`** — construct a scope with captured
  slots, call `as_object`, `Reflect::get` each expected key,
  assert `true`; assert missing keys read back as `undefined`.
* **`$slots` expression path** — one integration test through
  the expression evaluator: `pp-if="$slots.footer"` renders
  iff a `pp-slot="footer"` template was passed.
* **`Children::has_slot`** — mount a component, verify the
  method returns the same answer as `$slots` in a parallel
  template.

## 6. Alternatives considered

### 6.1 `pp-if-slot="footer"` directive

```html
<template pp-if-slot="footer">
  <footer><slot name="footer"/></footer>
</template>
```

Single-purpose directive, one parse-time keyword. Rejected:

* **Overlaps with `pp-if`.** Authors already know `pp-if` takes
  an expression; forcing them to remember a second form for a
  related question is gratuitous.
* **Doesn't compose.** `pp-if-slot="footer"` can't mix with
  other booleans (`pp-if-slot="footer" && open`); needs another
  directive or a devolution into an expression anyway.
* **Doesn't help handlers.** We still want the Rust-side
  answer for animation gating — meaning two mechanisms to
  maintain, one for each side.

A single `$slots` magic gives template authors `pp-if`, `pp-show`,
`pp-class`, computed-prop composition, everything — all free.

### 6.2 Expose slot *content*

`$slots.footer` returning a VNode / DOM fragment reference
(Vue-style). Rejected for v1:

* Pocopine slots materialise into real DOM at walk time
  (RFC 011 §5.2). A "VNode-ish" handle would be a lazy wrapper
  around a `DocumentFragment` the walker consumes on first
  `<slot>` encounter — authors who grab the fragment before the
  slot mounts, or after, would hit lifecycle cliffs.
* Not needed for any motivating use case. Presence is what
  authors ask for; access is what `<slot>` already provides.

### 6.3 Keep `has_slot` on `Children` only (RFC 046's original plan)

Reaches only handler authors. Every real use case in §2.1 is
primarily a template concern; the RFC-046-only proposal would
ship the less-used surface while leaving the idiomatic one
missing. This RFC split adds the template form (`$slots`) —
cheap — and keeps the handler parity path (`Children::has_slot`
/ `slots::has`) for the animation and state-branching cases.

### 6.4 Name it `$has_slot` or `$has_slots`

Verb-style matches `$dispatch`. Rejected: `$slots.footer`
reads as "the footer slot" and nests naturally into
expressions (`!$slots.header && !$slots.footer`); a verb form
requires parentheses (`$has_slot('footer') && !$has_slot('header')`)
and mis-advertises itself as a general method instead of a
reflection object.

## 7. Rollout

1. Land `slots::has` + `slots::as_object` in `pocopine-core`.
2. Wire `$slots` into `magics.rs`.
3. Add `Children::has_slot` on the RFC-046 extractor.
4. Remove `has_slot` language from RFC 046 (the DOM-iteration
   extractor); RFC 046 focuses purely on rendered-child
   iteration.
5. Update `docs/guides/poco/` with the new magic.
6. Migrate one Pine primitive as a reference — `PineDialog`
   footer wrapping is the cleanest showcase.

No breaking changes. `Slots(Vec<String>)` (RFC 032 Tier 4) stays
for authors who've adopted it; deprecation is a follow-up
question once `Children::has_slot` / `$slots` have run for a
release.

## 8. Open questions

* **Should `$slots` include the default bucket as `"default"`?**
  Leaning yes for parity — `$slots.default` answers "did the
  user put any unnamed children in this tag?" which is exactly
  the question Combobox / Autocomplete want when deciding
  whether to render their own placeholder. Current walker
  behaviour (slot store is empty when nothing was captured)
  already gives this for free.
* **Should `$slots` proxy-trap arbitrary property reads so
  `$slots.anything` is a safe truthy/falsy read, or should it
  be a plain object where missing keys hit `undefined`?** Plain
  object is simpler and matches pocopine's expression
  evaluator's existing treatment of missing properties; propose
  plain-object unless authors hit a sharp edge.
* **Should `slots::has` be re-exported from `pocopine::prelude`
  or live in `pocopine::slots::has`?** Module-qualified reads
  better at the call site (`pocopine::slots::has(scope, name)`);
  not worth polluting the prelude for a rare call.
* **Pairing with `pp-fallthrough` or `cx!`?** Out of scope; if
  a future RFC adds automatic slot-presence-driven class
  injection (`:class="{ 'has-footer': $slots.footer }"`
  sugar), this RFC's primitives are what it builds on.
