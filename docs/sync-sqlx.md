# SQLx Sync Helpers

`pocopine-sync-sqlx` is the host/server SQLx adapter for
`pocopine-sync-crud`. Add it as a direct host/server dependency when a
CRUD resource uses SQLx transactions. It gives production CRUD resources
a SQLx-backed transaction runner and a durable accepted-mutation log
helper.

The crate is not an ORM and does not generate application SQL. Apps still
own schema, migrations, indexes, authorization predicates, conflict
checks, and SQLx queries.

## What Ships

The first SQLx slice contains:

- `SqlxCrudTransactionRunner<DB>` for SQLx `Pool<DB>` transactions,
- backend aliases and constructors behind SQLx features:
  `postgres(...)`, `mysql(...)`, and `sqlite(...)`,
- `SqlxCrudMutationLog<DB>` for durable mutation replay idempotency,
- default mutation-log schema constants for SQLite, Postgres, and MySQL,
- SQLite integration tests for commit, rollback, scoped lookup,
  reservation, and durable replay.

The transaction runner is generic over `DB: sqlx::Database` because SQLx
has a generic `sqlx::Transaction<'c, DB>` type. The mutation log has
backend-specific impls behind `postgres`, `mysql`, and `sqlite` features
because SQL placeholders and production DDL details are backend-specific.

Application CRUD sources are still backend-specific. Postgres, MySQL, and
SQLite differ on `RETURNING`, placeholder syntax, JSON/native text
storage, upsert behavior, and lock/isolation behavior. Pocopine should
not hide those differences behind a fake portable SQL layer.

## Dependencies

The `postgres`, `mysql`, and `sqlite` features enable the corresponding
SQLx backend plus `runtime-tokio` for the helper crate. The app's own
`sqlx` dependency should still enable the backend, runtime, TLS, and
`macros` features it uses directly.

Postgres example:

```toml
[dependencies]
pocopine-sync = "..."
pocopine-sync-crud = "..."
pocopine-sync-sqlx = { version = "...", features = [
  "postgres",
  "tls-rustls",
] }
sqlx = { version = "0.8", features = [
  "postgres",
  "runtime-tokio",
  "tls-rustls-ring-webpki",
  "macros",
] }
```

SQLite test/native example:

```toml
pocopine-sync-sqlx = { version = "...", features = [
  "sqlite",
] }
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }
```

MySQL example:

```toml
pocopine-sync-sqlx = { version = "...", features = [
  "mysql",
  "tls-rustls",
] }
sqlx = { version = "0.8", features = [
  "mysql",
  "runtime-tokio",
  "tls-rustls-ring-webpki",
  "macros",
] }
```

## Mutation Log Schema

The accepted-mutation log stores the sync replay key. It does not store
application rows and it is not the source of truth for reads.

Default SQLite/Postgres shape:

```sql
create table if not exists __pocopine_crud_mutations (
  scope text not null,
  mutation_id text not null,
  op text not null,
  row_key text,
  payload text not null,
  primary key (scope, mutation_id)
);
```

Default MySQL shape:

```sql
create table if not exists __pocopine_crud_mutations (
  scope varchar(191) not null,
  mutation_id varchar(191) not null,
  op varchar(16) not null,
  row_key varchar(191),
  payload text not null,
  primary key (scope, mutation_id)
);
```

The MySQL default uses bounded indexed columns because MySQL cannot use
unbounded `text` columns as a primary key. Apps with longer mutation ids
or scope values should create an app-specific table and point
`SqlxCrudMutationLog::with_table(...)` at it. The default MySQL schema is
therefore suitable for bounded application ids, not for the full 1024-byte
sync identifier envelope. Use an app-specific hash/binary key or shorter
canonical ids when full-length values are possible.

The `scope` value must match the authorization domain of the resource:
tenant id, organization id, account id, or another app-owned partition.
Do not use one global scope for multi-tenant data.

## Server Wiring

```rust
use pocopine_auth::RequestContext;
use pocopine_sync::{SyncError, SyncResult};
use pocopine_sync_crud::TransactionalCrudSource;
use pocopine_sync_sqlx::{postgres, SqlxCrudMutationLog};

#[derive(Clone)]
pub struct Customers {
    pub pool: sqlx::PgPool,
}

fn tenant_scope(ctx: &RequestContext) -> SyncResult<String> {
    ctx.require_user()
        .map(|user| user.id.clone())
        .map_err(|_| SyncError::client("sync tenant scope requires an authenticated user"))
}

pub fn customers_stream(
    pool: sqlx::PgPool,
) -> SyncResult<impl pocopine_sync::SyncStreamSource> {
    let runner = postgres(pool.clone());
    let mutation_log = SqlxCrudMutationLog::<sqlx::Postgres>::new(tenant_scope);

    customers::resource(Customers { pool })?
        .id(|row: &Customer| row.id)
        .version(|row: &Customer| row.version)
        .transactional(runner, mutation_log)
}
```

The generated `customers::resource(...)` helper comes from
`pocopine-sync-crud`. The SQLx crate supplies only the transaction runner
and mutation log. The app still implements `CrudSource` for reads and
`TransactionalCrudSource<sqlx::Transaction<'static, sqlx::Postgres>>`
for writes.

## Transactional Source Shape

The app-owned source writes normal SQLx queries against the active
transaction handle:

```rust
#[pocopine_sync_crud::async_trait]
impl TransactionalCrudSource<sqlx::Transaction<'static, sqlx::Postgres>>
    for Customers
{
    async fn create_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        ctx: &RequestContext,
        id: uuid::Uuid,
        draft: CustomerDraft,
    ) -> SyncResult<Customer> {
        let tenant_id = tenant_scope(ctx)?;

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
        .fetch_one(&mut **tx)
        .await
        .map_err(pocopine_sync_sqlx::sync_sqlx_error)
    }

    // save_in_tx and remove_in_tx perform the same base_version checks
    // as the non-transactional CrudSource methods, but against `tx`.
}
```

The `&mut **tx` form is the SQLx 0.8 pattern for executing against the
connection inside a borrowed `Transaction`.

## Runtime Flow

For each pushed mutation on a transactional CRUD resource:

```text
begin SQLx transaction
  -> reserve accepted mutation by (scope, mutation_id)
  -> if already accepted, acknowledge exact replay or reject changed contents
  -> apply create/save/remove through TransactionalCrudSource
  -> commit accepted writes
  -> roll back conflicts, rejections, and backend errors
publish live invalidation after commit
```

The reservation is inserted before the source write. If the source write
fails, conflicts, or rejects the mutation, the transaction rolls back and
the reservation disappears. If a duplicate retry races with the original
write, the primary key on `(scope, mutation_id)` is the concurrency
boundary; the loser reads the already accepted mutation and only an exact
payload replay is acknowledged.

## Backend Notes

Postgres:

- feature: `postgres`,
- placeholders: `$1`, `$2`, etc.,
- `RETURNING` is the preferred write path,
- `text` primary key columns are acceptable for the default schema.

SQLite:

- feature: `sqlite`,
- placeholders: `?`,
- used for local crate tests with an in-memory database,
- useful for native apps, but browser-local sync still uses
  `pocopine-sync-sqlite`, not SQLx.

MySQL:

- feature: `mysql`,
- placeholders: `?`,
- default schema uses bounded indexed columns,
- apps often need a write followed by a select inside the same
  transaction instead of relying on Postgres-style `RETURNING`.

## Non-Goals

`pocopine-sync-sqlx` does not:

- generate SQL or migrations,
- infer tenant predicates,
- prove authorization correctness,
- replace app-level idempotency for payments, inventory, emails, or
  third-party side effects,
- provide database changefeed/CDC adapters.

Changefeed adapters are a later roadmap item. This crate only makes the
current CRUD mutation path durable and easier to wire with SQLx.
