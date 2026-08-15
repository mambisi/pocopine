---
title: "Compiling .poco + .rs + .css → registered component"
description: "The user writes three files. The #[component] macro ties them together at compile time; no separate build step is needed."
---

# Compiling `.poco` + `.rs` + `.css` → registered component

A pocopine component is three files: a Rust struct, a `.poco` template, and an optional stylesheet. The `#[component]` macro links them at compile time — no separate build step, no codegen script.

## The macro surface

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "counter",       // optional; defaults to kebab-case of the struct ident
    template = "Counter.poco",  // optional; defaults to "<StructIdent>.poco"
    style = "Counter.css",  // optional
)]
pub struct Counter {
    pub count: i32,
}
```

`template` and `style` are **paths relative to the `.rs` source file**, matching `include_str!`'s own resolution rules. Both are optional. When `template` is omitted, the macro looks for `<StructIdent>.poco` alongside the `.rs` file.

A struct whose kebab-case name matches a real HTML element (e.g. `Button`, `Section`) is rejected at compile time to prevent tag collisions in parent templates.

### Additional arguments

Beyond the basics, `#[component]` accepts:

| Argument | What it does |
|---|---|
| `role = "..."` | Maps to a semantic root tag (`"interactive"` → `<button>`, `"visual"` → `<span>`, `"panel"` → `<div>`, …) and emits `data-pine-role` on the root. Templates using a role write `<root>` as the root placeholder; the macro rewrites it. |
| `display = "..."` | Injects `<custom-tag> { display: <value>; }` at registration, overriding the browser's default `display: inline` for custom elements. `role = "panel"` / `role = "scope"` default to `display: contents` when no explicit value is set. |
| `transition = "..."` | Default enter/leave animation preset. Overridable per-instance via the `transition` HTML attribute. |
| `transition_in` / `transition_out` | Asymmetric animation presets. |
| `animate = "flip"` | Enables FLIP layout animation on keyed `pp-for` rows. |
| `uses = [TypeA, TypeB]` | Registers child component types transitively from this component's `register()` and activates compile-time slot-contract checking. |
| `extends = [TypeA, TypeB]` | Bundle marker: this type re-exports the registration of every listed type. Mutually exclusive with `template`, `style`, `role`, `display`, and animation args. |
| `template = poco! { … }` | An inline template written as bare HTML instead of a file path. See below. |

## Inline templates: `poco!`

`template` takes one of two forms — a path to a `.poco` file, or an inline
`poco!` body:

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! {
    <div class="counter">
        <button pp-on:click="increment">+</button>
        <span pp-text="count"></span>
    </div>
})]
pub struct Counter {
    pub count: i32,
}
```

The body is ordinary HTML with the usual `pp-*` directives — not a DSL, and
not Rust mixed into markup. Both forms run the identical compile-time ladder
(parse, single-root, slot contracts, `pp-for` plans, template-path
validation), so an inline template is checked exactly as strictly as a file
one, with errors pointing at the offending line inside your `.rs`.

`poco!` also works on its own, returning a `PocoTemplate`:

```rust
const ROWS: PocoTemplate = poco! { <li>a</li> <li>b</li> };
```

Standalone templates may be fragments; the single-root rule applies only once
a template becomes a component's.

### Text that Rust's lexer rejects

The body is tokenized by rustc before pocopine ever sees it, and some ordinary
prose does not survive that step: apostrophes (`don't`), typographic symbols
(`— … © · ← ⌘`), emoji, and backslashes. Wrap such text in quotes — a string
literal is a single token, so its contents are never inspected:

```poco
<p>"Don't stop — © 2026 · ⌘K 🎉"</p>
```

Quoted runs land in the template as static text, HTML-escaped for you, so
`"5 < 10 & rising"` renders correctly with no entity juggling. Quoting is
per-run — `<p>Hello "don't" world</p>` mixes freely — and attribute values are
never affected.

Prose that looks like a Rust lifetime is fine unquoted (`'tis`, `the 'static
lifetime`), because those lex as real tokens. When something does need
quoting, `pocopine build`, `run` and `dev` tell you which character and where
before cargo runs, so you get a message naming the template rather than a bare
tokenizer error.

Reach for a `.poco` file when a template is large enough to deserve one. Both
forms are fully supported by Stylekit class extraction and `pocopine lsp`.

### `pocopine fmt` owns the boundary

Rather than leaving "how big is too big" to taste, a rule decides it:

```
pocopine fmt           # apply the rules
pocopine fmt --check   # report only, non-zero exit if anything would change
pocopine fmt --fix     # also apply rules configured as `warn`
```

By default a template under **150 lines** is pulled inline and its `.poco`
file removed, and an inline body at or over that is reported so you can
extract it with `--fix`. Both directions preserve indentation, so moving a
template back and forth returns the identical file.

Tune it in `Cargo.toml`:

```toml
[package.metadata.pocopine.fmt]
inline-threshold = 150
inline-small-templates = "fix"   # off | warn | fix
extract-large-inline = "warn"
format-markup = "fix"
print-width = 120
```

It also formats the markup itself — indentation and line wrapping — in `.poco`
files and `poco!` bodies alike. Attributes are never rewritten, so directives
survive verbatim, and whitespace-sensitive elements (`<pre>`, `<textarea>`)
are left exactly as written. Use a `markup-fmt-ignore` comment to opt a
subtree out:

```poco
<!-- markup-fmt-ignore -->
<div>   spacing kept   </div>
```

Two things `pocopine fmt` will not do. It does not rewrite template *text*: a
template holding characters the Rust lexer rejects is reported and left alone,
since escaping automatically would double-escape any HTML entities already
there. And it never touches markup it could not parse — that belongs to the
compiler's diagnostics, not the formatter's guesswork.

## What the macro emits

`#[component]` expands to three things:

1. **`impl ComponentState for Counter`** — proxy `get`/`set`/`keys`/`invoke` over public fields via `serde_wasm_bindgen`. Fields marked `#[prop]` are parent-writable; unmarked fields are component-internal state.
2. **`impl Counter { pub fn register() { … } }`** — registers the component, its compiled template, and optional stylesheet with the runtime.
3. **`impl Component for Counter`** — the `Component` trait impl used by `app!{}` and bundle markers.

The generated `register()` body looks like this:

```rust
pub fn register() {
    if !mark_registered::<Counter>() {
        return; // idempotent; safe to call multiple times
    }
    register_component_with_mount(
        "counter",
        concat!(module_path!(), "::", "Counter"),
        || Scope::new(Rc::new(RefCell::new(Counter::default()))),
        Some(Counter::mount_template),
    );
    // Dependency pin: cargo rebuilds when Counter.poco changes
    const _: &str = include_str!("Counter.poco");
    register_template(
        "counter",
        compile_template(include_str!("Counter.poco"), "counter", None),
    );
    // (style, display, and uses registrations follow if set)
}
```

Key points:

1. **`include_str!` pins cargo's rebuild graph.** Editing a `.poco` or `.css` file invalidates the build cache automatically. No `build.rs` or `rerun-if-changed` is needed.
2. **`compile_template` runs at registration time.** It injects a `data-pp-scope-id="counter"` attribute into the template's root element so the mount runtime can locate the component root without scanning the whole subtree. For components with a `role`, it also rewrites the `<root>` placeholder to the appropriate HTML tag.
3. **`inject_style` is idempotent.** The first call for a component name appends `<style data-pp-component="counter">` to `<head>`. Subsequent calls for the same name are no-ops, so calling `register()` multiple times is safe.
4. **`mark_registered` short-circuits transitive re-registration.** When component A declares `uses = [B, C]`, A's `register()` calls `B::register()` and `C::register()`. The guard prevents redundant work and breaks cycles.

## Compile-time template validation

Before any registration code runs, the macro reads the `.poco` off disk and validates it using `pocopine-template-parser`:

- The template is parsed through `parse_strict`, which uses `html5ever` under the hood.
- The macro enforces a **single-root rule**: exactly one element root is permitted. Comments, whitespace, and html5ever's auto-inserted synthetic nodes (e.g. `<tbody>`) are ignored — only authored element roots count.
- Errors are rendered as `annotate-snippets`-style blocks pointing at the offending line and column in the `.poco` file.

Template resolution uses a two-tier strategy:

1. **`Span::local_file()`** (nightly `proc_macro_span`) — resolves the `.poco` relative to the calling `.rs` file. This is the primary path for cargo builds and rust-analyzer file-backed evaluations.
2. **Manifest-dir filesystem walk** — falls back when the span is synthetic (rust-analyzer speculative expansion). Searches `CARGO_MANIFEST_DIR` for the template filename; refuses to resolve when zero or more than one match exists.

If neither tier finds the file, the macro skips validation silently and lets the cargo build catch the error with a concrete span.

Set `POCOPINE_TEMPLATES_LENIENT=1` to downgrade template errors from build-fatal to rustc warnings. Useful when migrating a large template base; not recommended for steady-state development.

## Runtime behaviour

The compiled template HTML is cached by `register_template`. On first mount, the runtime parses it once into a `<template>` element (browser HTML parser, fast path) and caches the `HTMLTemplateElement`. Every subsequent mount clones the `.content` `DocumentFragment` via `cloneNode(true)` instead of re-parsing the HTML string.

Directive binding for eligible directives (`pp-text`, `pp-show`, `pp-bind`/`:prop`, `pp-on`/`@event`, `pp-ref`) on native HTML elements is resolved at compile time by the `#[component]` macro (RFC-058). Component tags are compiled subtree boundaries, but their own parent-facing forms are planned explicitly: `pp-show`, reactive props, host listeners, component `pp-model`, and `pp-ref`. Structural `<template pp-if>` / `pp-for` / `pp-teleport` sites also compile into plans, and their single body root may be a component tag. The compiled template plan is emitted as Rust code alongside the component struct and executes when the mount entry point runs; there is no recursive runtime directive walk to recover an unplanned component-host attribute.

## What the macro does not do

- **It does not do deep semantic analysis of the template body.** Structural validation (single root, slot contracts via `uses = [...]`) runs at compile time; runtime directive semantics are validated when the generated plan code executes.
- **It does not bundle or minify.** CSS is injected as-is.
- **It does not manage your app's component registry.** You call `Counter::register()` (or `App::register::<Counter>()`) from your startup code, before `pocopine::run()`.
