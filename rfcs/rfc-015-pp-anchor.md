# RFC 015 — `pp-anchor` (popover / floating element positioning)

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-006-pp-teleport.md`](./rfc-006-pp-teleport.md), [`rfc-014-focus-utilities.md`](./rfc-014-focus-utilities.md), [`rfc-016-pp-resize-pp-intersect.md`](./rfc-016-pp-resize-pp-intersect.md) |

## 1. Summary

Position a floating element (dropdown, tooltip, menu, combobox
listbox) relative to an anchor element. Works in every major browser
today — the directive uses JS-computed `position: fixed` layout, not
CSS Anchor Positioning, because Firefox support is still gated
behind a preference.

```html
<button pp-ref="trigger" pp-on:click="open = true">Open menu</button>

<template pp-teleport="body" pp-if="open">
  <div pp-anchor:bottom-start.offset.8.flip="trigger"
       class="menu">
    ...
  </div>
</template>
```

The directive value is the anchor — either a `pp-ref` name on the
current scope or a CSS selector. The arg is the placement. Modifiers
configure offset and auto-flipping.

## 2. Non-goals

- **Native CSS anchor positioning path.** Could be added as an
  optional fast path once Firefox ships (currently behind
  `layout.css.anchor-positioning.enabled`). For now, **one** code
  path — JS — so behaviour matches across browsers.
- **Shift / slide / arrow positioning.** Floating UI exposes these
  as separate middlewares; for v0 we only ship placement + flip +
  offset. Arrow positioning is straightforward to add in a later
  RFC once Pine has a real `PineTooltip` / `PineMenu`.
- **Virtual anchors** (`{ getBoundingClientRect() }`-only
  references). Out of scope; every anchor is a real DOM element.
- **`position: absolute` strategy.** Harder to get right — the
  anchor and the floater can sit under different containing
  blocks. `position: fixed` side-steps that entirely, at the cost
  of not scrolling with ancestors (which is what popovers want
  anyway).
- **Collision detection against arbitrary scroll containers.**
  Viewport only. `fixed` + teleport covers the common case.
- **FLIP / transition integration.** `pp-transition` already owns
  that surface; `pp-anchor` commits its geometry before the
  transition frame.

## 3. Surface

```html
<div pp-anchor[:<placement>][.<modifier>...]="<anchor>">
```

### 3.1 `<anchor>` resolution

The attribute value is resolved in this order:

1. If it's a non-empty identifier (matches `^[A-Za-z_][\w-]*$`) and
   a `pp-ref` with that name exists on the current scope, use that
   element.
2. Otherwise, try `document.querySelector(<value>)`.

Fails silently (directive is a no-op) when neither resolves. No
throw, no console warn — mirrors `pp-model`'s no-op behaviour when
its ref can't be found.

### 3.2 `<placement>`

Four sides × three alignments = twelve placements:

```
top         top-start         top-end
bottom      bottom-start      bottom-end
left        left-start        left-end
right       right-start       right-end
```

No arg defaults to `bottom`. Plain `top`/`bottom`/`left`/`right` is
shorthand for the `-center` variant (center along the cross axis).

### 3.3 Modifiers

| modifier         | effect                                                           |
|------------------|------------------------------------------------------------------|
| `.offset.<N>`    | Gap between anchor and floater in px. Default `0`. Accepts negative integers too (`-4`). |
| `.flip`          | If the floater would overflow the viewport on the main axis, swap to the opposite side when that side has more room. |

### 3.4 Handler side (optional)

None. The directive is pure DOM work; no callbacks into the
component. If authors need to react to position changes (e.g. to
set an arrow offset in a future RFC), that surface will come in
under a separate name.

## 4. Semantics

### 4.1 Position strategy

`pp-anchor` sets **`position: fixed`** on the floater
unconditionally, plus `top` / `left`, and clears `right` / `bottom`
to `auto` to prevent two-anchor layout bugs.

This means:

- The floater does NOT scroll with an ancestor. (Desired for
  popovers that escape `overflow: hidden` via `pp-teleport`.)
- Ancestor `transform` doesn't create a containing block for
  `position: fixed` on Firefox/Safari (it does on Chrome; we
  **side-step** the inconsistency by recommending teleport-to-body
  in Pine components, the same as Headless UI / Radix do).

### 4.2 Placement math

Let `a = anchor.getBoundingClientRect()`, `f =
floater.getBoundingClientRect()`, `vw/vh = window.inner{Width,Height}`,
`o = offset` (px).

For main = vertical (`top` / `bottom`):
```
top    y = a.top    - f.height - o
bottom y = a.bottom + o
start  x = a.left
center x = a.left + (a.width - f.width) / 2
end    x = a.right - f.width
```

For main = horizontal (`left` / `right`):
```
left   x = a.left   - f.width  - o
right  x = a.right            + o
start  y = a.top
center y = a.top + (a.height - f.height) / 2
end    y = a.bottom - f.height
```

No clamping at this stage — overflow is handled by `.flip` (below);
users who want hard clamping will get a dedicated `.shift` modifier
in a later RFC.

### 4.3 `.flip`

Only flips on the **main axis** (the side axis). Algorithm:

```
preferred  = requested side
opposite   = opposite of requested side
needed     = floater.extent_on_main + offset
room[side] = distance from anchor's edge on that side to viewport edge

if room[preferred] < needed && room[opposite] > room[preferred]:
    side := opposite
```

Cross-axis alignment is unchanged — menus that hug the right edge
keep hugging it even after flipping from `bottom-end` to `top-end`.

### 4.4 Reposition triggers

The directive computes on:

1. **Install** — first bind (after the floater is in the DOM).
2. **Window scroll** — passive listener on `window` (capture: true
   so scroll events from nested scrollers also fire).
3. **Window resize** — plain listener on `window`.
4. **Floater resize** — `ResizeObserver` on the floater itself
   (catches content changes: "menu grew a row", "tooltip text
   wrapped").
5. **Anchor resize** — `ResizeObserver` on the anchor (catches
   layout shifts that move the anchor without scrolling the page).

Initial compute is scheduled via `tick::next` (RFC-014) — one
microtask after the directive runs — so the floater's measured
dimensions reflect any reactive content that committed in the same
frame.

### 4.5 Teardown

`walker::release_subtree` calls `directives::anchor::release(&el)`
which:
- Disconnects both `ResizeObserver`s.
- Removes the `scroll` and `resize` listeners from `window`.

Same pattern as `resize.rs` / `intersect.rs` — observer + closures
stashed under a private key (`__pp_anchor_state`) on the floater.

## 5. Implementation

New module `crates/pocopine-core/src/directives/anchor.rs` — ~250
lines. Stored state (per floater):

```rust
struct AnchorState {
    anchor: Element,
    placement: Placement,
    offset: f64,
    flip: bool,
    reposition: Function,      // shared JS closure for all triggers
    anchor_observer: ResizeObserver,
    floater_observer: ResizeObserver,
    scroll_closure: Closure<dyn FnMut(Event)>,
    resize_closure: Closure<dyn FnMut(Event)>,
}
```

Registered in `directives::registry()`; `release()` wired from
`walker::release_subtree`.

No new web-sys features needed — everything used
(`ResizeObserver`, `DomRect`, `Window`, `EventTarget`) is already
enabled by RFC-016.

## 6. Edge cases

- **Anchor not yet laid out.** `getBoundingClientRect()` returns a
  zero-rect; the floater ends up at `(0, 0)`. First paint isn't
  ideal, but the `ResizeObserver` on the anchor catches the first
  real layout and fires a reposition. To avoid a flash, Pine
  components should pair `pp-anchor` with `pp-transition`'s
  enter class and start the floater invisible until the first
  reposition commits.
- **Floater larger than viewport.** No clamping; users can set
  `max-height: 100vh; overflow: auto` on the floater root.
- **Anchor inside an iframe.** Out of scope — the same-frame
  assumption is baked in.
- **Anchor removed from the DOM.** `getBoundingClientRect()`
  returns zeros. `.flip` still activates based on the zero-rect,
  which means the floater snaps to `(0,0)` or `(−w,−h)`. Authors
  should use `pp-if="open && anchor_present"` to keep anchor +
  floater in sync; we do not force-hide the floater.
- **Two floaters anchored to the same trigger.** Independent
  observers and listeners; no aggregation. Works as expected.
- **`pp-anchor` without a floater that's actually floating.** It
  still sets `position: fixed`. That's the contract — the
  directive's *job* is to make the element float.

## 7. Example: Pine-style dropdown

```html
<!-- AppMenu.poco -->
<div class="app-menu">
  <button pp-ref="trigger"
          pp-on:click="open = !open">
    Account ▾
  </button>

  <template pp-teleport="body" pp-if="open">
    <div pp-anchor:bottom-end.offset.6.flip="trigger"
         class="menu-panel"
         pp-on:keydown.escape="open = false">
      <a pp-route href="/profile">Profile</a>
      <a pp-route href="/settings">Settings</a>
      <hr />
      <button pp-on:click="logout">Sign out</button>
    </div>
  </template>
</div>
```

Anchor resolution: the `trigger` ref exists on the component's
scope, so `pp-anchor="trigger"` picks it up directly. No CSS
selector needed. The teleport escapes any ancestor `overflow:
hidden`, and `position: fixed` (applied by `pp-anchor`) keeps the
menu pinned to the viewport even while the page scrolls under it.
