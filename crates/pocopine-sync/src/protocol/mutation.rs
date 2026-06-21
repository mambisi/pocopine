use serde::{Deserialize, Serialize};

use crate::SyncResult;

use super::{MutationId, RowKey, RowVersion, SyncOp};

/// Client mutation envelope. The first slice defines the wire format;
/// concrete mutation application belongs to stream sources.
///
/// The two server-side sidecars (`migrated_payload`, `migration_error`)
/// are set by the framework's `push_handler` when a stale-schema
/// request runs through `SyncStreamSource::migrate_payload`. They are
/// `#[serde(skip)]`, never on the wire, and always `None` for
/// client-built mutations. Sources consume them via
/// `take_processing_payload` AFTER consulting their idempotency log
/// against the ORIGINAL `payload` — so a retry of an
/// already-accepted mutation succeeds even when the registered
/// migrator now rejects (or panics) on the same inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutation<M> {
    pub id: MutationId,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
    /// Server-side sidecar: the result of `migrate_payload` on this
    /// mutation. `Some(Ok(value))` is the migrated payload to apply;
    /// `Some(Err(reason))` is a migration failure to surface IF the
    /// mutation isn't already idempotent-accepted; `None` means no
    /// migration was needed (request schema matches server schema).
    #[serde(skip)]
    pub migration_outcome: Option<MigrationOutcome<M>>,
}

/// Outcome of `migrate_payload` for one mutation, carried as a
/// server-only sidecar inside `ClientMutation`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationOutcome<M> {
    /// Successful migration: this is the payload to deserialize and
    /// apply (the original wire payload is preserved in `payload`
    /// for idempotency-log comparison).
    Migrated(M),
    /// Migration failed. The reason should surface as a per-mutation
    /// rejection — but ONLY if the source's idempotency log doesn't
    /// already have this mutation_id+payload as accepted. A retry of
    /// a previously-accepted mutation should always succeed
    /// regardless of whether the current migrator would accept it.
    Failed { reason: String },
}

impl<M> ClientMutation<M> {
    /// Build a mutation with a caller-supplied id.
    pub fn new(id: MutationId, op: SyncOp, payload: M) -> Self {
        Self {
            id,
            key: None,
            op,
            base_version: None,
            payload,
            migration_outcome: None,
        }
    }

    /// Take the payload the source should apply, consuming the
    /// migration sidecar. Returns:
    ///
    /// * `Ok(value)` — the value to deserialize and apply (migrated
    ///   if migration ran successfully, otherwise the original wire
    ///   payload).
    /// * `Err(reason)` — the migrator rejected this mutation. The
    ///   caller must already have checked its idempotency log
    ///   against `mutation.payload` — only mutations NOT in the log
    ///   should surface this error.
    ///
    /// `mutation.payload` is left intact for idempotency comparisons.
    pub fn take_processing_payload(&mut self) -> Result<M, String>
    where
        M: Clone,
    {
        match self.migration_outcome.take() {
            Some(MigrationOutcome::Migrated(value)) => Ok(value),
            Some(MigrationOutcome::Failed { reason }) => Err(reason),
            None => Ok(self.payload.clone()),
        }
    }

    /// Build an upsert mutation with a caller-supplied id.
    pub fn upsert(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Upsert, payload)
    }

    /// Build a delete mutation with a caller-supplied id.
    pub fn delete(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Delete, payload)
    }

    /// Build a reset mutation with a caller-supplied id.
    pub fn reset(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Reset, payload)
    }

    /// Build a mutation scoped to a row.
    pub fn for_row(
        id: MutationId,
        op: SyncOp,
        key: impl Into<String>,
        payload: M,
    ) -> SyncResult<Self> {
        Self::new(id, op, payload).key(key)
    }

    /// Build an upsert mutation scoped to a row.
    pub fn upsert_row(id: MutationId, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::upsert(id, payload).key(key)
    }

    /// Build a delete mutation scoped to a row.
    pub fn delete_row(id: MutationId, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::delete(id, payload).key(key)
    }

    /// Attach a row key.
    pub fn key(mut self, key: impl Into<String>) -> SyncResult<Self> {
        self.key = Some(RowKey::new(key)?);
        Ok(self)
    }

    /// Attach an already validated row key.
    pub fn row_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Attach a base row version for conflict detection.
    pub fn base_version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.base_version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated base row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach an optional already validated base row version.
    pub fn base_row_version(mut self, version: Option<RowVersion>) -> Self {
        self.base_version = version;
        self
    }
}

/// Client mutation before the local store reserves a durable mutation id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutationDraft<M> {
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
}

impl<M> ClientMutationDraft<M> {
    /// Build a draft mutation for the given operation.
    pub fn new(op: SyncOp, payload: M) -> Self {
        Self {
            key: None,
            op,
            base_version: None,
            payload,
        }
    }

    /// Build an upsert draft.
    pub fn upsert(payload: M) -> Self {
        Self::new(SyncOp::Upsert, payload)
    }

    /// Build a delete draft.
    pub fn delete(payload: M) -> Self {
        Self::new(SyncOp::Delete, payload)
    }

    /// Build a reset draft.
    pub fn reset(payload: M) -> Self {
        Self::new(SyncOp::Reset, payload)
    }

    /// Build a draft mutation scoped to a row.
    pub fn for_row(op: SyncOp, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::new(op, payload).key(key)
    }

    /// Build an upsert draft scoped to a row.
    pub fn upsert_row(key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::upsert(payload).key(key)
    }

    /// Build a delete draft scoped to a row.
    pub fn delete_row(key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::delete(payload).key(key)
    }

    /// Attach a row key.
    pub fn key(mut self, key: impl Into<String>) -> SyncResult<Self> {
        self.key = Some(RowKey::new(key)?);
        Ok(self)
    }

    /// Attach an already validated row key.
    pub fn row_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Attach a base row version for conflict detection.
    pub fn base_version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.base_version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated base row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach an optional already validated base row version.
    pub fn base_row_version(mut self, version: Option<RowVersion>) -> Self {
        self.base_version = version;
        self
    }

    /// Convert this draft into a wire mutation after an id is reserved.
    pub fn with_id(self, id: MutationId) -> ClientMutation<M> {
        ClientMutation {
            id,
            key: self.key,
            op: self.op,
            base_version: self.base_version,
            payload: self.payload,
            migration_outcome: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_mutation_helpers_build_row_scoped_mutations() {
        let id = MutationId::new("device_abc:1").unwrap();
        let version = RowVersion::new("row_1").unwrap();

        let mutation = ClientMutation::upsert_row(id.clone(), "post_1", "payload")
            .unwrap()
            .base_row_version(Some(version.clone()));

        assert_eq!(mutation.id, id);
        assert_eq!(mutation.key.unwrap().as_str(), "post_1");
        assert_eq!(mutation.op, SyncOp::Upsert);
        assert_eq!(mutation.base_version, Some(version));
        assert_eq!(mutation.payload, "payload");
    }

    #[test]
    fn client_mutation_draft_helpers_build_row_scoped_mutations() {
        let id = MutationId::new("device_abc:1").unwrap();
        let version = RowVersion::new("row_1").unwrap();

        let mutation = ClientMutationDraft::delete_row("post_1", ())
            .unwrap()
            .base_row_version(Some(version.clone()))
            .with_id(id.clone());

        assert_eq!(mutation.id, id);
        assert_eq!(mutation.key.unwrap().as_str(), "post_1");
        assert_eq!(mutation.op, SyncOp::Delete);
        assert_eq!(mutation.base_version, Some(version));
    }
}
