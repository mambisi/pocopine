# RFC 007 — `pp-for` keyed iteration

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — (extends [`rfc-004-pp-for.md`](./rfc-004-pp-for.md) §11) |
| **Related** | [`rfc-004-pp-for.md`](./rfc-004-pp-for.md), [Vue keyed v-for](https://vuejs.org/api/built-in-directives.html#v-for), [Alpine x-bind:key](https://alpinejs.dev/directives/for#keys) |

## 1. Summary

Add `pp-key` as a companion attribute to `pp-for`. Each iteration's
clone is tagged with a **stable key** derived from the item; on
reactive re-runs, the directive reuses clones whose keys still appear
in the new array — updating their loop scope in place — instead of
tearing the whole list down and rebuilding. Clones for keys that no
longer appear get unmounted. New keys get fresh clones.

```html
<template pp-for="story in stories" pp-key="story.id">
  <li class="story" pp-text="story.title"></li>
</template>
```

Without `pp-key`, `pp-for` keeps its v0 whole-rebuild behavior
(RFC-004 §7.1). `pp-key` is opt-in; adding it never regresses
correctness, only DX (preserved focus, scroll, input state,
transitions).

## 2. Motivation

The RFC-004 whole-rebuild loop was fine for "print a list of things
that don't change often." It breaks down fast once items carry state:

* A **search form**'s input field loses focus every keystroke if the
  list re-renders underneath it.
* A **vote button** mid-animation gets yanked and replaced.
* A comment tree with **collapse state** loses its collapsed nodes
  on any sibling change.
* A **large list** pays O(N) DOM churn even when one item changed.

Keys turn each of those from a framework bug into a correctness
guarantee. Every other modern framework (React, Vue, Svelte, Solid,
Alpine) lands on the same shape for the same reason.

## 3. Non-goals

* **Automatic keying by index.** Index is almost always the wrong
  key — it ties identity to position, which defeats the whole point.
  If the author doesn't provide `pp-key`, we keep the existing naive
  (whole-rebuild) path. No implicit index keying.
* **Complex move minimisation (LIS algorithm).** v1 uses the simple
  "insert each reused / new clone in order" approach — correct in all
  cases, and O(N) DOM operations even when every clone is already in
  place (browsers no-op a same-position `insertBefore`). An LIS-based
  pass can come later if profiling demands it.
* **Destructuring keys** (`pp-key="[row.id, row.version]"`). A single
  dotted path expression only — same grammar as every other
  directive's value.
* **`pp-for-else`** (render when empty). Covered by `pp-if="!items.length"`
  today; no new directive.
* **Keyed `pp-teleport`** — still one target per template host.

## 4. Surface

Two attributes on the same `<template>` host:

| Attribute | Meaning |
|---|---|
| `pp-for="<item> in <path>"` | Same as RFC-004 §4. |
| `pp-key="<dotted-path>"` | **Optional.** Path evaluated against the per-item loop scope. Its string form becomes the clone's identity. |

`pp-key` is resolved through the same `resolve_path` the other
directives use, which means you can reach into the item, `$index`,
`$first`, `$last`, or any parent-scope field — but the common case is
`<loop-item>.<id-field>`.

## 5. Semantics

### 5.1 Key derivation

For each element `item` of the items array, evaluate
`resolve_path(loop_scope_proxy, pp_key_expr)`. Stringify the result:

* string → as-is
* number / bool → canonical `to_string`
* null / undefined → **warning** plus fallback key `__pp_for_null_<index>`
  (so at least the render succeeds instead of collapsing distinct
  items under the same "undefined" key)
* object / array → `JSON.stringify`

### 5.2 Diff

Given the previous pass's `Vec<PrevItem>` (ordered list of
`{element, scope_id, loop_state, key}`) and the new items:

1. Build a `HashMap<String, PrevItem>` from the previous pass.
2. For each new item, in order, with its derived key:
   * If the key is already in the map: **reuse**. Pop it out of the
     map. Update the loop scope in place (new item value, new index,
     new total). Fire `trigger_scope(loop_scope.id)` so every effect
     reading a field through the loop proxy re-evaluates.
   * Otherwise: **create**. Fresh `LoopScope`, fresh `Scope`, clone
     the template body, `bind_scope_to` onto the clone, push. The new
     clone gets walked so its directives bind.
3. Items left in the map are **removed**. Remove each clone via
   `parent.remove_child(&element)`; the MutationObserver releases
   its effects + scope as usual.
4. **Reorder**: iterate the new sequence and for each clone call
   `parent_node.insert_before(&element, Some(template))`. This is
   idempotent when the element is already at the right position and
   moves it otherwise.

### 5.3 Duplicate keys

If two items in a single pass derive the same key, the second hit
gets treated as "new" (fresh clone) and we log a console warning.
This is a programming error, not a runtime condition — in v1 we
prefer "render something sane + warn" over "render nothing."

### 5.4 When `pp-key` is absent

Exactly today's behavior: every pass tears down every prior clone
and creates fresh ones (RFC-004 §7.1). Opt-in, no silent upgrades.

## 6. Examples

### 6.1 HN search bar (preserve focus)

```html
<section>
  <input class="search__input"
         pp-model="query"
         pp-on:input.debounce.300="search"
         pp-ref="search" />

  <ol class="stories">
    <template pp-for="story in stories" pp-key="story.id">
      <li class="story">
        <a pp-bind:href="story.title_href" pp-text="story.title"></a>
      </li>
    </template>
  </ol>
</section>
```

The `<input>` is outside the loop, so keying the list means each
keystroke debounce → new results → existing story clones reused
when their id is unchanged. No DOM thrashing under the cursor.

### 6.2 Keying by index (explicit)

```html
<template pp-for="row in rows" pp-key="$index">
  ...
</template>
```

Uses the existing `$index` magic as the key. Still a whole-rebuild
semantically (index changes when order changes) but at least it
makes the choice explicit.

### 6.3 Recursive comment tree

```html
<template pp-for="child in comment.children" pp-key="child.id">
  <hn-comment pp-bind:comment="child"></hn-comment>
</template>
```

Each `<hn-comment>` component mounts once per distinct id; toggling
a parent's collapse state doesn't rebuild every descendant.

## 7. Implementation

Single `pp-for` module, branching on `pp-key` presence at setup time:

```rust
match &key_expr {
    Some(path) => render_keyed(...),
    None       => render_naive(...),   // existing code, unchanged
}
```

`render_keyed` owns a `Vec<PrevItem>` that it swaps every pass.
`PrevItem` stashes the element, the loop scope id, and the
`Rc<RefCell<LoopScope>>` so we can mutate `item` / `index` / `total`
on reuse without going through the serde round-trip.

Updating the loop scope in place + `trigger_scope(id)` is how
reused clones pick up new data. Effects subscribed to keys on the
loop proxy re-evaluate against the updated `LoopScope::get`.

## 8. Edge cases

* **Array length zero** with previous entries: every previous entry
  drops to "remove" branch. Equivalent to today's "tear down all"
  path.
* **Reordering with no additions or removals**: every clone reused
  + every `insertBefore` call repositions the one that moved. No new
  scopes, no walker work.
* **Item identity preserved but fields changed**: same key →
  reuse → `trigger_scope` → effects re-evaluate. Matches Vue.
* **pp-transition on the clone**: transition state pinned by
  `__pp_tx_id` lives on the element. Reuse keeps it → in-flight
  enter/leave survive reorders.
* **pp-ref inside a keyed clone**: reused clones keep the same
  element identity → the `refs` entry they registered under their
  loop scope is still valid.
* **Nested keyed loops**: each clone's template body is walked
  independently on creation; a nested `pp-for pp-key` inside a
  clone gets its own `Vec<PrevItem>` per clone-instance, closed over
  by each nested effect.

## 9. Alternatives considered

* **`:key="..."` in the pp-for string**
  (`pp-for="story in stories :key=story.id"`). Inline saves one
  attribute but complicates the parser and diverges from the
  one-directive-per-attribute rule the rest of the surface follows.
* **Require `pp-key` always.** Cleaner behaviour but breaks every
  existing `pp-for` call site with no upgrade path. Not worth the
  disruption for v1.
* **Auto-fallback to index key.** Silently wrong; see §3.
* **`pp-key` as a separate directive** (`#[component]`-style
  registration). Doesn't compose — `pp-key` is meaningless without
  `pp-for`, and putting it in the registry would imply it can work
  standalone.

## 10. Out of scope (future work)

* LIS-based move minimisation.
* Multi-field compound keys / array keys.
* Keyed `<pp-outlet>` so router transitions can reuse page scopes.
* `pp-transition-group`-style list-item enter/leave transitions.
