use std::{fmt, marker::PhantomData, sync::Arc};

use pocopine_auth::RequestContext;
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
use pocopine_sync::{MutationId, RowKey, SyncOp};
use pocopine_sync::{SyncError, SyncResult};
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
use pocopine_sync_crud::{
    CrudAcceptedMutation, CrudMutationReservation, TransactionalCrudMutationLog,
};
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
use serde_json::Value;
use sqlx::Database;
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
use sqlx::{ColumnIndex, Decode, Row, Transaction, Type};

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
use crate::sync_sqlx_error;

/// Default table used by [`SqlxCrudMutationLog`].
pub const DEFAULT_CRUD_MUTATION_LOG_TABLE: &str = "__pocopine_crud_mutations";

/// SQLite schema for the default CRUD mutation log table.
pub const SQLITE_CRUD_MUTATION_LOG_SCHEMA: &str = r#"create table if not exists __pocopine_crud_mutations (
  scope text not null,
  mutation_id text not null,
  op text not null,
  row_key text,
  payload text not null,
  primary key (scope, mutation_id)
)"#;

/// Postgres schema for the default CRUD mutation log table.
pub const POSTGRES_CRUD_MUTATION_LOG_SCHEMA: &str = r#"create table if not exists __pocopine_crud_mutations (
  scope text not null,
  mutation_id text not null,
  op text not null,
  row_key text,
  payload text not null,
  primary key (scope, mutation_id)
)"#;

/// MySQL schema for the default CRUD mutation log table.
///
/// The default uses bounded indexed columns because MySQL cannot index
/// unbounded `text` columns in a primary key. Apps with longer mutation ids or
/// scope values should use their own table and point [`SqlxCrudMutationLog`] at
/// it with [`SqlxCrudMutationLog::with_table`]. The helper validates sync token
/// shape, but it does not know the configured MySQL column widths.
pub const MYSQL_CRUD_MUTATION_LOG_SCHEMA: &str = r#"create table if not exists __pocopine_crud_mutations (
  scope varchar(191) not null,
  mutation_id varchar(191) not null,
  op varchar(16) not null,
  row_key varchar(191),
  payload text not null,
  primary key (scope, mutation_id)
)"#;

type ScopeFn = dyn Fn(&RequestContext) -> SyncResult<String> + Send + Sync;

/// SQLx-backed idempotency log for transactional CRUD sync writes.
///
/// The log stores accepted mutation contents by `(scope, mutation_id)`. Scope
/// must match the authorization domain of the resource, usually a tenant,
/// organization, or authenticated user id. The helper intentionally stores a
/// JSON string and sync metadata only; application rows still live in the
/// app-owned tables.
#[derive(Clone)]
pub struct SqlxCrudMutationLog<DB>
where
    DB: Database,
{
    table: String,
    #[cfg_attr(
        not(any(feature = "postgres", feature = "mysql", feature = "sqlite")),
        allow(dead_code)
    )]
    scope: Arc<ScopeFn>,
    _marker: PhantomData<fn() -> DB>,
}

impl<DB> fmt::Debug for SqlxCrudMutationLog<DB>
where
    DB: Database,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlxCrudMutationLog")
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

impl<DB> SqlxCrudMutationLog<DB>
where
    DB: Database,
{
    /// Build a mutation log with an app-owned authorization scope function.
    pub fn new(
        scope: impl Fn(&RequestContext) -> SyncResult<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            table: DEFAULT_CRUD_MUTATION_LOG_TABLE.to_string(),
            scope: Arc::new(scope),
            _marker: PhantomData,
        }
    }

    /// Build a mutation log using one fixed scope.
    ///
    /// This is useful for single-tenant tests and demos. Multi-tenant
    /// production apps should prefer [`Self::new`] and derive scope from the
    /// authenticated request context.
    pub fn constant_scope(scope: impl Into<String>) -> Self {
        let scope = scope.into();
        Self::new(move |_| Ok(scope.clone()))
    }

    /// Build a mutation log using the fixed `"global"` scope.
    ///
    /// This is intended for tests and single-tenant resources.
    pub fn global() -> Self {
        Self::constant_scope("global")
    }

    /// Use an app-owned table name.
    ///
    /// The identifier may be either `table` or `schema.table`; each identifier
    /// segment must be ASCII alphanumeric or `_`, and must not start with a
    /// digit. This deliberately avoids raw SQL injection through table names.
    pub fn with_table(mut self, table: impl Into<String>) -> SyncResult<Self> {
        self.table = validate_table_name(table.into())?;
        Ok(self)
    }

    /// Return the configured table name.
    pub fn table(&self) -> &str {
        &self.table
    }

    #[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
    fn scope(&self, ctx: &RequestContext) -> SyncResult<String> {
        let scope = (self.scope)(ctx)?;
        let trimmed = scope.trim();
        if trimmed.is_empty() || trimmed != scope {
            return Err(SyncError::client(
                "SQLx CRUD mutation log scope must be non-empty and trimmed",
            ));
        }
        Ok(scope)
    }
}

#[cfg(any(feature = "postgres", feature = "sqlite"))]
macro_rules! impl_sqlx_crud_mutation_log_conflict_clause {
    ($feature:literal, $db:path, $conflict:literal, $p1:literal, $p2:literal, $p3:literal, $p4:literal, $p5:literal) => {
        #[cfg(feature = $feature)]
        #[pocopine_sync_crud::async_trait]
        impl<RowValue> TransactionalCrudMutationLog<Transaction<'static, $db>, RowValue>
            for SqlxCrudMutationLog<$db>
        where
            RowValue: Clone + Send + Sync + 'static,
        {
            async fn reserve_accepted_mutation_in_tx(
                &self,
                tx: &mut Transaction<'static, $db>,
                ctx: &RequestContext,
                accepted: CrudAcceptedMutation,
            ) -> SyncResult<CrudMutationReservation> {
                let scope = self.scope(ctx)?;
                let mutation_id = accepted.mutation_id.clone();
                let payload = serde_json::to_string(&accepted.payload)?;
                let row_key = accepted.key.as_ref().map(|key| key.as_str().to_string());
                let sql = format!(
                    "insert into {} (scope, mutation_id, op, row_key, payload) values ({}, {}, {}, {}, {}){}",
                    self.table, $p1, $p2, $p3, $p4, $p5, $conflict
                );

                let result = sqlx::query(&sql)
                    .bind(scope)
                    .bind(mutation_id.as_str().to_string())
                    .bind(op_to_db(accepted.op).to_string())
                    .bind(row_key)
                    .bind(payload)
                    .execute(&mut **tx)
                    .await
                    .map_err(sync_sqlx_error)?;

                if result.rows_affected() == 1 {
                    return Ok(CrudMutationReservation::Reserved);
                }

                let scope = self.scope(ctx)?;
                let sql = format!(
                    "select mutation_id, op, row_key, payload from {} where scope = {} and mutation_id = {}",
                    self.table, $p1, $p2
                );
                let row = sqlx::query(&sql)
                    .bind(scope)
                    .bind(mutation_id.as_str().to_string())
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(sync_sqlx_error)?
                    .ok_or_else(|| {
                        SyncError::backend(
                            "SQLx CRUD mutation reservation conflict was not readable",
                        )
                    })?;

                let existing = accepted_from_row(&mutation_id, row)?
                    .ok_or_else(|| SyncError::backend("SQLx CRUD mutation reservation was empty"))?;
                Ok(CrudMutationReservation::AlreadyAccepted(existing))
            }
        }
    };
}

#[cfg(feature = "mysql")]
macro_rules! impl_mysql_sqlx_crud_mutation_log {
    () => {
        #[pocopine_sync_crud::async_trait]
        impl<RowValue>
            TransactionalCrudMutationLog<Transaction<'static, sqlx::MySql>, RowValue>
            for SqlxCrudMutationLog<sqlx::MySql>
        where
            RowValue: Clone + Send + Sync + 'static,
        {
            async fn reserve_accepted_mutation_in_tx(
                &self,
                tx: &mut Transaction<'static, sqlx::MySql>,
                ctx: &RequestContext,
                accepted: CrudAcceptedMutation,
            ) -> SyncResult<CrudMutationReservation> {
                let scope = self.scope(ctx)?;
                let mutation_id = accepted.mutation_id.clone();
                let payload = serde_json::to_string(&accepted.payload)?;
                let row_key = accepted.key.as_ref().map(|key| key.as_str().to_string());
                let sql = format!(
                    "insert into {} (scope, mutation_id, op, row_key, payload) values (?, ?, ?, ?, ?)",
                    self.table
                );

                match sqlx::query(&sql)
                    .bind(scope)
                    .bind(mutation_id.as_str().to_string())
                    .bind(op_to_db(accepted.op).to_string())
                    .bind(row_key)
                    .bind(payload)
                    .execute(&mut **tx)
                    .await
                {
                    Ok(_) => Ok(CrudMutationReservation::Reserved),
                    Err(sqlx::Error::Database(db_error)) if db_error.is_unique_violation() => {
                        let scope = self.scope(ctx)?;
                        let sql = format!(
                            "select mutation_id, op, row_key, payload from {} where scope = ? and mutation_id = ?",
                            self.table
                        );
                        let row = sqlx::query(&sql)
                            .bind(scope)
                            .bind(mutation_id.as_str().to_string())
                            .fetch_optional(&mut **tx)
                            .await
                            .map_err(sync_sqlx_error)?
                            .ok_or_else(|| {
                                SyncError::backend(
                                    "SQLx CRUD mutation reservation conflict was not readable",
                                )
                            })?;

                        let existing = accepted_from_row(&mutation_id, row)?.ok_or_else(|| {
                            SyncError::backend("SQLx CRUD mutation reservation was empty")
                        })?;
                        Ok(CrudMutationReservation::AlreadyAccepted(existing))
                    }
                    Err(err) => Err(sync_sqlx_error(err)),
                }
            }
        }
    };
}

#[cfg(feature = "sqlite")]
impl_sqlx_crud_mutation_log_conflict_clause!(
    "sqlite",
    sqlx::Sqlite,
    " on conflict(scope, mutation_id) do nothing",
    "?",
    "?",
    "?",
    "?",
    "?"
);
#[cfg(feature = "mysql")]
impl_mysql_sqlx_crud_mutation_log!();
#[cfg(feature = "postgres")]
impl_sqlx_crud_mutation_log_conflict_clause!(
    "postgres",
    sqlx::Postgres,
    " on conflict(scope, mutation_id) do nothing",
    "$1",
    "$2",
    "$3",
    "$4",
    "$5"
);

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
fn accepted_from_row<R>(
    mutation_id: &MutationId,
    row: R,
) -> SyncResult<Option<CrudAcceptedMutation>>
where
    R: Row,
    for<'row> String: Decode<'row, R::Database> + Type<R::Database>,
    for<'row> Option<String>: Decode<'row, R::Database> + Type<R::Database>,
    usize: ColumnIndex<R>,
{
    let stored_mutation_id =
        MutationId::new(row.try_get::<String, _>(0).map_err(sync_sqlx_error)?)?;
    if &stored_mutation_id != mutation_id {
        return Err(SyncError::backend(
            "SQLx CRUD mutation lookup returned mismatched mutation id",
        ));
    }

    let op = op_from_db(row.try_get::<String, _>(1).map_err(sync_sqlx_error)?)?;
    let key = row
        .try_get::<Option<String>, _>(2)
        .map_err(sync_sqlx_error)?
        .map(RowKey::new)
        .transpose()?;
    let payload =
        serde_json::from_str::<Value>(&row.try_get::<String, _>(3).map_err(sync_sqlx_error)?)?;

    Ok(Some(CrudAcceptedMutation::new(
        stored_mutation_id,
        op,
        key,
        payload,
    )))
}

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
fn op_to_db(op: SyncOp) -> &'static str {
    match op {
        SyncOp::Upsert => "upsert",
        SyncOp::Delete => "delete",
        SyncOp::Reset => "reset",
    }
}

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
fn op_from_db(value: String) -> SyncResult<SyncOp> {
    match value.as_str() {
        "upsert" => Ok(SyncOp::Upsert),
        "delete" => Ok(SyncOp::Delete),
        "reset" => Ok(SyncOp::Reset),
        _ => Err(SyncError::backend(format!(
            "unknown SQLx CRUD mutation op: {value}"
        ))),
    }
}

fn validate_table_name(table: String) -> SyncResult<String> {
    let mut parts = table.split('.');
    let Some(first) = parts.next() else {
        return Err(SyncError::client(format!(
            "invalid SQLx CRUD mutation log table name: {table}"
        )));
    };
    let second = parts.next();

    if parts.next().is_none()
        && validate_identifier(first)
        && second.is_none_or(validate_identifier)
    {
        Ok(table)
    } else {
        Err(SyncError::client(format!(
            "invalid SQLx CRUD mutation log table name: {table}"
        )))
    }
}

fn validate_identifier(identifier: &str) -> bool {
    let mut chars = identifier.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use http::{HeaderMap, Method, Uri};
    use pocopine_sync_crud::{
        CrudAcceptedMutation, CrudMutationReservation, CrudTransactionRunner,
        TransactionalCrudMutationLog,
    };

    use crate::{sqlite, SqlxCrudTransactionRunner};

    use super::*;

    fn ctx_with_tenant(tenant: &'static str) -> RequestContext {
        let mut headers = HeaderMap::new();
        headers.insert("x-tenant-id", tenant.parse().unwrap());
        RequestContext::new(Method::POST, Uri::from_static("/sync"), headers)
    }

    fn tenant_log() -> SqlxCrudMutationLog<sqlx::Sqlite> {
        SqlxCrudMutationLog::new(|ctx| {
            ctx.header("x-tenant-id")
                .map(str::to_string)
                .ok_or_else(|| SyncError::client("missing tenant"))
        })
    }

    async fn reserve_in_tx(
        log: &SqlxCrudMutationLog<sqlx::Sqlite>,
        tx: &mut Transaction<'static, sqlx::Sqlite>,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation,
    ) -> SyncResult<CrudMutationReservation> {
        <SqlxCrudMutationLog<sqlx::Sqlite> as TransactionalCrudMutationLog<
            Transaction<'static, sqlx::Sqlite>,
            (),
        >>::reserve_accepted_mutation_in_tx(log, tx, ctx, accepted)
        .await
    }

    #[test]
    fn table_name_validation_rejects_raw_sql() {
        let log = SqlxCrudMutationLog::<sqlx::Sqlite>::global()
            .with_table("public.__pocopine_crud_mutations")
            .unwrap();
        assert_eq!(log.table(), "public.__pocopine_crud_mutations");

        let err = SqlxCrudMutationLog::<sqlx::Sqlite>::global()
            .with_table("__pocopine_crud_mutations; drop table posts")
            .unwrap_err();
        assert!(err.to_string().contains("invalid SQLx CRUD mutation"));

        let err = SqlxCrudMutationLog::<sqlx::Sqlite>::global()
            .with_table("too.many.segments")
            .unwrap_err();
        assert!(err.to_string().contains("invalid SQLx CRUD mutation"));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_mutation_log_round_trips_by_scope() {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(SQLITE_CRUD_MUTATION_LOG_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();

        let runner: SqlxCrudTransactionRunner<sqlx::Sqlite> = sqlite(pool.clone());
        let log = tenant_log();
        let mutation_id = MutationId::new("device_a:1").unwrap();
        let key = RowKey::new("post_1").unwrap();
        let accepted = CrudAcceptedMutation::new(
            mutation_id.clone(),
            SyncOp::Upsert,
            Some(key),
            serde_json::json!({"id": "post_1", "title": "Draft"}),
        );

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            reserve_in_tx(
                &log,
                &mut tx,
                &ctx_with_tenant("tenant_a"),
                accepted.clone(),
            )
            .await
            .unwrap(),
            CrudMutationReservation::Reserved
        );
        runner.commit(tx).await.unwrap();

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            reserve_in_tx(
                &log,
                &mut tx,
                &ctx_with_tenant("tenant_a"),
                accepted.clone(),
            )
            .await
            .unwrap(),
            CrudMutationReservation::AlreadyAccepted(accepted.clone())
        );
        assert_eq!(
            reserve_in_tx(
                &log,
                &mut tx,
                &ctx_with_tenant("tenant_b"),
                accepted.clone(),
            )
            .await
            .unwrap(),
            CrudMutationReservation::Reserved
        );
        runner.rollback(tx).await.unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_mutation_log_reserves_before_duplicate_replay() {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(SQLITE_CRUD_MUTATION_LOG_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();

        let runner: SqlxCrudTransactionRunner<sqlx::Sqlite> = sqlite(pool.clone());
        let log = SqlxCrudMutationLog::<sqlx::Sqlite>::global();
        let ctx = RequestContext::new(Method::POST, Uri::from_static("/sync"), HeaderMap::new());
        let mutation_id = MutationId::new("device_a:reserved").unwrap();
        let accepted = CrudAcceptedMutation::new(
            mutation_id.clone(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            serde_json::json!({"id": "post_1", "title": "Draft"}),
        );

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            reserve_in_tx(&log, &mut tx, &ctx, accepted.clone())
                .await
                .unwrap(),
            CrudMutationReservation::Reserved
        );
        runner.commit(tx).await.unwrap();

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            reserve_in_tx(&log, &mut tx, &ctx, accepted.clone())
                .await
                .unwrap(),
            CrudMutationReservation::AlreadyAccepted(accepted.clone())
        );

        let changed = CrudAcceptedMutation::new(
            mutation_id,
            SyncOp::Delete,
            Some(RowKey::new("post_1").unwrap()),
            serde_json::json!({"id": "post_1"}),
        );
        assert_eq!(
            reserve_in_tx(&log, &mut tx, &ctx, changed).await.unwrap(),
            CrudMutationReservation::AlreadyAccepted(accepted)
        );
        runner.rollback(tx).await.unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_mutation_log_rolls_back_with_transaction() {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(SQLITE_CRUD_MUTATION_LOG_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();

        let runner: SqlxCrudTransactionRunner<sqlx::Sqlite> = sqlite(pool.clone());
        let log = SqlxCrudMutationLog::<sqlx::Sqlite>::global();
        let ctx = RequestContext::new(Method::POST, Uri::from_static("/sync"), HeaderMap::new());
        let mutation_id = MutationId::new("device_a:2").unwrap();
        let accepted = CrudAcceptedMutation::new(
            mutation_id.clone(),
            SyncOp::Delete,
            Some(RowKey::new("post_2").unwrap()),
            serde_json::json!({"id": "post_2"}),
        );

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            reserve_in_tx(&log, &mut tx, &ctx, accepted.clone())
                .await
                .unwrap(),
            CrudMutationReservation::Reserved
        );
        runner.rollback(tx).await.unwrap();

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            reserve_in_tx(&log, &mut tx, &ctx, accepted).await.unwrap(),
            CrudMutationReservation::Reserved
        );
        runner.rollback(tx).await.unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_mutation_log_rejects_untrimmed_scope() {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(SQLITE_CRUD_MUTATION_LOG_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();

        let runner: SqlxCrudTransactionRunner<sqlx::Sqlite> = sqlite(pool.clone());
        let log = SqlxCrudMutationLog::<sqlx::Sqlite>::constant_scope(" tenant ");
        let ctx = RequestContext::new(Method::POST, Uri::from_static("/sync"), HeaderMap::new());
        let accepted = CrudAcceptedMutation::new(
            MutationId::new("device_a:scope").unwrap(),
            SyncOp::Delete,
            Some(RowKey::new("post_2").unwrap()),
            serde_json::json!({"id": "post_2"}),
        );
        let mut tx = runner.begin().await.unwrap();
        let err = reserve_in_tx(&log, &mut tx, &ctx, accepted)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("non-empty and trimmed"));
        runner.rollback(tx).await.unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_mutation_log_rejects_unknown_stored_op() {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(SQLITE_CRUD_MUTATION_LOG_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "insert into __pocopine_crud_mutations (scope, mutation_id, op, row_key, payload) values (?, ?, ?, ?, ?)",
        )
        .bind("global")
        .bind("device_a:bad_op")
        .bind("merge")
        .bind("post_1")
        .bind(r#"{"id":"post_1"}"#)
        .execute(&pool)
        .await
        .unwrap();

        let runner: SqlxCrudTransactionRunner<sqlx::Sqlite> = sqlite(pool.clone());
        let log = SqlxCrudMutationLog::<sqlx::Sqlite>::global();
        let ctx = RequestContext::new(Method::POST, Uri::from_static("/sync"), HeaderMap::new());
        let accepted = CrudAcceptedMutation::new(
            MutationId::new("device_a:bad_op").unwrap(),
            SyncOp::Delete,
            Some(RowKey::new("post_1").unwrap()),
            serde_json::json!({"id": "post_1"}),
        );
        let mut tx = runner.begin().await.unwrap();
        let err = reserve_in_tx(&log, &mut tx, &ctx, accepted)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown SQLx CRUD mutation op"));
        runner.rollback(tx).await.unwrap();
    }
}
