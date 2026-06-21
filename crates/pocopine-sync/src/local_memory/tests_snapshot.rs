use super::*;
use crate::{
    LocalChangeBatch, LocalSnapshotBatch, RowKey, SyncChange, SyncCollectionName, SyncCursor,
    SyncLocalStore, SyncOp, SyncRow, SyncStreamName,
};

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
    let after_reset = SyncRow::new("post_after", serde_json::json!({"title": "After"})).unwrap();
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
                        SyncRow::new("ignored", serde_json::json!({"title": "Pre-reset"})).unwrap(),
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
async fn memory_store_apply_changes_reset_without_row_clears_rows() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![
                SyncRow::new("post_1", serde_json::json!({"title": "One"})).unwrap(),
                SyncRow::new("post_2", serde_json::json!({"title": "Two"})).unwrap(),
            ],
            Some(SyncCursor::new("cursor_1").unwrap()),
        ))
        .await
        .unwrap();

    let cursor = SyncCursor::new("cursor_2").unwrap();
    store
        .apply_changes(LocalChangeBatch::new(
            stream.clone(),
            collection,
            vec![SyncChange {
                stream: stream.clone(),
                collection: SyncCollectionName::new("posts").unwrap(),
                key: None,
                op: SyncOp::Reset,
                row: None,
                cursor: cursor.clone(),
            }],
            Some(cursor),
        ))
        .await
        .unwrap();

    let snapshot = store.hydrate_stream(&stream).await.unwrap();
    assert!(snapshot.rows.is_empty());
    assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
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
async fn save_snapshot_records_application_schema_version() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    store
        .save_snapshot(
            LocalSnapshotBatch::new(
                stream.clone(),
                SyncCollectionName::new("posts").unwrap(),
                vec![],
                None,
            )
            .with_application_schema_version(Some(7)),
        )
        .await
        .unwrap();
    let s = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(s.application_schema_version, Some(7));
}

#[tokio::test]
async fn save_snapshot_with_none_preserves_existing_application_schema_version() {
    // `save_snapshot` with `application_schema_version = None` must
    // NOT clobber a previously-recorded value — only an explicit
    // `Some` overwrites. Mirrors the SQLite UPSERT's coalesce.
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    store
        .save_snapshot(
            LocalSnapshotBatch::new(
                stream.clone(),
                SyncCollectionName::new("posts").unwrap(),
                vec![],
                None,
            )
            .with_application_schema_version(Some(3)),
        )
        .await
        .unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            SyncCollectionName::new("posts").unwrap(),
            vec![],
            None,
        ))
        .await
        .unwrap();
    let s = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(s.application_schema_version, Some(3));
}
