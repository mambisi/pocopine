# Sync CRUD Macro API Contract

This document is the concrete author-facing contract for
`pocopine-sync-crud` resource macros. It shows what server code writes,
what client code sees, and how the generated module maps back to the
runtime sync and conflict-resolution layers.

The macro generates CRUD and sync glue only. It does not generate SQL,
schema, authorization rules, mutation-log storage, or conflict UI.

## Resource Definition

An app defines normal row and draft types, then implements `CrudSource`
for an app-owned repository type.

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Customer {
    pub id: uuid::Uuid,
    pub name: String,
    pub email: String,
    pub version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomerDraft {
    pub name: String,
    pub email: String,
}

#[derive(Clone)]
pub struct Customers {
    pub pool: sqlx::PgPool,
}

#[pocopine_sync_crud::resource(name = "customers")]
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
        let tenant_id = tenant_from(ctx)?;

        sqlx::query_as!(
            Customer,
            r#"
            select id, name, email, version
            from customers
            where tenant_id = $1
            order by name
            limit $2
            "#,
            tenant_id,
            limit as i64,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(sync_db_error)
    }

    async fn get(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
    ) -> pocopine_sync::SyncResult<Option<Customer>> {
        let tenant_id = tenant_from(ctx)?;

        sqlx::query_as!(
            Customer,
            r#"
            select id, name, email, version
            from customers
            where tenant_id = $1 and id = $2
            "#,
            tenant_id,
            id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(sync_db_error)
    }

    async fn create(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
    ) -> pocopine_sync::SyncResult<Customer> {
        let tenant_id = tenant_from(ctx)?;

        sqlx::query_as!(
            Customer,
            r#"
            insert into customers (tenant_id, id, name, email, version)
            values ($1, $2, $3, $4, 1)
            returning id, name, email, version
            "#,
            tenant_id,
            id,
            draft.name,
            draft.email,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(sync_db_error)
    }

    async fn save(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudWriteResult<Customer>> {
        let tenant_id = tenant_from(ctx.clone())?;
        let expected_version = base_version
            .as_ref()
            .map(|version| version.as_str().parse::<i64>())
            .transpose()
            .map_err(|err| pocopine_sync::SyncError::client(err.to_string()))?;

        let row = if let Some(expected_version) = expected_version {
            sqlx::query_as!(
                Customer,
                r#"
                update customers
                set name = $3, email = $4, version = version + 1
                where tenant_id = $1 and id = $2 and version = $5
                returning id, name, email, version
                "#,
                tenant_id,
                id,
                draft.name,
                draft.email,
                expected_version,
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(sync_db_error)?
        } else {
            sqlx::query_as!(
                Customer,
                r#"
                update customers
                set name = $3, email = $4, version = version + 1
                where tenant_id = $1 and id = $2
                returning id, name, email, version
                "#,
                tenant_id,
                id,
                draft.name,
                draft.email,
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(sync_db_error)?
        };

        match row {
            Some(row) => Ok(pocopine_sync_crud::CrudWriteResult::applied(row)),
            None => Ok(pocopine_sync_crud::CrudWriteResult::stale(
                self.get(ctx, id).await?,
            )),
        }
    }

    async fn remove(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudRemoveResult<Customer>> {
        let tenant_id = tenant_from(ctx.clone())?;
        let expected_version = base_version
            .as_ref()
            .map(|version| version.as_str().parse::<i64>())
            .transpose()
            .map_err(|err| pocopine_sync::SyncError::client(err.to_string()))?;

        let deleted = match expected_version {
            Some(expected_version) => {
                sqlx::query!(
                    "delete from customers where tenant_id = $1 and id = $2 and version = $3",
                    tenant_id,
                    id,
                    expected_version,
                )
                .execute(&self.pool)
                .await
                .map_err(sync_db_error)?
                .rows_affected()
            }
            None => {
                sqlx::query!(
                    "delete from customers where tenant_id = $1 and id = $2",
                    tenant_id,
                    id,
                )
                .execute(&self.pool)
                .await
                .map_err(sync_db_error)?
                .rows_affected()
            }
        };

        if deleted == 1 {
            Ok(pocopine_sync_crud::CrudRemoveResult::applied())
        } else {
            Ok(pocopine_sync_crud::CrudRemoveResult::stale(
                self.get(ctx, id).await?,
            ))
        }
    }
}
```

The associated types are the shared contract:

- `Id` is the app-visible resource id and maps to a sync row key.
- `Row` is the rendered/canonical row shape stored in `CollectionState<Row>`.
- `Draft` is the write payload sent by generated create/save methods.

The `tenant_from` and `sync_db_error` helpers above are app-owned code.
`RowVersion` is an opaque validated string newtype, so apps with numeric
database versions parse `row_version.as_str()` at their database boundary.

## Generated Module

For the example above, the macro generates a sibling `customers` module.
If the sync name is not a valid Rust module identifier, use
`#[pocopine_sync_crud::resource(name = "tenant-customers", module = customers)]`.

Conceptually, authors can treat the generated code as this shape:

```rust
pub mod customers {
    #[allow(unused_imports)]
    use super::*;

    pub const NAME: &str = "customers";

    pub type Id = uuid::Uuid;
    pub type Row = Customer;
    pub type Draft = CustomerDraft;

    pub type CreateOptions = pocopine_sync_crud::CreateOptions<Row>;
    pub type SaveOptions = pocopine_sync_crud::SaveOptions<Row>;
    pub type RemoveOptions = pocopine_sync_crud::RemoveOptions;
    pub type View = pocopine_sync_crud::LocalResourceView<Id, Row>;
    pub type ViewState = pocopine_sync_crud::LocalResourceViewState<Id, Row>;
    pub type Outcome = pocopine_sync_crud::CrudOutcome<Id, Row>;
    pub type Queued = pocopine_sync_crud::Queued<Id>;
    pub type Client<C> = pocopine_sync_crud::CrudClientResource<C, Id, Row>;

    pub struct Resource<C: 'static> {
        sync: pocopine_sync::SyncClient,
        handle: pocopine_sync::Handle<C>,
        selector: pocopine_sync::CollectionSelector<C, Row>,
    }

    impl<C: 'static> Resource<C>
    where
        Row: 'static,
    {
        pub fn new(
            sync: &pocopine_sync::SyncClient,
            handle: pocopine_sync::Handle<C>,
            selector: pocopine_sync::CollectionSelector<C, Row>,
        ) -> Self;

        pub fn collection(&self) -> pocopine_sync::SyncResult<pocopine_sync::SyncCollection<C, Row>>;
        pub fn open(&self) -> pocopine_sync::SyncResult<()>;
        pub fn pull(&self) -> pocopine_sync::SyncResult<()>;
        pub fn view(&self) -> pocopine_sync::SyncResult<pocopine_sync_crud::LocalResourceView<Id, Row>>;
        pub fn client(&self) -> pocopine_sync::SyncResult<Client<C>>;

        pub async fn create(&self, id: Id, draft: Draft) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn create_with_options(
            &self,
            id: Id,
            draft: Draft,
            options: CreateOptions,
        ) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn save(&self, id: Id, draft: Draft) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn save_with_options(
            &self,
            id: Id,
            draft: Draft,
            options: SaveOptions,
        ) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn remove(&self, id: Id) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn remove_with_options(
            &self,
            id: Id,
            options: RemoveOptions,
        ) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn use_server(&self, id: Id) -> pocopine_sync::SyncResult<bool>;
        pub fn observe_view<F>(&self, callback: F) -> pocopine_sync::SyncResult<()>
        where
            Id: pocopine_sync_crud::ResourceId,
            Row: Clone,
            F: Fn(&ViewState, Option<&ViewState>) + 'static;
        pub async fn retry_local(
            &self,
            id: Id,
            draft: Draft,
        ) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn retry_local_with_options(
            &self,
            id: Id,
            draft: Draft,
            options: SaveOptions,
        ) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn merge_with(
            &self,
            id: Id,
            draft: Draft,
        ) -> pocopine_sync::SyncResult<Outcome>;
        pub async fn merge_with_options(
            &self,
            id: Id,
            draft: Draft,
            options: SaveOptions,
        ) -> pocopine_sync::SyncResult<Outcome>;
    }

    pub fn use_resource<C: 'static>(
        sync: &pocopine_sync::SyncClient,
        handle: pocopine_sync::Handle<C>,
        selector: pocopine_sync::CollectionSelector<C, Row>,
    ) -> Resource<C>;

    #[cfg(not(target_arch = "wasm32"))]
    pub fn resource(
        source: Customers,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudResourceBuilder<Customers>>;

    pub fn new_id() -> pocopine_sync::SyncResult<Id>;
    pub fn view(
        state: &pocopine_sync::CollectionState<Row>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::LocalResourceView<Id, Row>>;
    pub fn client<C: 'static>(
        collection: pocopine_sync::SyncCollection<C, Row>,
        state: &pocopine_sync::CollectionState<Row>,
    ) -> pocopine_sync::SyncResult<Client<C>>;
    pub fn collection<C: 'static>(
        sync: &pocopine_sync::SyncClient,
        handle: pocopine_sync::Handle<C>,
        selector: pocopine_sync::CollectionSelector<C, Row>,
    ) -> pocopine_sync::SyncResult<pocopine_sync::SyncCollection<C, Row>>;
}
```

The generated module copies the associated type right-hand sides from the
`CrudSource` impl. That keeps the shared aliases visible to wasm client
code even though `CrudSource` itself is server-only.

The macro gates the original `CrudSource` impl to server targets. If the
source type itself contains server-only fields such as `sqlx::PgPool`,
the app must also gate that source type or keep it in a server-only
module. Shared row and draft types should remain available to both
targets.

`Resource<C>` is an ergonomic wrapper around three existing runtime
pieces: a cloned `SyncClient`, the component/store `Handle<C>`, and the
selector into `CollectionState<Row>`. It rebuilds the cheap
`SyncCollection` and typed `LocalResourceView` when each operation runs.
The lower-level `collection(...)` and `client(...)` functions remain
available as escape hatches and tests can still target the runtime layer
directly.

The generated helpers use `pocopine_sync::Handle`, re-exported from
`pocopine-core`, so apps can depend on `pocopine-sync` without requiring
the umbrella `pocopine` crate in generated code. Client components will
usually pass `pocopine::this::<Self>()`, whose type is the same handle.

`view(...)` and `client(...)` are fallible because the typed view checks
that existing sync rows can be converted back into the resource `Id`. The
generated `Resource::view()` and `Resource::client()` wrappers can also
return a borrow-contention error if the same component/store state is
already borrowed. Other failures usually mean the app changed its id
encoding or loaded rows for the wrong resource type.

## Server API

Server setup calls the generated module's server helper, then supplies the
production hooks explicitly:

```rust
pub async fn build_server(pool: sqlx::PgPool) -> anyhow::Result<pocopine_server::Server> {
    let customers = customers::resource(Customers { pool: pool.clone() })?
        .id(|row: &Customer| row.id)
        .version(|row: &Customer| row.version)
        .transactional(
            pocopine_sync_sqlx::postgres(pool.clone()),
            pocopine_sync_sqlx::SqlxCrudMutationLog::<sqlx::Postgres>::new(tenant_scope),
        );

    let sync = pocopine_sync::SyncServer::builder()
        .guarded_stream(customers, pocopine_auth::require_auth())
        .events(live_backend(pool.clone()))
        .build();

    Ok(pocopine_server::Server::new(router())
        .plugin(pocopine_sync::sync_server_plugin(sync))
        .try_finalize()?)
}
```

What the server sees:

- `customers::resource(source)` registers stream name `"customers"`.
- `.id(...)` maps each returned row to the resource id and sync row key.
- `.version(...)` maps each returned row to the canonical row version.
- `.transactional(...)` binds source writes and accepted mutation ids to
  one database transaction.
- `SyncServer::builder().guarded_stream(...)` owns the auth boundary.

What the server handles at runtime:

1. `/open` discovers the guarded stream and checks the guard.
2. `/pull` calls `CrudSource::list` and returns canonical rows.
3. `/push` deserializes the CRUD payload envelope.
4. On the transactional path, replayed mutation ids are deduped by the
   mutation log in the same transaction boundary used for writes.
5. `save` and `remove` pass the client `base_version` into the source so
   the source can compare and write atomically inside the same transaction.
6. Accepted writes record the mutation id before commit, and live
   invalidation is published after commit.
7. Stale writes return `Conflict` with the server row when available.
8. Invalid payloads, auth failures, and domain validation failures return `Rejected`.

The server resource helper intentionally does not generate transactions
or SQL. Production code should use `.transactional(...)` and implement
`CrudTransactionRunner`, `TransactionalCrudSource`, and
`TransactionalCrudMutationLog` against the same database and tenant scope
as the source query. The accepted-mutation table needs a unique key over
that scope and `mutation_id`; the lookup alone is not a concurrency
boundary.

For SQLx-backed resources, `pocopine-sync-sqlx` supplies the transaction
runner and durable accepted-mutation log helper. Apps still implement
`CrudSource` and `TransactionalCrudSource` with normal backend-specific
SQLx queries. See [`sync-sqlx.md`](./sync-sqlx.md).

## Client API

Client components keep normal `CollectionState<Row>` fields. The
generated module binds that state to the sync client and typed CRUD
runtime.

```rust
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Customer {
    pub id: uuid::Uuid,
    pub name: String,
    pub email: String,
    pub version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomerDraft {
    pub name: String,
    pub email: String,
}

#[derive(Default)]
pub struct CustomersPage {
    pub customers: pocopine_sync::CollectionState<Customer>,
    pub name: String,
    pub email: String,
    pub status: String,
    pub error: Option<String>,
}

fn customers_state(
    page: &mut CustomersPage,
) -> &mut pocopine_sync::CollectionState<Customer> {
    &mut page.customers
}

#[handlers]
impl CustomersPage {
    fn customers(&self) -> customers::Resource<Self> {
        customers::use_resource(
            &self.plugin::<pocopine_sync::SyncClient>(),
            pocopine::this::<Self>(),
            customers_state,
        )
    }

    pub fn on_mount(&mut self) {
        if let Err(err) = self.customers().open() {
            self.error = Some(err.to_string());
        }
    }

    pub fn create_customer(&mut self) {
        let id = match customers::new_id() {
            Ok(id) => id,
            Err(err) => {
                self.error = Some(err.to_string());
                return;
            }
        };

        let draft = CustomerDraft {
            name: self.name.clone(),
            email: self.email.clone(),
        };
        let optimistic = Customer {
            id,
            name: draft.name.clone(),
            email: draft.email.clone(),
            version: 0,
        };
        let customers = self.customers();

        dispatch!(
            customers
                .create_with_options(
                    id,
                    draft,
                    customers::CreateOptions::new().optimistic(optimistic),
                )
                .await,
            |state, result| {
                state.handle_customer_outcome(result);
            }
        );
    }

    pub fn save_customer(&mut self, row: Customer) {
        let draft = CustomerDraft {
            name: self.name.clone(),
            email: self.email.clone(),
        };
        let optimistic = Customer {
            name: draft.name.clone(),
            email: draft.email.clone(),
            ..row.clone()
        };
        let customers = self.customers();

        dispatch!(
            customers
                .save_with_options(
                    row.id,
                    draft,
                    customers::SaveOptions::new().optimistic(optimistic),
                )
                .await,
            |state, result| {
                state.handle_customer_outcome(result);
            }
        );
    }

    pub fn remove_customer(&mut self, id: uuid::Uuid) {
        let customers = self.customers();

        dispatch!(customers.remove(id).await, |state, result| {
            state.handle_customer_outcome(result);
        });
    }

    fn handle_customer_outcome(
        &mut self,
        result: pocopine_sync::SyncResult<customers::Outcome>,
    ) {
        match result {
            Ok(pocopine_sync_crud::CrudOutcome::Queued(queued)) => {
                self.status = format!("queued {}", queued.mutation_id);
                self.error = None;
            }
            Ok(pocopine_sync_crud::CrudOutcome::Accepted { id, row }) => {
                self.status = format!("saved {id}: {}", row.name);
                self.error = None;
            }
            Ok(pocopine_sync_crud::CrudOutcome::Removed { id }) => {
                self.status = format!("removed {id}");
                self.error = None;
            }
            Ok(pocopine_sync_crud::CrudOutcome::Rejected { id, reason }) => {
                self.status = format!("rejected {id}");
                self.error = Some(reason);
            }
            Ok(pocopine_sync_crud::CrudOutcome::Conflict {
                id,
                server_row,
                reason,
            }) => {
                self.status = format!("conflict {id}");
                self.error = Some(reason);
                if let Some(server_row) = server_row {
                    self.name = server_row.name;
                    self.email = server_row.email;
                }
            }
            Err(err) => {
                self.status = "sync error".to_string();
                self.error = Some(err.to_string());
            }
        }
    }
}
```

What the client sees:

- `customers::use_resource(sync, handle, selector)` is the normal entry point.
- `Resource::open()` and `Resource::pull()` apply the generated stream name.
- `Resource::view()` returns typed rendered rows, pending flags, conflicts, and canonical base versions.
- `Resource::create/save/remove` use `WritePolicy::QueueOffline` defaults.
- `Resource::*_with_options` lets callers set optimistic rows, explicit base versions, or `RequireOnline`.
- `Resource::use_server` clears a local conflict marker after the user accepts the known server row.
- `Resource::retry_local` and `Resource::merge_with` queue a new save against the latest canonical base version.
- `customers::collection(...)` and `customers::client(...)` remain lower-level escape hatches.
- `customers::new_id()` creates an offline-capable id when the id type supports local id generation.
- `customers::Outcome` is the typed create/save/remove result.

What the client handles at runtime:

1. `open()` hydrates cached canonical rows and pending mutations from the local store, then pulls the server snapshot.
2. `view()` exposes rendered rows, pending flags, conflicts, and canonical `base_version` values.
3. `QueueOffline` reserves a durable mutation id and returns `CrudOutcome::Queued` after local enqueue, not after server acceptance.
4. `RequireOnline` waits for `/push` and returns accepted, removed, rejected, or conflict outcomes from the server response.
5. `save` and `remove` default `base_version` from `LocalResourceView::base_version(&id)`.
6. A pull received while local writes are pending updates canonical rows first, then replays pending local overlays over the new canonical base.
7. Rejections roll back to the latest canonical row.
8. Conflicts keep user-visible data available and mark the row so the app can show explicit resolution UI.
9. `use_server` clears only the local conflict marker; it does not purge queued pending mutations for that row.
10. `retry_local` and `merge_with` leave the conflict marker visible until the server accepts the new save.

## Outcome Mapping

| Server result | Push response | Online client outcome |
| --- | --- | --- |
| `create -> Row` | accepted mutation plus returned row | `Accepted { id, row }` |
| `save -> CrudWriteResult::Applied(row)` | accepted mutation plus returned row | `Accepted { id, row }` |
| `remove -> CrudRemoveResult::Applied` | accepted mutation without a row | `Removed { id }` |
| `CrudWriteResult::Conflict(conflict)` | conflict with optional server row and reason | `Conflict { id, server_row, reason }` |
| `CrudRemoveResult::Conflict(conflict)` | conflict with optional server row and reason | `Conflict { id, server_row, reason }` |
| malformed payload, auth failure, replay mismatch, or domain rejection | rejected mutation with reason | `Rejected { id, reason }` |

## Conflict Contract

Generated CRUD does not silently merge conflicts. The contract is:

- accepted write: clear pending state and apply the returned canonical row,
- accepted remove: clear pending state and remove the row,
- rejected write: drop the pending mutation and rebase to canonical rows,
- conflicted write: drop the pending mutation, mark the row conflicted, and expose the server row when supplied,
- offline write: keep the optimistic row visible and durable until replay resolves it.

This preserves the local-first invariant documented in
`sync-conflict-architecture.md`:

```text
rendered rows = canonical server rows + pending local overlay
```

The shipped helper surface is intentionally conservative:

```rust
customers.use_server(id).await?;
customers.retry_local(id, draft).await?;
customers.merge_with(id, merged_draft).await?;
```

`use_server` clears only the local conflict marker and keeps the known
server row. `retry_local` and `merge_with` queue a new save through the
same mutation lifecycle as `save`, using the latest canonical
`base_version` by default. They do not hide the conflict until the server
accepts the retry.

`discard_local` is not generated yet. That name implies a durable
row-scoped pending queue purge, and the current local-store contract does
not expose that operation. Future fluent options builders and transaction
convenience methods must still map back to explicit sync mutations and
must not overwrite newer server data by default.
