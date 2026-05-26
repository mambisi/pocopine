# Sync shape subscriptions

Shape subscriptions let you declare typed filter parameters on a sync resource so a single registration serves many filtered views — "issues in workspace W", "messages in channel C", "todos assigned to me" — without registering one `SyncStreamName` per filter combination.

This page is the cookbook. See [RFC 085](../rfcs/rfc-085-shape-subscriptions.md) for the design rationale and [`docs/sync-local-first-gaps.md` Axis 2](./sync-local-first-gaps.md) for the gap analysis.

## Declaring params

Add a `params(...)` clause to the resource attribute. Each entry is a `field_name: Type` pair, where the type encodes the comparator semantics:

```rust
use pocopine_sync_crud::{params, resource, async_trait, CrudSource};

#[resource(
    name = "issues",
    schema_version = 1,
    params(
        workspace_id: WorkspaceId,                       // required equality
        assignee_id: Option<UserId>,                     // optional equality
        status: params::InSet<Status>,                   // membership in a set
        created_at: params::Range<DateTime<Utc>>,        // bounded range
        title: params::Contains,                         // substring match
    ),
)]
#[async_trait]
impl CrudSource for Issues { /* ... */ }
```

The macro reads each declared type, infers the comparator kind by name + arity, and generates:

* A typed `issues::StreamParams` struct mirroring the declared shape.
* `StreamParams::serialize_params()` (typed → wire `BTreeMap<String, Value>`).
* `StreamParams::extract(&wire)` (wire → typed, with structural validation).
* A `field` module of zero-sized markers — one per declared field — for the typed query DSL.
* A fluent `Resource::stream()` builder with one setter per field.
* A declarative `Resource::query()` builder with `.where_eq`, `.where_in`, `.where_range`, `.where_contains`.
* An auto-wired `SyncStreamSource::validate_params` override that calls `StreamParams::extract` and surfaces structural errors as `BadRequest` before any `pull` / `push` runs.

## The comparator vocabulary

The vocabulary is **bounded and fixed**. No SQL passthrough, no nested predicate trees. Each declared field maps to exactly one comparator:

| Declared type                       | Wire shape                                                   | Builder setter                  | Query DSL                                 |
|-------------------------------------|--------------------------------------------------------------|---------------------------------|-------------------------------------------|
| `T` (bare)                          | `"field": <value>`                                           | `.field(value)`                 | `.where_eq(field::field, value)`          |
| `Option<T>`                         | `"field": <value>` (or absent on `None`)                     | `.field(value)`                 | `.where_eq(field::field, value)`          |
| `params::InSet<T>`                  | `"field": { "in": [v1, v2, ...] }`                           | `.field_in([...])`              | `.where_in(field::field, [...])`          |
| `params::Range<T>`                  | `"field": { "from": ..., "to": ..., "inclusive": [b, b] }`   | `.field_range(Range::closed(...))` | `.where_range(field::field, ...)`      |
| `params::Contains`                  | `"field": { "contains": "needle", "case_sensitive": false }` | `.field_contains("needle")`     | `.where_contains(field::field, "needle")` |

All builder setters that take collections (`InSet`, `Contains`) return `SyncResult<Self>` — empty in-sets and empty needles are rejected at construction time because they always either select nothing or everything.

`params::Range` helpers: `Range::closed(from, to)`, `Range::half_open(from, to)`, `Range::at_least(from)`, `Range::at_most(to)`, `Range::greater_than(from)`, `Range::less_than(to)`.

## `Resource::stream()` vs `Resource::query()`

Two surfaces for the same primitive. **They produce identical wire `BTreeMap<String, Value>` maps**, so subscriptions built via either surface share cache keys (and, once the Batch 2b subscription registry lands, share the underlying `SyncCollection` task).

### `Resource::stream()` — fluent setters

```rust
let view = issues_resource.stream()
    .workspace_id(workspace)
    .status_in([Status::Open, Status::InProgress])?
    .title_contains("auth")?
    .observe()?;
```

Each method name encodes the comparator (`workspace_id` for required eq, `status_in` for `InSet`, `title_contains` for `Contains`). The macro chooses the suffix from the declared kind.

### `Resource::query()` — declarative

```rust
use issues::field;

let view = issues_resource.query()
    .where_eq(field::workspace_id, workspace)?
    .where_in(field::status, [Status::Open, Status::InProgress])?
    .where_contains(field::title, "auth")?
    .observe()?;
```

The query DSL is type-safe: calling the wrong comparator method on a field fails to compile, because the `field::*` markers implement exactly one comparator trait each. For example:

```rust
// `workspace_id` was declared `WorkspaceId` (required eq), so:
let view = issues_resource.query()
    .where_in(field::workspace_id, [w1, w2])?;
//  ^^^^^^^^^ compile error: `__Field_workspace_id` does not implement
//            `FieldInSet<WorkspaceId>` — workspace_id is eq-only.
```

This guarantee comes for free from the sealed-trait pattern in `pocopine_sync_crud::query`. The same applies to `where_range` (only fields declared `Range<T>` compile) and `where_contains` (only `Contains` fields).

### When to use which

Pick whichever reads better at the call site. Two heuristics:

* If you're conditionally adding constraints (`if let Some(status) = ...`), the `Resource::stream()` builder reads more cleanly — each setter is a separate statement.
* If you're inlining a query as a single expression with field references that auto-complete, `Resource::query().where_eq(field::..., ...)` is the more discoverable form.

## Subscription sharing semantics

Two subscriptions with identical `(stream_name, params)` pairs are **logically the same subscription**. The wire envelope is the same; the cache key derives from the same map. Once Batch 2b lands a `SyncClient::subscribe` registry that refcounts the underlying `SyncCollection`, two `observe_view` calls with equivalent params will share:

* One `/open` request to the server.
* One durable cursor in the local store.
* One materialized `CollectionState` slice in memory.
* One live wake-up subscription.

Today (pre-registry), each `Resource::stream() / query()` call constructs an independent `SyncCollection`. The wire shape is still consistent, so server-side they appear as the same subscription; client-side they materialize separately.

## Live invalidation (v1)

When the server commits a write that affects a stream, it publishes a wake-up on the per-collection topic. Every client subscribed to that stream receives the wake-up and triggers a fresh `/pull` with its captured params. The pull response carries only the rows that match — the server's `pull` implementation filters by `request.params`.

In v1, every subscribed client on a stream wakes for every write, even if its params wouldn't match. The client filters via the pull response. This is a bandwidth tradeoff: simpler server-side state, slightly more pull traffic. v2 (future) will add opt-in per-`(name, params_hash)` topics for high-fanout collections.

## Tombstone-on-filter-departure

A row that transitions out of a subscription's predicate set (e.g. an issue moves from `status=open` to `status=closed`, and the client is subscribed to `status_in=[open]`) must DISAPPEAR from the client's view, even though the row still exists on the server.

The source's incremental `pull` is responsible for emitting a synthetic `SyncOp::Delete` for such rows. The CRUD framework does not auto-emit these yet — source authors should compare new-shape rows against `request.params` and emit `SyncChange { op: Delete, ... }` for rows that left the set.

Auto-emission is tracked as a follow-up — the macro will eventually generate the predicate evaluator from the declared `params(...)` and inject it into the source's pull pipeline.

## Common pitfalls

* **Forgetting to declare a param the server filters on.** The source's `pull` reads `request.params`, but the wire envelope only carries what the client declared. If the server expects a `workspace_id` but the param isn't in `params(...)`, the macro's auto-validator rejects every subscription as "missing required `workspace_id`." Declare the param or change the server's filter.

* **`SyncStreamSource::push` mutations don't carry params.** Pushes carry the mutation's `workspace_id` inside the payload; the framework doesn't re-validate against the subscription's params. A mutation that creates a row in workspace V while the client is subscribed to workspace W will still be ACCEPTED by the source — but the new row won't appear in the W view (it doesn't match). This is the intended behavior; clients should structure their mutations against the workspace they're actively viewing.

* **`InSet` and `Contains` are non-empty.** An empty `InSet` would select no rows (almost always a bug); the framework rejects it at construction time. Same for `Contains` — an empty needle matches every row, which is the same as omitting the param.

* **Auth scopes aren't params.** Don't lift the JWT's `tenant_id` claim into a subscription param — that lets a malicious client subscribe to another tenant by sending a different `tenant_id`. Auth scopes belong in `RequestContext`, evaluated server-side; params are app-level filters within an already-authorized scope.

* **Bumping `schema_version` doesn't reset params.** Schema versioning (RFC's prior axis) clears cached rows + pending mutations on bump. It does NOT clear the durable `app_schema_version` keyed on `(stream, params_hash)` — each (stream, params) combo carries its own cached version. This is correct: different filter shapes are independent caches.

## See also

* [RFC 085 — Shape subscriptions](../rfcs/rfc-085-shape-subscriptions.md) — design rationale, alternatives considered, migration plan.
* [Sync schema versioning](./sync-schema-versioning.md) — the prior axis; how cached caches recover after a server schema bump.
* [Local-first roadmap](./sync-local-first-roadmap.md) — overall positioning of the sync framework.
