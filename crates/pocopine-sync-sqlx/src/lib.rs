//! SQLx helpers for Pocopine sync CRUD resources.
//!
//! This crate is deliberately small. It wires SQLx transactions into
//! `pocopine-sync-crud`; it does not generate SQL, own application schema, or
//! act as an ORM. Apps still implement `CrudSource` /
//! `TransactionalCrudSource` with normal SQLx queries.

#![cfg(not(target_arch = "wasm32"))]

use std::marker::PhantomData;

use pocopine_sync::SyncError;
use pocopine_sync_crud::{CrudTransactionRunner, TransactionFuture};
use sqlx::{Database, Pool, Transaction};

mod mutation_log;

pub use mutation_log::{
    SqlxCrudMutationLog, DEFAULT_CRUD_MUTATION_LOG_TABLE, MYSQL_CRUD_MUTATION_LOG_SCHEMA,
    POSTGRES_CRUD_MUTATION_LOG_SCHEMA, SQLITE_CRUD_MUTATION_LOG_SCHEMA,
};

/// SQLx transaction type used by [`SqlxCrudTransactionRunner`].
pub type SqlxCrudTransaction<DB> = Transaction<'static, DB>;

/// Map a SQLx error into Pocopine's sync error type.
pub fn sync_sqlx_error(_error: sqlx::Error) -> SyncError {
    SyncError::backend("SQLx backend operation failed")
}

/// Transaction runner backed by a SQLx pool.
///
/// `Pool::begin` returns an owned transaction handle, so the CRUD sync adapter
/// can apply the row write and accepted-mutation log insert before committing.
#[derive(Debug)]
pub struct SqlxCrudTransactionRunner<DB>
where
    DB: Database,
{
    pool: Pool<DB>,
    _marker: PhantomData<fn() -> DB>,
}

impl<DB> Clone for SqlxCrudTransactionRunner<DB>
where
    DB: Database,
{
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            _marker: PhantomData,
        }
    }
}

impl<DB> SqlxCrudTransactionRunner<DB>
where
    DB: Database,
{
    /// Build a transaction runner from an existing SQLx pool.
    pub fn new(pool: Pool<DB>) -> Self {
        Self {
            pool,
            _marker: PhantomData,
        }
    }

    /// Borrow the underlying SQLx pool.
    pub fn pool(&self) -> &Pool<DB> {
        &self.pool
    }

    /// Return the underlying SQLx pool.
    pub fn into_pool(self) -> Pool<DB> {
        self.pool
    }
}

impl<DB> CrudTransactionRunner for SqlxCrudTransactionRunner<DB>
where
    DB: Database,
{
    type Tx = SqlxCrudTransaction<DB>;

    fn begin<'runner>(&'runner self) -> TransactionFuture<'runner, Self::Tx> {
        Box::pin(async move { self.pool.begin().await.map_err(sync_sqlx_error) })
    }

    fn commit<'runner>(&'runner self, tx: Self::Tx) -> TransactionFuture<'runner, ()> {
        Box::pin(async move { tx.commit().await.map_err(sync_sqlx_error) })
    }

    fn rollback<'runner>(&'runner self, tx: Self::Tx) -> TransactionFuture<'runner, ()> {
        Box::pin(async move { tx.rollback().await.map_err(sync_sqlx_error) })
    }
}

/// Postgres transaction runner alias.
#[cfg(feature = "postgres")]
pub type PgCrudTransactionRunner = SqlxCrudTransactionRunner<sqlx::Postgres>;

/// MySQL transaction runner alias.
#[cfg(feature = "mysql")]
pub type MySqlCrudTransactionRunner = SqlxCrudTransactionRunner<sqlx::MySql>;

/// SQLite transaction runner alias.
#[cfg(feature = "sqlite")]
pub type SqliteCrudTransactionRunner = SqlxCrudTransactionRunner<sqlx::Sqlite>;

/// Build a Postgres CRUD transaction runner.
#[cfg(feature = "postgres")]
pub fn postgres(pool: sqlx::PgPool) -> PgCrudTransactionRunner {
    SqlxCrudTransactionRunner::new(pool)
}

/// Build a MySQL CRUD transaction runner.
#[cfg(feature = "mysql")]
pub fn mysql(pool: sqlx::MySqlPool) -> MySqlCrudTransactionRunner {
    SqlxCrudTransactionRunner::new(pool)
}

/// Build a SQLite CRUD transaction runner.
#[cfg(feature = "sqlite")]
pub fn sqlite(pool: sqlx::SqlitePool) -> SqliteCrudTransactionRunner {
    SqlxCrudTransactionRunner::new(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_runner_commits_and_rolls_back() {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query("create table events (name text not null)")
            .execute(&pool)
            .await
            .unwrap();

        let runner = sqlite(pool.clone());
        let mut tx = runner.begin().await.unwrap();
        sqlx::query("insert into events (name) values ('committed')")
            .execute(&mut *tx)
            .await
            .unwrap();
        runner.commit(tx).await.unwrap();

        let committed: i64 = sqlx::query_scalar("select count(*) from events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(committed, 1);

        let mut tx = runner.begin().await.unwrap();
        sqlx::query("insert into events (name) values ('rolled back')")
            .execute(&mut *tx)
            .await
            .unwrap();
        runner.rollback(tx).await.unwrap();

        let committed: i64 = sqlx::query_scalar("select count(*) from events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(committed, 1);
    }
}
