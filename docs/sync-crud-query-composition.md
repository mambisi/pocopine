# CRUD + Query: How They Compose

`pocopine-sync-crud` and `pocopine-sync-query` are not competing crates.
They solve different halves of the same problem:

```
                READS                              WRITES
                ─────                              ──────
        pocopine-sync-query              pocopine-sync-crud
        ───────────────────              ───────────────────
        Typed query DSL                  Typed Draft + Row split
        #[query_resource]                #[resource] / resource()
        sealed-trait field markers       CrudSource trait (CRUD lifecycle)
        predicate-routed mutations       CrudMutationLog (idempotency)
        per-query state compartments     CrudTransactionRunner (atomic tx)
        #[query] reactive selectors      CrudWriteResult / CrudConflict
        §C per-(stream, params) topics   CrudMigrateFn (schema migration)
```

**The canonical sync app uses both.** CRUD owns the write path
(typed mutations, idempotency, transaction-bound database commits).
Query owns the read path (filtered subscriptions, derived selectors,
precise live wakeups). They share one row type and one
`SyncStreamSource`.

## When to use only one

| You need | Pick |
|---|---|
| Just `list` / `get` / `create` / `save` / `remove` over a single resource, no filtered views | **CRUD only.** Query adds infrastructure you don't need. |
| Read-heavy reactive UI over rows you write via a different system (CDC, server-only writes, external DB) | **Query only.** No mutation lifecycle to manage. |
| Multi-tenant app with filtered views AND optimistic mutations | **Both.** This is the canonical shape. |

Tenancy + filtered views without §C scales like O(N) per push (every
subscriber wakes for every mutation). With §C wired through the CRUD
path, fanout collapses to O(matching subscribers).

## The bridge: `.params_of`

`#[query_resource]` emits a typed projector `row_to_params_typed`.
`CrudResource::params_of(closure)` consumes it. With one line, a CRUD
source gets Query-grade live wakeup precision.

```rust
use pocopine_sync_query::query_resource;
use pocopine_sync_crud::{resource, CrudSource};

#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Serialize, Deserialize, /* … */)]
pub struct Issue {
    pub id: String,
    #[query_param(required)]    // ← tenant gate; partitions §C topics
    pub workspace_id: String,
    #[query_param]
    pub title: String,
    #[query_param]
    pub status: Status,
    pub version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct IssueDraft { /* fields a client may send */ }

struct IssueSource { /* sqlx pool, etc. */ }
impl CrudSource for IssueSource { /* list/get/create/save/remove */ }

let issues = resource("issues", IssueSource::default())?
    .id(|r| r.id.clone())
    .version(|r| r.version)
    .mutation_log(MemoryCrudMutationLog::<Issue>::new())
    .params_of(issues::row_to_params_typed);     // ← the bridge
```

That's the entire integration. CRUD still handles writes the way it
always did; the only change is `partition_for_topic` (client) and
`row_to_params` (server) now hash on `workspace_id` and route wakeups
to per-`(stream, params_hash)` topics.

## Data flow

```
                              ┌─────────────────────────┐
                              │     Browser client      │
                              └─────────────────────────┘
                                         │
            ┌────────────────────────────┼────────────────────────────┐
            │                            │                            │
            ▼                            ▼                            ▼
       reads (Query)              writes (CRUD)              live wakeup (§C)
   Issue::query()                CrudMutationPayload      query:sync:stream:
     .eq(workspace_id, "W1")       ::create(id, draft)      issues:<W1-hash>
     .observe(&client)            POST /sync/v1/push       SSE subscription
            │                            │                            ▲
            │                            │                            │
            └──────── POST /sync/v1/pull ┘                            │
                                         │                            │
                                         ▼                            │
                              ┌─────────────────────────┐             │
                              │   Server (one source)   │             │
                              │                         │             │
                              │  CrudResource           │             │
                              │   ├─ id_of              │             │
                              │   ├─ version_of         │             │
                              │   ├─ mutation_log       │             │
                              │   ├─ .transactional(…)  │             │
                              │   └─ .params_of(typed)──┼──┐          │
                              │                         │  │          │
                              │  IssueSource (CrudSource)  │          │
                              │   ├─ list                  │          │
                              │   ├─ get                   │          │
                              │   ├─ create  ──┐           │          │
                              │   ├─ save     ─┼─→ SQL     │          │
                              │   └─ remove  ──┘           │          │
                              └────────────────────────────┘          │
                                         │                            │
                                         ▼                            │
                              row → row_to_params_typed(&Issue)       │
                                  → {workspace_id: "W1"}              │
                                  → FNV-1a → W1-hash                  │
                                  → publish to topic ─────────────────┘
```

The same row type flows through both halves. The same
`SyncStreamSource` instance serves both `/pull` (reads) and `/push`
(writes). `.params_of` is the only addition that wasn't already
possible.

## What lives where

**Read path (`pocopine-sync-query`):**

- `#[query_resource]` on the row struct — generates field markers,
  predicate evaluator, `row_to_params` + `row_to_params_typed`,
  `partition_for_topic`.
- `Query<Row>` + `QueryBuilder` — the typed filter DSL.
- `QueryClient` / `QueryView<Row>` — subscription registry; reactive
  view onto canonical + pending rows.
- `#[query]` selectors — memoized reactive computations over views.

**Write path (`pocopine-sync-crud`):**

- `CrudSource` trait — `list`, `get`, `create`, `save`, `remove`.
  App-defined; backed by SQLx / SQLite / Firestore / whatever.
- `CrudMutationPayload` — typed `Create<Draft>` / `Save<Draft>` /
  `Remove<Id>` envelope.
- `CrudMutationLog` — pluggable idempotency (`MemoryCrudMutationLog`
  for tests, `SqlxCrudMutationLog` for production).
- `CrudTransactionRunner` + `.transactional(runner, log)` — atomic
  write + idempotency-log insert in one DB transaction.
- `CrudWriteResult` / `CrudConflict` / `CrudRemoveResult` — typed
  optimistic-concurrency outcomes.
- `CrudMigrateFn` + `.migrate_with(f)` — stale-schema payload
  migration during push.

**Bridge (`pocopine-sync-crud` consuming `pocopine-sync-query` output):**

- `CrudResource::params_of(closure)` and the same on
  `TransactionalCrudResource`. Closure is typically
  `<resource>::row_to_params_typed` from `#[query_resource]`, but any
  `Fn(&Row) -> StreamParams` works.

## Why a typed sibling, not just `row_to_params(&Value)`

The macro already emits `row_to_params(&Value) -> SyncResult<StreamParams>`
for the `SyncStreamSource` trait boundary. `row_to_params_typed(&Row)`
is the sibling that CRUD consumes:

| Form | Signature | Cost | When to use |
|---|---|---|---|
| Value | `fn(&Value) -> SyncResult<StreamParams>` | Direct JSON projection — `row.get(name).clone()` per field | Hand-written `SyncStreamSource` impls that already have `&Value` |
| Typed | `fn(&Row) -> StreamParams` | `serde_json::to_value` per required field only | CRUD adapter, which already has `&Row` |

Both produce byte-identical `StreamParams` for the same row. The
macro has a test (`row_to_params_typed_agrees_with_value_form`) that
pins this — a divergence would silently miss every per-params wakeup.

## When to choose Query without CRUD

The audit found one significant gap in Query: there's no server-side
mutation helper. Users either lean on CRUD (the recommended path) or
hand-write `SyncStreamSource::push`. If you want filtered subscriptions
but writes flow through a different system (CDC pipeline, server-only
RPC, external database that emits change events) — Query alone is the
right choice, and you wire `SyncStreamSource::row_to_params` directly
to `<resource>::row_to_params` (the Value form).

## Migration: starting from CRUD-only

You already have a `CrudResource` and want to add filtered views. Three
steps:

1. Add `#[query_resource(name = "<stream>", schema_version = N)]` to
   the row struct (above `#[derive(...)]`). Mark the tenant-gate field
   (e.g. `workspace_id`) with `#[query_param(required)]`. Mark
   filterable fields with `#[query_param]`.
2. Add `.params_of(<stream>::row_to_params_typed)` to your CRUD builder
   chain.
3. On the host side, switch `LiveHub` from
   `.allow_topics(sync.live_topics())` to
   `.allow_topic_prefixes(sync.live_topic_prefixes())` so per-params
   topics are authorized.

That's it. Existing CRUD client code keeps working. New code can use
`Issue::query().eq(field::workspace_id, "W1").observe(&client)` for
filtered subscriptions.

## Migration: starting from Query-only

Less common, but: you have a hand-written `SyncStreamSource` with
filtered reads and want typed-Draft writes + idempotency. Steps:

1. Implement `CrudSource` for your existing storage backend (most of
   the work is already done — `pull` → `list`, `push` → `create` /
   `save` / `remove`).
2. Replace your hand-written `SyncStreamSource` registration with
   `resource(name, source).id(…).mutation_log(…).params_of(…)`.
3. Existing `Query<Row>` subscriptions keep working byte-for-byte —
   the macro-emitted `partition_for_topic` and `row_to_params` agree
   regardless of who's calling them.

## Related

- [`sync-crud-query-tutorial.md`](./sync-crud-query-tutorial.md) —
  full step-by-step build of a SQLite-backed multi-workspace issue
  tracker using this pattern
- [`sync.md`](./sync.md) — sync protocol, stream registration, live
  wakeup, server plugin
- [`sync-crud.md`](./sync-crud.md) — CRUD design, `CrudSource` API,
  mutation log, transaction binding
- [`sync-query-design.md`](./sync-query-design.md) — Query
  implementation design, predicate routing, `#[query_resource]` codegen
- [`sync-query-selector-mechanism.md`](./sync-query-selector-mechanism.md)
  — `#[query]` selectors and memoization
