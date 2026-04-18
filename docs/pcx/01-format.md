# `.pcx` file format

A `.pcx` file is **only** HTML with `pp-*` directives. Nothing else.
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

**`Counter.pcx`**

```html
<div pp-data="counter" pp-init="init" class="wrapper">
  <button pp-on:click="decrement">-</button>
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

**`Counter.rs`**

```rust
use pocopine::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "counter", template = "Counter.pcx", style = "Counter.css")]
pub struct Counter {
    pub count: i32,
}

#[handlers]
impl Counter {
    pub fn init(&mut self)      { self.count = 0; }
    pub fn increment(&mut self) { self.count += 1; }
    pub fn decrement(&mut self) { self.count -= 1; }
}
```

**`Counter.css`**

```css
.wrapper { display: flex; gap: 0.5rem; align-items: center; }
.count   { font-size: 2rem; font-weight: 600; min-width: 3ch; text-align: center; }
button   { padding: 0.5rem 1rem; }
```

## Rules for the `.pcx` body

* **Single root element.** It must carry `pp-data="name"` matching the
  `#[component(name = "...")]` in the paired `.rs`. Fragments aren't
  supported in the first milestone; wrap in a `<div>`.
* **Directive attributes** follow the runtime's `pp-*` naming rules
  (see `crates/pocopine-core/src/directives/mod.rs::parse_attr`):
  `pp-on:event.modifier`, `pp-bind:attr`, etc.
* **No Rust inside attribute values yet.** The current runtime treats
  the value as a bare identifier (field name or handler name).
  Full Rust expressions in attribute values are a future milestone
  (`pp-on:click="self.count += 1"`); until then, the field/handler
  identifier model stays.
* **Plain HTML comments are fine.** `<!-- ... -->` works. The compiler
  strips them.

## Rules for the `.rs` file

* Exactly one `#[component(name = "...", template = "...", style = "...")]`
  per file is the convention (more is allowed but unusual).
* `template` and `style` paths are **relative to the `.rs` file**, same
  as `include_str!`. Both accept missing files (warn, don't error) so a
  component can be authored without styles.
* The paired `.pcx` must exist at the `template` path; the macro reads
  and validates it at compile time (see `02-compilation.md`).

## Rules for the `.css` file

* Plain CSS. `style = "..."` in `#[component(...)]` opts the file into
  compile-time processing.
* `scoped` behavior is on by default — styles only apply to the
  component's template. Opt out per rule with `:global(...)`
  (implementation detail in `03-scoped-styles.md`).

## File naming / layout

Convention, not a rule:

```
components/
  Counter.pcx
  Counter.rs
  Counter.css
  TodoList.pcx
  TodoList.rs
  TodoList.css
```

The compiler does not require matching stems — the `template = "..."`
path is authoritative. Matching stems are convention and will probably
become a clippy-style lint warning later.

## Future: syntax highlighting

The whole point of keeping `.pcx` as "HTML with `pp-*` attributes" is
that an editor can:

1. Treat the file as HTML by default → every editor gets free HTML
   highlighting.
2. Recognize `pp-*="..."` attribute values and switch to a Rust-like
   grammar for the value. Today it's just an identifier; later it's
   an expression. Either way the inner grammar is small enough that a
   Tree-sitter injection or TextMate child grammar is maybe 100 lines
   per editor.

Nothing in the format blocks that; the format *enables* it by not
mixing languages at file scope.
