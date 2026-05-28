# `#[query]` selector mechanism

How the read-tracking, invalidation, and re-run machinery for `#[query]`-decorated functions actually works in `pocopine-sync-query`. Companion to [RFC 088 §B](../rfcs/rfc-088-sync-query-production-parity.md).

This is an explainer, not a spec. The RFC is the contract.

## TL;DR

A `#[query]` function is just Rust code. The framework instruments its **reads** — every `QueryView::rows()` call inside its body records the underlying `QuerySubscription` as tracked. When ANY tracked subscription's state changes, the framework re-runs the function. The new output is diffed against the cached one via `PartialEq`; the selector's own observers fire only when the output actually differs.

There is no virtual-DOM-style document tree. There is no per-row dependency graph. The tracking unit is the **subscription handle** — one per `Resource::query()` you call inside the function body.

## The four moving parts

### 1. Read tracking via a thread-local stack

```rust
thread_local! {
    static SELECTOR_STACK: RefCell<Vec<TrackingFrame>> = RefCell::new(Vec::new());
}

struct TrackingFrame {
    selector_id: SelectorId,
    tracked: Vec<Rc<dyn AnyTrackable>>,   // subscriptions OR other selectors
}

// QueryView::rows() (modified) calls this:
pub(crate) fn record_read(trackable: Rc<dyn AnyTrackable>) {
    SELECTOR_STACK.with(|stack| {
        if let Some(frame) = stack.borrow_mut().last_mut() {
            frame.tracked.push(trackable);
        }
    });
}
```

When no selector is running (the stack is empty), `record_read` is a no-op — ordinary component reads aren't tracked here. When a selector IS running, every `view.rows()` call inside its body pushes the corresponding subscription's `Rc` into the top frame.

### 2. Caching by `(SelectorId, ArgsHash)`

```rust
struct SelectorEntry<T> {
    selector_id: SelectorId,                                // FNV-1a(module_path + fn_name)
    args_hash: u64,
    cached_output: RefCell<Option<T>>,
    tracked: RefCell<Vec<Rc<dyn AnyTrackable>>>,
    listener_tokens: RefCell<Vec<UpdateToken>>,             // tokens we hold on each upstream
    listeners: RefCell<Vec<(u64, Rc<dyn Fn()>)>>,           // selector's own observers
    refcount: Cell<usize>,
}

struct QueryClientInner {
    // … existing subscription registry
    selectors: RefCell<HashMap<(SelectorId, u64), Rc<dyn AnySelector>>>,
}
```

**Cache hit:** `observe(client, args)` hashes args, looks up the entry, bumps refcount, returns a fresh `SelectorView<T>` wrapping the same entry. The output is reused; upstreams stay alive through the entry's `Rc`s.

**Cache miss:** install a tracking frame, run the function, capture the tracked set, wire listeners, store the entry.

### 3. Invalidation rides on `notify_listeners`

`QuerySubscription::notify_listeners` (existing today, see PR #146/#148) fires after every state mutation:

* `route_optimistic_changes` — after pushing a pending overlay.
* `route_canonical_changes` — after a server `/pull` reconciles canonical state.
* `route_canonical_pull` — driver-initiated canonical reconcile (PR #148).
* `dequeue_pending` — after a rollback removes overlays.

The selector's tracked-read callback is just another registered listener. When ANY tracked subscription notifies, the selector re-runs.

```text
              ┌──────────────────────────────────────────────┐
              │  upstream Project::query() subscription      │
              │  state mutated (canonical upsert, optimistic │
              │  push, /pull reconcile, rollback, …)         │
              └────────────────────┬─────────────────────────┘
                                   │
                       calls notify_listeners()
                                   │
                                   ▼
              ┌──────────────────────────────────────────────┐
              │  for each (id, callback) in listeners:       │
              │      callback()                              │
              └────────────────────┬─────────────────────────┘
                                   │
                       ──── one of the callbacks is ────
                                   │
                                   ▼
              ┌──────────────────────────────────────────────┐
              │  selector's tracked-read callback:           │
              │      let weak = Rc::downgrade(&entry);       │
              │      move || {                               │
              │          if let Some(e) = weak.upgrade() {   │
              │              e.rerun();                      │
              │          }                                   │
              │      }                                       │
              └────────────────────┬─────────────────────────┘
                                   │
                                   ▼
              ┌──────────────────────────────────────────────┐
              │  SelectorEntry::rerun:                       │
              │                                              │
              │  1. Drop old listener_tokens                 │
              │  2. Push frame; user_fn(args); pop frame.    │
              │  3. Compute set-diff: new ∩ old, new − old,  │
              │     old − new.                               │
              │  4. Drop tracked Rcs in (old − new);         │
              │     re-subscribe + push tokens for           │
              │     (new − old); keep (new ∩ old) intact.    │
              │  5. cached_output != new ? update + fire     │
              │     selector's own listeners.                │
              └──────────────────────────────────────────────┘
```

The whole mechanism is a wrapper around the existing `notify_listeners` plumbing. No new event bus, no new state machine, no new wire protocol.

### 4. Diff suppression on the function's return value

```rust
fn rerun(self: &Rc<Self>) {
    // Steps 1-4 from the diagram …
    let new = user_fn_callable(args);

    let mut cache = self.cached_output.borrow_mut();
    if cache.as_ref() == Some(&new) {
        // Same output. Cache stays; selector observers NOT fired.
        return;
    }
    *cache = Some(new.clone());
    drop(cache);

    let listeners = self.listeners.borrow().iter().cloned().collect::<Vec<_>>();
    for (_, callback) in listeners {
        callback();
    }
}
```

The framework never introspects the output's structure. It compares the whole `T` via `PartialEq`. If structurally equal → no user-facing notification. If different → cache update + listeners fire.

`T: PartialEq + Clone` is the trait-bound requirement. Opt out with `#[query(no_diff)]` for outputs that can't be `PartialEq` (closures, sockets, etc.) — every re-run then fires.

## Why subscription handles and not a document tree

The alternative model — track every row the function read, build a key-set dependency graph, re-run only when affected keys change — is roughly **Replicache/Zero's key-set tracking**:

```js
rep.subscribe(async (tx) => {
    const projects = await tx.scan({prefix: "project/"}).toArray();
    return projects.filter(p => p.workspace_id === w1);
}, render);
```

Their framework instruments `tx.scan` / `tx.get` to record the exact keys (`project/123`, `project/456`, …) the function touched. On mutation: did the change affect a tracked key? If no, skip the rerun entirely.

Trade-offs vs subscription-level tracking:

| | Subscription-level (ours) | Row-key-level (Replicache) |
|---|---|---|
| Re-run when an unrelated row in the same subscription changes | YES (then diff suppresses) | NO |
| Implementation complexity | Low — one `Rc` per query | High — set of keys per query + key-change tracking |
| Function model | `fn(args) -> T` | `fn(tx) -> T` (every access via tx) |
| Composition with other Rust crates | Trivial — it's just Rust | Awkward — tx threads everywhere |
| User code style | Direct `Resource::query()` calls | `tx.scan(...)` / `tx.get(...)` ceremony |
| Best for | Constrained query DSL (ours) | KV store with arbitrary access patterns |

Why we picked subscription-level:

1. **Our DSL already constrains access.** Users can't randomly `tx.get("project/123")`; they go through `Project::query().eq(...).rows()`. The query IS the dependency. Tracking the query is sufficient.
2. **The diff layer covers most of the precision gap.** A selector that reads 1000 rows and renders 5: an unrelated row change → rerun → same output → diff suppresses → user-facing observers don't fire. We pay CPU on the rerun, not UI re-render.
3. **Composition stays Rust-native.** No `tx` parameter threading through every helper. Selectors can call into ordinary Rust crates that use `QueryView`s normally.

The cost: selectors with cheap diffs and expensive function bodies will re-run more than strictly necessary. If profiling shows that's real, row-key tracking can layer on top in a future RFC — the user-facing API doesn't change.

## Concrete trace through the example function

```rust
#[query]
fn projects_with_open_issues(ws_id: String) -> Vec<(Project, Vec<Issue>)> {
    Project::query()
        .eq(field::workspace_id, &ws_id).rows()
        .into_iter()
        .map(|p| {
            let issues = Issue::query()
                .eq(field::project_id, &p.id)
                .any_of(field::status, [Status::Open]).rows();
            (p, issues)
        })
        .collect()
}

// First call:
let view = projects_with_open_issues::observe(&client, "W1".to_string());
```

Inside that first `observe`:

```text
1. Push TrackingFrame { selector_id=PWOI, tracked=[] } onto SELECTOR_STACK.

2. user_fn("W1") runs:
   - Project::query().eq(workspace_id="W1").rows() called:
       client.observe(query) returns Rc<Subscription_ProjectsInW1>.
       rows() calls record_read(Subscription_ProjectsInW1).
       SELECTOR_STACK.top().tracked.push(Rc<Subscription_ProjectsInW1>).
       Returns Vec<Project> with, say, 4 projects.

   - .into_iter().map(|p| { ... }) iterates 4 projects:
       Project p1: Issue::query().eq(project_id="p1").any_of(status=[Open]).rows():
           → record_read(Rc<Subscription_IssuesInP1Open>) once.
       Project p2: Issue::query().eq(project_id="p2").any_of(status=[Open]).rows():
           → record_read(Rc<Subscription_IssuesInP2Open>) once.
       Project p3, p4: same pattern.

   - .collect() produces Vec<(Project, Vec<Issue>)>.

3. Pop the TrackingFrame, capture tracked = [
     Rc<Subscription_ProjectsInW1>,
     Rc<Subscription_IssuesInP1Open>,
     Rc<Subscription_IssuesInP2Open>,
     Rc<Subscription_IssuesInP3Open>,
     Rc<Subscription_IssuesInP4Open>,
   ].

4. Cache the output. Wire on_update on each of the 5 subscriptions to a
   callback that calls selector.rerun().

5. Return SelectorView<Vec<(Project, Vec<Issue>)>> with refcount=1.
```

## Adding a row: three scenarios

The interesting question: when you add a row, how does the selector pick it up? It depends on whether the new row lands in an already-tracked subscription.

### Case 1 — New row matches a tracked subscription

```rust
client.mutate::<CreateIssue>(Issue {
    id: "i77",
    project_id: "p1",                  // ← p1 was in the iterated set
    status: Status::Open,
    title: "rate-limit auth",
    workspace_id: "W1",
});
```

What happens:

```text
mutate() ─► apply_local() returns [RowChange::Upsert(new_issue)]
              │
              ▼
        route_optimistic_changes::<Issue>("issues", mutation_id, &change)
              │
              │  walks every Subscription on the "issues" stream:
              │  ├─ Subscription_IssuesInP1Open: predicate (project_id=p1 AND status=Open)
              │  │     evaluates against new_issue → MATCHES
              │  │     push PendingOverlay { optimistic_row: Some(new_issue), … }
              │  │     touched = true
              │  ├─ Subscription_IssuesInP2Open: project_id=p2 ≠ p1 → SKIP
              │  ├─ Subscription_IssuesInP3Open: same, SKIP
              │  └─ Subscription_IssuesInP4Open: same, SKIP
              │
              ▼
        Subscription_IssuesInP1Open.notify_listeners()
              │
              │  one of its listeners is the selector's rerun callback
              ▼
        selector.rerun()
              │
              ├─ drop old listener_tokens
              ├─ push TrackingFrame
              ├─ run user_fn("W1"):
              │     Project::query()...rows()       returns [p1, p2, p3, p4]
              │                                     records Subscription_ProjectsInW1
              │     for p1: Issue::query()...rows()
              │        ↑ returns canonical (4 prior issues) + pending overlay (the new one)
              │          = 5 issues total       records Subscription_IssuesInP1Open
              │     for p2: ... records Subscription_IssuesInP2Open (same as before)
              │     for p3, p4: same
              │
              ├─ pop frame, compare tracked set: identical to before, no churn
              │
              ├─ new output =
              │       [(p1, [4 old + 1 new]), (p2, [...]), (p3, [...]), (p4, [...])]
              │
              ├─ cached output =
              │       [(p1, [4 old]),         (p2, [...]), (p3, [...]), (p4, [...])]
              │
              ├─ PartialEq: differ at projects[0].issues.len()
              │
              ▼
        update cache, fire selector's own listeners ─► UI re-renders
```

Two details from this trace:

* **The merged read happens inside `view.rows()`.** When the selector re-reads `Issue::query()...rows()`, the `rows()` impl merges canonical rows with pending overlays (PR #146's BTreeMap merge), so the optimistic Upsert overlay shows up in the result. No special "pending vs canonical" logic in the selector body — it just reads `.rows()` and gets the current view.
* **Cross-subscription mutations don't reach the selector.** If the new issue had `project_id: "p99"` and no matching subscription existed, `route_optimistic_changes` would walk all 4 tracked Issue subscriptions, none would match, and no `notify_listeners` would fire. The selector stays silent. That's the routing engine's predicate gate eliminating cross-tenant / cross-shape noise BEFORE it reaches the listener chain.

### Case 2 — New row lands in a NOT-yet-tracked subscription

Suppose Project `p5` exists in W1 but the selector hasn't iterated it (race: `p5` was created moments ago, between selector observations). The selector's tracked set: only `Subscription_IssuesIn{P1,P2,P3,P4}Open`.

You call `mutate::<CreateIssue>(Issue { project_id: "p5", … })`:

```text
route_optimistic_changes::<Issue>("issues", …):
    walks subscriptions on "issues" stream:
    ├─ IssuesInP1Open  (predicate p1) → SKIP
    ├─ IssuesInP2Open  (predicate p2) → SKIP
    ├─ IssuesInP3Open  (predicate p3) → SKIP
    └─ IssuesInP4Open  (predicate p4) → SKIP

  NO subscription matches. notify_listeners does NOT fire on any "issues"
  subscription. The selector does NOT rerun.
```

Is that a bug? No — because the lifecycle goes through the Projects subscription first:

```text
Step A: someone (or you, or a /pull) creates Project p5.
        Subscription_ProjectsInW1 matches predicate (workspace_id="W1").
        PendingOverlay pushed for p5. notify_listeners fires.

Step B: selector.rerun() — Subscription_ProjectsInW1 woke us up.
        user_fn now iterates [p1, p2, p3, p4, p5] (the merged view includes p5).
        For p5: Issue::query().eq(project_id, "p5").any_of(status, [Open])
                ↑ client.observe() creates a NEW Subscription_IssuesInP5Open
                  (didn't exist before; refcount=1; driver task spawned).
                .rows() returns empty initially (driver's /pull hasn't completed).
        Selector tracks the new subscription. Output:
        [(p1, [...]), (p2, [...]), (p3, [...]), (p4, [...]), (p5, [])]
        Differs from cache → fire selector observers.

Step C: driver for Subscription_IssuesInP5Open completes /open + /pull.
        Returns the existing p5 issues including the one you created.
        route_canonical_pull upserts them; notify_listeners fires.

Step D: selector.rerun() — IssuesInP5Open woke us up.
        user_fn iterates [p1..p5] again. For p5, .rows() now returns the issue.
        Output: [..., (p5, [the issue])]
        Diff differs → fire observers.
```

Between Step A and Step D there's a brief window where `p5` appears with `issues: []`. That's normal loading behavior — same as observing any new query for the first time. The user sees the project appear immediately; the issues populate when the driver's `/pull` completes.

The order matters: Projects has to update FIRST so the selector discovers the new Issue subscription. The routing engine guarantees this because Issue subscriptions can only be discovered THROUGH a Project iteration; the selector body's control flow enforces the order.

### Case 3 — Removing rows works the same way

For completeness:

```rust
client.mutate::<DeleteIssue>("i77".into());
// triggers RowChange::Delete(key="i77")
```

Same routing logic:

* `route_optimistic_changes` walks subscriptions; the row's key matches `Subscription_IssuesInP1Open`'s canonical or pending → push a Delete-shaped overlay (with `evicted_key`) → `notify_listeners`.
* Selector reruns; `Issue::query()...rows()` now excludes the deleted row (because `view.rows()` honors `evicted_key`, per PR #148 fix #2/#3).
* Output diff differs → observers fire.

Same flow for **predicate-departure** (an issue moving from `Open` to `Closed` while the selector only wants `Open`): the routing engine pushes a Delete-shaped overlay onto `Subscription_IssuesInP1Open` and the selector reruns to an output without that issue.

## Summary table

| New row scenario | Trigger path | Selector reruns? |
|---|---|---|
| Matches an already-tracked subscription | `notify_listeners` on that subscription | YES, once |
| Matches a NOT-yet-tracked subscription | Indirect: parent subscription updates → selector reruns + discovers new subscription → driver fetches → new subscription's notify fires the next rerun | YES, two reruns (intermediate empty state) |
| Doesn't match any tracked subscription | Routing engine drops it | NO (correctly) |
| Optimistic apply | Overlay pushed during `route_optimistic_changes`, same chain | YES |
| Server `/pull` reconcile | Overlay applied during `route_canonical_pull`, same chain | YES |
| Rollback (server rejects an optimistic mutation) | `dequeue_pending` calls `notify_listeners` after rolling back | YES |

## Composition: nested selectors

The `record_read` arg is `Rc<dyn AnyTrackable>` — a unified trait that BOTH `QuerySubscription` AND `SelectorEntry` implement. So selectors compose:

```rust
#[query]
fn dashboard(ws_id: String) -> DashboardView {
    let projects = projects_with_open_issues::observe(&CLIENT, ws_id.clone()).value();
    let comments = recent_comments::observe(&CLIENT, ws_id).value();
    DashboardView { projects, comments }
}
```

When `dashboard` runs:

1. Both inner selectors' `.value()` calls register THEIR entries with `dashboard`'s tracking frame.
2. When `projects_with_open_issues` reruns and its output differs, IT fires its own listeners. One of those listeners is `dashboard`'s rerun callback.
3. Same mechanism, one level up.

A diff-suppressed inner selector stops the cascade — `projects_with_open_issues` reruns but its output equals cached → it does NOT fire its listeners → `dashboard` does NOT rerun.

The composition is fully recursive. Selectors and subscriptions are interchangeable from the tracker's point of view.

## Lifecycle (refcount chain)

```text
let view = dashboard::observe(&client, "W1");
└─ dashboard entry: refcount = 1
   ├─ tracked Rcs (from dashboard's first run):
   │  ├─ projects_with_open_issues entry: refcount = 1
   │  │  └─ tracked Rcs:
   │  │     ├─ Subscription_ProjectsInW1: refcount = 1
   │  │     ├─ Subscription_IssuesInP1Open: refcount = 1
   │  │     └─ Subscription_IssuesInP2Open: refcount = 1
   │  └─ recent_comments entry: refcount = 1
   │     └─ Subscription_CommentsInW1: refcount = 1
   └─ listeners: [] (no observers attached yet)

drop(view);
└─ dashboard entry: refcount = 0 → removed from registry
   ├─ listener_tokens dropped → upstream selectors unsubscribed
   └─ tracked Rcs dropped:
      ├─ projects_with_open_issues entry: refcount = 0 → removed
      │  └─ its tracked Rcs dropped:
      │     ├─ Subscription_ProjectsInW1: refcount = 0 → driver epoch bumped, task exits
      │     ├─ Subscription_IssuesInP1Open: refcount = 0 → driver exits
      │     └─ Subscription_IssuesInP2Open: refcount = 0 → driver exits
      └─ recent_comments entry: refcount = 0 → removed
         └─ Subscription_CommentsInW1: refcount = 0 → driver exits
```

Drop is fully recursive via Rust's ownership. No explicit cleanup. The `Drop` impl on `SelectorEntry` (decrements its own refcount + removes from the registry on zero) is the only custom Drop logic — everything downstream cascades naturally.

## When the document-tree model would help

Two scenarios where our subscription-level model re-runs more than strictly necessary:

1. **Wide subscription, narrow output.** Selector reads `Project::query().eq(workspace_id, W1).rows()` (returns 1000 projects), then filters in-Rust to 5 visible ones. ANY project update in W1 fires a rerun. Replicache-style key tracking would only fire when one of those 5 specific projects changes.

   *Workaround today*: decompose into nested selectors — each row-level computation becomes its own selector, and the diff layer suppresses propagation through the chain.

2. **Mutation that touches many rows.** A bulk operation that updates 100 rows in one subscription triggers 1 `notify_listeners` call per batch (one per `route_*` invocation per subscription), so 1 rerun. But for 100 separate batches (rare), that'd be 100 reruns.

   *Workaround today*: batch the source mutations, or accept the rerun cost (cheap if the function is cheap; diff layer suppresses spurious renders).

If profiling shows these are real bottlenecks, row-key tracking can layer on top of the subscription tracking without changing the user-facing API. The selector function body stays identical; only `record_read` becomes richer.

## Open questions (settled before PR-B implementation)

* **Batched re-run coalescing — ship as v1 or accept-then-optimize?** Recommendation: accept-then-optimize. Document the property; add microtask coalescing in a follow-up if needed.
* **`refetch()` API surface — `client.refetch(&query)` only, or also `view.refetch_all()`?** Recommendation: `client.refetch(&query)` only. Selector-level refetch has cascade complexity.
* **Strict `PartialEq` vs hash-based shallow equality?** Recommendation: `PartialEq` for v1 (simple, correct); add `#[query(hash_eq)]` opt-in later if profiling shows comparison cost.
* **`Result<T, E>` selectors?** Recommendation: supported transparently — just require `Result<T, E>: PartialEq`. The macro doesn't special-case it.

## Related

* [`sync-query-selector-implementation.md`](sync-query-selector-implementation.md) — implementation map: where each piece lives, data flows, areas to scrutinize when reviewing or modifying the code.
* [RFC 086 — `pocopine-sync-query`](../rfcs/rfc-086-sync-query.md) — the routing engine + DSL.
* [RFC 087 — driver lifecycle](../rfcs/rfc-087-sync-query-driver.md) — what `notify_listeners` fires on.
* [RFC 088 — production parity (§B)](../rfcs/rfc-088-sync-query-production-parity.md) — the formal spec for `#[query]`.
