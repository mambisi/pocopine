use std::future;

use pocopine_sync::{
    ClientMutation, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot,
    MutationId, RowKey, SyncError, SyncLocalFuture, SyncLocalIdentity, SyncLocalStore, SyncResult,
    SyncStreamName,
};

const DEFAULT_DATABASE_NAME: &str = "pocopine_sync";

/// IndexedDB-backed [`SyncLocalStore`].
///
/// IndexedDB is only available in browser builds. The host stub keeps the crate
/// checkable in workspace-wide host builds and reports a clear error if used.
#[derive(Clone, Debug)]
pub struct IndexedDbLocalStore {
    database_name: String,
}

impl IndexedDbLocalStore {
    /// Open the default browser IndexedDB database.
    pub fn new() -> Self {
        Self {
            database_name: DEFAULT_DATABASE_NAME.to_string(),
        }
    }

    /// Open a named browser IndexedDB database.
    pub fn with_database_name(database_name: impl Into<String>) -> SyncResult<Self> {
        let database_name = validate_database_name(database_name.into())?;
        Ok(Self { database_name })
    }

    /// The IndexedDB database name used by this store.
    pub fn database_name(&self) -> &str {
        &self.database_name
    }

    fn unsupported<T: 'static>() -> SyncLocalFuture<'static, T> {
        Box::pin(future::ready(Err(SyncError::client(
            "pocopine-sync-indexdb is only available in browser builds",
        ))))
    }
}

impl Default for IndexedDbLocalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncLocalStore for IndexedDbLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        Self::unsupported()
    }

    fn save_identity(&self, _identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
        Self::unsupported()
    }

    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId> {
        Self::unsupported()
    }

    fn hydrate_stream(&self, _stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot> {
        Self::unsupported()
    }

    fn save_snapshot(&self, _snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()> {
        Self::unsupported()
    }

    fn apply_changes(&self, _changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()> {
        Self::unsupported()
    }

    fn enqueue_mutation(
        &self,
        _stream: &SyncStreamName,
        _mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()> {
        Self::unsupported()
    }

    fn mark_push_result(&self, _result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        Self::unsupported()
    }

    fn clear_conflict(&self, _stream: &SyncStreamName, _key: &RowKey) -> SyncLocalFuture<'_, ()> {
        Self::unsupported()
    }

    fn pending_mutations(
        &self,
        _stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        Self::unsupported()
    }
}

fn validate_database_name(database_name: String) -> SyncResult<String> {
    if database_name.trim().is_empty() || database_name.chars().any(char::is_control) {
        return Err(SyncError::client(format!(
            "invalid IndexedDB local-store database name: {database_name:?}"
        )));
    }
    Ok(database_name)
}
