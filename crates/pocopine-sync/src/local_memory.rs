use std::{
    collections::BTreeMap,
    future,
    sync::{Arc, Mutex},
};

use crate::{
    ClientMutation, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot,
    RowKey, SyncCollectionName, SyncError, SyncLocalFuture, SyncLocalIdentity, SyncLocalStore,
    SyncOp, SyncResult, SyncRow, SyncStreamName,
};

/// In-memory [`SyncLocalStore`] implementation for tests and demos.
///
/// This store is process-local and not durable. It pins the local-store
/// semantics before browser SQLite persistence is added.
#[derive(Clone, Debug, Default)]
pub struct MemoryLocalStore {
    inner: Arc<Mutex<MemoryLocalStoreInner>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryLocalStoreInner {
    identity: Option<SyncLocalIdentity>,
    streams: BTreeMap<SyncStreamName, MemoryStreamState>,
}

#[derive(Clone, Debug, Default)]
struct MemoryStreamState {
    collection: Option<SyncCollectionName>,
    cursor: Option<crate::SyncCursor>,
    rows: BTreeMap<RowKey, SyncRow<serde_json::Value>>,
    pending: BTreeMap<crate::MutationId, ClientMutation<serde_json::Value>>,
}

impl MemoryLocalStore {
    /// Build an empty in-memory local store.
    pub fn new() -> Self {
        Self::default()
    }

    fn with_inner<T>(
        &self,
        f: impl FnOnce(&mut MemoryLocalStoreInner) -> SyncResult<T>,
    ) -> SyncResult<T> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| SyncError::backend("memory local sync store lock poisoned"))?;
        f(&mut inner)
    }

    fn ready<T: 'static>(result: SyncResult<T>) -> SyncLocalFuture<'static, T> {
        Box::pin(future::ready(result))
    }
}

impl SyncLocalStore for MemoryLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        Self::ready(self.with_inner(|inner| Ok(inner.identity.clone())))
    }

    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            inner.identity = Some(identity);
            Ok(())
        }))
    }

    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            let Some(state) = inner.streams.get(&stream) else {
                return Ok(LocalStreamSnapshot::empty(stream));
            };
            Ok(LocalStreamSnapshot {
                stream,
                collection: state.collection.clone(),
                cursor: state.cursor.clone(),
                rows: state.rows.values().cloned().collect(),
                pending_mutations: state.pending.values().cloned().collect(),
            })
        }))
    }

    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(snapshot.stream).or_default();
            state.collection = Some(snapshot.collection);
            state.cursor = snapshot.cursor;
            state.rows = snapshot
                .rows
                .into_iter()
                .map(|row| (row.key.clone(), row))
                .collect();
            Ok(())
        }))
    }

    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(changes.stream).or_default();
            state.collection = Some(changes.collection);
            state.cursor = changes.cursor;

            for change in changes.changes {
                match change.op {
                    SyncOp::Upsert => {
                        if let Some(row) = change.row {
                            state.rows.insert(row.key.clone(), row);
                        }
                    }
                    SyncOp::Delete => {
                        if let Some(key) = change.key {
                            state.rows.remove(&key);
                        }
                    }
                    SyncOp::Reset => {
                        state.rows.clear();
                        if let Some(row) = change.row {
                            state.rows.insert(row.key.clone(), row);
                        }
                    }
                }
            }
            Ok(())
        }))
    }

    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(stream).or_default();
            state.pending.insert(mutation.id.clone(), mutation);
            Ok(())
        }))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(result.stream).or_default();
            state.cursor = result.cursor;

            for id in result.accepted {
                state.pending.remove(&id);
            }

            for rejected in result.rejected {
                state.pending.remove(&rejected.mutation_id);
            }

            for conflict in result.conflicts {
                state.pending.remove(&conflict.mutation_id);
                if let Some(mut row) = conflict.server_row {
                    row.pending = false;
                    row.conflict = true;
                    state.rows.insert(row.key.clone(), row);
                } else if let Some(key) = conflict.key {
                    if let Some(row) = state.rows.get_mut(&key) {
                        row.pending = false;
                        row.conflict = true;
                    }
                }
            }

            for mut row in result.rows {
                row.pending = false;
                row.conflict = false;
                state.rows.insert(row.key.clone(), row);
            }

            Ok(())
        }))
    }

    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            Ok(inner
                .streams
                .get(&stream)
                .map(|state| state.pending.values().cloned().collect())
                .unwrap_or_default())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LocalChangeBatch, LocalSnapshotBatch, MutationId, RowVersion, SyncChange,
        SyncCollectionName, SyncCursor, SyncDeviceId, SyncPushResponse,
    };

    #[tokio::test]
    async fn memory_store_persists_identity() {
        let store = MemoryLocalStore::new();
        let identity =
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 7)
                .unwrap();

        assert!(store.load_identity().await.unwrap().is_none());
        store.save_identity(identity.clone()).await.unwrap();

        assert_eq!(store.load_identity().await.unwrap(), Some(identity));
    }

    #[tokio::test]
    async fn memory_store_saves_snapshot_and_hydrates_rows() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        let row = SyncRow::new("post_1", serde_json::json!({"title": "First"})).unwrap();

        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection.clone(),
                vec![row.clone()],
                Some(SyncCursor::new("cursor_1").unwrap()),
            ))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();

        assert_eq!(snapshot.stream, stream);
        assert_eq!(snapshot.collection, Some(collection));
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_1");
        assert_eq!(snapshot.rows, vec![row]);
    }

    #[tokio::test]
    async fn memory_store_applies_incremental_changes() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        let row = SyncRow::new("post_1", serde_json::json!({"title": "First"})).unwrap();
        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection.clone(),
                vec![row],
                Some(SyncCursor::new("cursor_1").unwrap()),
            ))
            .await
            .unwrap();

        let updated = SyncRow::new("post_1", serde_json::json!({"title": "Updated"}))
            .unwrap()
            .version("row_2")
            .unwrap();
        let second = SyncRow::new("post_2", serde_json::json!({"title": "Second"})).unwrap();
        let cursor = SyncCursor::new("cursor_2").unwrap();

        store
            .apply_changes(LocalChangeBatch::new(
                stream.clone(),
                collection,
                vec![
                    SyncChange {
                        stream: stream.clone(),
                        collection: SyncCollectionName::new("posts").unwrap(),
                        key: Some(RowKey::new("post_1").unwrap()),
                        op: SyncOp::Upsert,
                        row: Some(updated.clone()),
                        cursor: cursor.clone(),
                    },
                    SyncChange {
                        stream: stream.clone(),
                        collection: SyncCollectionName::new("posts").unwrap(),
                        key: Some(RowKey::new("post_2").unwrap()),
                        op: SyncOp::Upsert,
                        row: Some(second.clone()),
                        cursor: cursor.clone(),
                    },
                ],
                Some(cursor),
            ))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();

        assert_eq!(snapshot.rows, vec![updated, second]);
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
    }

    #[tokio::test]
    async fn memory_store_replays_pending_and_clears_push_outcomes() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let mutation = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: Some(RowVersion::new("row_1").unwrap()),
            payload: serde_json::json!({"title": "Saved"}),
        };

        store
            .enqueue_mutation(&stream, mutation.clone())
            .await
            .unwrap();

        assert_eq!(
            store.pending_mutations(&stream).await.unwrap(),
            vec![mutation]
        );

        let row = SyncRow::new("post_1", serde_json::json!({"title": "Saved"}))
            .unwrap()
            .version("row_2")
            .unwrap();
        let mut response = SyncPushResponse::new(stream.clone());
        response
            .accepted
            .push(MutationId::new("device_abc:1").unwrap());
        response.rows.push(row.clone());
        response.cursor = Some(SyncCursor::new("cursor_2").unwrap());

        store
            .mark_push_result(LocalPushResult::from_response(response))
            .await
            .unwrap();

        assert!(store.pending_mutations(&stream).await.unwrap().is_empty());
        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.rows, vec![row]);
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
    }

    #[tokio::test]
    async fn memory_store_marks_conflict_rows() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        let row = SyncRow::new("post_1", serde_json::json!({"title": "Server"})).unwrap();
        let mut response = SyncPushResponse::new(stream.clone());
        response.conflicts.push(crate::SyncConflict {
            mutation_id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(row),
            reason: "base version is stale".to_string(),
        });

        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection,
                Vec::new(),
                None,
            ))
            .await
            .unwrap();
        store
            .mark_push_result(LocalPushResult::from_response(response))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();

        assert_eq!(snapshot.rows.len(), 1);
        assert!(snapshot.rows[0].conflict);
        assert!(!snapshot.rows[0].pending);
    }
}
