# `pocopine-sync-query`

**Status: 🚧 Work in progress (Phase 3 scaffold).** API surface is being built out; the runtime is not yet wired up. Use [`pocopine-sync-crud`](../pocopine-sync-crud/) for production today.

Query-centric local-first data layer for Pocopine sync. Parallel to `pocopine-sync-crud`; recommended for filtered multi-tenant apps once it ships.

## Why a second crate?

`pocopine-sync-crud` is designed to be **safe and rigid**:

* One Resource = one logical entity type, one logical view.
* No subscription parameters; one stream serves one shape.
* Optimistic writes go to "the" Resource state.

That design wins for TodoMVC, blog comments, settings pages. It does **not** win for multi-tenant SaaS apps where the same `Issues` table is observed under many filtered shapes (workspace switcher, status filter, assignee filter).

Retrofitting shape subscriptions into CRUD violates its invariants. See [RFC 086](../../rfcs/rfc-086-sync-query.md) for the full reasoning. The short version: **CRUD stays simple; this crate is the home for shape-aware data flow.**

## Design

Three primitives, drawn from the consensus across Replicache/Zero, ElectricSQL, PowerSync, InstantDB, and TanStack Query:

1. **`Query<Row>`** — a declarative description of "what data do I want?", with its own canonical hash identity.
2. **`Mutator`** — a transactional function that produces row changes; the engine evaluates each change against every active query's predicate and routes it to the views that match.
3. **`QueryClient`** — a refcounted registry that owns one `QuerySubscription` per distinct `Query`, with its own state, queue, and lifecycle.

See RFC 086 (`rfcs/rfc-086-sync-query.md`) for the design rationale and
RFC 090 (`rfcs/rfc-090-merge-crud-into-query.md`) for the merge that
folded `pocopine-sync-crud` into this crate.

## What ships in this branch

| File                              | Status  | Notes                                                  |
| --------------------------------- | ------- | ------------------------------------------------------ |
| `src/lib.rs`                      | ✅      | Module declarations + re-exports                       |
| `src/query.rs`                    | ✅      | `Query<Row>`, `QueryKey`, `OrderBy`, `Order` + builder, `MatchFn<Row>` |
| `src/params.rs`                   | ✅      | Comparator wrappers (`InSet`, `Range`, `Contains`)     |
| `src/predicate.rs`                | ✅      | Sealed comparator-trait gate + `range_contains` / `contains_matches` runtime helpers |
| `src/mutator.rs`                  | ✅      | `Mutator` trait + `RowChange` + `MutationOutcome`      |
| `src/state.rs`                    | ✅      | `QueryState<Row>` (per-query reactive state)           |
| `src/client.rs`                   | ✅      | `QueryClient` + refcounted `QuerySubscription` registry + routing engine |
| `src/wire.rs`                     | ✅      | Build `SyncOpenRequest` / `SyncPullRequest` / `SyncPushRequest` from typed queries |
| `pocopine-sync-query-macros`      | ✅      | `#[query_resource]` + `#[query]` macros: query DSL builders, comparator-trait impls, predicate evaluator, selector memoization |
| `src/selector.rs`                 | ✅      | `#[query]` runtime: tracking stack, `SelectorEntry`, `SelectorView`, `AnyTrackable`, `PartialEq` diff-suppression |
| Background-task drivers (wasm)    | ⏳ next | spawn-aware `/open` + `/pull` flow; live wakeup; offline replay |
| `examples/issue-tracker`          | ⏳ later| Linear-clone demo                                      |
| `docs/sync-query-cookbook.md`     | ⏳ later| User-facing cookbook                                   |

## Reference implementation

The branch `wip/sync-shape-subs-batch-4` is a **reference implementation** of shape subscriptions integrated into `pocopine-sync-crud`. It demonstrates what NOT to do — see the design doc's architectural-tension analysis. The wire protocol and macro DSL from that branch carry over to this crate; the client-side machinery does not.

## Quickstart

Install the plugin once at app boot:

```rust,ignore
fn app(app: App) -> App {
    app.plugin(pocopine_sync_query::query_client_plugin())
}
```

Declare a queryable resource by annotating the row struct directly.
Every `#[query_param]` field automatically gets `.eq()` and
`.any_of()` at the call site (both apply to any T). `.range()` and
`.contains()` are auto-emitted via a type-name heuristic — numeric
primitives, `String`, and common `DateTime`-y types get range;
`String` / `str` / `Cow` get contains. Custom newtypes the
heuristic misses can opt in explicitly. The macro auto-detects
`Option<T>` and treats those fields as nullable.

```rust,ignore
use pocopine_sync_query::query_resource;

// `#[query_resource]` must come BEFORE `#[derive(...)]` so it strips
// the per-field `#[query_param]` annotations before serde sees them.
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    // `required` makes this a tenant gate: predicate fails when the
    // query has no workspace_id filter (cross-tenant safety).
    #[query_param(required)]  pub workspace_id: String,
    // Bare #[query_param] = optional filter. Heuristic adds range
    // and/or contains based on the field type.
    #[query_param]            pub assignee_id: Option<UserId>,  // eq only
    #[query_param]            pub status: Status,               // eq + any_of
    #[query_param]            pub title: String,                // eq + any_of + range + contains
    #[query_param]            pub created_at: DateTime,         // eq + any_of + range
    // Heuristic missed: WorkspaceId(String) newtype needs explicit
    // `(contains)` to enable substring search on it.
    // #[query_param(contains)] pub slug: Slug,
}
```

In a component, get the client and observe:

```rust,ignore
fn on_ready(&self, qc: Plugin<Rc<QueryClient>>) {
    // Build + subscribe. `view` is reactive; drop to unsubscribe.
    // `.eq` takes `impl Into<M::Value>` so `&str` literals work
    // directly where the field was declared as `String`. `.range`
    // accepts native Rust range syntax (`a..b`, `a..=b`, `a..`,
    // `..b`, `..=b`) — `..` is rejected (matches everything).
    use issues::field;
    let view = Issue::query()
        .eq(field::workspace_id, self.workspace_id.as_str())
        .any_of(field::status, [Status::Open, Status::InProgress])?
        .contains(field::title, "auth")?
        .range(field::created_at, last_week..now)
        .order_by("created_at", Order::Desc)
        .limit(50)
        .observe(&qc);

    // Read rows — synchronous, snapshot of canonical + pending overlay.
    for issue in view.rows() {
        render(issue);
    }

    // Wire pocopine reactivity: the view bumps `version()` on every
    // state change; an `on_update` listener notifies the component.
    let scope = pocopine_core::current_scope_id().unwrap();
    let _token = view.on_update(move || {
        pocopine_core::scope::notify(scope, "issues_view");
    });
}
```

Run a mutation — no manual routing in user code:

```rust,ignore
struct CreateIssue;
impl Mutator for CreateIssue {
    type Payload = CreateIssuePayload;
    type Row = Issue;
    const NAME: &'static str = "create_issue";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Issue>> {
        vec![RowChange::Upsert(payload.clone().into())]
    }
    fn apply_remote(ctx: &dyn MutatorRemoteContext, payload: Self::Payload)
        -> MutatorRemoteFuture<Issue>
    {
        Box::pin(async move { ctx.push::<CreateIssue>(payload).await })
    }
}

// In a component:
qc.mutate::<CreateIssue>(payload, &remote_ctx).await?;
```

The engine routes `apply_local`'s row changes through every observing query's predicate evaluator. W1's view sees a W1 mutation immediately; W2's view doesn't. No "active subscription" plumbing in user code.

## Derived state with `#[query]` selectors

For derived values over queries — counts, filtered projections, joins, dashboards — wrap the computation in a `#[query]` function. The framework caches the result by `(fn identity, args)`, tracks which subscriptions the body reads, reruns when any tracked subscription changes, and suppresses downstream notifications when the new output equals the cached one (`PartialEq`).

```rust,ignore
use pocopine_sync_query::{query, QueryClient};

#[query]
fn open_issue_count(client: QueryClient, ws: String) -> u32 {
    let view = client.observe(
        Issue::query()
            .eq(issues::field::workspace_id, ws.clone())
            .any_of(issues::field::status, [Status::Open])?
            .build()
    );
    view.rows().len() as u32
}

// In a component:
let view = open_issue_count::observe(&qc, "W1".to_string());
view.value();                // current count
let _tok = view.on_update(|| pocopine_core::scope::notify(scope, "open_count"));
```

The convention: if the first arg's type is `QueryClient`, it's the selector's client handle — not hashed, not in `observe()`'s public arg list (which always takes `&QueryClient` first). Every other arg must be `Hash + Clone + 'static`; the return type must be `PartialEq + Clone + 'static`.

Selectors compose: `#[query] fn dashboard(...) { open_issue_count::observe(&client, ws).value() + … }`. An inner selector whose output is `PartialEq`-equal across reruns stops the cascade — the outer selector doesn't rerun. See [`docs/sync-query-selector-mechanism.md`](../../docs/sync-query-selector-mechanism.md) for the full design and [`docs/sync-query-selector-implementation.md`](../../docs/sync-query-selector-implementation.md) for the code map (layer diagram, data flows, file pointers).

## Live invalidation and multi-node deployments

Live wakeups for `#[query]` selectors (and any `#[query_resource]`-backed view) flow through the `pocopine-events` event spine. If you deploy more than one server process — Kubernetes replicas, Render auto-scaling, blue/green, anything horizontally scaled — the in-process `MemoryEventBackend` will silently drop cross-node invalidations: a mutation on Node 2 never wakes a subscriber on Node 1.

**Production needs a shared backend.** Swap to `RedisEventBackend` (already in tree, behind the `redis` feature) — one line at app boot, no code changes downstream:

```rust,ignore
let backend = pocopine_events::build_event_backend(
    pocopine_events::EventBackendConfig::Redis(
        pocopine_events::RedisEventConfig::new("redis://prod-redis:6379", "myapp")?,
    ),
)?;
let hub = pocopine_live::LiveHub::new(backend)
    .allow_topic_prefixes(sync.live_topic_prefixes()); // RFC 088 §C
```

RFC 088 §C (per-`(stream, params_hash)` topics) is also broker-agnostic — it changes the *topic naming* scheme, not the broker contract. For ~1M users single-node Redis is fine; if you cross ~10M distinct partition hashes, shard with Redis Cluster or move to NATS / Kafka.

Full picture, broker comparison, and topic-cardinality scaling notes in [`docs/live.md` §8 "Production Backends"](../../docs/live.md#8-production-backends).

## Docs

- [`docs/sync.md`](../../docs/sync.md) — end-to-end tutorial.
- [`docs/sync-server.md`](../../docs/sync-server.md) — `Source` /
  `SourceResource` / `MutationLog` contract reference.
- [`docs/sync-client.md`](../../docs/sync-client.md) — `QueryClient` /
  `Query<Row>` DSL / typed writes reference.

RFC 090 (merged) folded `pocopine-sync-crud` into this crate. The
`Source` trait + `SourceResource` adapter cover both former CRUD and
former Query use cases, with typed writes via the
`#[query_resource(draft = ...)]`-emitted
`Issue::create/update/delete(...).optimistic(...).push_typed(...)` API.
