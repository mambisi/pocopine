use pocopine_sync::{
    ClientMutation, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot,
    SyncError, SyncLocalFuture, SyncLocalIdentity, SyncLocalStore, SyncStreamName,
};

/// Browser SQLite local-store placeholder.
///
/// The schema and trait contract are shared with the native implementation,
/// but browser persistence requires a SQLite WASM + OPFS worker binding. Until
/// that lands, this type compiles on wasm and returns a clear error instead of
/// pretending that data is durable.
#[derive(Clone, Debug, Default)]
pub struct SqliteLocalStore;

impl SqliteLocalStore {
    pub fn new() -> Self {
        Self
    }

    fn unsupported<T>() -> SyncLocalFuture<'static, T> {
        Box::pin(async {
            Err(SyncError::unsupported(
                "pocopine-sync-sqlite browser OPFS backend is not implemented yet",
            ))
        })
    }
}

impl SyncLocalStore for SqliteLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        Self::unsupported()
    }

    fn save_identity(&self, _identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
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

    fn pending_mutations(
        &self,
        _stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        Self::unsupported()
    }
}
