---
title: "Scoped styles for .poco components"
description: "How component stylesheets are registered and injected, and the planned compile-time scoping strategy that keeps component CSS from leaking into the rest of the page."
---

# Scoped styles for `.poco` components

A component's `.css` file is linked via `style = "..."` in `#[component]`. At registration time the macro emits a call to `inject_style`, which appends a `<style data-pp-component="<name>">` element to `<head>`. The injected CSS is raw — the same text your `.css` file contains.

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(name = "counter", template = "Counter.poco", style = "Counter.css")]
pub struct Counter {
    pub count: i32,
}
```

`template` and `style` paths are resolved relative to the `.rs` source file, exactly like `include_str!`. Adding the `style` argument creates a cargo rebuild dependency: editing `Counter.css` invalidates the build.

## How injection works

`inject_style` is idempotent per component name. The first call for a given name creates the `<style>` tag; any subsequent call (e.g. from a second `register()` invocation) is a no-op. The style element carries `data-pp-component="<name>"` so browser devtools and server-side rendering can attribute each block to its component.

```rust
// Generated register() body (abbreviated)
pub fn register() {
    if !mark_registered::<Counter>() { return; }
    // …template registration…
    ::pocopine::__private::inject_style(
        "counter",
        include_str!("Counter.css"),
    );
}
```

Because the CSS lands in a plain `<style>` block, it follows normal cascade rules. Nothing prevents one component's selectors from matching another component's DOM. The planned scoping transform described below is what will change that.

## Planned: compile-time CSS scoping

The design goal is that CSS in a component's `.css` file applies only to elements stamped by that component's template. This is the same idea as Vue SFCs and Svelte, but the file layout is already clean — the CSS is in its own file, so scoping is a pure compile-time rewrite of that file's selectors.

The planned mechanism, for a component `name = "counter"`:

1. Compute `H = hash("counter")` — the first 8 hex chars of a deterministic hash (FNV-1a; no extra dependency, stable across builds and machines).
2. Define an attribute namespace: `data-pp-<H>`, e.g. `data-pp-a1b2c3d4`.
3. **Template pass** — walk the `.poco` HTML and append `data-pp-<H>` to every element. Done inside the `#[component]` macro so the runtime WASM sees already-rewritten strings.
4. **CSS pass** — parse `Counter.css` and append `[data-pp-<H>]` to the last compound of every selector.

Both passes run inside `pocopine-macros`; the runtime never sees un-scoped selectors.

### Opt-out with `:global()`

When scoping is active, a single rule can escape it:

```css
:global(body.dark) .wrapper { background: #000; }
```

The rewriter strips the `:global()` wrapper and does not append the attribute. Everything else in the file is scoped.

### Example

**Input** (`Counter.css`):

```css
.wrapper          { display: flex; }
.wrapper .count   { font-size: 2rem; }
button            { padding: 0.5rem; }
:global(body.dark) .wrapper { background: #000; }
```

**Output** after the scoping pass with `H = a1b2c3d4`:

```css
.wrapper[data-pp-a1b2c3d4]        { display: flex; }
.wrapper .count[data-pp-a1b2c3d4] { font-size: 2rem; }
button[data-pp-a1b2c3d4]          { padding: 0.5rem; }
body.dark .wrapper                 { background: #000; }  /* :global() — unscoped */
```

**Input** (`Counter.poco`):

```poco
<div class="wrapper">
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

**Template after the scoping pass:**

```html
<div class="wrapper" data-pp-a1b2c3d4>
  <span class="count" pp-text="count" data-pp-a1b2c3d4></span>
  <button pp-on:click="increment" data-pp-a1b2c3d4>+</button>
</div>
```

### Edge cases covered by the design

* **`:root`, `html`, `body`** — never scoped. A small allowlist in the rewriter skips these.
* **`::before` / `::after`** — the attribute is appended to the element part, before the pseudo-element: `button[data-pp-H]::before`.
* **`@keyframes`** — renamed to `<original>-<H>` so two components can both define `spin` without colliding. All `animation-name` references in the same file are substituted accordingly.
* **`@media` / `@supports`** — the at-rule itself is untouched; selectors inside are scoped normally.
* **Cross-component selectors** — `.parent .child` in `Parent.css` targeting a `.child` inside a child component's template breaks under scoping, because the child's elements carry only the child's `data-pp-<H>`. Use `:deep(.child)` to opt a descendant selector out of the trailing-attribute rule: `.parent[data-pp-H] .child` (the child's class is not scoped by the parent).

## Working with unscoped styles today

Until the scoping pass lands, write component CSS defensively:

* **Namespace by component name** — prefix every class with the component name: `.counter-wrapper`, `.counter-count`.
* **Use the custom-element tag as a selector root** — custom elements don't collide with HTML element names, so `counter .wrapper { … }` is already isolated.
* **Keep utilities in Pine Stylekit** — utility classes in `.poco` templates are resolved by the Stylekit compiler and emitted once at the project level, not per-component. No scoping concern there.
