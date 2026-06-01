# RFC 090 — Merge `pocopine-sync-crud` into `pocopine-sync-query`

* **Status:** Implemented
* **Author:** sync framework working group
* **Tracking branch:** `feat/rfc-090-merge-crud-into-query`
* **Supersedes:** the dual-crate boundary documented in
  [`sync-crud.md`](../docs/sync-crud.md) and
  [`sync-crud-query-composition.md`](../docs/sync-crud-query-composition.md)
* **Related:** [RFC 086 (pocopine-sync-query)](./rfc-086-sync-query.md),
  [RFC 087 (driver lifecycle)](./rfc-087-sync-query-driver.md),
  [RFC 088 (production parity)](./rfc-088-sync-query-production-parity.md)

## Summary

Delete `pocopine-sync-crud` and `pocopine-sync-crud-macros`. Absorb
their value-carrying primitives — the typed `Draft`/`Row` lifecycle,
the pluggable mutation log, the transaction-binding contract, the
typed conflict outcomes, the schema-migration hook — into
`pocopine-sync-query`. The result is one crate with one mental model:

- **Reads** through `Issue::query().eq(...)...observe(&client)`.
- **Writes** through `Issue::create(draft).push(&client)` (generated).
- **Server source** implements one trait (`Source`) that owns both
  paths.

The merge unlocks one thing the dual-crate split couldn't deliver:
**Query-aware server filtering.** Today `CrudSource::list(ctx, limit)`
fetches the full snapshot; the macro's predicate evaluator filters
client-side. After the merge, `Source::list(ctx, &Query<Row>)` hands
the typed query to storage so SQLx/SQLite/Firestore can push a
`WHERE workspace_id = ? AND status IN (?)` clause down to the
database. At 1M users that's the difference between a working
multi-tenant app and a non-working one.

This RFC is the decision artifact for "should we do this." It does
not specify the line-by-line migration; that lives in tracking PRs
under the phased plan in §6.

## Motivation

After PR #154 (the `.params_of` bridge) the dual-crate compose story
worked, but three sharp edges remained:

1. **Client-side state duplication.** A component using both CRUD
   (writes) and Query (reads) carries `CollectionState<Issue>` AND
   `QueryView<Issue>`. The two pull independently from the same
   canonical store, the SyncClient routes writes through
   `CollectionState`, the QueryClient routes reads through
   `QueryView`. "Two handles, one truth" — but the boilerplate is
   real and the lifecycle ordering between the two is subtle.

2. **No server-side query translation.** `CrudSource::list` is
   bandwidth-blind: it returns every row the caller could see,
   client predicates filter the rest. For a single-resource CRUD
   app that's fine. For a multi-tenant Query app subscribed to
   `workspace_id = "W1"`, the server still fetches and ships every
   row in every workspace. The macro emits a typed `Query<Row>` the
   server could use — but `CrudSource::list` doesn't accept it.

3. **Two crates is a paper boundary.** The audit established that
   the only thing CRUD owns standalone is the write infrastructure.
   Everything else either belongs to Query (predicate routing,
   `partition_for_topic`, selectors) or is duplicated (typed row,
   schema version, request context). And the `#[resource]` proc-
   macro that would justify CRUD as a complete user-facing layer
   isn't shipped — there's no production CRUD-using code to break.

The audit recommended "keep CRUD as the write layer, bridge with
`.params_of`." This RFC reverses that. The new finding: **server-
side query translation is impossible without a unified Source
trait, and that unification is worth more than the dual-crate
abstraction.**

## Goals

* **G1.** One trait — `Source` — replaces `CrudSource`. Methods:
  `list(ctx, query: &Query<Row>)`, `get(ctx, id)`,
  `create(ctx, id, draft)`, `save(ctx, id, draft, base_version)`,
  `remove(ctx, id, base_version)`. The query argument is
  optional in shape (queries with no filter clauses behave like
  CRUD's `list(limit)`) so existing impls port mechanically.
* **G2.** Client-side `QueryClient::push(mutation, optimistic)`
  routes the optimistic row through the predicate matcher into
  every relevant view's pending overlay — the same machinery that
  routes canonical changes, applied earlier.
* **G3.** `#[query_resource]` emits typed write methods:
  `Issue::create(draft) -> CreateBuilder<Issue>`,
  `Issue::save(id, draft) -> SaveBuilder<Issue>`,
  `Issue::remove(id) -> RemoveBuilder<Issue>`. Each carries the
  `CrudMutationPayload` wire envelope shape internally; the user
  never sees it.
* **G4.** Mutation queue + idempotency log + transaction binding
  move from `pocopine-sync-crud` into `pocopine-sync-query` with
  the same public shapes (`MutationLog<Row>`,
  `MemoryMutationLog<Row>`, `TransactionRunner`,
  `.transactional(runner, log)`). The traits get renamed (drop
  the `Crud` prefix) but semantics are preserved.
* **G5.** `CollectionState<T>` and `SyncClient::collection(...).push`
  stay shipped as back-compat shims that delegate into the new
  `QueryClient` machinery. They're marked `#[deprecated]` with a
  one-version sunset.
* **G6.** Server-side `Source::list` receives `&Query<Row>` and
  may translate filters to storage queries. Default impl ignores
  the query (CRUD-like behavior) so existing impls compile.

## Non-goals

* **Cross-resource transactions.** A single `Source::save` still
  runs in one source-defined transaction. Multi-resource workflows
  compose via separate `client.push` calls.
* **CRDT / conflict-free merge.** Same posture as RFC 086 — server
  is authoritative, conflicts surface via typed `WriteResult`
  variants.
* **Generic SQL DSL.** The query argument is `Query<Row>`, not a
  storage-agnostic SQL builder. SQLx impls hand-roll the WHERE
  clause from the query's fields; that's the contract.
* **Auto-generated `Source` impls.** Apps still write `Source` by
  hand. The macro doesn't generate SQL.
* **Renaming `pocopine-sync-query`.** The crate name stays.
  "Query" is the user-facing entry point; the new write methods
  are extensions, not a rebrand.

## Section A — The unified `Source` trait

### A.1 Trait shape

```rust
#[async_trait]
pub trait Source: Send + Sync + 'static {
    type Id: ResourceId;
    type Row: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;
    type Draft: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Return rows matching the query. Default impl ignores the
    /// query and returns up to `query.limit()` rows — implementations
    /// SHOULD override for filtered storage queries when feasible.
    async fn list(
        &self,
        ctx: RequestContext,
        query: &Query<Self::Row>,
    ) -> SyncResult<Vec<Self::Row>>;

    async fn get(
        &self,
        ctx: RequestContext,
        id: Self::Id,
    ) -> SyncResult<Option<Self::Row>>;

    async fn create(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SyncResult<Self::Row>;

    async fn save(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        base_version: Option<RowVersion>,
    ) -> SyncResult<WriteResult<Self::Row>>;

    async fn remove(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        base_version: Option<RowVersion>,
    ) -> SyncResult<RemoveResult<Self::Row>>;
}
```

### A.2 Query-aware list

The signature change `list(limit) → list(&Query<Row>)` is the
unlock. SQLx impls can introspect the query:

```rust
async fn list(&self, _ctx: RequestContext, query: &Query<Issue>) -> SyncResult<Vec<Issue>> {
    let mut sql = "SELECT * FROM issues WHERE 1=1".to_string();
    let mut binds: Vec<Value> = Vec::new();

    // Each `#[query_param(required)]` field MUST be present in the
    // query — the macro-generated `matches()` predicate enforces
    // it, and the source can rely on it.
    if let Some(ws) = query.params().get("workspace_id") {
        sql.push_str(" AND workspace_id = ?");
        binds.push(ws.clone());
    }

    if let Some(statuses) = query.params().get("status").and_then(in_set) {
        sql.push_str(&format!(" AND status IN ({})", placeholders(statuses.len())));
        binds.extend(statuses.iter().cloned());
    }

    if let Some(limit) = query.limit() {
        sql.push_str(&format!(" LIMIT {limit}"));
    }

    // Bind + execute.
    self.exec(sql, binds).await
}
```

The query argument is a structural map — `query.params()` is
`&BTreeMap<String, Value>` where the value shapes match the
macro's comparator wrappers (`{"in": [...]}`, `{"from", "to"}`,
etc.). Sources can pattern-match what they support and fall back
to in-memory filtering for unsupported predicates.

### A.3 Default impl preserves CRUD semantics

For source authors who don't want to translate filters to storage
(prototypes, low-tenant apps, sources whose backend lacks
filtering primitives):

```rust
async fn list(&self, ctx: RequestContext, query: &Query<Self::Row>) -> SyncResult<Vec<Self::Row>> {
    // Default: ignore the query, return up to `limit` rows.
    let limit = query.limit().unwrap_or(DEFAULT_SNAPSHOT_LIMIT) as usize;
    self.list_unfiltered(ctx, limit).await
}

async fn list_unfiltered(&self, ctx: RequestContext, limit: usize) -> SyncResult<Vec<Self::Row>>;
```

`list_unfiltered` is the CRUD-shaped fallback. The Source adapter
runs the query's predicate in-memory after the unfiltered fetch
(same as CRUD does today). Migration from `CrudSource` to `Source`
is one method rename for the unfiltered path.

## Section B — Client-side mutation lifecycle

### B.1 `QueryClient::push`

```rust
impl QueryClient {
    pub fn push<M>(
        &self,
        stream: &str,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<RowChange<...>>,
    ) -> SyncResult<()>
    where
        M: Serialize + 'static;
}
```

When called:

1. Reserve a `MutationId` from the durable counter.
2. Persist the mutation to the local store (durable across reloads).
3. Route the optimistic `RowChange` through every active
   `QuerySubscription`'s predicate matcher. Matching subscriptions
   apply the change to their pending overlay; views fire
   `on_update`.
4. POST `/sync/v1/push` in the background.
5. On response: matching subscriptions swap the row from pending
   → canonical, non-matching subscriptions ignore it. Server's
   §C publish brings sibling tabs in the same partition up to
   date.

This is the existing `QueryClient` routing engine — the same code
that handles canonical pulls — applied to the optimistic path.

### B.2 Mutation queue + idempotency log

The traits move from `pocopine-sync-crud` with name shortening:

| Was | Becomes |
|---|---|
| `CrudMutationLog<Row>` | `MutationLog<Row>` |
| `MemoryCrudMutationLog<Row>` | `MemoryMutationLog<Row>` |
| `TransactionalCrudMutationLog<Tx, Row>` | `TransactionalMutationLog<Tx, Row>` |
| `CrudMutationPayload<Id, Draft>` | `MutationPayload<Id, Draft>` |
| `CrudWriteResult<Row>` | `WriteResult<Row>` |
| `CrudRemoveResult<Row>` | `RemoveResult<Row>` |
| `CrudConflict<Row>` | `Conflict<Row>` |
| `CrudTransactionRunner` | `TransactionRunner` |
| `CrudMigrateFn` | `MigrateFn` |

Public API shapes (methods, generics, semantics) are preserved
verbatim — only names change. CRUD users do a global rename and
keep working.

### B.3 `SqlxMutationLog`

`pocopine-sync-sqlx` moves into the sync-query dep tree (or stays
where it is; the crate depends on `pocopine-sync` only). Its public
type `SqlxCrudMutationLog` becomes `SqlxMutationLog`. Same SQL
schema, same `with_scope_fn` constructor.

## Section C — Macro-emitted typed writes

### C.1 Generated `Issue::create / save / remove`

`#[query_resource]` gains write-side codegen. Given:

```rust
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    #[query_param(required)]
    pub workspace_id: String,
    #[query_param]
    pub status: Status,
    pub title: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct IssueDraft {
    pub workspace_id: String,
    pub status: Status,
    pub title: String,
}
```

The macro emits, in addition to today's read-side items:

```rust
impl Issue {
    pub fn create(draft: IssueDraft) -> CreateBuilder<Self, IssueDraft>;
    pub fn save(id: String, draft: IssueDraft) -> SaveBuilder<Self, IssueDraft>;
    pub fn remove(id: String) -> RemoveBuilder<Self>;
}
```

Each builder:

```rust
let outcome = Issue::create(draft)
    .optimistic(|id| Issue { id, workspace_id, status, title, ..Default::default() })
    .push(&client)
    .await?;
```

The `optimistic` closure is opt-in; without it, the row only
appears in subscriptions after the server confirms. The macro
attaches the wire envelope (`MutationPayload::create(id, draft)`),
the `key`, the stream name, and routes through `client.push`.

### C.2 Draft inference

The macro requires a sibling `<Row>Draft` type by convention:
`Issue` ↔ `IssueDraft`. If absent, the macro errors at expansion
with a clear message ("declare `IssueDraft` or pass
`draft = MyDraft` to `#[query_resource]`"). Override:

```rust
#[query_resource(name = "issues", schema_version = 1, draft = IssueWrite)]
```

### C.3 Mutation registration

The macro also generates a static mutation registry entry so
`client.push(...)` can look up the right serde shape. No runtime
registration needed; the macro emits one `inventory::submit!` entry
per resource at the call site.

## Section D — Migration story

### D.1 Phases (each = one PR)

```
┌──────────────────────────────────────────────────────────────────┐
│ Phase 1: Source trait + Query-aware list                         │
│  - Add `Source` trait alongside `CrudSource`                     │
│  - `CrudResource` keeps wrapping `CrudSource`                    │
│  - SQLx Issue example demonstrates filtered server-side list     │
│  - No breaking changes                                           │
└──────────────────────────────────────────────────────────────────┘
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│ Phase 2: Move mutation lifecycle into pocopine-sync-query        │
│  - `MutationLog`, `WriteResult`, `Conflict`, `TransactionRunner` │
│  - Public via pocopine-sync-query::write::*                      │
│  - pocopine-sync-crud re-exports them under old names for BC     │
└──────────────────────────────────────────────────────────────────┘
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│ Phase 3: `QueryClient::push` + view-side optimistic overlay      │
│  - The routing engine handles optimistic + canonical paths       │
│  - `CollectionState<T>` keeps working (back-compat shim          │
│    delegates into the query client)                              │
└──────────────────────────────────────────────────────────────────┘
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│ Phase 4: Macro typed write codegen                               │
│  - `Issue::create()` / `Issue::save()` / `Issue::remove()`       │
│  - Generated builders, optimistic-closure support                │
│  - Docs updated; tutorial rewritten                              │
└──────────────────────────────────────────────────────────────────┘
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│ Phase 5: Deprecate CRUD                                          │
│  - `pocopine-sync-crud` marked `#[deprecated]` with a clear      │
│    pointer to the Query equivalents                              │
│  - `CollectionState<T>` marked `#[deprecated]`                   │
│  - Migration guide doc                                           │
└──────────────────────────────────────────────────────────────────┘
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│ Phase 6: Delete CRUD                                             │
│  - Remove `pocopine-sync-crud` + `pocopine-sync-crud-macros`     │
│    from the workspace                                            │
│  - Delete `docs/sync-crud.md`, `sync-crud-macro-contract.md`,    │
│    `sync-crud-query-composition.md`                              │
│  - F for respect.                                                │
└──────────────────────────────────────────────────────────────────┘
```

Each phase ships as its own PR. Phases 1–4 are additive; Phase 5
flips deprecation flags; Phase 6 removes the deprecated surface.
Anyone tracking `main` between phases 1 and 5 sees both APIs side
by side with clear `#[deprecated]` warnings.

### D.2 User-side migration

For a hypothetical CRUD-using app (no shipping consumers today;
this is for completeness):

```diff
-use pocopine_sync_crud::{resource, CrudSource, MemoryCrudMutationLog};
+use pocopine_sync_query::{source, Source, MemoryMutationLog};

-let r = resource("issues", IssueSource::default())?
+let r = source("issues", IssueSource::default())?
     .id(|r| r.id.clone())
-    .mutation_log(MemoryCrudMutationLog::new())
-    .params_of(issues::row_to_params_typed);
+    .mutation_log(MemoryMutationLog::new());
+    // .params_of removed — the macro auto-wires partition routing.
```

Client side:

```diff
-let payload = CrudMutationPayload::create(id.clone(), draft);
-let mutation = payload.into_sync_draft()?.key(id.clone())?;
-self.plugin::<SyncClient>()
-    .collection(this, |s| &mut s.writes)
-    .stream(STREAM)
-    .and_then(|c| c.push_with_generated_id(mutation, Some(optimistic)));
+Issue::create(draft)
+    .optimistic(|id| Issue { id, /* ... */ })
+    .push(&self.plugin::<QueryClient>())?;
```

The component struct loses the `writes: CollectionState<Issue>`
field. Only `view: Option<Rc<QueryView<Issue>>>` remains.

### D.3 Estimated lines

Rough scope:

| Phase | Adds (LOC) | Removes (LOC) | Net |
|---|---|---|---|
| 1 — Source trait | +400 | 0 | +400 |
| 2 — Mutation lifecycle move | +600 | -200 (CRUD imports) | +400 |
| 3 — QueryClient::push | +800 | -300 (CollectionState write paths) | +500 |
| 4 — Macro typed writes | +500 | 0 | +500 |
| 5 — Deprecation flags | +50 | 0 | +50 |
| 6 — CRUD deletion | 0 | -7,860 | -7,860 |
| **Total** | **+2,350** | **-8,360** | **-6,010** |

Net workspace size shrinks by ~6,000 lines after Phase 6.

## Section E — Kill criteria

Stop the migration and roll back to dual-crate if any of:

* **E1.** A real production CRUD-using app surfaces (currently
  unknown) and the migration cost for them exceeds 1 person-day.
* **E2.** Phase 3 (`QueryClient::push`) reveals an unforeseen
  semantic conflict — e.g. the predicate matcher's behavior under
  partial optimistic state differs from CRUD's per-collection
  state in a way that breaks listener ordering. Rollback: keep
  CollectionState as the primary write state, add `params_of`-
  style shims everywhere CRUD has special behavior.
* **E3.** Server-side `Query<Row>` translation turns out to be
  unimplementable cleanly for SQLx — e.g. the comparator wrappers
  don't pattern-match cleanly without a Rust DSL layer above SQL.
  Rollback: keep CRUD-shaped `list(limit)` as the default and
  document Query-aware lists as opt-in.

After Phase 4 lands, the cost of rollback escalates sharply
(deleting code is easier than re-creating deprecated surface). Kill
criteria are scoped to Phases 1–3.

## Section F — Open questions

1. **Mutation routing under offline queue.** When a mutation is
   queued (offline), the optimistic overlay applies but the wire
   send is deferred. Today CRUD stores the offline queue per
   collection. After merge, where does the queue live —
   `QuerySubscription` (per stream/params) or `QueryClient` (global
   per stream)? Per-subscription means dropping the subscription
   loses queued writes; global means routing is more complex.
   **Provisional answer:** global per `(stream, mutation_id)` keyed
   queue in `QueryClient`, mirroring CRUD's per-stream queue.

2. **`order_by` and `limit` semantics under Query-aware list.**
   If the server filters via SQL but the order_by field isn't
   indexed, naive translation is slow. Should the macro emit
   compile-time hints that order_by fields must be indexed? Or
   just document the perf cliff?
   **Provisional answer:** document the cliff; index hints belong
   in the storage layer, not the source trait.

3. **Macro complexity budget.** Phase 4 grows the macro by ~500
   lines (Draft type detection, builder generation, optimistic
   closure plumbing). Worth checking against the rfcs/proc-macro-
   complexity-budget that doesn't exist but probably should.
   **Provisional answer:** ship and revisit if compile times
   regress noticeably.

## Appendix: anti-goals from the audit

For the record — concerns the audit raised that this RFC explicitly
does not address:

- **"Two crates is a paper boundary, not a real one"** — agree,
  this RFC kills it.
- **"Client-side state duplication"** — agree, Phase 3 + 4 kill it.
- **"No server-side query translation"** — agree, Phase 1 kills it.
- **"CRUD has irreplaceable production primitives"** — disagree
  after re-audit: every primitive moves into Query under a renamed
  shape. No semantic loss.
