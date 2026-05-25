# Sync CRUD Design

`pocopine-sync-crud` is an explicit helper crate on top of
`pocopine-sync`. It centralizes the common local-first CRUD lifecycle
while leaving persistence as normal Rust code. It is not an ORM and it is
not `pocopine-db`.

Apps still write their SQL, migrations, indexes, transactions, database
constraints, authorization checks, and domain validation. Pocopine owns
the sync protocol boundary, local mutation queue, optimistic overlay,
server outcome mapping, and generated resource ergonomics.

The concrete macro API is documented in
[`sync-crud-macro-contract.md`](./sync-crud-macro-contract.md). That file
is the review target for generated server/client API shape.

## Goal

Most CRUD apps need the same sync behavior:

- load a collection,
- create or update rows,
- delete rows,
- queue offline mutations,
- apply optimistic UI,
- replay pending writes after reconnect,
- handle accepted, rejected, and conflicted server outcomes.

That lifecycle is easy to get wrong when every app hand-builds
`ClientMutation` values and manually updates local sync state.
`pocopine-sync-crud` provides the tested lifecycle once, without
inventing a database abstraction language.

## Non-goals

- Do not generate SQL.
- Do not infer database table names.
- Do not replace SQLx, Diesel, raw SQLite, or custom repositories.
- Do not expose browser SQL as an app API.
- Do not become framework core.
- Do not require the umbrella `pocopine` crate for generated sync CRUD code.
- Do not silently merge conflicts.

## Crate Boundary

```text
pocopine-sync
  protocol, local store, mutation queue, live wake-up

pocopine-sync-sqlite
  local SQLite store for browser/native sync cache

pocopine-sync-crud
  typed CRUD source trait
  resource id boundary
  create/save/remove mutation payloads
  write policy and queued/outcome status types
  transaction binding contract
  resource macro
  generated resource module
  generated client wrapper
  typed client CRUD methods
  mapping from CRUD operations to sync push/pull
```

The app still owns the persistence implementation:

```text
Postgres with SQLx
SQLite with rusqlite/sqlx
custom repository
test double
```

## What Exists Now

The current implementation ships these reusable contracts and runtime
pieces:

- `ResourceId` and `new_id::<Id>()` for converting app ids to sync row keys and generating local-first ids when the id type supports it,
- `CrudSource` for app-owned server persistence code,
- `CrudMutationPayload::{create, save, remove}` and `into_sync_draft(...)` for mapping CRUD writes into `pocopine-sync` push drafts,
- `CreateOptions`, `SaveOptions`, `RemoveOptions`, `WritePolicy`, and `QueuedStatus` for the client lifecycle,
- `Transaction`, `TransactionBindable`, `TransactionRunner`, and `TransactionOptions::run(...)` so the public transaction API can stay `tx.with(resource)` while the app owns begin/commit/rollback,
- `CrudTransactionRunner`, `TransactionalCrudSource`, `TransactionalCrudMutationLog`, and `.transactional(...)` for production server resources that need the source write and accepted-mutation log insert in one transaction,
- `resource(name, source)?.id(...).version(...).mutation_log(...)` for registering a non-macro `CrudSource` as a `pocopine-sync` stream,
- `CrudMutationLog` and `MemoryCrudMutationLog` so accepted mutation ids are explicit and replayed writes do not silently run twice,
- exact replay validation so a reused mutation id with a different operation, row id, or payload is rejected,
- `local_resource_view(...)` and `LocalResourceView<Id, Row>` for typed read-side state over `CollectionState<Row>`,
- `LocalResourceViewState<Id, Row>` and `observe_local_resource_view(...)` for comparable resource-view subscriptions,
- `LocalResourceView::conflicts()` and `LocalResourceView::conflict_for(...)` for conflict UI lookup without raw sync rows,
- `client_resource(collection, view)` and `CrudClientResource` for non-macro client CRUD methods,
- `CrudClientResource::use_server`, `retry_local`, and `merge_with` as the first conservative conflict-resolution helpers,
- durable generated-id queueing for `WritePolicy::QueueOffline`,
- online-confirmed generated-id push for `WritePolicy::RequireOnline`,
- low-level `pocopine-sync` online-only push helpers,
- `#[pocopine_sync_crud::resource(name = "...")]` for generating a typed resource module from a `CrudSource` impl,
- generated resource aliases: `Id`, `Row`, `Draft`, `CreateOptions`, `SaveOptions`, `RemoveOptions`, `View`, `ViewState`, `Queued`, `Outcome`, and `Client<C>`,
- generated server registration: `customers::resource(source)`,
- generated client helpers: `customers::new_id()`, `view(...)`, `collection(...)`, `client(...)`, and `use_resource(...)`,
- generated `Resource<C>` methods: `open`, `pull`, `view`, `observe_view`, `client`, `create`, `create_with_options`, `save`, `save_with_options`, `remove`, `remove_with_options`, `use_server`, `retry_local`, `retry_local_with_options`, `merge_with`, and `merge_with_options`,
- route-level integration coverage proving a CRUD resource registered with `SyncServer` serves `/pull` and `/push` through the normal sync plugin.

The remaining higher-level layer is deliberately smaller now:

- fluent options builders such as `customers.create_options().optimistic(row).send(...)`,
- transaction convenience helpers such as `customers.transaction_options().require_online().run(...)`,
- macro tests that exercise more complete generated browser-style call sites,
- example app wiring that shows the generated helper in a real component,
- a true `discard_local` helper that also purges queued pending mutations for one row key.

## Server Trait

The server side starts with `CrudSource`. This trait is the adapter
boundary between Pocopine sync and the app's database code.

```rust
#[pocopine_sync_crud::async_trait]
pub trait CrudSource: Send + Sync + 'static {
    type Id: pocopine_sync_crud::ResourceId;
    type Row: Clone + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static;
    type Draft: Clone + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static;

    async fn list(
        &self,
        ctx: pocopine_auth::RequestContext,
        limit: usize,
    ) -> pocopine_sync::SyncResult<Vec<Self::Row>>;

    async fn get(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
    ) -> pocopine_sync::SyncResult<Option<Self::Row>>;

    async fn create(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> pocopine_sync::SyncResult<Self::Row>;

    async fn save(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudWriteResult<Self::Row>>;

    async fn remove(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudRemoveResult<Self::Row>>;
}
```

`list` receives the maximum row count the adapter will return in one
snapshot response. Put that limit into the database query. `save` and
`remove` receive the caller's base row version and must check it in the
same database operation as the write.

`get` returns `Option<Row>` so the source can hide whether a row is
missing or merely not visible to the caller. That avoids existence leaks.
Conflict recovery uses `get` to retrieve the server-visible row after a
stale write.

`create` receives the id because local-first optimistic writes need a row
id before the server round-trip. Apps that require server-generated ids
can still build a non-offline create flow later, but the local-first CRUD
path should prefer client-known ids.

## Resource Identity

`ResourceId` is the identity boundary. It keeps database identity policy
out of the sync protocol while giving the CRUD layer one place to
stringify ids for queued mutations, parse them back into app types, and
generate local-first ids when supported.

```rust
pub trait ResourceId:
    Clone
    + Eq
    + std::hash::Hash
    + std::fmt::Display
    + std::str::FromStr
    + serde::Serialize
    + serde::de::DeserializeOwned
    + Send
    + Sync
    + 'static
{
    fn generate_local() -> pocopine_sync::SyncResult<Self> {
        Err(pocopine_sync::SyncError::unsupported(
            "this resource id does not support local generation",
        ))
    }

    fn to_row_key(&self) -> pocopine_sync::SyncResult<pocopine_sync::RowKey> {
        pocopine_sync::RowKey::new(self.to_string())
    }

    fn from_row_key(row_key: &pocopine_sync::RowKey) -> pocopine_sync::SyncResult<Self> {
        Self::from_str(row_key.as_str()).map_err(|_| {
            pocopine_sync::SyncError::client(format!(
                "invalid resource id: {}",
                row_key.as_str()
            ))
        })
    }
}
```

Built-in implementations cover common ids:

- `uuid::Uuid`: encode/decode as a stable string and generate locally.
- `String`: encode/decode as-is after validation; generation should use an explicit app wrapper if the app wants ULIDs, CUIDs, or another string strategy.
- integer ids such as `i64`: encode/decode for existing rows, but do not generate locally by default because database auto-increment ids are not local-first safe.
- custom and composite ids: implement `Display`, `FromStr`, and `ResourceId` in the app with a stable string format.

The generated resource module exposes the specialized helper:

```rust
let customer_id = customers::new_id()?;
```

Apps can still pass ids explicitly when the id comes from a route
parameter, another table, or a database adapter that does not support
safe local generation.

## Server Wiring

A resource is registered explicitly. The resource name becomes the
default sync stream, local collection name, and live wake-up namespace.

```rust
let customers = customers::resource(Customers {
    pool: pool.clone(),
})?
.id(|row: &Customer| row.id)
.version(|row: &Customer| row.version)
.transactional(
    pocopine_sync_sqlx::postgres(pool.clone()),
    pocopine_sync_sqlx::SqlxCrudMutationLog::<sqlx::Postgres>::new(tenant_scope),
);

let sync = pocopine_sync::SyncServer::builder()
    .guarded_stream(customers, pocopine_auth::require_auth())
    .events(live_backend)
    .build();

let server = pocopine_server::Server::new(router)
    .plugin(pocopine_sync::sync_server_plugin(sync))
    .try_finalize()?;
```

There is no separate stream setting in the first version. If a later
version needs stream overrides, add them only after a real use case proves
the need.

The adapter requires an idempotency path before it implements
`SyncStreamSource`. This is deliberate: a replayed `MutationId` must not
run `create`, `save`, or `remove` twice. The recommended production path
is `.transactional(...)`, shown above.

`MemoryCrudMutationLog` is available for tests and single-process demos
with the older non-transactional `.mutation_log(...)` terminator:

```rust
let customers = customers::resource(Customers::new())?
    .id(|row: &Customer| row.id)
    .version(|row: &Customer| row.version)
    .memory_mutation_log();
```

The non-transactional `.mutation_log(...)` terminator remains available
for simple adapters and compatibility, but it cannot force the source
write and log insert to share one database transaction.

On replay, the adapter only acknowledges an accepted mutation id when the
operation, row id, and serialized payload are an exact match for the
previously accepted mutation. It intentionally does not return cached rows
from the mutation log; the next pull is the source of canonical row data.
This avoids leaking a row accepted under one principal into another
principal's retry path.

The transaction-backed path requires three app-owned pieces:

- `CrudTransactionRunner` begins, commits, and rolls back the database
  transaction handle.
- `TransactionalCrudSource<Tx>` applies `create_in_tx`, `save_in_tx`, and
  `remove_in_tx` using that handle.
- `TransactionalCrudMutationLog<Tx, Row>` reserves, checks, and records
  accepted mutation ids using the same handle and tenant/auth scope.

`pocopine-sync-sqlx` now supplies the common SQLx transaction runner and
accepted-mutation log helper for Postgres, MySQL, and SQLite feature
flags. Apps still implement `CrudSource` and `TransactionalCrudSource`
with normal SQLx queries. See [`sync-sqlx.md`](./sync-sqlx.md) for the
schema and backend-specific notes.

The sync adapter opens one transaction per mutation in a push request:

```text
begin transaction
  -> reserve accepted mutation id for this tenant/auth scope
  -> if already accepted, acknowledge exact replay or reject changed contents
  -> apply source write and base_version check
  -> commit accepted writes
  -> roll back conflicts, rejections, and backend errors
```

The mutation id is reserved before the source write so concurrent retries
cannot both pass the idempotency check. Conflicts and rejected outcomes
roll back the transaction, which removes the reservation and leaves the
client free to correct and retry the mutation.

The accepted-mutation table should enforce a unique key over the same
authorization scope used by the source query, for example
`(tenant_id, mutation_id)`. The reservation insert, not a prior lookup,
is the concurrency boundary that prevents two transactions from both
applying the same write.

`MutationId` only dedupes sync replay. It is not a complete business
idempotency key. External side effects such as payments, inventory
reservations, email sends, uniqueness-sensitive workflows, or third-party
API calls still need app-level idempotency keys and domain-specific
transaction rules.

## Client Runtime

Generated modules hide the two moving parts that the non-macro runtime
uses directly: the low-level sync collection and the typed local resource
view.

```rust
pub struct CustomersPage {
    customers: pocopine_sync::CollectionState<Customer>,
}

fn customers_state(
    page: &mut CustomersPage,
) -> &mut pocopine_sync::CollectionState<Customer> {
    &mut page.customers
}

let customers = customers::use_resource(
    &self.plugin::<pocopine_sync::SyncClient>(),
    pocopine::this::<CustomersPage>(),
    customers_state,
);

customers.open()?;
```

Create/save/remove call the same `CrudClientResource` runtime that the
non-macro API exposes:

```rust
let outcome = customers
    .create_with_options(
        customer_id,
        CustomerDraft { name, email },
        customers::CreateOptions::new().optimistic(optimistic_customer),
    )
    .await?;

match outcome {
    pocopine_sync_crud::CrudOutcome::Queued(queued) => {
        self.last_mutation = Some(queued.mutation_id);
    }
    pocopine_sync_crud::CrudOutcome::Accepted { row, .. } => {
        self.form_name = row.name;
    }
    pocopine_sync_crud::CrudOutcome::Rejected { reason, .. } => {
        self.error = reason;
    }
    pocopine_sync_crud::CrudOutcome::Conflict {
        server_row, reason, ..
    } => {
        self.conflict_reason = Some(reason);
        self.server_customer = server_row;
    }
    pocopine_sync_crud::CrudOutcome::Removed { .. } => {}
}
```

The lower-level helpers are still available when an app needs them:

```rust
let collection = customers::collection(
    &self.plugin::<pocopine_sync::SyncClient>(),
    pocopine::this::<CustomersPage>(),
    customers_state,
)?;
let client = customers::client(collection, &self.customers)?;
```

`customers::Resource<C>` stores a cloned `SyncClient`, a component/store
`Handle<C>`, and the selector. It rebuilds a cheap `SyncCollection` and a
fresh `LocalResourceView` for each operation. The returned async futures
therefore own the runtime handle they need and do not borrow the component
state across `await`.

## Write Policy

The default write policy is `QueueOffline`. A queued outcome means the
mutation id has been durably reserved and the pending mutation has been
stored locally. It is not proof that the server accepted the write.

```rust
customers.create(id, draft).await?;
customers.save(id, draft).await?;
customers.remove(id).await?;
```

`RequireOnline` waits for a server push response:

```rust
let outcome = customers
    .save_with_options(
        customer.id,
        CustomerDraft {
            name: edited_name,
            email: edited_email,
        },
        customers::SaveOptions::new()
            .optimistic(edited_customer)
            .require_online(),
    )
    .await?;
```

`RequireOnline` returns `Accepted`, `Removed`, `Rejected`, or `Conflict`.
If the browser cannot complete the request, the operation fails and no
pending mutation is queued. Failed attempts can leave gaps in mutation ids
because mutation ids are still reserved before the request.

## Local Resource View

`LocalResourceView<Id, Row>` is the typed read side generated components
should render:

```rust
let view = customers.view()?;

if view.has_pending() {
    self.badge = "syncing".to_string();
}

if view.has_conflicts() {
    self.badge = "conflict".to_string();
}

for conflict in view.conflicts() {
    self.conflict_ids.push(conflict.id.clone());
}

if let Some(row) = view.conflict_for(&customer_id) {
    self.conflict_reason = format!("row {} needs review", row.id);
}

for row in &view.rows {
    match row.status {
        pocopine_sync_crud::LocalResourceRowStatus::Synced => {}
        pocopine_sync_crud::LocalResourceRowStatus::Pending => {
            // render local draft styling
        }
        pocopine_sync_crud::LocalResourceRowStatus::Conflict => {
            // render conflict affordance
        }
    }
}
```

`LocalResourceRow::base_version` is the latest canonical server version
known for that id, even when the visible row includes a local optimistic
overlay. `save` and `remove` use that value by default. Pending writes for
the same id are not silently merged into a synthetic version; stale server
responses still return explicit conflicts.

Conflict resolution helpers use the same view. `use_server(id)` clears a
local conflict marker after the user chooses the known canonical server
row. It does not remove unrelated pending mutations for that row. A true
"discard local pending edits" helper is deferred until the local-store
contract has a durable row-scoped pending-mutation purge operation.

`retry_local(id, draft)` and `merge_with(id, draft)` queue a new save
using the latest canonical `base_version` from the view. The conflict
marker remains visible until the server accepts the retry and returns a
new canonical row.

## Local Resource Subscriptions

`view()` is an owned snapshot. It is still useful inside event handlers,
server-rendered tests, and one-shot calculations. Component UIs usually
need a subscription instead: run once with the current typed view, then
run again whenever the collection's owning scope is updated by open,
pull, push, conflict clearing, or any other `Handle::update` path.

Generated resources expose that path as a scoped observer:

```rust
let page = pocopine::this::<CustomersPage>();

customers.observe_view(move |state, previous| {
    page.update(|page| {
        match state {
            pocopine_sync_crud::LocalResourceViewState::Ready(view) => {
                page.pending_count = view.pending_count;
                page.conflict_count = view.conflict_count;
                page.rows = view.rows.iter().map(|row| row.value.clone()).collect();
            }
            pocopine_sync_crud::LocalResourceViewState::Error(err) => {
                page.error = err.clone();
            }
        }

        if previous.map(|prev| prev.has_conflicts()) != Some(state.has_conflicts()) {
            page.show_conflict_banner = state.has_conflicts();
        }
    });
});
```

The observer is framework-native:

- it is installed against the current Pocopine scope,
- it is released automatically when that scope unmounts,
- it tracks the resource owner's scope and re-runs when that scope is
  triggered,
- it passes the same typed `LocalResourceView` shape that CRUD writes use,
- it never exposes raw `CollectionState`, `SyncRow`, or protocol mutation
  structs to component code.

In browser components, the first callback invocation will be deferred to
the next tick so callers can install the observer from lifecycle code and
still call `Handle::update` inside the callback without re-entering the
lifecycle borrow. Native host tests install synchronously so they can
assert observer state without a browser microtask queue.

`LocalResourceViewState` is a small comparable wrapper around either a
ready view or a local view-construction error such as an
already-borrowed component/store handle:

```rust
pub enum LocalResourceViewState<Id, Row> {
    Ready(LocalResourceView<Id, Row>),
    Error(String),
}

impl<Id, Row> LocalResourceViewState<Id, Row> {
    pub fn view(&self) -> Option<&LocalResourceView<Id, Row>>;
    pub fn error(&self) -> Option<&str>;
    pub fn has_pending(&self) -> bool;
    pub fn has_conflicts(&self) -> bool;
}
```

The error variant stores a displayable string instead of `SyncError`.
The generated `observe_view(...)` method avoids repeated callbacks by
comparing sync metadata such as row ids, versions, row status, pending
mutation ids, counters, and errors. It does not require row payload
equality, so create/save/remove and observe paths can share the same row
types. Payload changes that come from normal sync operations still update
observable metadata such as row versions or pending mutation ids. The
fingerprint intentionally does not treat `last_reason` by itself as a
render trigger; it is diagnostic context attached to the state that
already changed.

This is still a component subscription, not a database query planner. It
does not read arbitrary SQLite tables, push filters into SQL, or replace
the server-side source query. Database-specific query adapters remain a
later layer.

## Generated Client API

The generated module exposes the normal author path:

```rust
let customers = customers::use_resource(
    &self.plugin::<pocopine_sync::SyncClient>(),
    pocopine::this::<CustomersPage>(),
    customers_state,
);

customers.open()?;
customers.pull()?;
let view = customers.view()?;

let page = pocopine::this::<CustomersPage>();
customers.observe_view(move |state, _previous| {
    page.update(|page| {
        if let Some(view) = state.view() {
            page.syncing = view.syncing;
            page.conflict_count = view.conflict_count;
        }
    });
})?;

customers
    .create(customer_id, CustomerDraft { name, email })
    .await?;

customers
    .save(
        customer.id,
        CustomerDraft {
            name: edited_name,
            email: edited_email,
        },
    )
    .await?;

customers.remove(customer.id).await?;

customers.use_server(customer.id).await?;

customers
    .retry_local(
        customer.id,
        CustomerDraft {
            name: edited_name.clone(),
            email: edited_email.clone(),
        },
    )
    .await?;

customers
    .merge_with(
        customer.id,
        CustomerDraft {
            name: merged_name,
            email: merged_email,
        },
    )
    .await?;
```

Advanced callers can override defaults with `_with_options`:

```rust
customers
    .create_with_options(
        customer_id,
        CustomerDraft { name, email },
        customers::CreateOptions::new().optimistic(customer),
    )
    .await?;

customers
    .save_with_options(
        customer.id,
        CustomerDraft {
            name: edited_name,
            email: edited_email,
        },
        customers::SaveOptions::new()
            .base_version(customer_version)
            .optimistic(edited_customer),
    )
    .await?;

customers
    .remove_with_options(
        customer.id,
        customers::RemoveOptions::new().base_version(customer_version),
    )
    .await?;
```

Generated option aliases remain the explicit data shape behind both
`_with_options` and later fluent builders. Their default write policy is
`WritePolicy::QueueOffline`.

## Mutation Lifecycle

The runtime owns this lifecycle for generated client CRUD methods:

1. load or create the durable `SyncDeviceId`,
2. reserve the next durable mutation counter,
3. build `MutationId = "{device_id}:{counter}"`,
4. enqueue the mutation in `SyncLocalStore` before the network request,
5. apply the default or explicit optimistic behavior locally,
6. push to the server when online,
7. persist accepted/rejected/conflict outcomes,
8. refresh canonical rows through pull/live wake-up.

When offline, generated create/save/remove methods succeed once the
mutation is durably queued. They return a generated queued mutation
handle/state, not proof that the server accepted the write.

Remove methods have different optimistic behavior from create/save:

1. allocate a durable mutation id,
2. enqueue a delete mutation,
3. optimistically remove the local row,
4. restore the row on rejection,
5. mark conflict if the server reports a stale version,
6. clear pending state on acceptance.

## Conflict Contract

Generated CRUD does not silently merge conflicts:

- accepted write: clear pending state and apply the returned canonical row,
- accepted remove: clear pending state and remove the row,
- rejected write: drop the pending mutation and rebase to canonical rows,
- conflicted write: drop the pending mutation, mark the row conflicted, and expose the server row when the server supplied one,
- offline write: keep the optimistic row visible and durable until replay resolves it.

This preserves the local-first invariant:

```text
rendered rows = canonical server rows + pending local overlay
```

`CrudOutcome::Conflict` returns `server_row: Option<Row>` for rendering
and comparison, not `Option<SyncRow<Row>>`. The row version stays in the
canonical sync state. A retry should read the refreshed
`LocalResourceView::base_version(&id)` after reconciliation or after a
pull, rather than deriving a retry version from the typed conflict row.

Generated resources expose the first explicit resolution helpers:

```rust
customers.use_server(id).await?;
customers.retry_local(id, draft).await?;
customers.merge_with(id, merged_draft).await?;
```

`use_server` clears the local conflict marker and keeps the known server
row. `retry_local` and `merge_with` enqueue a new save against the latest
canonical base. They intentionally keep the conflict visible until the
server accepts the retry. There is no `discard_local` helper yet because
that name implies pending local mutations for the row are removed from
the durable queue; the current local-store contract does not expose that
operation.

If `get` returns `None`, the client should treat the row as gone or not
visible. It should not assume the caller is allowed to know which case it
is.

## Transactions And Online Policy

CRUD needs a transaction boundary for server-side writes and custom
domain operations. Pocopine provides two related contracts, but the app
still owns the database code.

Server-side sync replay should use the transaction-backed resource path:

```text
begin transaction
  -> reserve accepted mutation id
  -> if already accepted, acknowledge exact replay or reject changed contents
  -> run one CRUD source write and base_version check
  -> commit accepted writes
  -> rollback conflicts, rejections, and errors
  -> after commit, publish sync/live invalidations
```

Live invalidation must happen after the commit. If the wake-up publish
fails, Pocopine should log it or route it through an outbox/job later; it
must not roll back an already committed database transaction.

The public operation body should stay centered on `tx.with(resource)`:

```rust
tx.with(customers).create(id, draft).await?;
tx.with(customers).save(id, draft).await?;
tx.with(customers).remove(id).await?;
tx.with(customers).get(id).await?;
```

Single CRUD mutations should transact independently on the server. If a
pending queue replays five mutations and the third conflicts, the first
two should remain committed. Apps that require all-or-nothing behavior
should model that as one explicit domain operation, then run it through
the transaction API.

Author-facing transaction convenience methods are separate from sync
replay. They are represented by `TransactionRunner`,
`TransactionOptions::run(...)`, and `tx.with(resource)`, and are not
generated yet. When generated later, they should stay centered on the
same public operation body rather than exposing raw sync envelopes.

## Pre-CRUD Sync Helpers

`pocopine-sync` owns the low-level pieces that the CRUD macro targets:

- token newtypes such as `RowKey`, `RowVersion`, `MutationId`, and `SyncStreamName` implement `Display`, `FromStr`, and `AsRef<str>`,
- `ClientMutation` supports explicit-id construction for advanced or server-owned flows,
- `ClientMutationDraft` supports row-scoped drafts for the normal generated-id path,
- `SyncRow` has helpers for attaching an already validated row version and local pending/conflict flags,
- `CollectionState` exposes current row and base-version lookups so generated save/remove defaults can use the locally known version without duplicating state scans,
- `SyncClient`, `SyncCollection`, `CollectionSelector`, and `Handle` are exported by `pocopine-sync` for generated client wrappers.

This keeps `pocopine-sync-crud` focused on resource typing, source
adapters, and generated ergonomics. It maps resource ids into sync row
keys, then calls lower-level sync helpers rather than constructing
protocol structs by hand.
