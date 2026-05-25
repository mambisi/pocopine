use std::{future::Future, pin::Pin};

use pocopine_sync::SyncResult;

use crate::TransactionOptions;

/// Boxed future returned by transaction runners and bound transaction code.
pub type TransactionFuture<'tx, T> = Pin<Box<dyn Future<Output = SyncResult<T>> + Send + 'tx>>;

/// Transaction wrapper that binds resources to an active transaction handle.
pub struct Transaction<'tx, Tx> {
    raw: &'tx mut Tx,
}

impl<'tx, Tx> Transaction<'tx, Tx> {
    pub fn new(raw: &'tx mut Tx) -> Self {
        Self { raw }
    }

    pub fn raw(&mut self) -> &mut Tx {
        self.raw
    }

    pub fn with<R>(&mut self, resource: R) -> R::Bound<'_>
    where
        R: TransactionBindable<Tx>,
    {
        resource.bind(self.raw)
    }
}

/// Internal binding contract behind `tx.with(resource)`.
pub trait TransactionBindable<Tx>: Sized {
    type Bound<'tx>
    where
        Self: 'tx,
        Tx: 'tx;

    fn bind<'tx>(self, tx: &'tx mut Tx) -> Self::Bound<'tx>;
}

/// App/database owned transaction lifecycle.
///
/// Implementations should begin a database transaction, call the supplied
/// closure with [`Transaction`], commit on `Ok`, and roll back on `Err`.
/// Pocopine owns the contract shape; the app still owns the concrete database
/// handle and transaction implementation.
pub trait TransactionRunner: Send + Sync + 'static {
    type Tx: Send + 'static;

    fn transaction<'runner, T, F>(&'runner self, f: F) -> TransactionFuture<'runner, T>
    where
        T: Send + 'runner,
        F: for<'tx> FnOnce(Transaction<'tx, Self::Tx>) -> TransactionFuture<'tx, T>
            + Send
            + 'runner;
}

/// App/database owned transaction lifecycle for server-side CRUD sync writes.
///
/// This runner returns an owned transaction handle so the sync adapter can
/// apply one mutation, record the accepted mutation id, and commit or roll back
/// as one unit. It is separate from [`TransactionRunner`], whose callback shape
/// is intended for author-facing `tx.with(resource)` helpers.
pub trait CrudTransactionRunner: Send + Sync + 'static {
    type Tx: Send + 'static;

    fn begin<'runner>(&'runner self) -> TransactionFuture<'runner, Self::Tx>;

    fn commit<'runner>(&'runner self, tx: Self::Tx) -> TransactionFuture<'runner, ()>;

    fn rollback<'runner>(&'runner self, tx: Self::Tx) -> TransactionFuture<'runner, ()>;
}

impl TransactionOptions {
    /// Run work inside an app-provided transaction lifecycle.
    ///
    /// The current options value is carried through this API so generated CRUD
    /// helpers can use the same fluent shape for queue-offline and
    /// require-online transaction calls. The database transaction lifecycle is
    /// delegated to the supplied [`TransactionRunner`].
    pub async fn run<R, T, F>(&self, runner: &R, f: F) -> SyncResult<T>
    where
        R: TransactionRunner,
        T: Send,
        F: for<'tx> FnOnce(Transaction<'tx, R::Tx>) -> TransactionFuture<'tx, T> + Send,
    {
        let _ = self.write_policy;
        runner.transaction(f).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use pocopine_sync::SyncError;
    #[cfg(not(target_arch = "wasm32"))]
    use std::sync::{Arc, Mutex};

    #[test]
    fn transaction_with_uses_bind_contract() {
        struct RawTx {
            log: Vec<&'static str>,
        }

        struct Audit;

        struct AuditInTx<'tx> {
            tx: &'tx mut RawTx,
        }

        impl<'tx> AuditInTx<'tx> {
            fn record(self, message: &'static str) {
                self.tx.log.push(message);
            }
        }

        impl TransactionBindable<RawTx> for Audit {
            type Bound<'tx> = AuditInTx<'tx>;

            fn bind<'tx>(self, tx: &'tx mut RawTx) -> Self::Bound<'tx> {
                AuditInTx { tx }
            }
        }

        let mut raw = RawTx { log: Vec::new() };
        let mut tx = Transaction::new(&mut raw);
        tx.with(Audit).record("created");

        assert_eq!(tx.raw().log, vec!["created"]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn transaction_options_run_commits_on_success_and_rolls_back_on_error() {
        #[derive(Clone, Default)]
        struct FakeDb {
            events: Arc<Mutex<Vec<&'static str>>>,
        }

        struct RawTx {
            events: Arc<Mutex<Vec<&'static str>>>,
        }

        impl TransactionRunner for FakeDb {
            type Tx = RawTx;

            fn transaction<'runner, T, F>(&'runner self, f: F) -> TransactionFuture<'runner, T>
            where
                T: Send + 'runner,
                F: for<'tx> FnOnce(Transaction<'tx, Self::Tx>) -> TransactionFuture<'tx, T>
                    + Send
                    + 'runner,
            {
                Box::pin(async move {
                    self.events.lock().unwrap().push("begin");
                    let mut raw = RawTx {
                        events: self.events.clone(),
                    };
                    let result = f(Transaction::new(&mut raw)).await;
                    if result.is_ok() {
                        self.events.lock().unwrap().push("commit");
                    } else {
                        self.events.lock().unwrap().push("rollback");
                    }
                    result
                })
            }
        }

        struct Audit;

        struct AuditInTx<'tx> {
            tx: &'tx mut RawTx,
        }

        impl<'tx> AuditInTx<'tx> {
            fn record(self, message: &'static str) {
                self.tx.events.lock().unwrap().push(message);
            }
        }

        impl TransactionBindable<RawTx> for Audit {
            type Bound<'tx> = AuditInTx<'tx>;

            fn bind<'tx>(self, tx: &'tx mut RawTx) -> Self::Bound<'tx> {
                AuditInTx { tx }
            }
        }

        let db = FakeDb::default();
        let value = TransactionOptions::new()
            .run(&db, |mut tx| {
                Box::pin(async move {
                    tx.with(Audit).record("write");
                    Ok::<_, SyncError>("ok")
                })
            })
            .await
            .unwrap();

        assert_eq!(value, "ok");

        let err = TransactionOptions::new()
            .run(&db, |mut tx| {
                Box::pin(async move {
                    tx.with(Audit).record("fail");
                    Err::<(), _>(SyncError::backend("boom"))
                })
            })
            .await
            .unwrap_err();

        assert!(err.to_string().contains("boom"));
        assert_eq!(
            db.events.lock().unwrap().as_slice(),
            ["begin", "write", "commit", "begin", "fail", "rollback"]
        );
    }
}
