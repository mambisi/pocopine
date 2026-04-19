# RFC 014 — Focus & timing utilities

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-006-pp-teleport.md`](./rfc-006-pp-teleport.md) (dialogs), [`rfc-013-pp-on-key-modifiers.md`](./rfc-013-pp-on-key-modifiers.md) (keyboard coverage) |

## 1. Summary

Ship two small sibling modules — `pocopine::focus` and
`pocopine::tick` — that together cover the imperative primitives every
overlay / transition / input-dismissing component needs:

```rust
use pocopine::{focus, tick};

// Remember the element that had focus before a dialog opens.
let saved = focus::save();

// Inside the dialog — keep focus inside this container while it's open.
let trap = focus::trap(&dialog_root);

// Explicitly blur the current element (dismiss mobile keyboard, etc).
focus::blur();

// On close — restore focus to whatever had it before.
trap.release();
focus::restore(saved);

// Auto-focus the first focusable — but wait a tick so enter-animation
// / layout / teleport insertion settle before the browser scrolls
// the focused element into view.
tick::next(|| { focus::auto_focus_first(&dialog_root); });
```

Every primitive is browser-supported today (no experimental API) — they
compose `Element.focus()` / `HTMLElement.blur()` + tabindex traversal +
`document.activeElement` for the focus side, and
`queueMicrotask` / `requestAnimationFrame` for the tick side.

## 2. Non-goals

- **Scroll-lock** on body while an overlay is open. Style concern;
  `overflow: hidden` on the user's app root is the canonical answer.
- **ARIA wiring** (`aria-modal`, `aria-labelledby` plumbing). Pine
  components set these on their own templates; we don't inject
  attributes here.
- **Focus ring styling.** CSS concern.
- **Visible-focus-only detection** (`:focus-visible` semantics at
  JS level). Browsers own this.
- **Inert attribute management.** `<dialog>` native element
  integration — separate RFC.

## 3. Surface

### 3.1 `pocopine::focus`

```rust
pub mod focus {
    /// Snapshot of the currently-focused element. Hold it across an
    /// overlay's open state; restore it when closing.
    pub struct Saved(/* Option<Element> */);

    /// Capture `document.activeElement` right now.
    pub fn save() -> Saved;

    /// Refocus the element captured by `save`. No-op if the saved
    /// element is detached or `None`.
    pub fn restore(saved: Saved);

    /// Remove focus from whatever element currently has it by
    /// calling `.blur()` on `document.activeElement`. Useful for
    /// dismissing a soft keyboard on mobile or clearing focus when
    /// closing an overlay without a meaningful restore target.
    /// No-op if nothing is focused or `activeElement` is `body`.
    pub fn blur();

    /// Install a focus trap inside `container`. Tab cycles within
    /// the container's focusable descendants; Shift+Tab cycles
    /// backward; Tab/Shift+Tab on the edge wraps. The trap also
    /// redirects focus back in if it escapes via mouse / JS.
    pub fn trap(container: &Element) -> TrapHandle;

    /// Handle returned by [`trap`]. Call `release()` or drop it to
    /// remove the listeners.
    pub struct TrapHandle(/* ... */);
    impl TrapHandle {
        pub fn release(self);
    }

    /// Find the first focusable element inside `container` (button,
    /// a[href], input, select, textarea, [tabindex not -1]) and call
    /// `.focus()` on it. Returns whether a focus target was found.
    pub fn auto_focus_first(container: &Element) -> bool;

    /// The "focusable" selector the module uses internally — exposed
    /// so Pine components can reuse it without duplicating the
    /// string.
    pub const FOCUSABLE_SELECTOR: &str = /* ... */;
}
```

### 3.2 `pocopine::tick`

```rust
pub mod tick {
    /// Schedule `f` on the next microtask. Fires *after* the current
    /// synchronous frame finishes (so reactive updates from
    /// `set_state` / signal writes have committed to the DOM), but
    /// *before* the browser paints. Equivalent to Vue's `nextTick`.
    ///
    /// Use for: "focus the input after the dialog mounts",
    /// "measure the element right after `pp-show` toggles it on",
    /// "scroll to a row after `pp-for` re-renders".
    pub fn next<F: FnOnce() + 'static>(f: F);

    /// Schedule `f` on the next animation frame (requestAnimationFrame).
    /// Fires right before the *next* paint — one frame later than
    /// [`next`]. Use when you need layout + style already resolved
    /// (e.g. read `getBoundingClientRect` on an element whose size
    /// depends on freshly applied classes).
    pub fn next_frame<F: FnOnce() + 'static>(f: F);
}
```

Both helpers own the `Closure` they allocate; it's dropped automatically
after the callback fires.

## 4. Semantics

### 4.1 `save` / `restore`

`save()` reads `document.activeElement` once. The resulting `Saved`
holds an `Option<Element>`; `None` when nothing was focused (e.g.
document body without `tabindex`).

`restore()` calls `.focus()` on the saved element if it's still
connected (`Node.isConnected === true`). Detached elements are
dropped silently.

### 4.2 `trap`

Install keydown and focusin listeners on `document` (not the
container — a trap should catch attempts to escape via JS / mouse
clicks outside). The trap:

* On `keydown: Tab` / `Shift+Tab`: compute the focusable list inside
  the container, find the current index, cycle.
* On `focusin`: if `document.activeElement` isn't inside the
  container (and the container is still connected), pull focus
  back to the first focusable inside.

`TrapHandle::release()` removes both listeners. Drop-implementing it
also releases, so `let _trap = focus::trap(root);` tied to a
`Scope` / `on_unmount` lifetime works naturally.

Stacking traps: the innermost (most recently installed) trap wins
— it re-anchors focus on `focusin` events that escape its
container. When released, the prior trap resumes its role (its
listeners were never removed). No explicit stack data structure;
the DOM-native event ordering handles it.

### 4.3 `blur`

Reads `document.activeElement`; if it's an `HTMLElement` and not the
document body, calls `.blur()` on it. `body` is treated as "nothing
focused" so stray calls on page-level handlers don't accidentally
blur the whole page scroll container on Safari.

### 4.4 `auto_focus_first`

Query-selects inside `container` for:

```
a[href], area[href], button:not([disabled]),
input:not([disabled]):not([type=hidden]),
select:not([disabled]), textarea:not([disabled]),
[tabindex]:not([tabindex="-1"])
```

Returns the first hit, calls `.focus()`, returns `true`. Returns
`false` when nothing matches (so callers can decide to focus the
container itself instead).

### 4.5 `tick::next` / `tick::next_frame`

`next(f)` wraps `f` in a `Closure::once` and hands it to
`queueMicrotask` — fires before paint, after the current task. The
closure is `FnOnce`, so the `Closure` instance is consumed on invoke;
no `forget()` leak.

`next_frame(f)` uses `window.request_animation_frame`. Fires on the
next paint tick, strictly after `next`. Same one-shot closure
lifetime.

Neither helper returns a handle; a fired closure is dropped
automatically. If cancellation matters (rare), authors can guard
inside the closure with a `Cell<bool>` captured from the call site.

### 4.6 Interaction with `pp-ref`

Pine components combine the two:

```rust
#[handlers]
impl PineDialog {
    pub fn on_mount(&mut self) {
        self.focus_saved = Some(focus::save());
        if let Some(root) = refs::get("root") {
            self.focus_trap = Some(focus::trap(&root));
            // Wait a microtask so the enter-transition has a chance
            // to apply its initial class — otherwise the browser
            // scrolls to the focus target before the dialog is
            // visible, producing a jump.
            tick::next(move || {
                focus::auto_focus_first(&root);
            });
        }
    }
    pub fn on_unmount(&mut self) {
        if let Some(trap) = self.focus_trap.take() {
            trap.release();
        }
        // Blur first so the soft keyboard dismisses on mobile even
        // if the saved element is gone (e.g. route change).
        focus::blur();
        if let Some(saved) = self.focus_saved.take() {
            focus::restore(saved);
        }
    }
}
```

## 5. Implementation

Two new files:

- `crates/pocopine-core/src/focus.rs` — `save`, `restore`, `blur`,
  `trap`, `auto_focus_first`. ~170 lines.
- `crates/pocopine-core/src/tick.rs` — `next`, `next_frame`. ~30
  lines. Uses `Closure::once` so the closure auto-drops after fire.

Notes:

- Thread-local effects / closures not needed — the utilities are
  imperative; authors call them from handlers.
- Focus-trap listeners installed on `document` via
  `add_event_listener_with_callback`. Each `TrapHandle` owns the
  boxed `Closure` + a tiny `release` method that calls
  `remove_event_listener_with_callback` and drops the closure.
- `tick::next` prefers `queueMicrotask` when available; falls back
  to `Promise.resolve().then(f)` on older engines (no-op gap in
  practice — all modern browsers expose `queueMicrotask`).

Both modules are re-exported from `pocopine::{focus, tick}`.

## 6. Edge cases

- **Container removed while trap is active.** The `focusin` listener
  sees `!container.isConnected`; noops. Authors should still
  `release()` to clean up listeners — drop does this automatically.
- **No focusable children.** Trap doesn't crash; Tab does nothing.
  `auto_focus_first` returns `false`.
- **Container loses connection before restore.** `restore()` skips
  silently — caller doesn't have to unwrap.
- **Nested traps (dialog inside dialog).** Inner trap's `focusin`
  handler fires first (document-level listeners run in the order
  they were registered; the newest is last to register, but JS
  `document.addEventListener` doesn't guarantee ordering across
  capture boundaries). v0: document both traps install on `document`
  with bubbling; innermost wins by checking against its own
  container first.

## 7. Alternatives considered

- **DOM-level `inert` attribute.** Cleaner but less flexible — we
  want the trap to survive parent re-renders, which `inert`
  scoping doesn't handle gracefully across `pp-if` boundaries.
- **A single macro that wires everything** (`#[dialog]` attribute).
  Hides too much; Pine dialogs want explicit control (e.g.
  delayed focus during enter animations).
- **Integrate with `<dialog>` element.** Native `<dialog>` has its
  own focus management, but doesn't compose with teleport-based
  custom overlays. Future: a `pp-dialog` directive that uses
  `showModal()` when the host is a `<dialog>`.
