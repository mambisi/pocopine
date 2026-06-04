# 2026-04-19 — walker ↔ MutationObserver cycles

| Field | Value |
|---|---|
| **Area** | `crates/pocopine-core/src/walker.rs`, `crates/pocopine-core/src/directives/for_.rs`, `crates/pocopine-core/src/devtools.rs` |
| **Discovered via** | HN example, `/item/:id` thread pages with recursive `<hn-comment>` subtrees. Devtools overlay burning CPU on inspect-hover. |
| **Commits that landed the fix** | `275234c`, `e4c8bb7`, `8e9a630`, `8f797cc` |
| **Tests that lock it** | `crates/pocopine/tests/walker.rs` |

## 1. TL;DR

A recursive `<hn-comment>` tree under a keyed `pp-for` rendered a few
levels, then the browser locked up. A separate symptom: the devtools
panel pegged CPU as soon as it expanded. Four small-but-nasty bugs in
the walker / observer handshake all fed the same failure mode
(amplifying work per reactive tick), and together they turned an
O(N) mount into O(N × D²) with a constant that doubled per mutation.

## 2. Impact

- Deep HN threads (20+ nested comments) froze the tab.
- Devtools overlay unusable when it surfaced a large scope tree —
  inspect-mode hover was the worst case because every setInterval
  render went through the same cycle.
- No data loss, no corruption. Purely a liveness / perf regression.

## 3. Symptoms

- "Comments are looping infinitely." — the page rendered blank or
  partially-rendered, CPU at 100%, memory climbing.
- The list page (`/`) was fine — non-recursive `pp-for` exposed no
  issues.
- Pattern-matching the scenarios made the diagnosis easier:
  recursion + reactive re-render was the thing that hurt.

## 4. Root causes

### 4.1 LoopScope pin fought the component-mount guard

`pp-for` clones its template body per item and pins a `LoopScope` on
the clone via `walker::bind_scope_to`, which writes the private
`SCOPE_ID_KEY`. The walker's mount guard was:

```rust
if is_registered(&tag) && get_private(el, SCOPE_ID_KEY).is_none()
    && get_private(el, "__pp_mounted").is_none() { mount_component(el, &tag); }
```

When the clone root was a registered component tag like
`<hn-comment>`, the LoopScope's `SCOPE_ID_KEY` tripped the guard —
the walker thought the component was already mounted and skipped
`mount_component`. Every iteration was an empty tag.

**Fix**: drop `SCOPE_ID_KEY.is_none()` from the guard. `__pp_mounted`
alone guards re-mount. The LoopScope (on the tag) and the component
scope (on its template root) coexist cleanly — they live on
different elements.

### 4.2 MutationObserver double-walked explicit inserts

`pp-for`, `pp-if`, and `pp-teleport` all do `insert_before` followed
by a synchronous `walker::walk(&clone)` inside an effect body.
Our global `MutationObserver` on `<body>` picked up the insert in
the next microtask and walked the SAME clone again. Every directive
inside the subtree ended up with two effects subscribed to the same
deps. On each reactive update the duplicate effects fired in
lockstep; for a recursive tree the count doubled per depth level.

**Fix**: mark the element with a private `WALKED_KEY` at the end of
`walker::walk`. The observer's `addedNodes` handler skips anything
that already carries the flag.

### 4.3 Keyed `pp-for` reorders tore down reused scopes

`insert_before` on an element that's already attached to the DOM
moves it. The DOM reports the move as `removedNodes` + `addedNodes`
records for the same node. The observer's `removedNodes` handler was
calling `release_subtree` unconditionally — which killed the
LoopScope, released every effect in the subtree, then re-walked on
the `addedNodes` side and built fresh ones. On a recursive tree,
every reorder rebuilt the whole descendant chain.

**Fix**: `Node.isConnected` check. A reparent ends with the node
still connected to the document (just somewhere else). Only truly
detached nodes (`isConnected == false`) get `release_subtree`.

### 4.4 Mount hook always fired `trigger_scope`

`fire_mount_hook` called `trigger_scope(scope_id)` after every
component mount, even when the component had no user-defined
`on_mount` to mutate anything. Combined with the `pp-bind` cascade
in a recursive tree, a blanket sweep per mount was the amplifier
that made §4.2 and §4.3 visible.

**Fix**: `ComponentState::has_on_mount()` (default `false`). The
`#[handlers]` macro overrides it to `true` only when the author
defined `on_mount`. `fire_mount_hook` reads the flag and skips both
the call and the sweep when it's false.

### 4.5 Devtools innerHTML poll was observer-visible

The overlay rebuilds its `innerHTML` on a 200ms `setInterval`. That
shows up to the observer as a `childList` mutation on the panel
root with many `removedNodes` + many `addedNodes`. The panel owns
its own DOM — app-facing walker work on those nodes is pure waste,
and when the panel was large, "pure waste" × 5Hz felt like a loop.

**Fix**: skip any `MutationRecord` whose `target` is the devtools
panel root (or a descendant). The panel is a runtime-managed island.

## 5. How the four bugs compounded

Pre-fix, one `StoryDetail` mount went like this on a thread with
depth D and N nodes:

1. `mount_component` clones StoryDetail template, walks it.
2. Keyed `pp-for` over `comments` clones + walks each top-level
   `<hn-comment>`. Because of §4.1, nothing actually mounted.
3. `fire_mount_hook` fires `trigger_scope` per mount (§4.4), which
   re-runs every effect in the newly-mounted scope — pp-text,
   pp-html, pp-if, and the nested CL.
4. The CL re-runs, reads children, reuses clones, which fires
   `trigger_scope` on each per-item loop scope, which fires pp-bind,
   which writes each grandchild's `comment` field, which triggers
   the grandchild's effects, which re-run CL at *that* level…
5. Every `insert_before` in step 4 (for reorders) triggered the
   observer into §4.3's tear-down + re-walk.
6. Every clone walked in steps 2-5 got double-walked in the next
   microtask by §4.2, doubling every effect.

The list page never exercised 3-5 because the iterated body didn't
itself contain a `pp-for`.

## 6. Invariants to preserve

New contributors working on the walker, pp-for, or directive code
should check against these:

### 6.1 "Managed" elements are off-limits to the observer

If the runtime walks or created an element, it should carry
`WALKED_KEY` by the time the `MutationObserver` callback runs for
its insertion. Re-walking an element you already walked creates
duplicate effects that never clean up.

### 6.2 Removals are "truly detached" only

A "removed" `MutationRecord` does not mean "unmount." Anytime you
see a removal in the observer, check `Node.isConnected` first — if
it's still true, it's a move. Only truly detached nodes should
call `release_subtree`.

### 6.3 LoopScope and ComponentScope can coexist on the same tag

`pp-for`'s `bind_scope_to` pins the per-item LoopScope on the clone
root. If that clone root is a registered component tag, the component
scope lives on the clone root's **first element child** (the
component template's root). Don't assume an element with
`SCOPE_ID_KEY` already set is a component root — check
`__pp_mounted` separately for that.

### 6.4 Lifecycle hooks cost nothing when absent

`on_mount` / `on_unmount` and any future lifecycle method should be
gated on a `has_on_*` flag so the no-hook path avoids both the call
*and* any related `trigger_scope` sweeps. A recursive component
tree will amplify any non-trivial per-mount work by depth × fanout.

### 6.5 Runtime-owned subtrees are islands

Anything the runtime owns *and* mutates on a timer (the devtools
panel today, eventually other debug/ops overlays) has to be
excluded from the app-facing `MutationObserver`. Adding a similar
surface in the future: mark its root with an ID or flag and filter
records whose target is inside it.

## 7. Tests

`crates/pocopine/tests/walker.rs` covers:

- `component_tag_inside_pp_for_mounts_and_binds` — §4.1
- `keyed_reorder_reuses_clones` — §4.3 (asserts `is_same_node`
  before/after reorder)
- `keyed_removal_releases_missing` — basic removal
- `observer_doesnt_double_walk_new_clones` — §4.2 (asserts the
  `<li>` has exactly one text child)
- `without_on_mount_renders_initial_state_cleanly` — §4.4

Run:

```bash
wasm-pack test --firefox --headless crates/pocopine
```

No standalone test for the devtools cycle (§4.5) yet — the fix
relies on a specific DOM marker (`#__pp_devtools_root`) and is
easy to eyeball via the HN demo. Worth adding if the filter logic
grows.

## 8. What would have caught this sooner

- **Integration tests at the walker layer from day one.** The unit
  tests in `crates/pocopine-core/tests/reactive.rs` exercise the
  reactive primitives but never a full walk-render-mutate cycle.
  `crates/pocopine/tests/walker.rs` now does that — new directives
  that land should drop a fixture there.
- **A "walked once" invariant check in debug builds.** If a
  `walker::walk` call ever sees an element that already has
  `WALKED_KEY`, we could panic under `#[cfg(debug_assertions)]`.
  Might be worth doing as a follow-up.
- **Effect-count assertions in tests.** We don't expose a public
  "how many effects are bound to this element?" count — adding one
  (even as a test-only hook) would let us nail down the
  double-walk case with a hard assertion instead of relying on
  textContent artifacts.
