# RFC-112: PathAccess — typed nested-path access

**Status:** IMPLEMENTED (this branch)
**Crates:** `pocopine-core` (`path_access`), `pocopine-macros` (`#[derive(PathAccess)]`, `#[component]`/`#[store]` dispatch), `pocopine` (re-exports)
**Relates to:** RFC-024 §7 update (the snapshot round-trip this accelerates and falls back to), RFC-095 W2 (fingerprints), RFC-113 (nested signals — built on this substrate)

## Summary

A recursive, serde-style derive giving the runtime **typed access to
nested locations** addressed by template path strings:

```rust
#[derive(Serialize, Deserialize, Default, PathAccess)]
struct Settings { theme: String, limits: Limits }   // Limits also derives
```

Two operations, composing one level at a time:

- `path_set(&mut self, path, &JsValue) -> bool` — write a leaf by
  deserializing **at the leaf arm, where the concrete type is known**.
  A `get_mut() -> &mut ?` shape cannot cross `dyn ComponentState`
  (the leaf type varies per runtime path), so the value is pushed
  down instead of a reference pulled up.
- `path_fingerprint(&self, path) -> Option<u64>` — the nested
  analogue of `field_fingerprint`, feeding RFC-113's leaf-granular
  dirty sweep.

`Vec<T: PathAccess>` traverses numeric segments; `Option<T>`
traverses through `Some`; empty path means *the value itself* (what
a `Vec` element hit with an exhausted path needs).

## Why

Since the RFC-024 §7 update, deep template writes work — via a
snapshot round-trip: serialize the whole container, mutate the JS
snapshot, deserialize the whole container back. Costs that
accumulate: a keystroke into a field of a big `Vec` round-trips the
entire `Vec`; `#[serde(skip)]` siblings get reset to `Default` by the
whole-container deserialize; map fields aren't reachable at all
(serde_wasm_bindgen emits ES `Map`s that `Reflect` can't walk).
A native leaf write is O(leaf), touches nothing else, and gives
RFC-113 its fingerprint reach.

## Design

- **Dispatch across unknown types — autoref specialization.** The
  macros emit descent calls without knowing whether a child type
  implements `PathAccess`:
  `PathSetDispatch(&mut self.field).__poc_dispatch_set(rest, value)`.
  Method resolution picks the by-value impl (bound on `PathAccess`)
  when the bound holds; otherwise autoref finds the by-reference
  fallback which returns `false`/`None` — terminating the native
  path so the runtime degrades to the snapshot round-trip. Foreign
  field types need nothing.
- **`ComponentState` hooks.** `set_path` / `path_fingerprint`
  (defaults: `false`/`None`); `#[component]` and `#[store]` emit
  per-field dispatch arms (serde-skipped fields excluded — their
  projections never carry them, so template paths can't reach them).
- **The write lane.** `path::write_segments_with` tries
  `ScopeAccess::write_path` (→ `scope::write_path_tracked`:
  `set_path`, container-projection invalidation, the RFC-113
  trigger lattice) before the snapshot fallback. Behavior parity:
  `set_leaf` mirrors the macro `set`'s empty-string→`null` retry.
- **Exports.** Trait at `pocopine::PathAccess`, derive under the
  same name (the serde convention); both in the prelude.

## Non-goals

- Map keys in paths (needs a read-side story first — ES `Map`
  projections aren't template-walkable).
- A `path_get` read lane (v1 reads keep walking container
  projections; leaf projections are RFC-113's v2).
- Enums, tuple structs, generics beyond what `split_for_impl` gives.

## Prior art & alternatives (deep-researched, 2026-07-03)

Adversarially-verified survey of the 2024–2026 ecosystem; every claim
below survived 3-vote verification against primary sources.

- **bevy_reflect `GetPath`** — the reference string-path design (one
  blanket impl over `#[derive(Reflect)]`), but: not dyn-compatible
  (traversal must flow through `dyn PartialReflect`), derive metadata
  costs **~1.7 KiB/type of wasm after `wasm-opt -Oz`** (bevy PR
  #15030's own tables; engine-type registration = +11.46% wasm), and
  bevy's own path author flags per-segment dyn dispatch as too slow
  for hot access (discussion #10285), proposing offset-based
  Swift-style keypaths (Oct 2023) — **never shipped**, in bevy or any
  standalone crate.
- **Leptos `reactive_stores`** — eliminates runtime strings via
  `#[derive(Store)]` typed accessors + numeric `StorePath` into an
  `Arc<RwLock>` trigger registry with a this/children split (their
  answer to our trigger lattice). Rejected by construction: mandatory
  `.write()`/`.set()` proxies + write-observation detection violate
  the plain-Rust-handlers and diffing invariants. Cautionary data:
  two correctness bugs in its first two months (#3338 re-entrant
  RwLock wasm panic on nested keyed Vecs; #3523 missed descendant
  notifications, fixed by making every leaf write O(path-depth)
  trigger-map lookups).
- **facet / facet-reflect** — the only candidate that could subsume
  serde AND this derive under one `#[derive(Facet)]` (Peek/Poke +
  FieldPath). Self-described experimental with acknowledged soundness
  issues; the "const shapes avoid registry-style wasm weight" claim
  was REFUTED 0-3 in verification. Re-evaluate if it stabilizes.
- **`field_access` & keypath/lens crates** — single-level or
  typed-only; none traverse dotted runtime paths across unnameable
  intermediate types.

Verdict: absent language-level field projections, every design
converges on derive-generated per-type descent (match arms here,
const metadata in bevy/facet, accessor codegen in Leptos); the
differences are cost trades, not elegance wins. This design occupies
the point the constraints define. The one evidenced refinement (an
inference, not established art — no framework found doing it): let
the component macro emit compile-time-resolved typed accessor
closures for OWN-field paths (bevy's `animated_field!` retreat from
dyn paths is the precedent), reserving the runtime string match for
cross-type `$store` paths — worth considering only if a profile ever
shows the per-hop match on the write path.

## Tests

`pocopine-core/tests/nested_signals.rs` (hand-implemented trait —
runtime semantics without the macro layer) +
`pocopine/tests/path_access_ui.rs` (trybuild compile-pass: derive +
autoref dispatch for deriving AND non-deriving fields + nested
`pp-model` under RFC-111 validation).
