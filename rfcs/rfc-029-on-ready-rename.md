# RFC 029 — Rename `post_mount` → `on_ready`

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-026-post-mount-watch-field.md`](./rfc-026-post-mount-watch-field.md) |

## 1. Summary

RFC-026 shipped a second mount-time lifecycle hook under the name
`post_mount(&self)` — the deferred, read-only counterpart to
`on_mount(&mut self)`. In practice the name is awkward: `post_` is
not a prefix pocopine uses anywhere else, and it makes the hook
sound like a follow-up artefact rather than a first-class state.

This RFC renames it to **`on_ready(&self)`**:

- Matches the `on_*` prefix family (`on_mount`, `on_unmount`,
  `on_cleanup`).
- Reads the way it behaves: by the time it runs, the scope's
  subtree is bound, the initial effect pass has committed, and the
  component is *ready* for proxy-reading observation
  (`watch_field`, `refs::get`, `$id`).
- Mirrors framework vocabulary that already means "mount is
  done and fully wired" (Vue's `mounted`, Svelte's `onMount`).

## 2. Non-goals

- **No new hooks.** `on_mount` / `on_unmount` stay as they are;
  both already cover the corresponding Vue/Svelte positions
  (`mounted` / `beforeUnmount`). This RFC is a pure name-only
  change.
- **No behaviour changes.** The microtask-deferred scheduling, the
  `&self` receiver, the skip-if-unused walker short-circuit, and
  the `#[watch(field)]` auto-generation all stay byte-for-byte.
- **No deprecation window.** `post_mount` is an internal hook
  name introduced two RFCs ago — there's no backwards-compat
  surface worth preserving; the rename is a mechanical pass.

## 3. Surface

### 3.1 User-facing

Before:
```rust
#[handlers]
impl PineOverlay {
    pub fn post_mount(&self) {
        install_trigger_listeners(current_scope_id().unwrap());
    }
}
```

After:
```rust
#[handlers]
impl PineOverlay {
    pub fn on_ready(&self) {
        install_trigger_listeners(current_scope_id().unwrap());
    }
}
```

`#[watch(field)]` continues to work — its macro expansion targets
the renamed trait method, invisibly.

### 3.2 Trait surface

`ComponentState` / `HandlerDispatch` both rename:

| Old | New |
|---|---|
| `fn post_mount(&self) {}` | `fn on_ready(&self) {}` |
| `fn has_post_mount(&self) -> bool` | `fn has_on_ready(&self) -> bool` |

Walker call site renames its snapshot variable from `has_post` →
`has_ready` and the scheduled closure body from
`scope.state.borrow().post_mount()` →
`scope.state.borrow().on_ready()`.

### 3.3 Macro surface

`#[handlers]` now recognises a method named `on_ready` (not
`post_mount`) and emits the corresponding trait override. The
`#[watch(field)]` sugar continues to auto-generate an `on_ready`
implementation when one isn't explicitly written.

## 4. Implementation notes

Single-sweep rename:

1. `crates/pocopine-core/src/handler.rs` — rename trait methods
   + doc comments.
2. `crates/pocopine-core/src/scope.rs` — rename `ComponentState`
   methods + doc comments.
3. `crates/pocopine-core/src/walker.rs` — rename local variable
   + delegate method name in `fire_mount_hook`.
4. `crates/pocopine-macros/src/lib.rs` — rename the first-pass
   scan identifier, the generated override method, and the
   `#[watch]`-generated `on_ready` body.
5. `crates/pine/src/tooltip/mod.rs` — the one Pine overlay that
   writes the hook directly (the others use `#[watch]`).

All existing tests — `walker.rs`, Pine's `tests/pine.rs`, the
demo — continue to pass unchanged because the rename is
mechanical and the semantics are identical.

## 5. Why not `ready` (no prefix)?

`on_ready` is one character longer but keeps the `on_*` family
consistent: every user-facing lifecycle hook in pocopine starts
with `on_` (`on_mount`, `on_unmount`, `on_cleanup`, `on_error`
if we ever add it). Breaking that pattern for a single hook
would be its own papercut.
