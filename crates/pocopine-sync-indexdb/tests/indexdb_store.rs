#![cfg(target_arch = "wasm32")]

use pocopine_sync::{
    ClientMutation, LocalChangeBatch, LocalPendingMutation, LocalPushResult, LocalSnapshotBatch,
    MutationId, RowKey, RowVersion, SyncChange, SyncCollectionName, SyncCursor, SyncDeviceId,
    SyncLocalIdentity, SyncLocalStore, SyncOp, SyncPushResponse, SyncRow, SyncStreamName,
};
use pocopine_sync_indexdb::IndexedDbLocalStore;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn database_name(test: &str) -> String {
    format!(
        "pocopine_sync_indexdb_{test}_{}_{}",
        js_sys::Date::now(),
        js_sys::Math::random()
    )
}

#[wasm_bindgen_test(async)]
async fn indexeddb_store_persists_identity_snapshot_and_pending_mutations() {
    let store = IndexedDbLocalStore::with_database_name(database_name("persistence")).unwrap();
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 7).unwrap();
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    let row = SyncRow::new("post_1", serde_json::json!({"title": "First"})).unwrap();
    let mutation = ClientMutation {
        id: MutationId::new("device_abc:7").unwrap(),
        key: Some(RowKey::new("post_2").unwrap()),
        op: SyncOp::Upsert,
        base_version: Some(RowVersion::new("row_1").unwrap()),
        payload: serde_json::json!({
            "op": "create",
            "payload": {"id": "post_2", "draft": {"title": "Pending"}}
        }),
    };
    let optimistic = SyncRow::new("post_2", serde_json::json!({"title": "Pending"})).unwrap();

    store.save_identity(identity.clone()).await.unwrap();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![row.clone()],
            Some(SyncCursor::new("cursor_1").unwrap()),
        ))
        .await
        .unwrap();
    store
        .enqueue_pending_mutation(
            &stream,
            LocalPendingMutation::new(mutation.clone())
                .with_optimistic_row(Some(optimistic.clone())),
        )
        .await
        .unwrap();

    let reopened = IndexedDbLocalStore::with_database_name(store.database_name()).unwrap();
    assert_eq!(reopened.load_identity().await.unwrap(), Some(identity));

    let snapshot = reopened.hydrate_stream(&stream).await.unwrap();
    assert_eq!(snapshot.collection, Some(collection));
    assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_1");
    assert_eq!(snapshot.rows, vec![row]);
    assert_eq!(snapshot.pending_mutations[0].mutation, mutation);
    assert_eq!(
        snapshot.pending_mutations[0].optimistic_row.as_ref(),
        Some(&optimistic)
    );
    assert_eq!(
        reopened.pending_mutations(&stream).await.unwrap(),
        vec![snapshot.pending_mutations[0].mutation.clone()]
    );
}

#[wasm_bindgen_test(async)]
async fn indexeddb_store_applies_changes_and_push_results() {
    let store = IndexedDbLocalStore::with_database_name(database_name("changes")).unwrap();
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    let original = SyncRow::new("post_1", serde_json::json!({"title": "Original"})).unwrap();
    let updated = SyncRow::new("post_1", serde_json::json!({"title": "Updated"}))
        .unwrap()
        .version("row_2")
        .unwrap();
    let pending = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: Some(RowVersion::new("row_1").unwrap()),
        payload: serde_json::json!({"title": "Updated"}),
    };

    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![original],
            Some(SyncCursor::new("cursor_1").unwrap()),
        ))
        .await
        .unwrap();
    store.enqueue_mutation(&stream, pending).await.unwrap();
    store
        .apply_changes(LocalChangeBatch::new(
            stream.clone(),
            collection,
            vec![SyncChange {
                stream: stream.clone(),
                collection: SyncCollectionName::new("posts").unwrap(),
                key: Some(RowKey::new("post_1").unwrap()),
                op: SyncOp::Upsert,
                row: Some(updated.clone()),
                cursor: SyncCursor::new("cursor_2").unwrap(),
            }],
            Some(SyncCursor::new("cursor_2").unwrap()),
        ))
        .await
        .unwrap();

    let mut response = SyncPushResponse::new(stream.clone());
    response
        .accepted
        .push(MutationId::new("device_abc:1").unwrap());
    response.rows.push(updated.clone());
    response.cursor = Some(SyncCursor::new("cursor_3").unwrap());
    store
        .mark_push_result(LocalPushResult::from_response(response))
        .await
        .unwrap();

    assert!(store.pending_mutations(&stream).await.unwrap().is_empty());
    let snapshot = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_3");
    assert_eq!(snapshot.rows, vec![updated]);
}
