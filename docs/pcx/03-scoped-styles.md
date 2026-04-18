# Scoped styles for `.pcx` components

Goal: CSS in a component's `.css` file applies only to that
component's template by default. Same idea as Vue SFC / Svelte, but
the file layout is cleaner — the CSS is already in its own file, so
the "scoping" is purely a compile-time transform of that file's
contents, not a section-extraction problem.

## Strategy

For a component `name = "counter"`:

1. Compute `H = hash("counter")` — first 8 hex chars of a
   deterministic digest (FNV-1a or blake3 truncated). Stable across
   builds and machines.
2. Attribute namespace: `data-pp-<H>`, e.g. `data-pp-a1b2c3d4`.
3. **Template pass**: walk the `.pcx` HTML, append `data-pp-<H>` to
   every element. Done at `#[component]` expansion time; the macro
   emits the rewritten HTML as the literal string it registers.
4. **CSS pass**: parse `Counter.css`, append `[data-pp-<H>]` to every
   selector's last compound. Done at the same expansion time.

Both passes happen inside the `pocopine-macros` (or a helper
`pocopine-pcx`) crate. The runtime wasm sees already-scoped strings.

## Scoping is the default

Because styles live in their own file, the simplest rule is: **if
`style = "..."` is set, scope the contents**. No `scoped` flag on the
attribute. Opt-out is per-rule inside the CSS.

```rust
#[component(name = "counter", style = "Counter.css")]  // scoped
```

To opt a single rule out of scoping, use `:global(selector)`:

```css
:global(body.dark) .wrapper { background: #000; }
```

Rewriter rule: selectors containing `:global(...)` have the
`:global()` wrapper stripped and the `[data-pp-H]` *not* appended.
Everything else in the file is scoped.

## Examples

**Input** (`Counter.css`):

```css
.wrapper          { display: flex; }
.wrapper .count   { font-size: 2rem; }
button            { padding: 0.5rem; }
:global(body.dark) .wrapper { background: #000; }
```

**Output** after scoping with `H = a1b2c3d4`:

```css
.wrapper[data-pp-a1b2c3d4]       { display: flex; }
.wrapper .count[data-pp-a1b2c3d4]{ font-size: 2rem; }
button[data-pp-a1b2c3d4]         { padding: 0.5rem; }
body.dark .wrapper               { background: #000; }  /* :global() unscoped */
```

**Input** (`Counter.pcx`):

```html
<div pp-data="counter" class="wrapper">
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

**Output** registered template:

```html
<div pp-data="counter" class="wrapper" data-pp-a1b2c3d4>
  <span class="count" pp-text="count" data-pp-a1b2c3d4></span>
  <button pp-on:click="increment" data-pp-a1b2c3d4>+</button>
</div>
```

## Edge cases

* **`:root`, `html`, `body`** — never scope. A small allowlist in the
  rewriter.
* **`::before` / `::after`** — append the attribute to the element
  part, before the pseudo-element: `button[data-pp-H]::before`.
* **`@keyframes`** — rename to `keyframes-<H>` (or the original name
  suffixed) so two components can both define `spin` without colliding.
  Substitute in `animation-name` references within the same file.
* **`@media` / `@supports`** — the at-rule itself is untouched;
  selectors inside are scoped normally.
* **Cross-component selectors**: `.parent .child` in `Parent.css`
  expecting to hit a `.child` inside an imported `Child.pcx` **breaks**
  under scoping. Document this; `:deep(.child)` opt-out rewrites to
  `.parent[data-pp-H] .child` (drops the trailing attribute).

## Implementation notes

* **CSS parser**: `lightningcss` (pure Rust, compiles to wasm, handles
  minify + autoprefix). Overkill for v0 but the right dep to commit to
  — regex-based selector munging breaks on attribute selectors with
  commas inside strings.
* **HTML parser**: hand-rolled single-pass tokenizer is fine for v0.
  The `.pcx` is our format, we can forbid edge cases. If we ever want
  full HTML5, reach for `html5ever`.
* **Hash function**: FNV-1a (no extra dep; runs in proc-macro context
  without blowing compile times). Switch to `blake3` only if we hit a
  collision in practice — 32 bits of hash over component names in one
  app is ample.

## Deferred

* CSS Modules (local class renaming). Scoped attributes cover the
  common case; modules are an alternative strategy for a different
  milestone.
* `@import` resolution inside the component's CSS. Same reason.
* Autoprefixing — let `lightningcss` do it when we flip the feature
  on, with a browserslist config at workspace root.
* Source maps — wait until a user asks.
