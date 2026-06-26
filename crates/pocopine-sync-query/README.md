# `pocopine-sync-query`

Query-centric, local-first data layer for Pocopine. Filtered subscriptions, predicate-routed optimistic mutations, reactive selectors, and typed writes — built on the `pocopine-sync` wire protocol, durable local store, and live-wakeup channel.

This is the framework's data-layer crate. One `Query` API spans the full range: simple single-shape entities (TodoMVC, settings, blog comments) **and** filtered multi-tenant shapes, where the same `Issues` table is observed under many filters at once (a workspace switcher, a status filter, an assignee filter). The `Source` trait plus the macro-emitted typed write builders cover every create/update/delete.

## When to reach for it

* You want server data mirrored into reactive local state with optimistic writes.
* You observe one entity type under more than one filtered shape and need each view to update independently.
* You want derived values (counts, projections, dashboards) that recompute only when their inputs change.

## Design

Three primitives, drawn from the consensus across Replicache/Zero, ElectricSQL, PowerSync, InstantDB, and TanStack Query:

1. **`Query<Row>`** — a declarative description of "what data do I want?", with its own canonical hash identity.
2. **`Mutator`** — a transactional function that produces row changes; the engine evaluates each change against every active query's predicate and routes it to the views that match.
3. **`QueryClient`** — a refcounted registry that owns one `QuerySubscription` per distinct `Query`, with its own state, queue, and lifecycle.

See [RFC 086](../../rfcs/rfc-086-sync-query.md) for the design rationale,
[RFC 087](../../rfcs/rfc-087-sync-query-driver.md) for the per-subscription
driver lifecycle, and [RFC 088](../../rfcs/rfc-088-sync-query-production-parity.md)
for the production-parity surface (typed writes, offline replay, live invalidation).

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

Selectors compose: `#[query] fn dashboard(...) { open_issue_count::observe(&client, ws).value() + … }`. An inner selector whose output is `PartialEq`-equal across reruns stops the cascade — the outer selector doesn't rerun. See [`docs/internal/sync-query-selector-mechanism.md`](../../docs/internal/sync-query-selector-mechanism.md) for the full design and [`docs/internal/sync-query-selector-implementation.md`](../../docs/internal/sync-query-selector-implementation.md) for the code map (layer diagram, data flows, file pointers).

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

Full picture, broker comparison, and topic-cardinality scaling notes in [`docs/tutorials/live-invalidation.md` §8 "Production Backends"](../../docs/tutorials/live-invalidation.md#8-production-backends).

## Docs

- [`docs/tutorials/issue-tracker-sync.md`](../../docs/tutorials/issue-tracker-sync.md) — end-to-end tutorial.
- [`docs/guides/data/sync-server.md`](../../docs/guides/data/sync-server.md) — `Source` /
  `SourceResource` / `MutationLog` contract reference.
- [`docs/guides/data/sync-client.md`](../../docs/guides/data/sync-client.md) — `QueryClient` /
  `Query<Row>` DSL / typed writes reference.

The `Source` trait + `SourceResource` adapter cover every server-side
read/write shape, with typed writes via the
`#[query_resource(draft = ...)]`-emitted
`Issue::create/update/delete(...).optimistic(...).push_typed(...)` API.
