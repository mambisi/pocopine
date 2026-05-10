#![cfg(target_arch = "wasm32")]

use pocopine_sync::{
    ClientMutation, LocalPushResult, LocalSnapshotBatch, MutationId, RowKey, SyncCollectionName,
    SyncCursor, SyncDeviceId, SyncLocalIdentity, SyncLocalStore, SyncOp, SyncRow, SyncStreamName,
};
use pocopine_sync_sqlite::SqliteLocalStore;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn sqlite_wasm_store_round_trips_cached_rows_and_pending_mutations() {
    let store = SqliteLocalStore::with_database_name(test_database_name()).unwrap();
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_browser").unwrap(), 3)
            .unwrap();
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    let row = SyncRow::new("post_1", serde_json::json!({"title": "Cached"}))
        .unwrap()
        .version("row_1")
        .unwrap();

    store.save_identity(identity.clone()).await.unwrap();
    assert_eq!(store.load_identity().await.unwrap(), Some(identity));

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
    assert_eq!(snapshot.collection, Some(collection));
    assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_1");
    assert_eq!(snapshot.rows, vec![row]);

    let mutation = ClientMutation {
        id: MutationId::new("device_browser:3").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: Some(pocopine_sync::RowVersion::new("row_1").unwrap()),
        payload: serde_json::json!({"title": "Updated"}),
    };
    store
        .enqueue_mutation(&stream, mutation.clone())
        .await
        .unwrap();
    assert_eq!(
        store.pending_mutations(&stream).await.unwrap(),
        vec![mutation]
    );

    store
        .mark_push_result(LocalPushResult {
            stream: stream.clone(),
            accepted: vec![MutationId::new("device_browser:3").unwrap()],
            rejected: Vec::new(),
            rows: vec![
                SyncRow::new("post_1", serde_json::json!({"title": "Updated"}))
                    .unwrap()
                    .version("row_2")
                    .unwrap(),
            ],
            conflicts: Vec::new(),
            cursor: Some(SyncCursor::new("cursor_2").unwrap()),
        })
        .await
        .unwrap();

    assert!(store.pending_mutations(&stream).await.unwrap().is_empty());
    let updated = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(updated.cursor.unwrap().as_str(), "cursor_2");
    assert_eq!(updated.rows[0].value["title"], "Updated");

    let _ = sqlite_wasm::close().await;
}

fn test_database_name() -> String {
    let random = (js_sys::Math::random() * 1_000_000_000.0) as u32;
    format!(
        "pocopine_sync_test_{}_{}.sqlite3",
        js_sys::Date::now() as u64,
        random
    )
}
