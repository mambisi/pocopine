use std::{fmt, marker::PhantomData, sync::Arc};

use pocopine_auth::RequestContext;
use pocopine_sync::{MutationId, RowKey, SyncError, SyncOp, SyncResult};
use pocopine_sync_crud::{CrudAcceptedMutation, TransactionalCrudMutationLog};
use serde_json::Value;
use sqlx::{ColumnIndex, Database, Decode, Row, Transaction, Type};

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
/// it with [`SqlxCrudMutationLog::with_table`].
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

    fn scope(&self, ctx: &RequestContext) -> SyncResult<String> {
        let scope = (self.scope)(ctx)?;
        if scope.trim().is_empty() {
            return Err(SyncError::client(
                "SQLx CRUD mutation log scope must not be empty",
            ));
        }
        Ok(scope)
    }
}

macro_rules! impl_sqlx_crud_mutation_log {
    ($feature:literal, $db:path, $p1:literal, $p2:literal, $p3:literal, $p4:literal, $p5:literal) => {
        #[cfg(feature = $feature)]
        #[pocopine_sync_crud::async_trait]
        impl<RowValue> TransactionalCrudMutationLog<Transaction<'static, $db>, RowValue>
            for SqlxCrudMutationLog<$db>
        where
            RowValue: Clone + Send + Sync + 'static,
        {
            async fn accepted_mutation_in_tx(
                &self,
                tx: &mut Transaction<'static, $db>,
                ctx: &RequestContext,
                mutation_id: &MutationId,
            ) -> SyncResult<Option<CrudAcceptedMutation>> {
                let scope = self.scope(ctx)?;
                let sql = format!(
                    "select op, row_key, payload from {} where scope = {} and mutation_id = {}",
                    self.table, $p1, $p2
                );
                let row = sqlx::query(&sql)
                    .bind(scope)
                    .bind(mutation_id.as_str().to_string())
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(sync_sqlx_error)?;

                let Some(row) = row else {
                    return Ok(None);
                };

                accepted_from_row(mutation_id, row)
            }

            async fn record_accepted_mutation_in_tx(
                &self,
                tx: &mut Transaction<'static, $db>,
                ctx: &RequestContext,
                accepted: CrudAcceptedMutation,
            ) -> SyncResult<()> {
                let scope = self.scope(ctx)?;
                let payload = serde_json::to_string(&accepted.payload)?;
                let row_key = accepted.key.as_ref().map(|key| key.as_str().to_string());
                let sql = format!(
                    "insert into {} (scope, mutation_id, op, row_key, payload) values ({}, {}, {}, {}, {})",
                    self.table, $p1, $p2, $p3, $p4, $p5
                );

                sqlx::query(&sql)
                    .bind(scope)
                    .bind(accepted.mutation_id.as_str().to_string())
                    .bind(op_to_db(accepted.op).to_string())
                    .bind(row_key)
                    .bind(payload)
                    .execute(&mut **tx)
                    .await
                    .map_err(sync_sqlx_error)?;

                Ok(())
            }
        }
    };
}

impl_sqlx_crud_mutation_log!("sqlite", sqlx::Sqlite, "?", "?", "?", "?", "?");
impl_sqlx_crud_mutation_log!("mysql", sqlx::MySql, "?", "?", "?", "?", "?");
impl_sqlx_crud_mutation_log!("postgres", sqlx::Postgres, "$1", "$2", "$3", "$4", "$5");

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
    let op = op_from_db(row.try_get::<String, _>(0).map_err(sync_sqlx_error)?)?;
    let key = row
        .try_get::<Option<String>, _>(1)
        .map_err(sync_sqlx_error)?
        .map(RowKey::new)
        .transpose()?;
    let payload =
        serde_json::from_str::<Value>(&row.try_get::<String, _>(2).map_err(sync_sqlx_error)?)?;

    Ok(Some(CrudAcceptedMutation::new(
        mutation_id.clone(),
        op,
        key,
        payload,
    )))
}

fn op_to_db(op: SyncOp) -> &'static str {
    match op {
        SyncOp::Upsert => "upsert",
        SyncOp::Delete => "delete",
        SyncOp::Reset => "reset",
    }
}

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
    if table.split('.').all(validate_identifier) {
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

#[cfg(test)]
mod tests {
    use http::{HeaderMap, Method, Uri};
    use pocopine_sync_crud::{
        CrudAcceptedMutation, CrudTransactionRunner, TransactionalCrudMutationLog,
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

    async fn accepted_in_tx(
        log: &SqlxCrudMutationLog<sqlx::Sqlite>,
        tx: &mut Transaction<'static, sqlx::Sqlite>,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<CrudAcceptedMutation>> {
        <SqlxCrudMutationLog<sqlx::Sqlite> as TransactionalCrudMutationLog<
            Transaction<'static, sqlx::Sqlite>,
            (),
        >>::accepted_mutation_in_tx(log, tx, ctx, mutation_id)
        .await
    }

    async fn record_in_tx(
        log: &SqlxCrudMutationLog<sqlx::Sqlite>,
        tx: &mut Transaction<'static, sqlx::Sqlite>,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation,
    ) -> SyncResult<()> {
        <SqlxCrudMutationLog<sqlx::Sqlite> as TransactionalCrudMutationLog<
            Transaction<'static, sqlx::Sqlite>,
            (),
        >>::record_accepted_mutation_in_tx(log, tx, ctx, accepted)
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
            accepted_in_tx(&log, &mut tx, &ctx_with_tenant("tenant_a"), &mutation_id)
                .await
                .unwrap(),
            None
        );
        record_in_tx(
            &log,
            &mut tx,
            &ctx_with_tenant("tenant_a"),
            accepted.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            accepted_in_tx(&log, &mut tx, &ctx_with_tenant("tenant_a"), &mutation_id)
                .await
                .unwrap(),
            Some(accepted.clone())
        );
        assert_eq!(
            accepted_in_tx(&log, &mut tx, &ctx_with_tenant("tenant_b"), &mutation_id)
                .await
                .unwrap(),
            None
        );
        runner.commit(tx).await.unwrap();

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            accepted_in_tx(&log, &mut tx, &ctx_with_tenant("tenant_a"), &mutation_id)
                .await
                .unwrap(),
            Some(accepted)
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
        record_in_tx(&log, &mut tx, &ctx, accepted).await.unwrap();
        runner.rollback(tx).await.unwrap();

        let mut tx = runner.begin().await.unwrap();
        assert_eq!(
            accepted_in_tx(&log, &mut tx, &ctx, &mutation_id)
                .await
                .unwrap(),
            None
        );
        runner.rollback(tx).await.unwrap();
    }
}
