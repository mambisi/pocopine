# RFC 005 — `pp-transition` (enter / leave animations)

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [Alpine's `x-transition`](https://alpinejs.dev/directives/transition), [Headless UI's `<Transition>`](https://headlessui.com/react/transition), [`rfc-004-pp-for.md`](./rfc-004-pp-for.md) |

## 1. Summary

Add `pp-transition` — a directive that applies a **CSS-class sequence**
when an element mounts or unmounts. The class names are author-supplied
strings, so Tailwind utilities (`opacity-0`, `scale-95`,
`duration-300`, …) drop in unchanged. Hooks into `pp-show` and
`pp-if` for v0; list transitions are explicitly deferred.

```html
<div pp-show="open"
     pp-transition:enter="transition ease-out duration-300"
     pp-transition:enter-start="opacity-0 scale-95"
     pp-transition:enter-end="opacity-100 scale-100"
     pp-transition:leave="transition ease-in duration-200"
     pp-transition:leave-start="opacity-100 scale-100"
     pp-transition:leave-end="opacity-0 scale-95">
  …
</div>
```

Matches the Headless UI `<Transition>` surface 1:1 — the Tailwind
ecosystem's canonical pattern — so designers who already know it
don't re-learn anything.

## 2. Motivation

Three mounts/unmounts in pocopine today are "instant":

* `pp-show` flips between `display: none` and the default.
* `pp-if` clones-and-inserts or removes-the-clone.
* Router outlet swaps pages.

All three are visibly janky on any real UI. The standard fix in the
Alpine / Tailwind world is a declarative transition directive that
interpolates between two class sets. Authors want that here too, and
they want it to play nicely with Tailwind because that's what the
design system is usually written in.

## 3. Non-goals

Shipping these later (or never) keeps the surface tight:

* **List transitions** (per-item enter/leave inside `pp-for`) — separate
  RFC. Requires key-tracked diffing, which is itself deferred past v0.
* **Router / outlet transitions** — nice to have, but conceptually a
  different event (page swap, not a scope toggle). Can layer on later.
* **`.duration` / `.scale` / `.opacity` shorthand modifiers** — Alpine
  has these for convenience; we lean on Tailwind classes instead. A
  future RFC can add them if demand shows up.
* **JS-side animations** (Web Animations API, `element.animate`).
  Defeats the "designer can tune in CSS" goal.
* **`transitionstart` / group coordination / staggered children.**
* **`@starting-style` / View Transitions API** integration — wait for
  Safari parity.
* **Sanitization of class strings.** They're author-controlled
  template attributes, same trust model as the rest of pp-\*.

## 4. Surface

Six attribute slots. All are optional — missing ones skip that
sub-phase, so fade-in-only is `pp-transition:enter-start` +
`pp-transition:enter-end` with no leave attrs at all.

| Attribute | When applied | Typical content |
|---|---|---|
| `pp-transition:enter` | Held for the whole enter phase. | `transition ease-out duration-300` |
| `pp-transition:enter-start` | One frame at the start of enter. | `opacity-0 scale-95` |
| `pp-transition:enter-end` | Rest of enter phase (replaces `start`). | `opacity-100 scale-100` |
| `pp-transition:leave` | Held for the whole leave phase. | `transition ease-in duration-200` |
| `pp-transition:leave-start` | One frame at the start of leave. | `opacity-100 scale-100` |
| `pp-transition:leave-end` | Rest of leave phase (replaces `start`). | `opacity-0 scale-95` |

The value of each attribute is a plain class string (space-separated),
merged into the element's existing `class` attribute via
`classList.add` / `.remove`.

There is **no bare `pp-transition` without a sub-attribute** in v0 —
without classes, there's nothing to do.

## 5. State machine

### 5.1 Enter

Triggered when the element becomes visible (`pp-show` flipping true,
or `pp-if` inserting the clone):

1. Add `enter` + `enter-start` classes.
2. Force a style flush (`getComputedStyle`) so the browser commits the
   start state.
3. On the next animation frame, remove `enter-start` and add
   `enter-end`.
4. Wait for `transitionend` on the element. Fallback timer: computed
   `transition-duration + transition-delay`, plus a 50ms slop.
5. On completion (or cancel — see §5.3), remove `enter` + `enter-end`.

### 5.2 Leave

Triggered when the element would become hidden/unmounted. The
"hide / remove" action is **deferred** until after the animation.

1. Add `leave` + `leave-start`.
2. Style flush.
3. Next animation frame: remove `leave-start`, add `leave-end`.
4. Wait for `transitionend` (or fallback timer).
5. On completion, remove `leave` + `leave-end`, then perform the
   actual mutation:
   * `pp-show`: set `display: none`.
   * `pp-if`: `parent.remove_child(&clone)`.

### 5.3 Cancellation

A toggle can flip mid-transition (user hovers then un-hovers a
dropdown inside the 300ms). Cancelling means:

* Clear the fallback timer.
* Remove all six classes.
* Start the opposite phase from the current rendered state — which
  already has the "end" styles applied, so the reverse transition
  starts from that state naturally.

Per-element transition state lives in a struct stashed on the element
(Reflect-key private, like the existing scope plumbing) so show.rs
and if_.rs can read "is a transition pending?" without re-parsing
attributes.

## 6. Integration points

### 6.1 `pp-show`

Currently:

```rust
if truthy { style.remove_property("display"); }
else      { style.set_property("display", "none"); }
```

With transition present on the element, the effect dispatches through
the transition state machine instead:

```rust
if truthy {
    transition::enter(&el, || { style.remove_property("display"); });
} else {
    transition::leave(&el, || { style.set_property("display", "none"); });
}
```

The callback runs *before* enter (so the element is visible for the
animation) and *after* leave (so it stays visible through the fade-out).

### 6.2 `pp-if`

The mount path already walks a freshly-cloned element into the DOM.
After `walker::walk(&clone_root)`, if the clone has a transition,
kick the enter sequence.

The unmount path today is a direct `parent.remove_child(&clone)`.
With a transition, defer that removal through
`transition::leave(&clone, || parent.remove_child(&clone))`. A
re-flip to truthy during leave cancels removal and swaps into the
enter sequence, re-using the same clone (no re-walk).

`pp-if`'s own state cell (`Option<Element>`) still tracks the current
clone; the transition layer tracks phase-within-that-element.

## 7. Edge cases

* **Element has no `transition-*` CSS** — `transitionend` never
  fires, fallback timer resolves at `duration + delay + 50ms`. With
  an effective duration of 0, the fallback fires next tick. Works.
* **Element already transitioning another property** — we listen for
  `transitionend` without filtering `propertyName`. First event wins.
  For Tailwind's `transition` (all properties) this is fine; for
  surgical `transition-property: opacity` setups, still fine.
* **Element with `display: none` from stylesheet (not `pp-show`)** —
  the enter classes are added, but nothing is visible. No way around
  this without mutating the cascade, which we won't. Document it:
  transitions require the element to be in the flow.
* **`pp-show` + `pp-if` on the same element** — unsupported (no
  obvious semantic). The walker can warn; the directive order is
  source-attribute order.
* **Nested transitions** — e.g. outer `pp-if` with inner `pp-show`.
  Each element is independent; the inner transition just sees its
  own mount event when the outer's enter completes.

## 8. Implementation sketch

Single new module `crates/pocopine-core/src/directives/transition.rs`
with two public entry points:

```rust
/// Parse the six attributes once and stash a Transition struct on the
/// element. Called from the walker for any element with any
/// `pp-transition:*` attribute present.
pub fn setup(call: &DirectiveCall);

/// Run the enter sequence, then fire `on_done`. Cancels any in-flight
/// leave. If no transition is set up on `el`, invokes `on_done`
/// synchronously.
pub fn enter(el: &Element, on_done: impl FnOnce() + 'static);

/// Run the leave sequence, then fire `on_done`. Cancels any in-flight
/// enter. If no transition is set up on `el`, invokes `on_done`
/// synchronously.
pub fn leave(el: &Element, on_done: impl FnOnce() + 'static);
```

Internals:

* `Transition { enter, enter_start, enter_end, leave, leave_start, leave_end: Vec<String>, phase: Cell<Phase> }`
* Stored behind an `Rc<RefCell<Transition>>` in an `Element` private
  key.
* `raf(callback)` helper wraps `requestAnimationFrame` via
  `web_sys::Window::request_animation_frame`.
* `schedule_end(el, duration_ms, callback)` sets up the
  `transitionend` listener + fallback timer; whichever fires first
  cleans up both.

`DirectiveCall`'s `arg` field already carries the sub-attribute
(`"enter"`, `"enter-start"`, …) so the directive registry entry is
just `"transition"` — one handler, six possible args. The handler
upgrades the element to have a `Transition` if it doesn't yet, then
writes the relevant class list.

No new web-sys features — `setTimeout`/`clearTimeout` are already in
via `wasm-bindgen-futures`; `requestAnimationFrame` needs adding to
the `Window` feature set if not present.

## 9. Alternatives considered

* **WAAPI (`Element.animate`)** — cleaner imperative API, but hides
  timing inside JS where a designer can't tune it from CSS. Rejected
  — the Tailwind-friendliness goal is dispositive.
* **Bare `pp-transition` with default fade** — nice ergonomics, but
  introduces a hidden default stylesheet the author has to know about
  to override. Can be added later as pure sugar.
* **Per-directive transition (`pp-show.transition`)** — couples the
  transition to a single consumer. Separate directive is more flexible
  (e.g. can attach to plain components in v1).
* **Register `pp-transition` as a setup attribute, not a runtime
  directive** — the walker could detect the prefix and call `setup`
  unconditionally. Rejected for now to keep the directive registry
  as the single entry point; one extra call per element is cheap.

## 10. Out of scope (future work)

* List transitions (`pp-for` item enter/leave) — separate RFC once the
  keyed-diff work lands (currently deferred from RFC-004 §11).
* Route transitions inside `<pp-outlet>`.
* `pp-transition` on plain pocopine components (currently we only
  transition DOM elements, not component roots, because component
  mount/unmount doesn't flow through show/if today).
* Declarative group coordination (`pp-transition-group`).
