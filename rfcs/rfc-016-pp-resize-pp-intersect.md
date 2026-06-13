# RFC 016 — `pp-resize` and `pp-intersect`

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-014-focus-utilities.md`](./rfc-014-focus-utilities.md), [Alpine `x-resize`](https://alpinejs.dev/plugins/resize), [Alpine `x-intersect`](https://alpinejs.dev/plugins/intersect) |

## 1. Summary

Two thin DOM-observer directives that land the most common
"measure / react" primitives Pine components need (responsive
layout, sticky/observable elements, lazy loading, infinite scroll,
view logging):

```html
<!-- width + height of the element, live. -->
<div pp-resize="on_resize">
  <p pp-text="w"></p>
</div>

<!-- fire when element is at least 50% in view. -->
<img pp-intersect.threshold.50="load_image" />

<!-- pair of enter/leave handlers. -->
<section pp-intersect:enter="on_show"
         pp-intersect:leave="on_hide" />
```

Both directives are **handler-based**, not expression-sugar — they
take the name of a method on the component's state and pass typed
arguments. This matches every other `pp-*` directive that dispatches
to the scope and sidesteps needing assignment support in the
expression evaluator (RFC-012 is intentionally read-only).

## 2. Non-goals

- **Alpine-style inline assignment** (`pp-resize="w = $width"`).
  Blocked on adding assignment to the expr evaluator; out of scope
  here and not planned — the evaluator stays read-only.
- **`$width` / `$height` magics.** Handler args are clearer, typed
  (`f64`), and can't accidentally subscribe the wrong effect.
- **`rootMargin` / `root` configuration via JSON.** Modifier-syntax
  only; complex rootMargin shapes stay via the `.margin` modifier.
- **`scroll` directive.** Browsers will get `ScrollTimeline` /
  scroll-driven animations natively; meanwhile `pp-on:scroll.window`
  already works.
- **`MutationObserver` directive.** The framework already uses one
  internally; surfacing it is a separate decision.

## 3. `pp-resize`

### 3.1 Surface

```html
<div pp-resize="<handler>"> ... </div>
<div pp-resize.document="<handler>"> ... </div>
<div pp-resize.border-box="<handler>"> ... </div>
```

Attribute value is the name of a handler method on the nearest
component state. The method receives two `f64` args — content-box
width and height (in CSS pixels).

```rust
#[handlers]
impl Responsive {
    pub fn on_resize(&mut self, w: f64, h: f64) {
        self.width  = w;
        self.height = h;
    }
}
```

### 3.2 Modifiers

| modifier     | effect                                                           |
|--------------|------------------------------------------------------------------|
| `.document`  | observe `document.documentElement` instead of the host element   |
| `.border-box`| report border-box dimensions instead of content-box              |

### 3.3 Semantics

- The directive installs a **single** `ResizeObserver` per element
  via `new ResizeObserver(cb)` + `obs.observe(el)`. No second
  observer for modifier changes — one observer can serve the whole
  lifetime.
- Fires **on initial observe** (browsers' `ResizeObserver` always
  emits one synthetic entry at subscribe time) — handler runs once
  on first paint.
- `.document` swaps the observation target to
  `document.documentElement` but keeps the observer scoped to this
  directive instance; multiple `.document` observers on the page are
  independent.
- The observer is stored on the element as `__pp_resize_obs` and
  disconnected from `walker::release_subtree` via
  `directives::resize::release(&el)` — symmetric to
  `transition::release` / `teleport::release`.

## 4. `pp-intersect`

### 4.1 Surface

```html
<div pp-intersect="<handler>"> ... </div>
<div pp-intersect:enter="<handler>"> ... </div>
<div pp-intersect:leave="<handler>"> ... </div>
```

The `:enter` form is an alias for the bare form. Bare `pp-intersect`
and `pp-intersect:enter` both fire once **entering** the viewport;
`pp-intersect:leave` fires when the element stops intersecting.

Handler receives one `f64` arg — the `intersectionRatio`
(`0.0..=1.0`). Authors who don't care can write a zero-arg handler.

```rust
#[handlers]
impl LazyImage {
    pub fn load(&mut self) { self.visible = true; }
    // or, if ratio matters:
    pub fn fade(&mut self, ratio: f64) { self.opacity = ratio; }
}
```

### 4.2 Modifiers

| modifier        | effect                                                         |
|-----------------|----------------------------------------------------------------|
| `.once`         | disconnect after the first matching fire                       |
| `.half`         | shorthand for `threshold = 0.5`                                |
| `.full`         | shorthand for `threshold = 0.99`                               |
| `.threshold.N`  | numeric threshold. `N` is `0–100` (percentage). E.g. `.threshold.25` → 0.25 |
| `.margin.<v...>`| `rootMargin`. Values are CSS-margin-shaped (1, 2, or 4 values), each a bare number (px), `<n>px`, or `<n>%` |

Modifier order is irrelevant. Unknown modifiers are ignored.

### 4.3 `.margin` parsing

`.margin.<v1>` → `<v1>` (applied to all four sides).
`.margin.<v1>.<v2>` → `<v1> <v2>` (top/bottom, left/right).
`.margin.<v1>.<v2>.<v3>.<v4>` → `<v1> <v2> <v3> <v4>` (top, right,
bottom, left).

Each value:
- A bare number (`200`, `-100`): treated as `Npx`.
- `Npx` or `N%`: passed through as-is.
- Anything else: ignored, value falls back to `0px`.

Negative values shrink the viewport boundary; positive expand. Match
Alpine conventions:

```html
<div pp-intersect.margin.200px="...">            <!-- within 200px of viewport -->
<div pp-intersect.margin.10%.25px.25.25px="..."> <!-- mixed units -->
<div pp-intersect.margin.-100px="...">           <!-- 100px inside the viewport -->
```

### 4.4 Semantics

- Install `IntersectionObserver` with
  `{ threshold: <resolved>, rootMargin: <resolved> }`.
- Handler fires on entries where:
  - for `:enter` / bare: `entry.isIntersecting === true`
  - for `:leave`: `entry.isIntersecting === false` **and** the
    handler fired at least once for `:enter` (prevents firing on
    the initial off-screen synthetic callback).
- `.once` disconnects after the first handler fire, regardless of
  arg variant.
- Threshold resolution picks the most specific modifier. If both
  `.half` and `.threshold.25` are present, `.threshold.25` wins
  (explicit > shortcut). If no threshold modifier, default is `0`.
- Cleanup via `directives::intersect::release(&el)`.

## 5. Implementation

Two new directive modules under
`crates/pocopine-core/src/directives/`:

- `resize.rs` — ~100 lines. Stores `ResizeObserver` + `Closure` on
  the element under `__pp_resize_obs`.
- `intersect.rs` — ~180 lines. Parses modifiers, installs
  `IntersectionObserver`. Stores on element under
  `__pp_intersect_obs`.

Both register in `directives::registry()`. Both export
`pub fn release(el: &Element)` called from
`walker::release_subtree`.

Cargo features added to `web-sys`:

- `ResizeObserver`, `ResizeObserverEntry`, `ResizeObserverOptions`,
  `DomRectReadOnly`
- `IntersectionObserver`, `IntersectionObserverEntry`,
  `IntersectionObserverInit`

## 6. Edge cases

- **Handler misspelling.** `invoke_handler` already silently ignores
  unknown keys; same behavior here.
- **Handler that takes wrong arg types.** `FromHandlerArg`
  conversion returns `None` → the generated `#[handlers]` match arm
  skips the call. Equivalent to a misspelled key.
- **Element re-parented.** ResizeObserver / IntersectionObserver
  both track the element, not its parent. Re-parenting doesn't
  re-fire. If the element is physically *removed* from the DOM,
  `release_subtree` disconnects the observer.
- **`.document` inside a teleported subtree.** The observer targets
  `documentElement`, which is always the same. Safe.
- **Threshold `N > 100`.** Clamped to `1.0`.
- **Negative threshold (from parse fail).** Clamped to `0.0`.

## 7. Examples

### Responsive layout

```rust
#[handlers]
impl Columns {
    pub fn on_resize(&mut self, w: f64, _h: f64) {
        self.cols = if w < 640.0 { 1 }
                    else if w < 1024.0 { 2 }
                    else { 3 };
    }
}
```

```html
<div class="grid" pp-resize="on_resize"
     :style="`grid-template-columns: repeat(${cols}, 1fr)`">
  ...
</div>
```

### Lazy-load an image

```html
<img src="/placeholder.png"
     :src="src"
     pp-intersect.once="load" />
```

```rust
#[handlers]
impl LazyImage {
    pub fn load(&mut self) {
        self.src = self.full.clone();
    }
}
```

### Infinite scroll sentinel

```html
<ul>
  <li pp-for="item in items" pp-text="item.title"></li>
</ul>
<div class="sentinel" pp-intersect.margin.200px="load_more"></div>
```

```rust
#[handlers]
impl Feed {
    pub fn load_more(&mut self) {
        if self.loading { return; }
        self.loading = true;
        dispatch!(fetch_next(self.cursor).await, |s, page| {
            s.items.extend(page.items);
            s.cursor = page.cursor;
            s.loading = false;
        });
    }
}
```
