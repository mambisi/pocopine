# Sync CRUD Design

`pocopine-sync-crud` is an explicit helper crate on top of
`pocopine-sync`. It starts with the typed contracts that the non-macro
runtime adapter and later proc macro will target. It is not an ORM and
it is not `pocopine-db`.

The crate should centralize the sync mutation lifecycle for ordinary CRUD
apps while leaving persistence as normal Rust code. Apps still write
their SQL, migrations, indexes, transactions, and database-specific
logic themselves.

## Goal

Most CRUD apps need the same sync behavior:

- load a collection,
- read one row,
- create or update a row,
- delete a row,
- queue offline mutations,
- apply optimistic UI,
- replay pending writes after reconnect,
- handle accepted, rejected, and conflicted server outcomes.

That lifecycle is easy to get wrong when every app hand-builds
`ClientMutation` values and manually updates local sync state.
`pocopine-sync-crud` should provide the tested lifecycle once, without
inventing a database abstraction language.

## Non-goals

- Do not generate SQL.
- Do not infer database table names.
- Do not replace SQLx, Diesel, raw SQLite, or custom repositories.
- Do not expose browser SQL as an app API.
- Do not become framework core.
- Do not require `pocopine` to re-export optional sync CRUD types.

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
  proc-macro generated resource module
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

The first crate slice provides the reusable contracts:

- `ResourceId` and `new_id::<Id>()` for converting app ids to sync row
  keys and generating local-first ids when the id type supports it,
- `CrudSource` for app-owned server persistence code,
- `CrudMutationPayload::{create, save, remove}` and
  `into_sync_draft(...)` for mapping CRUD writes into `pocopine-sync`
  push drafts,
- `CreateOptions`, `SaveOptions`, `RemoveOptions`, `WritePolicy`, and
  `QueuedStatus` for the client lifecycle the generated API will expose,
- `Transaction`, `TransactionBindable`, `TransactionRunner`, and
  `TransactionOptions::run(...)` so the public transaction API can stay
  `tx.with(resource)` while the app owns begin/commit/rollback,
- `resource(name, source)?.id(...).version(...).mutation_log(...)` for
  registering a non-macro `CrudSource` as a `pocopine-sync` stream,
- `CrudMutationLog` and `MemoryCrudMutationLog` so accepted mutation ids
  are explicit and replayed writes do not silently run twice,
- `client_resource(collection, view)` and `CrudClientResource` for the
  non-macro client runtime that generated modules should call,
- `#[pocopine_sync_crud::resource(name = "...")]` for generating a typed
  resource module from a `CrudSource` impl,
- low-level `pocopine-sync` online-only push helpers that give
  `WritePolicy::RequireOnline` a runtime target without changing the
  queue-offline default.

The crate now generates the first typed resource module: stream name,
associated type aliases, server `resource(source)` registration, typed
`new_id()`, `view(...)`, and `client(...)` helpers. The generated client
handle still targets the non-macro `CrudClientResource` runtime; fluent
`use_resource()` convenience wrappers remain future work.

The concrete generated server/client API is tracked in
[`sync-crud-macro-contract.md`](./sync-crud-macro-contract.md). That
contract is the review target for macro changes.

The current non-macro client shape is explicit about the two moving
parts a generated module will hide: the low-level sync collection and the
typed local resource view.

```rust
use pocopine_sync_crud::{
    client_resource, local_resource_view, CreateOptions, CrudOutcome,
};

let view = local_resource_view::<uuid::Uuid, Customer>(&self.customers)?;
let customers = client_resource(customers_collection, view);

let outcome = customers
    .create_with_options(
        customer_id,
        CustomerDraft { name, email },
        CreateOptions {
            optimistic: Some(optimistic_customer),
            ..Default::default()
        },
    )
    .await?;

match outcome {
    CrudOutcome::Queued(queued) => {
        tracing::debug!(mutation_id = %queued.mutation_id, "customer queued");
    }
    CrudOutcome::Accepted { .. }
    | CrudOutcome::Removed { .. }
    | CrudOutcome::Rejected { .. }
    | CrudOutcome::Conflict { .. } => {}
}
```

Ordinary CRUD callers should not build `ClientMutationDraft` directly.
That remains the protocol escape hatch for custom sync flows that do not
fit generated create/save/remove methods.

## Proc Macro Shape

The public API should be generated by a proc macro, not exposed as a
generic builder type. The macro reads a normal `CrudSource`
implementation and generates a typed resource module for server
registration and browser calls.

The current macro accepts
`#[pocopine_sync_crud::resource(name = "...")]`. If the resource name is
not a valid Rust module identifier, authors can add
`module = module_name` while keeping the sync stream name unchanged. A
valid sync token is non-empty, at most 1024 bytes, has no
leading/trailing whitespace, and contains no control characters.

Sketch:

```rust
#[pocopine_sync_crud::resource(name = "customers")]
#[pocopine_sync_crud::async_trait]
impl pocopine_sync_crud::CrudSource for Customers {
    type Id = uuid::Uuid;
    type Row = Customer;
    type Draft = CustomerDraft;

    async fn list(
        &self,
        ctx: pocopine_auth::RequestContext,
    ) -> pocopine_sync::SyncResult<Vec<Customer>> {
        // app-owned SQL
    }

    async fn get(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
    ) -> pocopine_sync::SyncResult<Option<Customer>> {
        // app-owned SQL
    }

    async fn create(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
    ) -> pocopine_sync::SyncResult<Customer> {
        // app-owned SQL
    }

    async fn save(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
    ) -> pocopine_sync::SyncResult<Customer> {
        // app-owned SQL
    }

    async fn remove(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
    ) -> pocopine_sync::SyncResult<()> {
        // app-owned SQL
    }
}
```

The macro generates a module named after the resource:

```rust
customers::resource(source)
customers::new_id()
customers::view(&state)
customers::client(collection, &state)
customers::CreateOptions
customers::SaveOptions
customers::RemoveOptions
customers::Queued
customers::Outcome
```

The generated API should not make users name or import a public builder
type. Lower-level sync state machines can use internal builder structs if
helpful, but those are implementation details.

## Server Trait

The server side starts with a trait. The trait is the adapter boundary
between Pocopine sync and the app's database code.

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

`ResourceId` is the identity boundary. It keeps database identity policy out
of the sync protocol while still giving the CRUD layer one place to
stringify ids for queued mutations, parse them back into app types, and
generate local-first ids when the id type supports it.

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

The CRUD code encodes ids with `id.to_string()` and decodes queued ids
with `Self::from_str(...)`, mapping parse failures into `SyncError`.
That keeps common Rust id types usable without wrapper methods.
Requiring `Into<String>` would be too narrow because foreign types such
as `uuid::Uuid` implement `Display` and `FromStr`, but not
`Into<String>`.

Built-in implementations should cover common app/database ids:

- `uuid::Uuid`: encode/decode as a stable string and generate locally.
- `String`: encode/decode as-is after validation; generation should use
  an explicit app wrapper if the app wants ULIDs, CUIDs, or another
  string strategy.
- integer ids such as `i64`: encode/decode for existing rows, but do
  not generate locally by default because database auto-increment ids
  are not local-first safe.
- custom and composite ids: implement `Display`, `FromStr`, and
  `ResourceId` in the app, using a stable string format that does not
  depend on a database vendor.

The generated resource module can expose `new_id()` by calling
`pocopine_sync_crud::new_id::<Id>()` when the id type supports local
generation:

```rust
let customer_id = customers::new_id()?;
customers
    .create(customer_id, CustomerDraft { name, email })
    .await?;
```

Apps can still pass ids explicitly. That remains the clearest path when
the id comes from a route parameter, another table, or a database
adapter that does not support safe local generation.

`get` returns `Option<Row>` so the source can hide whether a row is
missing or merely not visible to the caller. That avoids existence leaks.

`create` receives the id too. Local-first optimistic writes need a row
id before the server round-trip so the client can render, queue, replay,
and dedupe the mutation safely. Apps that require server-generated IDs
can still build a non-offline create flow later, but the local-first CRUD
path should prefer client-known ids.

## SQLx Example

Persistence is normal SQLx. Pocopine does not inspect this SQL.

```rust
pub struct Customers {
    pool: sqlx::PgPool,
}

#[pocopine_sync_crud::async_trait]
impl pocopine_sync_crud::CrudSource for Customers {
    type Id = uuid::Uuid;
    type Row = Customer;
    type Draft = CustomerDraft;

    async fn list(
        &self,
        ctx: pocopine_auth::RequestContext,
        limit: usize,
    ) -> pocopine_sync::SyncResult<Vec<Customer>> {
        sqlx::query_as!(
            Customer,
            r#"
            select id, tenant_id, name, email, version
            from customers
            where tenant_id = $1
            order by name
            limit $2
            "#,
            ctx.tenant_id(),
            limit as i64
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| pocopine_sync::SyncError::backend(err.to_string()))
    }

    async fn get(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
    ) -> pocopine_sync::SyncResult<Option<Customer>> {
        sqlx::query_as!(
            Customer,
            r#"
            select id, tenant_id, name, email, version
            from customers
            where id = $1 and tenant_id = $2
            "#,
            id,
            ctx.tenant_id()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| pocopine_sync::SyncError::backend(err.to_string()))
    }

    async fn create(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
    ) -> pocopine_sync::SyncResult<Customer> {
        sqlx::query_as!(
            Customer,
            r#"
            insert into customers (id, tenant_id, name, email)
            values ($1, $2, $3, $4)
            returning id, tenant_id, name, email, version
            "#,
            id,
            ctx.tenant_id(),
            draft.name,
            draft.email
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|err| pocopine_sync::SyncError::backend(err.to_string()))
    }

    async fn save(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudWriteResult<Customer>> {
        let row = sqlx::query_as!(
            Customer,
            r#"
            update customers
            set name = $2, email = $3, version = version + 1
            where id = $1
              and tenant_id = $4
              and ($5::text is null or version::text = $5)
            returning id, tenant_id, name, email, version
            "#,
            id,
            draft.name,
            draft.email,
            ctx.tenant_id(),
            base_version.as_ref().map(|version| version.as_str())
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| pocopine_sync::SyncError::backend(err.to_string()))?;

        if let Some(row) = row {
            return Ok(pocopine_sync_crud::CrudWriteResult::applied(row));
        }

        let server_row = self.get(ctx, id).await?;
        Ok(pocopine_sync_crud::CrudWriteResult::stale(server_row))
    }

    async fn remove(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudRemoveResult<Customer>> {
        let result = sqlx::query!(
            r#"
            delete from customers
            where id = $1
              and tenant_id = $2
              and ($3::text is null or version::text = $3)
            "#,
            id,
            ctx.tenant_id(),
            base_version.as_ref().map(|version| version.as_str())
        )
        .execute(&self.pool)
        .await
        .map_err(|err| pocopine_sync::SyncError::backend(err.to_string()))?;

        if result.rows_affected() == 1 {
            return Ok(pocopine_sync_crud::CrudRemoveResult::applied());
        }

        let server_row = self.get(ctx, id).await?;
        Ok(pocopine_sync_crud::CrudRemoveResult::stale(server_row))
    }
}
```

## Server Wiring

The resource is registered explicitly. The resource name becomes the
default sync stream, local collection name, and live wake-up namespace.

```rust
let customers = pocopine_sync_crud::resource("customers", Customers {
    pool: pool.clone(),
})?
.id(|row: &Customer| row.id)
.version(|row: &Customer| row.version)
.mutation_log(CustomerMutationLog { pool: pool.clone() });

let sync = pocopine_sync::SyncServer::builder()
    .guarded_stream(customers, pocopine_auth::require_auth())
    .events(live_backend)
    .build();

let server = pocopine_server::Server::new(router)
    .plugin(pocopine_sync::sync_server_plugin(sync))
    .try_finalize()?;
```

There is no separate `stream "customers"` setting in the first version.
The stream name and collection name are automatic from the resource
name. If a later version needs stream overrides, they should be added
only after a real use case proves the need.

The adapter requires a mutation log before it implements
`SyncStreamSource`. This is deliberate: a replayed `MutationId` must not
run `create`, `save`, or `remove` twice. `MemoryCrudMutationLog` is
available for tests and single-process demos:

```rust
let customers = pocopine_sync_crud::resource("customers", Customers::new())?
    .id(|row: &Customer| row.id)
    .version(|row: &Customer| row.version)
    .memory_mutation_log();
```

Production apps should implement `CrudMutationLog` against the same
database as the `CrudSource`, and record accepted mutation ids in the
same transaction as the row write. The idempotency lookup must be scoped
to the same authorization domain as the source query, for example
`(tenant_id, mutation_id)`, not only `mutation_id`.

On replay, the adapter only acknowledges an accepted mutation id when the
operation, row id, and serialized payload are an exact match for the
previously accepted mutation. It intentionally does not return cached rows
from the mutation log; the next pull is the source of canonical row data.
This avoids leaking a row accepted under one principal into another
principal's retry path.

## Client Runtime Walkthrough

The non-macro runtime is the author-visible shape until the proc macro
generates resource modules. It keeps the app's component state in
`pocopine-sync`, then layers typed CRUD methods over that state.

```rust
pub struct CustomersPage {
    customers: pocopine_sync::CollectionState<Customer>,
}

fn customers_state(
    page: &mut CustomersPage,
) -> &mut pocopine_sync::CollectionState<Customer> {
    &mut page.customers
}
```

Open/pull still belongs to the sync collection:

```rust
let collection = sync
    .collection(pocopine::this::<CustomersPage>(), customers_state)
    .stream("customers")?;

collection.open()?;
```

Create/save/remove use `CrudClientResource`. The handle is cheap to
rebuild for each event handler because it only binds the current sync
collection and the current typed view.

```rust
use pocopine_sync_crud::{
    client_resource, local_resource_view, CreateOptions, CrudOutcome,
};

let collection = sync
    .collection(pocopine::this::<CustomersPage>(), customers_state)
    .stream("customers")?;
let view = local_resource_view::<uuid::Uuid, Customer>(&self.customers)?;
let customers = client_resource(collection, view);

let outcome = customers
    .create_with_options(
        customer_id,
        CustomerDraft { name, email },
        CreateOptions {
            optimistic: Some(optimistic_customer),
            ..Default::default()
        },
    )
    .await?;

match outcome {
    CrudOutcome::Queued(queued) => {
        self.last_mutation = Some(queued.mutation_id);
    }
    CrudOutcome::Accepted { row, .. } => {
        self.form_name = row.name;
    }
    CrudOutcome::Rejected { reason, .. } => {
        self.error = reason;
    }
    CrudOutcome::Conflict {
        server_row, reason, ..
    } => {
        self.conflict_reason = Some(reason);
        self.server_customer = server_row;
    }
    CrudOutcome::Removed { .. } => {
        // Not expected for create; included for exhaustive matching.
    }
}
```

The default write policy is `QueueOffline`. A queued outcome means the
mutation id has been durably reserved and the pending mutation has been
stored locally. In the browser runtime Pocopine then starts a background
push; the queued outcome is not server acceptance.

`RequireOnline` changes the contract:

```rust
use pocopine_sync_crud::{SaveOptions, WritePolicy};

let outcome = customers
    .save_with_options(
        customer.id,
        CustomerDraft {
            name: edited_name,
            email: edited_email,
        },
        SaveOptions {
            optimistic: Some(edited_customer),
            write_policy: WritePolicy::RequireOnline,
            ..Default::default()
        },
    )
    .await?;
```

`RequireOnline` waits for a server push response and returns
`Accepted`, `Removed`, `Rejected`, or `Conflict`. If the browser cannot
complete the request, the operation fails and no pending mutation is
queued. The host implementation returns `SyncError::Unsupported` because
confirmed pushes require the browser fetch runtime.

`RequireOnline` still reserves a durable mutation id before the request,
so failed attempts can leave gaps in mutation ids. If the request errors
after an optimistic row was applied, collection state reflects the push
error/rollback for that mutation before the method returns `Err`.

`LocalResourceView` is the read side generated components should render:

```rust
let view = local_resource_view::<uuid::Uuid, Customer>(&self.customers)?;

if view.has_pending() {
    self.badge = "syncing".to_string();
}

if view.has_conflicts() {
    self.badge = "conflict".to_string();
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
overlay. `save` and `remove` use that value by default. Pending writes
for the same id are not silently merged into a synthetic version; stale
server responses still return explicit conflicts.

## Generated Client API

The current macro layer wraps the runtime above through
`customers::client(collection, &state)`, which returns the typed
`CrudClientResource` handle. A later convenience layer can add
`use_resource()` and fluent open/options helpers on top of the same
runtime contract.

Future ergonomic shape:

```rust
let customers = customers::use_resource();

customers.open();
```

The default methods are the normal path:

```rust
customers
    .create(customer_id, CustomerDraft { name, email })
    .await?;

customers
    .save(customer.id, CustomerDraft {
        name: edited_name,
        email: edited_email,
    })
    .await?;

customers.remove(customer.id).await?;
```

The generated methods choose good sync defaults:

- `create` queues durably, generates a mutation id, and uses an
  optimistic row when it can derive one from id + draft or a local
  resource default.
- `save` uses the current local row version as `base_version` when
  available, then applies the draft as an optimistic local update.
- `remove` uses the current local row version when available and hides
  the row locally until the server accepts, rejects, or conflicts it.

Advanced users can override those defaults with `_with_options`:

```rust
customers.create_with_options(
    customer_id,
    CustomerDraft { name, email },
    customers::CreateOptions {
        optimistic: Some(customer),
        ..Default::default()
    },
)
.await?;

customers.save_with_options(
    customer.id,
    CustomerDraft {
        name: edited_name,
        email: edited_email,
    },
    customers::SaveOptions {
        base_version: Some(customer_version),
        optimistic: Some(edited_customer),
        ..Default::default()
    },
)
.await?;

customers.remove_with_options(
    customer.id,
    customers::RemoveOptions {
        base_version: Some(customer_version),
        ..Default::default()
    },
)
.await?;
```

The macro can also generate `OpenOptions`-style fluent options methods
for advanced call sites that read better as chained configuration:

```rust
customers
    .create_options()
    .optimistic(customer)
    .send(customer_id, CustomerDraft { name, email })
    .await?;

customers
    .save_options()
    .base_version(customer_version)
    .optimistic(edited_customer)
    .send(customer.id, CustomerDraft {
        name: edited_name,
        email: edited_email,
    })
    .await?;

customers
    .remove_options()
    .base_version(customer_version)
    .send(customer.id)
    .await?;
```

The public authoring shape is the method chain, not a generic upsert
builder type users need to understand. The simple methods remain the
recommended path; `_with_options` and fluent options methods exist for
cases where defaults are not enough.

Generated options structs remain useful as the explicit data shape behind
both `_with_options` and the fluent options methods. Their default write
policy is `WritePolicy::QueueOffline`:

```rust
pub struct CreateOptions<Row> {
    pub optimistic: Option<Row>,
    pub write_policy: pocopine_sync_crud::WritePolicy,
}

pub struct SaveOptions<Row> {
    pub base_version: Option<pocopine_sync::RowVersion>,
    pub optimistic: Option<Row>,
    pub write_policy: pocopine_sync_crud::WritePolicy,
}

pub struct RemoveOptions {
    pub base_version: Option<pocopine_sync::RowVersion>,
    pub write_policy: pocopine_sync_crud::WritePolicy,
}
```

Generated queued state remains simple:

```rust
pub struct Queued {
    pub mutation_id: pocopine_sync::MutationId,
    pub id: uuid::Uuid,
    pub status: pocopine_sync_crud::QueuedStatus,
}
```

## Generated Mutation Lifecycle

The non-macro runtime already owns the sync lifecycle that generated
client CRUD methods should call:

1. load or create the durable `SyncDeviceId`,
2. reserve the next durable mutation counter,
3. build `MutationId = "{device_id}:{counter}"`,
4. enqueue the mutation in `SyncLocalStore` before the network request,
5. apply the default or explicit optimistic behavior locally,
6. push to the server when online,
7. persist accepted/rejected/conflict outcomes,
8. refresh canonical rows through pull/live wake-up.

When offline, generated create/save/remove methods should still succeed
once the mutation is durably queued. They return a generated queued
mutation handle/state, not proof that the server accepted the write.

```rust
// generated inside the `customers` module
pub struct Queued {
    pub mutation_id: pocopine_sync::MutationId,
    pub id: uuid::Uuid,
    pub status: pocopine_sync_crud::QueuedStatus,
}
```

Remove methods have different optimistic behavior from create/save:

1. allocates a durable mutation id,
2. enqueues a delete mutation,
3. optimistically removes the local row,
4. restores the row on rejection,
5. marks conflict if the server reports a stale version,
6. clears pending state on acceptance.

## Pre-CRUD Sync Helpers

`pocopine-sync` owns the low-level pieces that the CRUD macro should
target. The CRUD crate should not rebuild these contracts:

- token newtypes such as `RowKey`, `RowVersion`, `MutationId`, and
  `SyncStreamName` implement `Display`, `FromStr`, and `AsRef<str>`,
- `ClientMutation` supports explicit-id construction for advanced or
  server-owned flows,
- `ClientMutationDraft` supports row-scoped drafts for the normal
  generated-id path,
- `SyncRow` has helpers for attaching an already validated row version
  and local `pending` / `conflict` flags,
- `CollectionState` exposes current row and base-version lookups so
  generated save/remove defaults can use the locally known version
  without duplicating state scans.

This keeps `pocopine-sync-crud` focused on resource typing, source
adapters, and generated ergonomics. It should map resource ids into
sync row keys, then call the lower-level sync helpers rather than
constructing protocol structs by hand.

## Transactions And Online Policy

CRUD needs a transaction boundary for server-side writes and custom
domain operations. Pocopine should provide the transaction lifecycle, but
the app still owns the database code. The rule is simple:

```text
begin transaction
  -> run user CRUD/repository/domain code
  -> commit on success
  -> rollback on error
  -> after commit, publish sync/live invalidations
```

Live invalidation must happen after the commit. If the wake-up publish
fails, Pocopine should log it or route it through an outbox/job later; it
must not roll back an already committed database transaction.

The public transaction API should make the transaction the visible owner
of the operation:

```rust
customers
    .transaction_options()
    .require_online()
    .run(ctx, |tx| async move {
        let customer = tx
            .with(customers)
            .create(customer_id, CustomerDraft { name, email })
            .await?;

        tx.with(audit_log)
            .record(customer.id, "created")
            .await?;

        Ok(customer)
    })
    .await?;
```

Use `tx.with(resource)` as the public verb. It is short, reads naturally
for CRUD resources and custom repositories, and avoids forcing every
resource method to grow an `_in` variant. Internally, the binding trait
can be named around `bind`, but users should not need to call it:

```rust
pub trait TransactionBindable<Tx> {
    type Bound<'tx>
    where
        Self: 'tx,
        Tx: 'tx;

    fn bind<'tx>(self, tx: &'tx mut Tx) -> Self::Bound<'tx>;
}
```

The shipped helper contract is:

```rust
pub type TransactionFuture<'tx, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = pocopine_sync::SyncResult<T>> + Send + 'tx>>;

pub trait TransactionRunner: Send + Sync + 'static {
    type Tx: Send + 'static;

    fn transaction<'runner, T, F>(&'runner self, f: F) -> TransactionFuture<'runner, T>
    where
        T: Send + 'runner,
        F: for<'tx> FnOnce(Transaction<'tx, Self::Tx>) -> TransactionFuture<'tx, T>
            + Send
            + 'runner;
}
```

The app/database adapter implements `TransactionRunner` by beginning a
transaction, calling the closure, committing on `Ok`, and rolling back on
`Err`. The author-facing operation body stays:

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

Offline writes and server-required writes are different policies:

```rust
pub enum WritePolicy {
    QueueOffline,
    RequireOnline,
}
```

`QueueOffline` is the local-first default. It reserves a durable mutation
id, enqueues the mutation locally, applies optimistic UI, and replays the
write later when the app can reach the server.

`RequireOnline` means server-confirmed before success. If the request
cannot reach the server, the operation fails locally and nothing is
queued. It is not a business idempotency guarantee: if the server commits
and the response is lost, a manual retry gets a new mutation id. Use
`RequireOnline` when offline replay is the wrong UX, and add an app-level
idempotency key for payment, inventory, uniqueness-sensitive, or other
side-effecting domains.

Generated simple methods should keep `QueueOffline` as the default:

```rust
customers.create(id, draft).await?;
customers.save(id, draft).await?;
customers.remove(id).await?;
```

Advanced call sites can opt into server-required behavior:

```rust
customers
    .save_options()
    .require_online()
    .base_version(row_version)
    .send(customer.id, draft)
    .await?;
```

The transaction options API should use the same policy vocabulary:

```rust
customers
    .transaction_options()
    .require_online()
    .run(ctx, |tx| async move {
        tx.with(customers).save(customer.id, draft).await
    })
    .await?;
```

## Offline Contract

With a durable local store installed, CRUD operations are offline-capable:

```text
open
  -> hydrate cached rows
  -> render immediately
  -> pull when online

create/save/remove
  -> allocate durable mutation id
  -> enqueue locally
  -> update optimistic UI
  -> replay when online
```

The guarantee is local durability, not final server success. The server
still authorizes and validates every replayed mutation.

Status values should make this explicit:

```text
queued
syncing
accepted
rejected
conflict
```

`LocalResourceView<Id, Row>` is the typed read side for this state. It
converts the low-level `CollectionState<Row>` into visible resource rows,
canonical `base_version` values, pending mutations, pending/conflict
flags, and collection metadata. Generated CRUD clients should consume this
view instead of exposing raw protocol structs to application code.

## Conflict And `get`

`get` is required in the trait because conflict recovery and detail pages
need a single-row refresh. When a save/remove conflicts, the CRUD layer
can call `get(ctx, key)` to fetch the current server-visible row and mark
it conflicted locally.

`CrudOutcome::Conflict` returns `server_row: Option<Row>` for rendering
and comparison, not `Option<SyncRow<Row>>`. The row version stays in the
canonical sync state. A retry should read the refreshed
`LocalResourceView::base_version(&id)` after reconciliation or after a
pull, rather than deriving a retry version from the typed conflict row.

If `get` returns `None`, the client should treat the row as gone or not
visible. It should not assume the caller is allowed to know which case it
is.

## Implementation Slices

The current `pocopine-sync-crud` foundation avoids proc-macro complexity
and ships the testable contract:

- `CrudSource` and `ResourceId`,
- CRUD payload types for create/save/remove,
- queued/outcome/status types,
- `WritePolicy` and transaction options,
- transaction binding API using `tx.with(resource)` publicly and `bind`
  internally,
- transaction lifecycle API using `TransactionRunner` and
  `TransactionOptions::run(...)`,
- helper APIs for optimistic rows,
- typed local resource views over `CollectionState<Row>`,
- resource registration against `SyncServer` through the existing
  `pocopine-sync` server plugin,
- snapshot pull through bounded `list(limit)` and row id extraction,
- source-owned atomic base-version checks for save/remove conflict
  detection,
- push routing for `create`, `save`, and `remove`,
- `CrudMutationLog` so accepted mutation ids are explicit and replayed
  mutation ids do not duplicate writes,
- exact replay validation so a reused mutation id with different
  operation, row id, or payload is rejected,
- non-macro `CrudClientResource` methods for create/save/remove,
- durable generated-id queueing for `QueueOffline`,
- online-confirmed generated-id push for `RequireOnline`,
- validation-only `#[resource(name = "...")]` macro scaffold for
  `CrudSource` impls,
- route-level integration coverage proving a CRUD resource registered
  with `SyncServer` serves `/pull` and `/push` through the normal sync
  plugin.

The remaining macro layer should come after this runtime contract is
stable:

- generated typed client resource module,
- generated create/save/remove client methods with good defaults,
- generated create/save/remove `_with_options` methods,
- generated `OpenOptions`-style fluent options methods for advanced call
  sites,
- generated `transaction_options().require_online().run(...)` helper,
- macro tests proving the generated code calls the same runtime
  contracts as the non-macro implementation.

The macro should generate CRUD/sync glue only. It must not generate SQL,
infer schema, or hide the app's database handle.
