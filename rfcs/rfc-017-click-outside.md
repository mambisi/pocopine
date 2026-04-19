# RFC 017 — `pp-on:click.outside`

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-013-pp-on-key-modifiers.md`](./rfc-013-pp-on-key-modifiers.md), [`rfc-015-pp-anchor.md`](./rfc-015-pp-anchor.md) |

## 1. Summary

Add an `outside` modifier to `pp-on:click` (and `pointerdown` /
`mousedown` / `touchstart`). Fires when the event target is **not
inside** the element the directive is bound to. Essential for every
dismissible overlay — menus, popovers, selects, dropdowns.

```html
<template pp-teleport="body" pp-if="open">
  <div class="menu"
       pp-anchor:bottom-start="trigger"
       pp-on:click.outside="open = false">
    ...
  </div>
</template>
```

## 2. Non-goals

- **Ignoring clicks on the trigger** itself. Users coordinate via
  `@click.stop` on the trigger or via state flags. Framework
  doesn't know which element "is" the trigger.
- **Iframe-aware detection.** A click inside an iframe does not
  bubble through to `document` — authors needing this write their
  own cross-frame plumbing.
- **Auto-dismiss on scroll / focusout.** Scroll-to-dismiss is a
  Pine-level UX choice; the directive doesn't force it.

## 3. Surface

Just an additional modifier — composes with everything else on
`pp-on`:

```html
<div pp-on:click.outside="close"></div>
<div pp-on:click.outside.stop="close"></div>
<div pp-on:pointerdown.outside="close"></div>
<div pp-on:mousedown.outside.self="close"></div>
```

`self` with `.outside` is nonsense (no event ever fires); treated
as "ignore the modifier combo and never fire." Not an error.

## 4. Semantics

When `.outside` is present on a `pp-on` directive, the event
listener attaches to `document` with `capture: true` (instead of
the host element). The handler fires iff:

1. The event's `target` is **not** contained by the host element
   (`!el.contains(ev.target)`).
2. The host element is still connected to the DOM.
3. Every other modifier (`ctrl`, `shift`, key names, etc.) matches.

Capture is used so the listener runs before other click handlers
on descendants — which would otherwise have a chance to
`stopPropagation()` and suppress the outside detection.

### 4.1 Interaction with other modifiers

| modifier | with `.outside` | note |
|----------|----------------|------|
| `.prevent` | ignored | `ev.preventDefault()` on an outside click would break normal page behaviour (link navigation, form submit). |
| `.stop` | honoured | Rare but supported. |
| `.self` | never fires | Nonsensical combination. |
| `.once` | honoured | Listener is removed after first outside click. |
| `.window` / `.document` | redundant | `.outside` already installs on `document`. These modifiers are ignored. |
| `.debounce.<ms>` | honoured | Useful for noisy pointerdown sequences. |
| key modifiers | honoured | Only meaningful on `keydown`/`keyup` — `click.outside` with `.ctrl` fires only on Ctrl+click outside. |

## 5. Implementation

Small addition to `crates/pocopine-core/src/directives/on.rs`:

```rust
let outside = call.modifiers.iter().any(|m| m == "outside");

let target: EventTarget = if outside {
    document()
} else if on_window { ... }
else if on_document { ... }
else { el.into() };

// Inside the closure, *before* the existing filters:
if outside {
    if !el_still_connected(&el) { return; }
    if let Some(t) = ev.target() {
        if let Ok(node) = t.dyn_into::<Node>() {
            if el.contains(Some(&node)) { return; }
        }
    }
}
```

`AddEventListenerOptions::set_capture(true)` when `outside` is set.
`prevent` is ignored when `outside` — a one-line early-return in the
closure.

## 6. Edge cases

- **Click on a teleported popover child.** The popover lives outside
  the host's DOM subtree (it's in `<body>`). `el.contains(target)`
  returns false — the handler fires. Authors who want the popover
  to also count as "inside" must capture the click before it hits
  document (e.g. put the `.outside` directive on the popover
  itself, not the trigger).
- **Element not yet mounted.** Handler is a no-op
  (`!el.isConnected`). `release_subtree` detaches the listener via
  the existing `once` / removed-element teardown path (actually the
  standard `on.rs` listener cleanup still applies).
- **Nested dialogs.** Each one installs its own document listener
  scoped to its own container. Independent.
- **Handler calls `ev.stopPropagation()`** from within. Only
  affects document listeners registered *after* this one — the
  canonical "close outer when clicking inner" flow still works
  because we use capture (our listener fires first).

## 7. Example — Pine combobox

```rust
#[handlers]
impl PineCombobox {
    pub fn close(&mut self) { self.open = false; }
}
```

```html
<div class="combobox">
  <input pp-ref="input"
         pp-on:focus="open = true"
         pp-on:keydown.escape="close" />

  <template pp-teleport="body" pp-if="open">
    <div class="listbox"
         pp-anchor:bottom-start.offset.4="input"
         pp-on:click.outside="close">
      <!-- options -->
    </div>
  </template>
</div>
```
