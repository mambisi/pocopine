#![cfg(target_arch = "wasm32")]

use pocopine_sync::{
    ClientMutation, LocalChangeBatch, LocalPendingMutation, LocalPushResult, LocalSnapshotBatch,
    MutationId, RowKey, RowVersion, SyncChange, SyncCollectionName, SyncCursor, SyncDeviceId,
    SyncLocalIdentity, SyncLocalStore, SyncOp, SyncPushResponse, SyncRow, SyncStreamName,
};
use pocopine_sync_indexdb::IndexedDbLocalStore;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;
use web_sys::{Event, IdbDatabase, IdbOpenDbRequest, IdbRequest, IdbTransactionMode};

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

        migration_outcome: None,
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
async fn indexeddb_store_reads_legacy_pending_mutation_shape() {
    let database_name = database_name("legacy_pending");
    let stream = SyncStreamName::new("posts").unwrap();
    let legacy = serde_json::json!({
        "stream": "posts",
        "collection": "posts",
        "cursor": "cursor_1",
        "rows": [],
        "pending_mutations": [{
            "id": "device_abc:1",
            "key": "post_1",
            "op": "upsert",
            "base_version": "row_1",
            "payload": {"title": "Legacy pending"}
        }]
    });
    seed_legacy_stream_state(&database_name, stream.as_str(), &legacy.to_string()).await;

    let store = IndexedDbLocalStore::with_database_name(database_name).unwrap();
    let snapshot = store.hydrate_stream(&stream).await.unwrap();

    assert_eq!(snapshot.pending_mutations.len(), 1);
    assert_eq!(
        snapshot.pending_mutations[0].mutation.id.as_str(),
        "device_abc:1"
    );
    assert_eq!(
        snapshot.pending_mutations[0].mutation.key.as_ref().unwrap(),
        &RowKey::new("post_1").unwrap()
    );
    assert!(snapshot.pending_mutations[0].optimistic_row.is_none());
    assert_eq!(
        store.pending_mutations(&stream).await.unwrap(),
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

        migration_outcome: None,
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
                origin: None,
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

async fn seed_legacy_stream_state(database_name: &str, stream: &str, json: &str) {
    let database = open_test_database(database_name).await;
    let transaction = database
        .transaction_with_str_and_mode("streams", IdbTransactionMode::Readwrite)
        .unwrap();
    let done = transaction_done(&transaction);
    let store = transaction.object_store("streams").unwrap();
    let request = store
        .put_with_key(&JsValue::from_str(json), &JsValue::from_str(stream))
        .unwrap();
    request_value(request).await.unwrap();
    done.await.unwrap();
    database.close();
}

async fn open_test_database(database_name: &str) -> IdbDatabase {
    let indexed_db = web_sys::window().unwrap().indexed_db().unwrap().unwrap();
    let request = indexed_db.open_with_u32(database_name, 1).unwrap();
    install_upgrade_handler(&request);
    request_value(request.unchecked_into())
        .await
        .unwrap()
        .dyn_into()
        .unwrap()
}

fn install_upgrade_handler(request: &IdbOpenDbRequest) {
    let handler_request = request.clone();
    let on_upgrade = Closure::<dyn FnMut(Event)>::new(move |_event| {
        if let Ok(database) = handler_request
            .result()
            .and_then(|value| value.dyn_into::<IdbDatabase>())
        {
            let _ = database.create_object_store("meta");
            let _ = database.create_object_store("streams");
        }
    });
    request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
    on_upgrade.forget();
}

fn request_value(request: IdbRequest) -> JsFuture {
    let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, reject| {
        let success_request = request.clone();
        let reject_from_success = reject.clone();
        let on_success =
            Closure::<dyn FnMut(Event)>::new(move |_event| match success_request.result() {
                Ok(value) => {
                    let _ = resolve.call1(&JsValue::UNDEFINED, &value);
                }
                Err(err) => {
                    let _ = reject_from_success.call1(&JsValue::UNDEFINED, &err);
                }
            });
        request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        on_success.forget();

        let error_request = request.clone();
        let on_error = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = reject.call1(&JsValue::UNDEFINED, &error_request.error().unwrap().into());
        });
        request.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
    });
    JsFuture::from(promise)
}

fn transaction_done(transaction: &web_sys::IdbTransaction) -> JsFuture {
    let transaction = transaction.clone();
    let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, reject| {
        let on_complete = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        transaction.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
        on_complete.forget();

        let error_transaction = transaction.clone();
        let on_error = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &error_transaction.error().unwrap().into(),
            );
        });
        transaction.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
    });
    JsFuture::from(promise)
}
