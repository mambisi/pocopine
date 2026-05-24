# Sync CRUD Macro API Contract

This document is the concrete author-facing contract for the
`pocopine-sync-crud` resource macro. It explains what the server sees,
what the client sees, and how both sides map back to the non-macro sync
runtime.

The macro generates CRUD and sync glue only. It does not generate SQL,
schema, authorization rules, mutation-log storage, or conflict UI.

## Resource Definition

An app defines normal row and draft types, then implements `CrudSource`
for its own repository type.

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
        let expected_version = parse_customer_version(base_version.as_ref())?;

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
            None => {
                let server_row = self.get(ctx, id).await?;
                Ok(pocopine_sync_crud::CrudWriteResult::stale(server_row))
            }
        }
    }

    async fn remove(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: uuid::Uuid,
        base_version: Option<pocopine_sync::RowVersion>,
    ) -> pocopine_sync::SyncResult<pocopine_sync_crud::CrudRemoveResult<Customer>> {
        let tenant_id = tenant_from(ctx.clone())?;
        let expected_version = parse_customer_version(base_version.as_ref())?;

        let deleted = if let Some(expected_version) = expected_version {
            sqlx::query!(
                r#"
                delete from customers
                where tenant_id = $1 and id = $2 and version = $3
                "#,
                tenant_id,
                id,
                expected_version,
            )
            .execute(&self.pool)
            .await
            .map_err(sync_db_error)?
            .rows_affected()
        } else {
            sqlx::query!(
                "delete from customers where tenant_id = $1 and id = $2",
                tenant_id,
                id,
            )
            .execute(&self.pool)
            .await
            .map_err(sync_db_error)?
            .rows_affected()
        };

        if deleted == 1 {
            Ok(pocopine_sync_crud::CrudRemoveResult::applied())
        } else {
            let server_row = self.get(ctx, id).await?;
            Ok(pocopine_sync_crud::CrudRemoveResult::stale(server_row))
        }
    }
}
```

The associated types are the shared contract:

- `Id` is the app-visible resource id and maps to a sync row key.
- `Row` is the rendered/canonical row shape stored in
  `CollectionState<Row>`.
- `Draft` is the write payload sent by generated create/save methods.

The `tenant_from`, `sync_db_error`, and `parse_customer_version` helpers
above are app-owned code. The macro neither generates nor requires those
helpers. `RowVersion` is an opaque validated string newtype, so apps with
numeric database versions should parse `row_version.as_str()` at their
database boundary.

## Generated Module

For the example above, the macro generates a sibling `customers` module.
Conceptually, authors can treat it as if this code existed:

If the sync name is not a valid Rust module identifier, use
`#[pocopine_sync_crud::resource(name = "tenant-customers", module = customers)]`.

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
    pub type Outcome = pocopine_sync_crud::CrudOutcome<Id, Row>;
    pub type Queued = pocopine_sync_crud::Queued<Id>;
    pub type Client<C> = pocopine_sync_crud::CrudClientResource<C, Id, Row>;

    #[cfg(not(target_arch = "wasm32"))]
    pub fn resource(
        source: Customers,
    ) -> pocopine_sync::SyncResult<
        pocopine_sync_crud::CrudResourceBuilder<Customers>,
    > {
        pocopine_sync_crud::resource(NAME, source)
    }

    pub fn new_id() -> pocopine_sync::SyncResult<Id> {
        pocopine_sync_crud::new_id()
    }

    pub fn view(
        state: &pocopine_sync::CollectionState<Row>,
    ) -> pocopine_sync::SyncResult<
        pocopine_sync_crud::LocalResourceView<Id, Row>,
    > {
        pocopine_sync_crud::local_resource_view(state)
    }

    pub fn client<C: 'static>(
        collection: pocopine_sync::SyncCollection<C, Row>,
        state: &pocopine_sync::CollectionState<Row>,
    ) -> pocopine_sync::SyncResult<Client<C>> {
        let view = view(state)?;
        Ok(pocopine_sync_crud::client_resource(collection, view))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn collection<C: 'static>(
        sync: &pocopine_sync::SyncClient,
        handle: pocopine::Handle<C>,
        selector: pocopine_sync::CollectionSelector<C, Row>,
    ) -> pocopine_sync::SyncResult<pocopine_sync::SyncCollection<C, Row>> {
        sync.collection(handle, selector).stream(NAME)
    }
}
```

The generated module does not reference `CrudSource` in its shared type
aliases. It copies the associated type right-hand sides from the impl so
the same module can be visible to wasm client code even though
`CrudSource` itself is server-only.

The generated macro output also gates the original `CrudSource` impl to
server targets. That lets authors keep the resource definition in a
shared crate without making the browser compile server-only traits,
database clients, or `async_trait`.

Only the impl is gated by the macro. If the source type itself contains
server-only fields such as `sqlx::PgPool`, the app must also gate that
source type or keep it in a server-only module. Shared row and draft
types should remain available to both targets.

`Client<C>` and `client(...)` are portable type/runtime helpers. Only
`collection(...)` is wasm-only because it references `pocopine::Handle`
and the browser sync plugin. `client(...)` owns the `LocalResourceView`
snapshot it builds from `&CollectionState<Row>`; the returned
`CrudClientResource` does not borrow the component state and can be moved
into an async `dispatch!(...).await` future. State changes still flow
through the owned `SyncCollection<C, Row>`, which carries the component
handle and selector.

`view(...)` and `client(...)` are fallible because the typed view checks
that existing sync rows can be converted back into the resource `Id`.
An empty collection is valid. Failures usually mean the app changed its
id encoding or loaded rows for the wrong resource type.

The generated `new_id()` wrapper specializes the runtime helper to the
resource `Id`, so callers never write turbofish syntax.

The generated `collection(...)` helper assumes the client crate depends
on the umbrella `pocopine` crate because it names `pocopine::Handle`.
Apps that build on `pocopine-sync` without the umbrella can still call
`SyncClient::collection(...).stream(customers::NAME)` directly, then
pass the returned streamed collection to `customers::client(...)`.

Generated outcome aliases expose the runtime fields exactly:

```rust
pub struct Queued<Id> {
    pub mutation_id: pocopine_sync::MutationId,
    pub id: Id,
    pub status: pocopine_sync_crud::QueuedStatus,
}

pub enum Outcome {
    Queued(Queued),
    Accepted { id: Id, row: Row },
    Removed { id: Id },
    Rejected { id: Id, reason: String },
    Conflict {
        id: Id,
        server_row: Option<Row>,
        reason: String,
    },
}
```

## Server API

Server setup calls the generated module's server resource helper, then
keeps the explicit production hooks visible:

```rust
pub async fn build_server(pool: sqlx::PgPool) -> anyhow::Result<pocopine_server::Server> {
    let customers = customers::resource(Customers { pool: pool.clone() })?
        .id(|row: &Customer| row.id)
        .version(|row: &Customer| row.version)
        .mutation_log(CustomerMutationLog { pool: pool.clone() });

    let sync = pocopine_sync::SyncServer::builder()
        .guarded_stream(customers, pocopine_auth::require_auth())
        .events(live_backend(pool.clone()))
        .build();

    let router = pocopine_server::axum::Router::new();

    Ok(pocopine_server::Server::new(router)
        .plugin(pocopine_sync::sync_server_plugin(sync))
        .try_finalize()?)
}
```

What the server sees:

- `customers::resource(source)` registers stream name `"customers"`.
- `.id(...)` maps each returned row to the resource id and sync row key.
- `.version(...)` maps each returned row to the canonical row version.
- `.mutation_log(...)` provides idempotency for accepted mutation ids.
- `SyncServer::builder().guarded_stream(...)` owns the auth boundary.

What the server handles at runtime:

1. `/open` discovers the guarded stream and checks the guard.
2. `/pull` calls `CrudSource::list` and returns canonical rows.
3. `/push` deserializes the CRUD payload envelope.
4. Replayed mutation ids are deduped by the mutation log.
5. `save` and `remove` compare the client `base_version` to the latest
   canonical version before calling app write code.
6. Accepted writes record the mutation id and publish live invalidation
   after commit.
7. Stale writes return `Conflict` with the server row when available.
8. Invalid payloads, auth failures, and domain validation failures return
   `Rejected`.

Server write results map to client outcomes through the sync push
response:

| Server result | Push response | Online client outcome |
| --- | --- | --- |
| `create -> Row` | accepted mutation plus returned row | `Accepted { id, row }` |
| `save -> CrudWriteResult::Applied(row)` | accepted mutation plus returned row | `Accepted { id, row }` |
| `remove -> CrudRemoveResult::Applied` | accepted mutation without a row | `Removed { id }` |
| `CrudWriteResult::Conflict(conflict)` | conflict with optional server row and reason | `Conflict { id, server_row, reason }` |
| `CrudRemoveResult::Conflict(conflict)` | conflict with optional server row and reason | `Conflict { id, server_row, reason }` |
| malformed payload, auth failure, replay mismatch, or domain rejection | rejected mutation with reason | `Rejected { id, reason }` |

The server resource helper intentionally does not hide `.mutation_log`.
Production idempotency must stay tied to the same database and tenant
scope as the source query.

## Client API

Client components keep normal `CollectionState<Row>` fields. The
generated module binds that state to the sync client and the typed CRUD
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
    pub fn on_mount(&mut self) {
        let result = customers::collection(
            &self.plugin::<pocopine_sync::SyncClient>(),
            pocopine::this::<Self>(),
            customers_state,
        )
        .and_then(|collection| collection.open());

        if let Err(err) = result {
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

        let collection = match customers::collection(
            &self.plugin::<pocopine_sync::SyncClient>(),
            pocopine::this::<Self>(),
            customers_state,
        ) {
            Ok(collection) => collection,
            Err(err) => {
                self.error = Some(err.to_string());
                return;
            }
        };

        let handle = match customers::client(collection, &self.customers) {
            Ok(handle) => handle,
            Err(err) => {
                self.error = Some(err.to_string());
                return;
            }
        };

        dispatch!(
            handle
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

        let result = customers::collection(
            &self.plugin::<pocopine_sync::SyncClient>(),
            pocopine::this::<Self>(),
            customers_state,
        )
        .and_then(|collection| customers::client(collection, &self.customers));

        let handle = match result {
            Ok(handle) => handle,
            Err(err) => {
                self.error = Some(err.to_string());
                return;
            }
        };

        dispatch!(
            handle
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
        let result = customers::collection(
            &self.plugin::<pocopine_sync::SyncClient>(),
            pocopine::this::<Self>(),
            customers_state,
        )
        .and_then(|collection| customers::client(collection, &self.customers));

        let handle = match result {
            Ok(handle) => handle,
            Err(err) => {
                self.error = Some(err.to_string());
                return;
            }
        };

        dispatch!(handle.remove(id).await, |state, result| {
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

- `customers::collection(...)` applies the generated stream name.
- `customers::client(collection, &state)` builds the typed runtime handle.
- `customers::new_id()` creates an offline-capable id when the id type
  supports local id generation.
- `customers::CreateOptions`, `SaveOptions`, and `RemoveOptions` are
  row-specialized aliases for the runtime options.
- `customers::Outcome` is the typed create/save/remove result.
- `customers::Queued` carries `mutation_id`, `id`, and `QueuedStatus` so
  UI can show pending state without parsing protocol strings.

What the client handles at runtime:

1. `collection.open()` hydrates cached canonical rows and pending
   mutations from the local store, then pulls the server snapshot.
2. `customers::view(&state)` exposes rendered rows, pending flags,
   conflicts, and canonical `base_version` values.
3. `QueueOffline` reserves a durable mutation id and returns
   `CrudOutcome::Queued` after local enqueue, not after server acceptance.
4. `RequireOnline` waits for `/push` and returns accepted, removed,
   rejected, or conflict outcomes from the server response.
5. `save` and `remove` default `base_version` from
   `LocalResourceView::base_version(&id)`.
6. A pull received while local writes are pending updates canonical rows
   first, then replays pending local overlays over the new canonical base.
7. Rejections roll back to the latest canonical row.
8. Conflicts keep user-visible data available and mark the row so the app
   can show explicit resolution UI.

## Conflict Contract

Generated CRUD does not silently merge conflicts. The first contract is:

- accepted write: clear pending state and apply the returned canonical row,
- accepted remove: clear pending state and remove the row,
- rejected write: drop the pending mutation and rebase to canonical rows,
- conflicted write: drop the pending mutation, mark the row conflicted,
  and expose the server row when the server supplied one,
- offline write: keep the optimistic row visible and durable until replay
  resolves it.

This preserves the local-first invariant documented in
`sync-conflict-architecture.md`:

```text
rendered rows = canonical server rows + pending local overlay
```

The generated API can add higher-level helpers later, such as
`retry_local`, `use_server`, and `merge_with`, but those helpers must map
back to explicit sync mutations and must not overwrite newer server data
by default. Those helpers are out of scope for the first generated macro
slice.
