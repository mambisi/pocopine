# RFC 021 — `pocopine::scroll_lock`

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-006-pp-teleport.md`](./rfc-006-pp-teleport.md), [`rfc-014-focus-utilities.md`](./rfc-014-focus-utilities.md) |

## 1. Summary

Lock the page's scroll while a Pine Dialog / Sheet / Drawer is
open, and restore it on close. Ref-counted so nested overlays don't
fight each other.

```rust
use pocopine::scroll_lock;

#[handlers]
impl PineDialog {
    pub fn on_mount(&mut self) {
        scroll_lock::lock();
        // ... focus trap etc.
    }
    pub fn on_unmount(&mut self) {
        scroll_lock::unlock();
    }
}
```

Without it: iOS Safari happily rubber-bands the page *underneath*
the dialog while the user swipes inside it, and desktop Chrome
jumps visible content when the body's scrollbar disappears. Both
look bad; both are a one-liner away from fixed.

## 2. Non-goals

- **Locking a scoped container** (e.g. "lock this `<div>`'s
  scroll, not the whole page"). Rare in Pine; deferred.
- **Preserving scroll position across lock-unlock-lock cycles**
  when the user scrolls inside the locked region. Browsers
  handle `overflow: hidden`'s implicit scroll-top pin correctly
  for the body; nothing to do.
- **Touch-action fiddling** (`touch-action: none`). `overflow:
  hidden` on `<body>` handles iOS rubber-banding for the cases
  Pine cares about. Dialogs with their own scrollable content
  keep working — their content is a descendant, not a sibling of
  `<body>`.
- **React-style `useLockBodyScroll` shape.** Imperative `lock()`
  / `unlock()` maps cleanly onto on_mount / on_unmount, which
  is how Pine handlers already do it.

## 3. Surface

```rust
pub mod scroll_lock {
    /// Lock page scroll. First call pins the scroll position,
    /// applies `overflow: hidden` to `<body>`, and compensates
    /// for the now-absent scrollbar so visible content doesn't
    /// jump. Subsequent calls just bump the internal counter.
    pub fn lock();

    /// Decrement the lock counter. When it hits zero, restore
    /// `<body>`'s prior styles. Call in on_unmount.
    pub fn unlock();

    /// How many active locks are held right now — useful for
    /// tests and debugging. Returns 0 when nothing is locked.
    pub fn depth() -> u32;
}
```

## 4. Semantics

### 4.1 Ref counting

Every `lock()` increments; every `unlock()` decrements (saturating
at 0 so a stray `unlock()` doesn't panic). Transition `0 → 1`
applies the lock side effects; transition `1 → 0` reverses them.
Any other transition is a no-op.

### 4.2 Side effects (on 0 → 1)

1. Record the document element's current `scrollbar-gutter`
   width by measuring `window.innerWidth -
   documentElement.clientWidth`. That's the scrollbar's real
   width including rounding.
2. Stash `<body>`'s existing inline `overflow` and `padding-right`
   into thread-local memory.
3. Set `body.style.overflow = "hidden"`.
4. Set `body.style.paddingRight = (prev_px + scrollbar_px) + "px"`
   so the page's right edge doesn't jump left when the scrollbar
   disappears.

### 4.3 Side effects (on 1 → 0)

1. Restore the stashed `overflow` (writing empty string if it
   was originally unset, which is the DOM's canonical "remove"
   for an inline style).
2. Restore the stashed `padding-right` the same way.

### 4.4 Idempotency / edge cases

- **No document / no body.** `lock()` silently no-ops — SSR
  contexts (not yet supported) or detached frames.
- **Multiple simultaneous calls to `lock()` without a matching
  `unlock()`.** Refcount persists; `<body>` stays locked. Pine
  components must call `unlock()` in on_unmount; we do not defend
  against author error beyond the saturating subtract.
- **Scrollbar already hidden** (body had `overflow: hidden`
  before). Measured gutter is 0; no padding compensation; still
  increments the counter so `unlock()` works as expected.
- **Page with a custom scrollbar library** (e.g. some Tailwind
  setups). We measure the live gutter; whatever width the
  scrollbar is, that's what we compensate for.

## 5. Implementation

New module `crates/pocopine-core/src/scroll_lock.rs` — ~70 lines.

```rust
use std::cell::{Cell, RefCell};

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static SAVED: RefCell<Option<Saved>> = const { RefCell::new(None) };
}

struct Saved { overflow: String, padding_right: String }

pub fn lock() { ... }
pub fn unlock() { ... }
pub fn depth() -> u32 { DEPTH.with(|d| d.get()) }
```

Re-export from `pocopine::scroll_lock`.

No new web-sys features — everything used (Document, HtmlElement,
Window, CssStyleDeclaration) is already enabled.

## 6. Example — Pine Drawer

```rust
#[component(template = "PineDrawer.poco")]
pub struct PineDrawer { open: bool }

#[handlers]
impl PineDrawer {
    pub fn on_mount(&mut self) {
        if self.open { scroll_lock::lock(); }
    }
    pub fn on_unmount(&mut self) {
        if self.open { scroll_lock::unlock(); }
    }
    // …and a watcher flips lock state as `open` toggles:
    pub fn watch_open(&mut self, was_open: bool) {
        match (was_open, self.open) {
            (false, true) => scroll_lock::lock(),
            (true, false) => scroll_lock::unlock(),
            _ => {}
        }
    }
}
```

## 7. Alternatives considered

- **`<body inert>`** — disables all interactive elements but
  doesn't stop scrolling. Wrong primitive.
- **`position: fixed` on `<body>`** — locks scroll but loses the
  scroll position on unlock unless we also save/restore it. More
  code, more edge cases, less widely tested. `overflow: hidden`
  is what Radix / Reach / Headless UI all settled on.
- **`preventDefault` on wheel / touchmove** — fragile; fights
  the browser's momentum scrolling; hurts accessibility.
