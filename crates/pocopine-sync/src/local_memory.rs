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
    pending: Vec<ClientMutation<serde_json::Value>>,
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
                pending_mutations: state.pending.clone(),
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
            let changes = changes_after_last_reset(changes.changes);
            if changes.had_reset {
                state.rows.clear();
            }

            for change in changes.items {
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
            if let Some(existing) = state
                .pending
                .iter_mut()
                .find(|existing| existing.id == mutation.id)
            {
                *existing = mutation;
            } else {
                state.pending.push(mutation);
            }
            Ok(())
        }))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(result.stream).or_default();
            if let Some(collection) = result.collection {
                state.collection = Some(collection);
            }
            if let Some(cursor) = result.cursor {
                state.cursor = Some(cursor);
            }

            for id in result.accepted {
                state.pending.retain(|mutation| mutation.id != id);
            }

            for rejected in result.rejected {
                state
                    .pending
                    .retain(|mutation| mutation.id != rejected.mutation_id);
            }

            for conflict in result.conflicts {
                state
                    .pending
                    .retain(|mutation| mutation.id != conflict.mutation_id);
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
                .map(|state| state.pending.clone())
                .unwrap_or_default())
        }))
    }
}

struct LocalChanges {
    had_reset: bool,
    items: Vec<crate::SyncChange<serde_json::Value>>,
}

fn changes_after_last_reset(changes: Vec<crate::SyncChange<serde_json::Value>>) -> LocalChanges {
    let Some(index) = changes
        .iter()
        .rposition(|change| change.op == SyncOp::Reset)
    else {
        return LocalChanges {
            had_reset: false,
            items: changes,
        };
    };

    LocalChanges {
        had_reset: true,
        items: changes.into_iter().skip(index).collect(),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
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
    async fn memory_store_pending_mutations_preserve_enqueue_order() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();

        for id in ["device_abc:10", "device_abc:2", "device_abc:1"] {
            store
                .enqueue_mutation(
                    &stream,
                    ClientMutation {
                        id: MutationId::new(id).unwrap(),
                        key: Some(RowKey::new("post_1").unwrap()),
                        op: SyncOp::Upsert,
                        base_version: None,
                        payload: serde_json::json!({ "id": id }),
                    },
                )
                .await
                .unwrap();
        }

        let pending = store.pending_mutations(&stream).await.unwrap();
        let ids: Vec<_> = pending
            .iter()
            .map(|mutation| mutation.id.as_str())
            .collect();

        assert_eq!(ids, vec!["device_abc:10", "device_abc:2", "device_abc:1"]);
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

    #[tokio::test]
    async fn memory_store_save_snapshot_replaces_previous_rows() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();

        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection.clone(),
                vec![
                    SyncRow::new("post_1", serde_json::json!({"title": "Old1"})).unwrap(),
                    SyncRow::new("post_2", serde_json::json!({"title": "Old2"})).unwrap(),
                ],
                Some(SyncCursor::new("cursor_1").unwrap()),
            ))
            .await
            .unwrap();

        let replacement =
            SyncRow::new("post_3", serde_json::json!({"title": "Only survivor"})).unwrap();
        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection,
                vec![replacement.clone()],
                Some(SyncCursor::new("cursor_2").unwrap()),
            ))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.rows, vec![replacement]);
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
    }

    #[tokio::test]
    async fn memory_store_save_snapshot_preserves_pending_mutations() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let mutation = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"title": "Local"}),
        };
        store
            .enqueue_mutation(&stream, mutation.clone())
            .await
            .unwrap();

        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", serde_json::json!({"title": "Server"})).unwrap()],
                Some(SyncCursor::new("cursor_1").unwrap()),
            ))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.pending_mutations, vec![mutation]);
        assert_eq!(snapshot.rows.len(), 1);
    }

    #[tokio::test]
    async fn memory_store_apply_changes_delete_missing_key_is_noop() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection.clone(),
                vec![SyncRow::new("post_1", serde_json::json!({"title": "Kept"})).unwrap()],
                None,
            ))
            .await
            .unwrap();

        let cursor = SyncCursor::new("cursor_2").unwrap();
        store
            .apply_changes(LocalChangeBatch::new(
                stream.clone(),
                collection.clone(),
                vec![SyncChange {
                    stream: stream.clone(),
                    collection,
                    key: Some(RowKey::new("never_existed").unwrap()),
                    op: SyncOp::Delete,
                    row: None,
                    cursor: cursor.clone(),
                }],
                Some(cursor),
            ))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].key.as_str(), "post_1");
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
    }

    #[tokio::test]
    async fn memory_store_apply_changes_reset_then_upsert_keeps_only_post_reset_rows() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection.clone(),
                vec![
                    SyncRow::new("old_1", serde_json::json!({"title": "Old1"})).unwrap(),
                    SyncRow::new("old_2", serde_json::json!({"title": "Old2"})).unwrap(),
                ],
                Some(SyncCursor::new("cursor_1").unwrap()),
            ))
            .await
            .unwrap();

        let cursor = SyncCursor::new("cursor_2").unwrap();
        let after_reset =
            SyncRow::new("post_after", serde_json::json!({"title": "After"})).unwrap();
        store
            .apply_changes(LocalChangeBatch::new(
                stream.clone(),
                collection.clone(),
                vec![
                    SyncChange {
                        stream: stream.clone(),
                        collection: collection.clone(),
                        key: Some(RowKey::new("ignored").unwrap()),
                        op: SyncOp::Upsert,
                        row: Some(
                            SyncRow::new("ignored", serde_json::json!({"title": "Pre-reset"}))
                                .unwrap(),
                        ),
                        cursor: cursor.clone(),
                    },
                    SyncChange {
                        stream: stream.clone(),
                        collection: collection.clone(),
                        key: None,
                        op: SyncOp::Reset,
                        row: None,
                        cursor: cursor.clone(),
                    },
                    SyncChange {
                        stream: stream.clone(),
                        collection,
                        key: Some(after_reset.key.clone()),
                        op: SyncOp::Upsert,
                        row: Some(after_reset.clone()),
                        cursor: cursor.clone(),
                    },
                ],
                Some(cursor),
            ))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.rows, vec![after_reset]);
    }

    #[tokio::test]
    async fn memory_store_conflict_with_key_only_marks_existing_row() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                collection,
                vec![SyncRow::new("post_1", serde_json::json!({"title": "Local"})).unwrap()],
                None,
            ))
            .await
            .unwrap();

        let mut response = SyncPushResponse::new(stream.clone());
        response.conflicts.push(crate::SyncConflict {
            mutation_id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: None,
            reason: "version mismatch".to_string(),
        });

        store
            .mark_push_result(LocalPushResult::from_response(response))
            .await
            .unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert!(snapshot.rows[0].conflict);
        assert!(!snapshot.rows[0].pending);
        assert_eq!(
            snapshot.rows[0].value,
            serde_json::json!({"title": "Local"})
        );
    }

    #[tokio::test]
    async fn memory_store_hydrate_returns_empty_for_unknown_stream() {
        let store = MemoryLocalStore::new();
        let stream = SyncStreamName::new("never_seen").unwrap();
        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert_eq!(snapshot.stream, stream);
        assert!(snapshot.collection.is_none());
        assert!(snapshot.cursor.is_none());
        assert!(snapshot.rows.is_empty());
        assert!(snapshot.pending_mutations.is_empty());
    }

    #[tokio::test]
    async fn memory_store_pending_mutations_isolate_streams() {
        let store = MemoryLocalStore::new();
        let posts = SyncStreamName::new("posts").unwrap();
        let comments = SyncStreamName::new("comments").unwrap();
        let post_mutation = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"title": "Post"}),
        };
        let comment_mutation = ClientMutation {
            id: MutationId::new("device_abc:2").unwrap(),
            key: Some(RowKey::new("comment_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"body": "Comment"}),
        };
        store
            .enqueue_mutation(&posts, post_mutation.clone())
            .await
            .unwrap();
        store
            .enqueue_mutation(&comments, comment_mutation.clone())
            .await
            .unwrap();

        assert_eq!(
            store.pending_mutations(&posts).await.unwrap(),
            vec![post_mutation]
        );
        assert_eq!(
            store.pending_mutations(&comments).await.unwrap(),
            vec![comment_mutation]
        );
    }
}
