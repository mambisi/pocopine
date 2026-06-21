use super::*;
use crate::{
    ClientMutation, LocalSnapshotBatch, MutationId, RowKey, SyncCollectionName, SyncCursor,
    SyncDeviceId, SyncLocalIdentity, SyncLocalStore, SyncOp, SyncRow, SyncStreamName,
};

#[tokio::test]
async fn purge_pending_for_row_drops_only_target_row_mutations() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let post_a = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: Some(RowKey::new("post_a").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({ "title": "A first edit" }),

        migration_outcome: None,
    };
    let post_a_again = ClientMutation {
        id: MutationId::new("device_abc:2").unwrap(),
        key: Some(RowKey::new("post_a").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({ "title": "A second edit" }),

        migration_outcome: None,
    };
    let post_b = ClientMutation {
        id: MutationId::new("device_abc:3").unwrap(),
        key: Some(RowKey::new("post_b").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({ "title": "B" }),

        migration_outcome: None,
    };
    for m in [&post_a, &post_a_again, &post_b] {
        store.enqueue_mutation(&stream, m.clone()).await.unwrap();
    }

    let purged = store
        .purge_pending_for_row(&stream, &RowKey::new("post_a").unwrap())
        .await
        .unwrap();

    assert_eq!(purged, 2, "both pending mutations for post_a removed");
    assert_eq!(
        store.pending_mutations(&stream).await.unwrap(),
        vec![post_b],
        "post_b mutation must survive",
    );
}

#[tokio::test]
async fn purge_pending_for_row_leaves_unkeyed_and_other_streams_alone() {
    let store = MemoryLocalStore::new();
    let posts = SyncStreamName::new("posts").unwrap();
    let comments = SyncStreamName::new("comments").unwrap();
    let unkeyed = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: None,
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"global": true}),

        migration_outcome: None,
    };
    let keyed_in_other_stream = ClientMutation {
        id: MutationId::new("device_abc:2").unwrap(),
        key: Some(RowKey::new("post_a").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({}),

        migration_outcome: None,
    };
    store
        .enqueue_mutation(&posts, unkeyed.clone())
        .await
        .unwrap();
    store
        .enqueue_mutation(&comments, keyed_in_other_stream.clone())
        .await
        .unwrap();

    let purged = store
        .purge_pending_for_row(&posts, &RowKey::new("post_a").unwrap())
        .await
        .unwrap();

    assert_eq!(purged, 0, "no mutation in posts targets post_a");
    // The unkeyed mutation in the same stream stays — its key is
    // None, so it doesn't match the target row.
    assert_eq!(
        store.pending_mutations(&posts).await.unwrap(),
        vec![unkeyed]
    );
    // The other stream's row-keyed mutation is fully untouched.
    assert_eq!(
        store.pending_mutations(&comments).await.unwrap(),
        vec![keyed_in_other_stream]
    );
}

#[tokio::test]
async fn purge_pending_for_row_empty_stream_returns_zero() {
    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new("posts").unwrap();
    let purged = store
        .purge_pending_for_row(&stream, &RowKey::new("post_x").unwrap())
        .await
        .unwrap();
    assert_eq!(purged, 0);
}

#[tokio::test]
async fn clear_all_streams_drops_rows_and_pendings_across_streams() {
    let store = MemoryLocalStore::new();
    let posts = SyncStreamName::new("posts").unwrap();
    let comments = SyncStreamName::new("comments").unwrap();
    let collection_posts = SyncCollectionName::new("posts").unwrap();
    let collection_comments = SyncCollectionName::new("comments").unwrap();

    store
        .save_snapshot(LocalSnapshotBatch::new(
            posts.clone(),
            collection_posts,
            vec![SyncRow::new("post_1", serde_json::json!({"title": "p1"})).unwrap()],
            Some(SyncCursor::new("c_posts").unwrap()),
        ))
        .await
        .unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            comments.clone(),
            collection_comments,
            vec![SyncRow::new("c_1", serde_json::json!({"body": "hi"})).unwrap()],
            Some(SyncCursor::new("c_comments").unwrap()),
        ))
        .await
        .unwrap();
    store
        .enqueue_mutation(
            &posts,
            ClientMutation {
                id: MutationId::new("device_abc:1").unwrap(),
                key: Some(RowKey::new("post_1").unwrap()),
                op: SyncOp::Upsert,
                base_version: None,
                payload: serde_json::json!({"title": "edit"}),

                migration_outcome: None,
            },
        )
        .await
        .unwrap();

    store.clear_all_streams().await.unwrap();

    let posts_after = store.hydrate_stream(&posts).await.unwrap();
    let comments_after = store.hydrate_stream(&comments).await.unwrap();
    assert!(posts_after.rows.is_empty());
    assert!(posts_after.cursor.is_none());
    assert!(posts_after.pending_mutations.is_empty());
    assert!(comments_after.rows.is_empty());
    assert!(comments_after.cursor.is_none());
}

#[tokio::test]
async fn clear_all_streams_preserves_identity_and_counter() {
    let store = MemoryLocalStore::new();
    store
        .save_identity(
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 42)
                .unwrap(),
        )
        .await
        .unwrap();

    store.clear_all_streams().await.unwrap();

    let identity = store.load_identity().await.unwrap().unwrap();
    assert_eq!(identity.device_id.as_str(), "device_abc");
    assert_eq!(identity.next_mutation_counter, 42);
}

#[tokio::test]
async fn clear_all_streams_is_safe_on_empty_store() {
    let store = MemoryLocalStore::new();
    store.clear_all_streams().await.unwrap();
    let stream = SyncStreamName::new("posts").unwrap();
    let snapshot = store.hydrate_stream(&stream).await.unwrap();
    assert!(snapshot.rows.is_empty());
}

#[tokio::test]
async fn clear_stream_wipes_one_stream_and_leaves_others_intact() {
    let store = MemoryLocalStore::new();
    let posts = SyncStreamName::new("posts").unwrap();
    let comments = SyncStreamName::new("comments").unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            posts.clone(),
            SyncCollectionName::new("posts").unwrap(),
            vec![SyncRow::new("post_1", serde_json::json!({"title": "p1"})).unwrap()],
            Some(SyncCursor::new("c_posts").unwrap()),
        ))
        .await
        .unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            comments.clone(),
            SyncCollectionName::new("comments").unwrap(),
            vec![SyncRow::new("c_1", serde_json::json!({"body": "hi"})).unwrap()],
            Some(SyncCursor::new("c_comments").unwrap()),
        ))
        .await
        .unwrap();
    store
        .enqueue_mutation(
            &posts,
            ClientMutation {
                id: MutationId::new("device_a:1").unwrap(),
                key: Some(RowKey::new("post_1").unwrap()),
                op: SyncOp::Upsert,
                base_version: None,
                payload: serde_json::json!({"title": "edit"}),

                migration_outcome: None,
            },
        )
        .await
        .unwrap();

    store.clear_stream(&posts).await.unwrap();

    let posts_after = store.hydrate_stream(&posts).await.unwrap();
    let comments_after = store.hydrate_stream(&comments).await.unwrap();
    assert!(posts_after.rows.is_empty());
    assert!(posts_after.pending_mutations.is_empty());
    assert!(posts_after.cursor.is_none());
    assert!(posts_after.application_schema_version.is_none());
    // The other stream is untouched.
    assert_eq!(comments_after.rows.len(), 1);
    assert_eq!(comments_after.rows[0].key.as_str(), "c_1");
}

#[tokio::test]
async fn clear_stream_is_safe_on_never_observed_stream() {
    let store = MemoryLocalStore::new();
    let unknown = SyncStreamName::new("unknown").unwrap();
    store.clear_stream(&unknown).await.unwrap();
}
