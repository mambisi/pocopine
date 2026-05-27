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

Read [`docs/sync-query-design.md`](../../docs/sync-query-design.md) for the implementation spec.

## What ships in this branch

| File                          | Status      | Notes                                                  |
| ----------------------------- | ----------- | ------------------------------------------------------ |
| `src/lib.rs`                  | ✅ scaffold | Module declarations + re-exports                       |
| `src/query.rs`                | ✅ done     | `Query<Row>`, `QueryKey`, `OrderBy`, `Order` + builder |
| `src/params.rs`               | ✅ done     | Comparator wrappers (`InSet`, `Range`, `Contains`)     |
| `src/predicate.rs`            | ✅ done     | Sealed comparator-trait gate + field markers           |
| `src/mutator.rs`              | ✅ done     | `Mutator` trait + `RowChange` + `MutationOutcome`      |
| `src/state.rs`                | ✅ done     | `QueryState<Row>` (per-query reactive state)           |
| `src/client.rs` (TBD)         | ⏳ next     | `QueryClient` + `QuerySubscription` runtime            |
| `src/view.rs` (TBD)           | ⏳ next     | `QueryView<Row>` typed-row wrapper                     |
| `src/wire.rs` (TBD)           | ⏳ next     | `SyncOpenRequest` / `SyncPullRequest` builders         |
| `pocopine-sync-query-macros`  | ⏳ later    | `#[query_resource]` and `Query<Row>::matches` codegen  |
| Examples + cookbook           | ⏳ later    | `examples/issue-tracker` + `docs/sync-query-cookbook`  |

## Reference implementation

The branch `wip/sync-shape-subs-batch-4` is a **reference implementation** of shape subscriptions integrated into `pocopine-sync-crud`. It demonstrates what NOT to do — see the design doc's architectural-tension analysis. The wire protocol and macro DSL from that branch carry over to this crate; the client-side machinery does not.

## Quickstart (target API)

This is the API we're building toward. It does NOT work yet — the runtime crate isn't wired up.

```rust,ignore
use pocopine_sync_query::{Query, QueryClient, Order};

let client: &QueryClient = pocopine::query_client();

// Subscribe to a filtered view.
let handle = client.subscribe(
    Issues::query()
        .where_eq(field::workspace_id, w1)?
        .where_in(field::status, [Status::Open, Status::InProgress])?
        .where_contains(field::title, "auth")?
        .order_by(field::created_at, Order::Desc)
        .limit(50)
        .build()
);

// Render. `handle.rows()` is a live snapshot; pass to your view.
let visible: Vec<Issue> = handle.rows();

// Run a mutation. The engine routes its row changes to every
// subscription whose predicate matches the resulting rows.
let payload = CreateIssuePayload { id: IssueId::new(), workspace_id: w1, /*...*/ };
client.mutate::<create_issue::Mutator>(payload).await?;
```

## CRUD vs Query — which one?

| You want to                                  | Use                  |
| -------------------------------------------- | -------------------- |
| Simple list + create/edit/delete of entities | `pocopine-sync-crud` |
| Single shape per entity type                 | `pocopine-sync-crud` |
| Filtered views by workspace / channel / tag  | `pocopine-sync-query`|
| Multi-tenant SaaS with subscription dedup    | `pocopine-sync-query`|
| Pagination + ordering                        | `pocopine-sync-query`|
| Build a Linear-clone                         | `pocopine-sync-query`|

Apps can use both. CRUD for `Settings`, Query for `Issues`.

## Roadmap

See [`docs/sync-query-design.md` §13](../../docs/sync-query-design.md) for the PR sequence.
