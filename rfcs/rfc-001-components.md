# RFC 001 — Components

| Field  | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-18 |
| **Supersedes** | — |
| **Related docs** | [`docs/components/`](../docs/components/), [`docs/pcx/`](../docs/pcx/) |

## 1. Summary

A pocopine component is a Rust struct plus a `#[handlers]` impl plus a
`.pcx` template plus an optional `.css` stylesheet. The macro derives
everything it can — tag name, template path, stylesheet path — so
annotating a struct with `#[component]` is typically all the author
writes. Composition is tag-based via bare kebab-case tags; props flow
through HTML attributes; state lives in struct fields; cross-subtree
state lives in stores. This RFC is the authoritative spec.

## 2. Motivation

Component ergonomics are the most exposed surface of a framework. The
first milestone shipped the runtime primitives but not the authoring
model; as soon as we let users build apps, they will ask:

* Where do files go?
* How do components talk to each other?
* How do I share state?
* Is this a prop? A store? A directive?

Left unspecified, every team answers differently and pocopine becomes
"a library" rather than "a framework." An opinionated, written-down
component model removes those decisions from the author and locks in
one consistent style across apps.

## 3. Goals

* Single canonical structure for every component, for every app.
* Zero ceremony for the common case: `#[component]` with no arguments.
* Composition reads as HTML, not configuration.
* No mixed-language files — Rust, HTML, CSS each stay in their native
  file type (see `feedback_pcx_format`).
* Multiple components per Rust module allowed (see
  `feedback_rust_modules`).

## 4. Non-goals

* Functional/hook-style components.
* Render functions (inline `html!` macro or similar).
* Prop APIs for callbacks (use event dispatch).
* Two-way prop binding (`pp-model:prop`) — not in v0.
* Named slots, `pp-for` iteration — deferred, gated by array
  reactivity (see `docs/reactivity/02-roadmap.md`).

## 5. Design

### 5.1 Declaration

A component is a struct annotated with `#[component]`, paired with a
`#[handlers] impl`.

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
}

#[handlers]
impl Counter {
    pub fn init(&mut self)      { self.count = 0; }
    pub fn increment(&mut self) { self.count += 1; }
}
```

* `Default + Serialize + Deserialize` are required derives.
* All reactive state is `pub`.
* Handlers take `&mut self` and nothing else (event/arg support is a
  later RFC).

### 5.2 Naming & auto-derivation

Given `pub struct <Ident>`, the macro derives:

| Attribute | Default |
|---|---|
| `name` | kebab-case of the struct ident |
| `template` | `"<Ident>.pcx"` resolved relative to the `.rs` file |
| `style` | `"<Ident>.css"` resolved relative to the `.rs` file, if the file exists |
| Tag | `<name>` (no prefix) |

All three are overridable via explicit args:
`#[component(name = "...", template = "...", style = "...")]`.

The macro rejects struct idents whose kebab-case matches a known
HTML5 element name (`Button`, `Article`, `Input`, `Form`, …) to
prevent silent collisions with real HTML elements. Authors rename
the struct or pass an explicit `name = "..."` when they need the
matching label in the Rust API.

### 5.3 File layout

```
src/
  components/
    counter.rs          # module
    Counter.pcx         # template
    Counter.css         # (optional)

    todo.rs             # module with multiple components
    TodoList.pcx
    TodoList.css
    TodoItem.pcx
    TodoItem.css
  lib.rs
```

* `.rs` = Rust module, can hold multiple components and helper types.
* `.pcx` and `.css` = per-component, named after the struct (PascalCase).
* Avoid per-component directories until a component demands its own
  subtree; start flat, promote later.

### 5.4 Template format (`.pcx`)

`.pcx` is HTML with `pp-*` directives — no embedded Rust or CSS.

* **Single root element.**
* Authored template **does not include `pp-data`** on the root; the
  compiler injects it when emitting the registered template string.
* Attribute values are bare identifiers (field or handler name) in v0.
  Rust expressions in values are a future RFC.
* Comments (`<!-- -->`) are allowed and stripped by the compiler.

### 5.5 Style format (`.css`) + scoping

A plain `.css` file. When a stylesheet is associated with a component,
it is **scoped by default** using the data-attribute strategy
(`data-pp-<hash>` on every template element; `[data-pp-<hash>]`
appended to every selector's last compound). Opt out per-rule with
`:global(...)`. Full spec: `docs/pcx/03-scoped-styles.md`.

Implementation uses `lightningcss` for parsing and rewriting.

### 5.6 State tiers

Four tiers, one canonical pattern each.

| Tier | Pattern |
|---|---|
| **Local** | `pub` field on the component struct |
| **Parent → Child** | HTML attribute on the child tag (static or `pp-bind:`) |
| **Child → Parent** | `$dispatch` a `CustomEvent`; parent listens with `pp-on:event` |
| **Global** | `#[store]` singleton, read via `$store.<name>` magic |

Anti-patterns (rejected by review):

* Global mutable state outside a store.
* Non-`Serialize` fields on a component struct.
* Reaching across components' scopes from user code.
* Manually-synced derived state (use `computed` once signals land).

### 5.7 Composition & props

A parent instantiates a child via `<<name>>` — bare kebab-case tag,
no prefix. The `#[component]` macro rejects idents whose kebab-case
matches an HTML element name, which is how we avoid collisions with
real tags.

Attributes on the tag become props:

```html
<todo-item id="1" label="Buy milk" done />
<todo-item pp-bind:id="current.id" pp-bind:label="current.label" />
```

Attribute semantics:

| Form | Behavior |
|---|---|
| `name="value"` | Static: sets the field once, on mount, not reactive. |
| `pp-bind:name="expr"` | Reactive: evaluates `expr` in the parent's scope, sets the child's field inside an `effect()`. Re-fires on parent changes. |
| `name` (no value, bool field) | Sets the bool to `true`. |

Parsing rules per field type:

| Field type | Attribute value format |
|---|---|
| `String` | verbatim |
| integers / floats | decimal literal |
| `bool` | presence-only or `"true"`/`"false"` |
| `Option<T>` | absent = `None`; else parse as `T` |
| `Vec<T>`, `HashMap<..>`, other `Deserialize` | JSON string |

Props are always one-way parent → child. For child-side mutations the
parent needs to see, dispatch an event.

Only `pub` fields are settable from attributes. Unknown prop names =
warning (dev build), no-op (release). Missing props = default value
(not an error).

### 5.8 Slots

Children inside a component tag become slot content. v0 supports
exactly one default slot per template, marked with the native HTML
`<slot>` element:

```html
<!-- Card.pcx -->
<div class="card">
  <header pp-text="title"></header>
  <slot></slot>
</div>
```

```html
<!-- usage -->
<card title="Hello">
  <p>Body text.</p>
</card>
```

`<slot>` is the native HTML element. Outside Shadow DOM (which we
don't use), it's inert — the browser renders it as a pass-through —
which means we can repurpose the tag as our slot marker with no
clash. The walker replaces the `<slot>` in the cloned template with
the children that were captured from the parent tag.

Slot content stays in the parent's scope — handlers referenced inside
slot content resolve to the parent's handlers. CSS scoping is via the
attribute strategy and is parent-scoped for slot content by design.

Named slots are deferred to a later RFC.

### 5.9 Iteration (deferred)

Planned surface, gated on the array-reactivity milestone:

```html
<todo-item
  pp-for="todo in todos"
  pp-bind:key="todo.id"
  pp-bind:id="todo.id"
  pp-bind:label="todo.label" />
```

`pp-bind:key` will be required. Syntax is committed so authors plan
around it; implementation arrives after array reactivity lands.

### 5.10 Lifecycle

Exactly one lifecycle hook: **`init`**. Runs once, after the scope is
bound and before any other directive on the element fires.

No other hooks exist (`mount`, `unmount`, `update`, etc.). Setup goes
in `init`; teardown will be handled by `on_cleanup` registered inside
an `effect()` when signals land. User code does not get a generic
unmount hook.

## 6. Runtime responsibilities

`pocopine-core` provides:

* Scope / `ComponentState` / proxy bridge — **exists.**
* Directive registry + walker + `MutationObserver` — **exists.**
* `register_component(name, ctor)` — **exists.**
* `register_template(name, html)` — **new, required for this RFC.**
* `inject_style(component, css)` — **new, required for this RFC.**
* Tag resolution in the walker (registered tag → `instantiate`) — **new.**
* Attribute-prop application on mount (static + `pp-bind:`) — **new.**
* Default slot capture + template clone — **new.**
* `pocopine::mount(name, target)` helper for client-side mounting —
  **new, optional.**

## 7. Compiler responsibilities (macros)

`pocopine-macros` provides:

* `#[component]` — **exists**, gains:
  * optional `name`, `template`, `style` keyword args (all derived by
    default);
  * `include_str!` emission for `template` / `style` so cargo tracks
    edits;
  * injection of `pp-data="<name>"` onto the template's root element
    before it's registered;
  * HTML-tag collision check — reject struct idents whose kebab-case
    matches a known HTML5 element name;
  * CSS scoping pass on the stylesheet string (scoped by default when
    `style` is present; `:global(...)` opt-out).
* `#[handlers]` — **exists**, unchanged by this RFC.
* `#[store]` — **new, deferred**, adds a singleton-component path for
  the global state tier.

## 8. Implementation plan

Ship in this order:

1. **Runtime: templates + styles registry** (`register_template`,
   `inject_style`).
2. **Runtime: tag resolution in the walker** — look up the element's
   tag name in the component registry, instantiate if registered,
   clone template.
3. **Macro: auto-derive `name` / `template` / `style` defaults** with
   overrides. Emit `include_str!`.
4. **Macro: inject `pp-data` on template root**, so authored
   templates lose the attribute.
5. **Runtime + macro: attribute-prop application on mount** (static
   first, then `pp-bind:` via reactive binding effect).
6. **Runtime: default slot capture + clone.**
7. **CSS scoping pass** (`lightningcss` integration in
   `pocopine-macros`).
8. **`$store` magic + `#[store]` macro** (deferred to next milestone
   unless a user need surfaces sooner).
9. **`pp-for` + named slots** (blocked by array-reactivity RFC).

Each step is individually landable and shippable.

## 9. Alternatives considered

* **Bare `<div pp-data="...">` for composition** (current runtime).
  Rejected: composition then reads as configuration rather than HTML,
  and there's no natural place to declare props.
* **Mount directive `<div pp-mount="todo-item">`**. Rejected: adds a
  directive that does what a tag can do; parent templates become
  uglier; props need their own directive too (`pp-props="..."`).
* **Vue/Svelte SFC-style single files**. Rejected up-front — see
  `feedback_pcx_format`. Users hated the mixed-language experience and
  the tooling implications.
* **Callback props**. Rejected for v0. Event dispatch is a strict
  superset (a callback is an event handler the child doesn't know the
  name of); forcing the event pattern keeps communication explicit.

## 10. Unresolved questions

* **`$store` surface.** The access path — `$store.cart.items` —
  implies a single-level namespace. Nested stores? Deferred until we
  see real use.
* **Where does `pp-bind:` live in the implementation?** The existing
  `pp-bind:attr` sets HTML attributes; the prop binding sets a proxy
  field. Same directive, two branches (element type check), or a new
  directive (`pp-prop:`)? Leaning toward same directive with a branch
  — simpler mental model.
* **SSR template delivery.** When the server renders a component's
  template into a response, does it send the pre-scoping HTML or the
  compiler's emitted string (with `pp-data` and scoping attrs)?
  Probably the emitted string, but the server-layer RFC will settle
  it.
* **`:deep(...)` scoping opt-out for cross-component selectors.**
  Mentioned in `docs/pcx/03-scoped-styles.md` but semantics aren't
  locked. Punt to the scoped-styles implementation PR.

## 11. Migration / impact

This RFC formalizes existing direction; no user-facing code written
against pocopine yet, so there's nothing to migrate. The counter
example under `examples/counter/` still uses the literal
`pp-data="counter"` form; once steps 1–4 of the implementation plan
land it will be updated to the new form.
