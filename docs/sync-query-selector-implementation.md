# `#[query]` selector — implementation map

Where each piece lives, how data flows through the layers, and what to scrutinize when reviewing or modifying the selector code. Companion to [`sync-query-selector-mechanism.md`](sync-query-selector-mechanism.md), which explains the *design*; this doc explains the *code*.

Line numbers are best-effort references — they drift; symbols and file paths are the durable handles.

## Layer overview

```text
┌──────────────────────────────────────────────────────────────────┐
│  USER CODE (downstream crate)                                    │
│                                                                  │
│  #[query] fn foo(client: QueryClient, ws: String) -> u32 { … }   │
│                                                                  │
│  foo::observe(&client, "W1".into())     ◄── proc-macro expansion │
└────────────────────┬─────────────────────────────────────────────┘
                     │
                     ▼
┌──────────────────────────────────────────────────────────────────┐
│  GENERATED MODULE  (pocopine-sync-query-macros)                  │
│                                                                  │
│  pub mod foo {                                                   │
│    const SELECTOR_ID = FNV-1a(module_path!() + "::foo");         │
│    pub fn observe(client: &QueryClient, ws: String)              │
│        -> SelectorView<u32> { …box args, build closure… }        │
│  }                                                               │
│  fn __pq_user_fn__foo(client: QueryClient, ws: String) -> u32 ───┼─┐
│  // ^ lifted SIBLING of the module so super:: still resolves     │ │
└────────────────────┬─────────────────────────────────────────────┘ │
                     │                                               │
                     ▼                                               │
┌──────────────────────────────────────────────────────────────────┐ │
│  RUNTIME  (pocopine-sync-query crate)                            │ │
│                                                                  │ │
│   QueryClient::observe_selector(id, hash, args, compute) ────────┼─┘
│                            │                                     │
│                            ▼                                     │
│   selectors: HashMap<(SelectorId, u64), Vec<Weak<…>>>            │
│             ◄── bucketed; dyn_eq disambiguates hash collisions   │
│                            │                                     │
│                            ▼                                     │
│   SelectorEntry<T> { compute, cached, args, keepers, listeners } │
│                            │                                     │
│                            │  rerun:                              │
│                            │   1. push frame                      │
│                            │   2. (self.compute)()  ────────────►│ user_fn body
│                            │   3. pop frame                       │  calls view.rows()
│                            │   4. wire listeners on tracked deps  │  → record_read
│                            │   5. PartialEq diff; fire on change  │
└────────────────────┬─────────────────────────────────────────────┘
                     │
                     ▼
┌──────────────────────────────────────────────────────────────────┐
│  EXISTING SUBSCRIPTION ENGINE  (unchanged)                       │
│                                                                  │
│   QuerySubscription<Row>.notify_listeners() fires after:         │
│     route_optimistic_changes / route_canonical_changes /         │
│     route_canonical_pull / dequeue_pending                       │
│                                                                  │
│   ◄── one of the listeners is the selector's rerun callback     │
└──────────────────────────────────────────────────────────────────┘
```

## Where each piece lives

### `src/selector.rs` (new module)

| Symbol | Notes |
|---|---|
| `SelectorId(u64)` | FNV-1a 64 of `module_path!() + "::" + fn_name`, computed at the user crate's compile time |
| `AnyTrackable` trait | Object-safe; `register_listener` + `unregister_listener`. Implemented by `QuerySubscription<Row>` (bridges existing plumbing) and `SelectorEntry<T>` (so selectors compose recursively) |
| `AnyArgs` trait + blanket impl | `dyn_eq` over any `T: PartialEq + 'static`. The blanket impl is what lets the macro box arbitrary tuples |
| `TrackToken` | RAII; holds `Weak<dyn AnyTrackable>` so a token outliving its source silently no-ops on drop |
| `SELECTOR_STACK` (thread_local) | Stack of `TrackingFrame { deps: Vec<(Rc<dyn AnyTrackable>, Box<dyn Any>)> }` |
| `record_read` / `currently_tracking` | The only hook into the runtime; no-op outside any selector |
| `AnySelector` trait | Object-safe; `as_rc_any` + `args()` |
| `SelectorEntry<T>` | The heart — compute closure, cached output, captured deps (keepers + listener tokens), own listener registry, re-entrancy guard, weak ref back to `QueryClientInner` |
| `SelectorEntry::rerun` | The 5-step algorithm in one place |
| `Drop for SelectorEntry` | Bucket-aware self-removal from the client's selectors map |
| `SelectorView<T>` | Thin user-facing wrapper; `value()` does `record_read` on its own entry so nested selectors compose |

### `src/client.rs` (modifications)

| What | Notes |
|---|---|
| `SelectorBucket` type alias | `Vec<Weak<dyn AnySelector>>` — separate chaining for the selectors hash map; satisfies clippy `type_complexity` |
| `register_listener_rc` | Takes a pre-built `Rc<dyn Fn()>`, avoids double-wrapping the selector's rerun callback (one closure is held by every upstream) |
| `AnyTrackable for QuerySubscription<Row>` | Two-line impl that bridges the existing listener plumbing into the trait-object interface |
| `QueryClientInner.selectors` | `HashMap<(SelectorId, u64), SelectorBucket>` — keys are weak; entries self-remove on Drop |
| `QueryClient::observe_selector` | The runtime entry point the macro targets |
| `QueryView::track_for_selector` | Called from `rows`/`len`/`is_empty`; bumps the subscription's refcount via a cloned `QueryHandle` keeper |
| `impl Clone for QueryHandle` | Bumps subscription refcount; needed for the keeper pattern so a selector's `tracked` list keeps the subscription alive between reruns |
| `impl Clone for QueryClient` | Selector compute closures must own a client (`Fn() + 'static` can't borrow) |

### `pocopine-sync-query-macros/src/lib.rs` (additions)

| Symbol | Notes |
|---|---|
| `#[proc_macro_attribute] query` | Rejects macro args (no `no_diff` opt-out yet) |
| `expand_query` | Main expansion; validates the fn shape — rejects `async`/`unsafe`/`const`/generic/`where`/variadic/`self`/destructuring/`ref` patterns |
| `partition_selector_attrs` | Routes user attrs to module / observe() / inner-fn by audience |
| `is_query_client_type` | First-arg-is-`QueryClient` heuristic — that arg is *not* hashed and *not* in `observe()`'s public arg list |
| `contains_impl_trait` | Recursive walker; rejects argument-position `impl Trait` anywhere in the type — top-level, nested in containers, assoc-type bindings, trait-object bounds |
| `path_args_contain_impl_trait` | Helper; walks `GenericArgument::Type` / `AssocType` / `Constraint` |

## Critical data flows

### A. First `observe()` — cache miss

```text
foo::observe(&client, "W1")
  │
  ├─ DefaultHasher::hash(ws) → args_hash
  ├─ Box::new((ws.clone(),)) as Box<dyn AnyArgs>  // for dyn_eq
  ├─ compute = move || __pq_user_fn__foo(client.clone(), ws.clone())
  │
  ▼
client.observe_selector(SELECTOR_ID, hash, args, compute)
  │
  ├─ Walk selectors.get(&(id, hash)) bucket → no live entry with dyn_eq match
  │
  ├─ Rc::new(SelectorEntry::new(...))
  ├─ selectors.entry(key).or_default().push(Weak::from(&entry))
  │  ◄── opportunistic GC of dead Weaks in the bucket
  │
  ▼
entry.rerun()
  │
  ├─ push_frame()
  ├─ (compute)() ── runs __pq_user_fn__foo
  │   └─ client.observe(query).rows()
  │       └─ track_for_selector()
  │           └─ record_read(subscription.clone() as Rc<dyn AnyTrackable>,
  │                          Box::new(handle.clone()))
  │                                  ▲          ▲
  │                                  │          └─ keeper: bumps sub.refcount
  │                                  └─ trackable: for listener register
  │
  ├─ pop_frame() → captured deps
  ├─ Drop old listener_tokens (none on first run)
  ├─ For each (trackable, keeper):
  │   ├─ cb = move || weak_self.upgrade().map(|s| s.rerun())
  │   ├─ id = trackable.register_listener(cb)  ── via AnyTrackable
  │   └─ store TrackToken + keeper
  │
  └─ Diff: cached was None → write Some(output); listeners empty → no fire
```

### B. Mutation reaches a tracked subscription

```text
client.mutate::<EchoMutator>(payload).await
  │
  ├─ route_optimistic_changes(...)
  │   └─ for matching sub: state.push_pending(); sub.notify_listeners()
  │                                                       │
  │                                                       ▼
  │                                          (snapshots listeners, fires each)
  │                                                       │
  │                                          ┌────────────┴──────────┐
  │                                          ▼                       ▼
  │                                 (other listeners)        selector's rerun cb
  │                                                                  │
  │                                                                  ▼
  │                                                        weak_entry.upgrade()
  │                                                                  │
  │                                                                  ▼
  │                                                        SelectorEntry::rerun()
  │                                                                  │
  │                                                                  ├─ recompute
  │                                                                  ├─ new ≠ cached?
  │                                                                  │   yes → fire selector's
  │                                                                  │         own listeners
  │                                                                  │   no  → STOP (diff
  │                                                                  │         suppression)
  │
  ├─ apply_remote(...).await → canonical changes
  ├─ route_canonical_changes(...)  → notify_listeners again (same chain)
  │
  └─ guard.disarm()
```

### C. Drop cascade

```text
drop(view: SelectorView<T>)
  │ entry.strong_count drops
  ▼
SelectorEntry::Drop
  ├─ inner.upgrade()? .selectors.try_borrow_mut()?
  ├─ bucket.retain(|w| w.strong_count() > 0)   // prune dead Weaks
  ├─ if bucket.is_empty() → selectors.remove(&key)
  │
  ▼ then drop the entry's fields (declaration order):
    ├─ keepers Vec drops
    │   └─ Each Box<dyn Any> drops → cloned QueryHandle drops
    │       └─ release_inner → subscription refcount--
    │           └─ if 0 → driver epoch bumped + registry removed
    └─ listener_tokens drops
        └─ Each TrackToken: Weak.upgrade()?.unregister_listener(id)
           ◄── safe if subscription already gone (Weak fails upgrade)
```

## Macro expansion shape

User writes:

```rust,ignore
#[query]
#[must_use]
pub fn issue_count(client: QueryClient, ws: String) -> u32 {
    client.observe(Issue::query().eq(field::workspace_id, ws).build())
          .rows().len() as u32
}
```

Becomes (approximately — see `expand_query` for the full shape):

```rust,ignore
// inner-fn attrs ⊕ #[doc(hidden)]; lives at PARENT scope so super:: still works
#[doc(hidden)]
fn __pq_user_fn__issue_count(client: QueryClient, ws: String) -> u32 {
    client.observe(Issue::query().eq(field::workspace_id, ws).build())
          .rows().len() as u32
}

// module-attrs (none in this example); module vis = user's vis
pub mod issue_count {
    use super::*;

    pub const SELECTOR_ID: SelectorId = SelectorId::new(
        __private::fnv1a64(concat!(module_path!(), "::issue_count").as_bytes())
    );

    // observe-attrs go here (e.g. #[must_use])
    #[must_use]
    pub fn observe(__pq_client: &QueryClient, ws: String) -> SelectorView<u32> {
        let mut h = DefaultHasher::new();
        Hash::hash(&ws, &mut h);
        let args_hash = h.finish();

        let __pq_args_boxed: Box<dyn AnyArgs> = Box::new((ws.clone(),));

        let __pq_client_for_compute = __pq_client.clone();   // emitted only if user has a `QueryClient` arg
        let __pq_arg_0 = ws.clone();                          // positional helper names (raw-ident-safe)

        let __pq_compute = move || {
            super::__pq_user_fn__issue_count(
                __pq_client_for_compute.clone(),
                __pq_arg_0.clone(),
            )
        };

        __pq_client.observe_selector(SELECTOR_ID, args_hash, __pq_args_boxed, __pq_compute)
    }
}
```

## Areas worth scrutiny

| Where | Concern | Why it's load-bearing |
|---|---|---|
| `SelectorEntry::rerun` | Re-entrancy + RefCell borrow safety | Snapshots listeners before firing; `running: Cell<bool>` guards re-entry; old tokens dropped BEFORE new ones registered |
| `Drop for SelectorEntry` | Bucket pruning + GC | `try_borrow_mut` is intentional (silent no-op on contention); fields drop in declaration order — `keepers` BEFORE `listener_tokens` |
| `QueryClient::observe_selector` | `Rc::downcast` safety | `SelectorId` encodes `(crate, module_path, fn_name)` → unique `T`. A failed downcast means a framework-invariant violation |
| `QueryView::track_for_selector` | Refcount hygiene | `QueryHandle::clone` is what keeps the underlying subscription alive across reruns. Without the keeper, the subscription could be GC'd while a selector's Weak listener still points at it |
| `expand_query` validation | Soundness of the cache key | The cache key is `(SelectorId, AnyArgs)`. Anything that makes the user fn generic over the call-site type (explicit generics, `impl Trait` anywhere in arg types) is rejected — otherwise two concrete instantiations could alias the same cached value |
| `partition_selector_attrs` | Attr routing correctness | doc/deprecated → module; must_use/track_caller → observe; lint → inner; cfg → all three |

## Test coverage map

| Concern | Test |
|---|---|
| Caching by `(id, args_hash)` | `tests/selector.rs::observe_selector_caches_by_id_and_args_hash` |
| Hash-collision soundness | `tests/selector.rs::hash_collision_does_not_alias_distinct_args` |
| Invalidation via real mutation | `tests/selector.rs::selector_reruns_when_tracked_subscription_changes` |
| Diff suppression | `tests/selector.rs::diff_suppression_blocks_listener_when_output_unchanged` |
| Nested composition | `tests/selector.rs::nested_selector_propagates_through_outer` |
| Cascade stop on inner diff-equal | `tests/selector.rs::nested_diff_suppression_stops_cascade` |
| Cascade drop + sub GC | `tests/selector.rs::drop_view_removes_entry_from_registry` |
| `SELECTOR_ID` stability | `tests/query_macro.rs::selector_id_is_stable_and_distinct` |
| End-to-end macro + mutation | `tests/query_macro.rs::macro_observe_returns_cached_value_and_reacts_to_mutations` |
| Sibling selector composition | `tests/query_macro.rs::macro_selector_composes_inside_another_selector` |
| `super::` resolution preserved | `tests/query_macro.rs::macro_preserves_super_resolution_in_body` |
| `mut`-arg preserved | `tests/query_macro.rs::macro_preserves_mut_arg_binding` |
| User attrs follow body | `tests/query_macro.rs::macro_forwards_user_attrs_to_lifted_body` |
| Doc on module (rustdoc surface) | `tests/query_macro.rs::macro_routes_doc_to_module` |
| `#[must_use]` on observe() | `tests/query_macro.rs::macro_routes_must_use_to_observe` |
| `#[track_caller]` accepted | `tests/query_macro.rs::macro_accepts_track_caller_attr` |
| Raw-ident args | `tests/query_macro.rs::macro_handles_raw_identifier_args` |
| `impl Trait` rejection (4 forms) | `tests/ui/query_impl_trait_{top_level,nested,assoc}.rs`, `tests/ui/query_async_fn.rs` |
