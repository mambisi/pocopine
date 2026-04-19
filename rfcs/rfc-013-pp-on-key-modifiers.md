# RFC 013 — Key modifiers on `pp-on`

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [Alpine key modifiers](https://alpinejs.dev/directives/on#keyboard-events) |

## 1. Summary

Extend `pp-on:keydown|keyup|keypress` with modifiers that filter the
handler by key + modifier-key state. Matching stops at the first
non-matching modifier — handler doesn't fire.

```html
<input pp-on:keydown.escape="close" />
<button pp-on:keydown.enter="submit">Save</button>
<menu pp-on:keydown.arrow-down="next"
      pp-on:keydown.arrow-up="prev"
      pp-on:keydown.home="first"
      pp-on:keydown.end="last" />
<div pp-on:keydown.ctrl.k="open_command_palette"></div>
```

## 2. Non-goals

- **Chord combos spanning multiple events** (`Ctrl+K then Ctrl+I`).
  Single-event matching only.
- **Numeric modifier for keys** (`.49` for `key==="1"`). Use `.one` /
  literal letter names.
- **`keyCode` fallback.** Deprecated browser API; we use
  `KeyboardEvent.key` only.

## 3. Surface

### 3.1 Named keys

| Modifier | Matches `KeyboardEvent.key` |
|---|---|
| `.escape` / `.esc` | `"Escape"` |
| `.enter` | `"Enter"` |
| `.tab` | `"Tab"` |
| `.space` | `" "` |
| `.backspace` | `"Backspace"` |
| `.delete` / `.del` | `"Delete"` |
| `.arrow-up` / `.up` | `"ArrowUp"` |
| `.arrow-down` / `.down` | `"ArrowDown"` |
| `.arrow-left` / `.left` | `"ArrowLeft"` |
| `.arrow-right` / `.right` | `"ArrowRight"` |
| `.home` | `"Home"` |
| `.end` | `"End"` |
| `.page-up` | `"PageUp"` |
| `.page-down` | `"PageDown"` |
| Any other single-letter / word modifier | matches `ev.key.to_lowercase()` literal |

The "any other" rule means `pp-on:keydown.k="open"` fires on the `k`
key; `pp-on:keydown.slash="focus_search"` matches the `/` key via
its `.key` string, etc. Casing is normalized to lowercase.

### 3.2 Modifier-key state

| Modifier | Condition |
|---|---|
| `.ctrl` | `ev.ctrlKey === true` |
| `.shift` | `ev.shiftKey === true` |
| `.alt` | `ev.altKey === true` |
| `.meta` | `ev.metaKey === true` |

All of these **require** the state — `pp-on:keydown.ctrl.k` fires
only when ctrl is held *and* the key is `k`. A named key like
`.enter` without a state modifier fires regardless of whether
modifier keys are held (Shift+Enter still matches `.enter`).

### 3.3 Combining with other pp-on modifiers

Key modifiers coexist with the existing set (`.prevent`, `.stop`,
`.self`, `.once`, `.window`, `.document`, `.debounce[.ms]`). Order is
irrelevant. Example:

```html
<input pp-on:keydown.escape.prevent="close" />
<form  pp-on:keydown.enter.prevent.stop="submit" />
```

## 4. Semantics

Between the event listener firing and the handler invocation,
`run_key_filter(ev, modifiers)` returns `true` iff:

1. The event is a `KeyboardEvent` (otherwise key modifiers all
   fail — fire only if no key modifier was specified).
2. Every named-key modifier in the list matches
   `ev.key.to_lowercase()` (after the alias table above).
3. Every modifier-state modifier in the list matches the
   corresponding property on the event.

Non-key modifiers (`prevent`, `stop`, etc.) are ignored by the
filter; they continue to be handled where they already are.

When the filter fails:

- `.prevent` / `.stop` are still applied (so the key still gets
  `preventDefault()`-ed if requested). The handler is *not*
  dispatched and the debounce timer isn't touched.

## 5. Implementation

Pure change to `crates/pocopine-core/src/directives/on.rs`. One new
helper:

```rust
fn key_filter_matches(ev: &Event, modifiers: &[String]) -> bool {
    // 1. Partition modifiers into {key names, modifier keys, others}.
    // 2. If no key modifiers present, pass.
    // 3. Cast to KeyboardEvent; if cast fails, key modifiers fail.
    // 4. Check each named key against ev.key().to_lowercase().
    // 5. Check each modifier-state against ev.ctrlKey / etc.
}
```

The event closure inside `run()` wraps the existing dispatch path:

```rust
if prevent { ev.prevent_default(); }
if stop    { ev.stop_propagation(); }
if !key_filter_matches(&ev, &modifiers) { return; }
if self_only && target != el { return; }
// ... existing debounce + invoke ...
```

## 6. Edge cases

- **Key events on non-keyboard sources.** Can't happen with
  `keydown`/`keyup`/`keypress` — but if a user writes
  `pp-on:click.enter`, the filter returns `false` (not a
  `KeyboardEvent`), so the handler never fires. Harmless.
- **Internationalised key names.** `ev.key` returns the produced
  character, so on a German layout `z` key labelled with the Latin
  `z` still produces `"z"`. Dead keys produce `"Dead"` until the
  second press completes. Authors concerned with physical keys
  should reach for `ev.code` via a typed handler (RFC-008).

## 7. Examples

### Close a dialog on Escape, block default browser behaviour:

```html
<template pp-teleport="body" pp-if="open">
  <div class="dialog" pp-on:keydown.escape.prevent="open = false">...</div>
</template>
```

### Menu with arrow-key navigation:

```rust
#[handlers]
impl PineMenu {
    pub fn on_key(&mut self, ev: KeyboardEvent) { /* fallback, all keys */ }
    pub fn next(&mut self) { self.focused = (self.focused + 1) % self.items.len(); }
    pub fn prev(&mut self) { self.focused = self.focused.saturating_sub(1); }
}
```

```html
<ul class="pine-menu"
    pp-on:keydown.arrow-down.prevent="next"
    pp-on:keydown.arrow-up.prevent="prev"
    pp-on:keydown.escape="close"
>
```

### Command palette shortcut:

```html
<body pp-on:keydown.ctrl.k.prevent="toggle_palette"></body>
```
