use super::*;
use crate::{
    ClientMutation, LocalPendingMutation, LocalPushResult, LocalSnapshotBatch, MutationId, RowKey,
    RowVersion, SyncCollectionName, SyncCursor, SyncLocalStore, SyncOp, SyncPushResponse, SyncRow,
    SyncStreamName,
};

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

        migration_outcome: None,
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

                    migration_outcome: None,
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
async fn memory_store_duplicate_mutation_id_replaces_in_place() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let first = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: Some(RowVersion::new("row_1").unwrap()),
        payload: serde_json::json!({"title": "First"}),

        migration_outcome: None,
    };
    let second = ClientMutation {
        id: MutationId::new("device_abc:2").unwrap(),
        key: Some(RowKey::new("post_2").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"title": "Second"}),

        migration_outcome: None,
    };
    let replacement = ClientMutation {
        id: first.id.clone(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: Some(RowVersion::new("row_2").unwrap()),
        payload: serde_json::json!({"title": "Replacement"}),

        migration_outcome: None,
    };

    store.enqueue_mutation(&stream, first).await.unwrap();
    store
        .enqueue_mutation(&stream, second.clone())
        .await
        .unwrap();
    store
        .enqueue_mutation(&stream, replacement.clone())
        .await
        .unwrap();

    assert_eq!(
        store.pending_mutations(&stream).await.unwrap(),
        vec![replacement, second]
    );
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
async fn memory_store_clears_conflict_rows() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    let row = SyncRow::new("post_1", serde_json::json!({"title": "Server"}))
        .unwrap()
        .conflict(true);

    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection,
            vec![row],
            None,
        ))
        .await
        .unwrap();

    store
        .clear_conflict(&stream, &RowKey::new("post_1").unwrap())
        .await
        .unwrap();

    let snapshot = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(snapshot.rows.len(), 1);
    assert!(!snapshot.rows[0].conflict);
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

        migration_outcome: None,
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
    assert_eq!(
        snapshot.pending_mutations,
        vec![LocalPendingMutation::new(mutation)]
    );
    assert_eq!(snapshot.rows.len(), 1);
}

#[tokio::test]
async fn memory_store_preserves_pending_optimistic_rows() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let mutation = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({
            "op": "create",
            "payload": {"id": "post_1", "draft": {"title": "Envelope only"}}
        }),

        migration_outcome: None,
    };
    let optimistic = SyncRow::new(
        "post_1",
        serde_json::json!({"id": "post_1", "title": "Visible"}),
    )
    .unwrap();

    store
        .enqueue_pending_mutation(
            &stream,
            LocalPendingMutation::new(mutation.clone())
                .with_optimistic_row(Some(optimistic.clone())),
        )
        .await
        .unwrap();

    let snapshot = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(snapshot.pending_mutations[0].mutation, mutation);
    assert_eq!(
        snapshot.pending_mutations[0].optimistic_row.as_ref(),
        Some(&optimistic)
    );
    assert_eq!(
        store.pending_mutations(&stream).await.unwrap(),
        vec![snapshot.pending_mutations[0].mutation.clone()]
    );
}

#[tokio::test]
async fn memory_store_push_result_preserves_cursor_when_response_has_none() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![SyncRow::new("post_1", serde_json::json!({"title": "Old"})).unwrap()],
            Some(SyncCursor::new("cursor_1").unwrap()),
        ))
        .await
        .unwrap();

    let row = SyncRow::new("post_1", serde_json::json!({"title": "New"})).unwrap();
    let mut response = SyncPushResponse::new(stream.clone());
    response.rows.push(row.clone());

    store
        .mark_push_result(LocalPushResult::from_response(response))
        .await
        .unwrap();

    let snapshot = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(snapshot.collection, Some(collection));
    assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_1");
    assert_eq!(snapshot.rows, vec![row]);
}

#[tokio::test]
async fn memory_store_push_result_only_clears_matching_pending_mutations() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let accepted = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"title": "Accepted"}),

        migration_outcome: None,
    };
    let rejected = ClientMutation {
        id: MutationId::new("device_abc:2").unwrap(),
        key: Some(RowKey::new("post_2").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"title": "Rejected"}),

        migration_outcome: None,
    };
    let still_pending = ClientMutation {
        id: MutationId::new("device_abc:3").unwrap(),
        key: Some(RowKey::new("post_3").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"title": "Pending"}),

        migration_outcome: None,
    };
    for mutation in [accepted.clone(), rejected.clone(), still_pending.clone()] {
        store.enqueue_mutation(&stream, mutation).await.unwrap();
    }

    let mut response = SyncPushResponse::new(stream.clone());
    response.accepted.push(accepted.id);
    response.rejected.push(crate::SyncRejectedMutation {
        mutation_id: rejected.id,
        key: rejected.key,
        reason: "not allowed".to_string(),
        code: None,
    });

    store
        .mark_push_result(LocalPushResult::from_response(response))
        .await
        .unwrap();

    assert_eq!(
        store.pending_mutations(&stream).await.unwrap(),
        vec![still_pending]
    );
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

        migration_outcome: None,
    };
    let comment_mutation = ClientMutation {
        id: MutationId::new("device_abc:2").unwrap(),
        key: Some(RowKey::new("comment_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"body": "Comment"}),

        migration_outcome: None,
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
