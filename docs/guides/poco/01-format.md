---
title: ".poco file format"
description: "A .poco file is only HTML with pp- directives. Nothing else. No embedded Rust. No embedded CSS. No <script> or <style> blocks."
---

# `.poco` file format

A `.poco` file is **only** HTML with `pp-*` directives. Nothing else.
No embedded Rust. No embedded CSS. No `<script>` or `<style>` blocks.

Rust lives in its own `.rs` file; CSS lives in its own `.css` file.
Three plain files, one component.

## Why

Mixed-section SFCs (`.vue`, `.svelte`) put three languages in one file.
That creates real friction:

* Syntax highlighting needs a custom grammar, or piles of embedded
  language hacks, per editor.
* Tooling (rustfmt, clippy, stylelint, prettier) can't run against a
  subsection without special wrappers.
* Rust-analyzer can't see Rust inside a `<script>` block without a
  bespoke integration.

Keeping each concern in its native file type lets every tool work
out of the box. A future editor plugin only has to do one thing:
inside a `pp-*="..."` attribute value, switch to a Rust-expression
grammar. That's a small, well-scoped plugin — not a full SFC plugin.

## Full example

Three files in the same directory:

**`Counter.poco`**

```poco
<div class="wrapper">
  <button pp-on:click="decrement">-</button>
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

**`Counter.rs`**

```rust
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Counter.poco", style = "Counter.css")]
pub struct Counter {
    pub count: i32,
}

#[handlers]
impl Counter {
    pub fn on_setup(&mut self) { self.count = 0; }
    pub fn increment(&mut self)  { self.count += 1; }
    pub fn decrement(&mut self)  { self.count -= 1; }
}
```

**`Counter.css`**

```css
.wrapper { display: flex; gap: 0.5rem; align-items: center; }
.count   { font-size: 2rem; font-weight: 600; min-width: 3ch; text-align: center; }
button   { padding: 0.5rem 1rem; }
```

## Rules for the `.poco` body

* **Single root element.** Fragments aren't supported; wrap in a `<div>`
  or any other block element. The `#[component]` macro enforces this at
  compile time and emits a diagnostic pointing at the second root if the
  rule is violated.
* **No `pp-data` or `pp-init`.** These directives were removed. The
  macro auto-stamps the scope marker on the root element — you never
  write it yourself. Initialization logic goes in the `on_setup`
  lifecycle hook in the paired `.rs` (see below).
* **Directive attributes** use the `pp-*` prefix:
  `pp-on:event.modifier`, `pp-bind:attr` (shorthand `:`), `pp-text`,
  `pp-show`, `pp-if`, `pp-for`, `pp-model`, `pp-ref`, `pp-html`, etc.
  Event bindings also accept the `@event` shorthand.
* **Attribute values are expressions**, not bare identifiers. `pp-text="count"`,
  `pp-text="count * 2"`, and `pp-bind:title="open ? 'close' : 'open'"` are
  all valid.
* **Plain HTML comments are fine.** `<!-- ... -->` works. The compiler
  strips them.

## Rules for the `.rs` file

* Exactly one `#[component(...)]` per file is the convention (more is
  allowed but unusual).
* All arguments to `#[component]` are optional:
  * `name` defaults to the kebab-case of the struct identifier
    (`Counter` → `counter`).
  * `template` defaults to `<StructIdent>.poco` relative to the `.rs` file.
  * `style` is omitted unless explicit.
* `template` and `style` paths are **relative to the `.rs` file**, matching
  `include_str!` semantics. A missing `template` file is a **compile
  error** in the default strict mode. A missing `style` file is silently
  skipped — components without styles are valid.
* The macro parses and validates the `.poco` at compile time
  (see `02-compilation.md`). Template errors produce annotated diagnostics
  pointing at the offending line.
* Lifecycle hooks go in the `#[handlers]` impl:
  * `on_setup(&mut self)` — runs before first render; use it for field
    initialization.
  * `on_mount(&mut self)` — runs after the component is inserted into the
    DOM.
  * `on_ready(&self)` — runs after `on_mount` and all child
    `on_mount` hooks have fired. Takes an immutable receiver —
    mutation goes through `this::<Self>().update(...)` or a deferred
    `tick::next` call.
  * `on_unmount(&mut self)` — runs when the component is removed from
    the DOM.

## Rules for the `.css` file

* Plain CSS. `style = "..."` in `#[component(...)]` inlines the file
  via `include_str!` and injects it into `<head>` at runtime as a
  `<style data-pp-component="...">` element.
* CSS is currently **global** — selectors can match elements outside
  the component. Compile-time scoping is planned; see
  `03-scoped-styles.md` for the design and status.

## File naming / layout

Convention, not a rule:

```
components/
  Counter.poco
  Counter.rs
  Counter.css
  TodoList.poco
  TodoList.rs
  TodoList.css
```

The compiler does not require matching stems — the `template = "..."`
path is authoritative. Matching stems are convention.

## Syntax highlighting

`.poco` files are HTML with `pp-*` attributes, so every editor already
highlights them correctly without plugins. A future editor plugin only
needs to do one additional thing: inside a `pp-*="..."` attribute value,
switch to a Rust-expression grammar. That injection is small enough for
a Tree-sitter grammar or a TextMate child grammar — not a full SFC
plugin.

Nothing in the format blocks that; the format *enables* it by not
mixing languages at file scope.
