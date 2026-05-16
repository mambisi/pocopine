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

#[cfg(test)]
mod tests {
    use super::*;

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
}
