use super::*;
use crate::{
    RowKey, SyncChange, SyncCollectionName, SyncCursor, SyncDeviceId, SyncOp, SyncPushResponse,
    SyncRejectedMutation, SyncRow,
};

#[test]
fn local_identity_starts_at_first_mutation_counter() {
    let identity = SyncLocalIdentity::new(SyncDeviceId::new("device_abc").unwrap());

    assert_eq!(identity.device_id.as_str(), "device_abc");
    assert_eq!(identity.next_mutation_counter, 1);
}

#[test]
fn local_identity_rejects_zero_next_counter() {
    let err = SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 0)
        .unwrap_err();

    assert!(err.to_string().contains("next mutation counter"));
}

#[test]
fn local_identity_reserves_mutation_id_and_advances_counter() {
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 7).unwrap();

    let (id, advanced) = identity.reserve_mutation_id().unwrap();

    assert_eq!(id.as_str(), "device_abc:7");
    assert_eq!(advanced.device_id.as_str(), "device_abc");
    assert_eq!(advanced.next_mutation_counter, 8);
}

#[test]
fn local_identity_rejects_counter_overflow_without_id() {
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), u64::MAX)
            .unwrap();

    let err = identity.reserve_mutation_id().unwrap_err();

    assert!(err.to_string().contains("next mutation counter"));
}

#[test]
fn generate_sync_device_id_returns_valid_device_token() {
    let id = generate_sync_device_id().unwrap();

    assert!(id.as_str().starts_with("device_"));
}

#[test]
fn mutation_id_generator_uses_device_id_and_monotonic_counter() {
    let mut generator =
        MutationIdGenerator::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 41)
            .unwrap();

    assert_eq!(
        generator.next_mutation_id().unwrap().as_str(),
        "device_abc:41"
    );
    assert_eq!(
        generator.next_mutation_id().unwrap().as_str(),
        "device_abc:42"
    );
    assert_eq!(generator.next_counter(), 43);
}

#[test]
fn mutation_id_generator_rejects_zero_next_counter() {
    assert!(
        MutationIdGenerator::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 0)
            .is_err()
    );
}

#[test]
fn mutation_id_generator_rejects_counter_overflow_without_advancing() {
    let mut generator =
        MutationIdGenerator::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), u64::MAX)
            .unwrap();

    let err = generator.next_mutation_id().unwrap_err();

    assert!(err.to_string().contains("next mutation counter"));
    assert_eq!(generator.next_counter(), u64::MAX);
}

#[test]
fn local_snapshot_empty_has_no_rows_or_pending_mutations() {
    let snapshot = LocalStreamSnapshot::empty(SyncStreamName::new("posts").unwrap());

    assert_eq!(snapshot.stream.as_str(), "posts");
    assert!(snapshot.collection.is_none());
    assert!(snapshot.cursor.is_none());
    assert!(snapshot.rows.is_empty());
    assert!(snapshot.pending_mutations.is_empty());
}

#[test]
fn local_pending_mutation_reads_legacy_wire_shape() {
    let legacy = serde_json::json!({
        "id": "device_abc:1",
        "key": "post_1",
        "op": "upsert",
        "base_version": "row_1",
        "payload": {"title": "Legacy"}
    });

    let pending: LocalPendingMutation = serde_json::from_value(legacy).unwrap();

    assert_eq!(pending.mutation.id.as_str(), "device_abc:1");
    assert_eq!(pending.mutation.key.as_ref().unwrap().as_str(), "post_1");
    assert!(pending.optimistic_row.is_none());
}

#[test]
fn local_pending_mutation_records_optimistic_row() {
    let mutation = ClientMutation {
        id: MutationId::new("device_abc:1").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"op": "create"}),

        migration_outcome: None,
    };
    let optimistic = SyncRow::new("post_1", serde_json::json!({"title": "Visible"})).unwrap();

    let pending =
        LocalPendingMutation::new(mutation.clone()).with_optimistic_row(Some(optimistic.clone()));
    let round_trip: LocalPendingMutation =
        serde_json::from_str(&serde_json::to_string(&pending).unwrap()).unwrap();

    assert_eq!(round_trip.mutation, mutation);
    assert_eq!(round_trip.optimistic_row, Some(optimistic));
}

#[test]
fn local_push_result_preserves_server_outcomes() {
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    let row = SyncRow::new("post_1", serde_json::json!({"title": "Saved"})).unwrap();
    let mut response = SyncPushResponse::new(stream.clone());
    response.collection = Some(collection.clone());
    response
        .accepted
        .push(MutationId::new("device_abc:1").unwrap());
    response.rejected.push(SyncRejectedMutation {
        mutation_id: MutationId::new("device_abc:2").unwrap(),
        key: Some(RowKey::new("post_2").unwrap()),
        reason: "invalid title".to_string(),
        code: None,
    });
    response.rows.push(row);
    response.cursor = Some(SyncCursor::new("cursor_2").unwrap());

    let result = LocalPushResult::from_response(response);

    assert_eq!(result.stream, stream);
    assert_eq!(result.collection, Some(collection));
    assert_eq!(result.accepted[0].as_str(), "device_abc:1");
    assert_eq!(result.rejected[0].reason, "invalid title");
    assert_eq!(result.rows[0].key.as_str(), "post_1");
    assert_eq!(result.cursor.unwrap().as_str(), "cursor_2");
}

#[test]
fn local_batches_preserve_stream_collection_and_cursor() {
    let stream = SyncStreamName::new("posts").unwrap();
    let collection = SyncCollectionName::new("posts").unwrap();
    let cursor = Some(SyncCursor::new("cursor_1").unwrap());
    let row = SyncRow::new("post_1", serde_json::json!({"title": "Saved"})).unwrap();
    let snapshot = LocalSnapshotBatch::new(
        stream.clone(),
        collection.clone(),
        vec![row.clone()],
        cursor.clone(),
    );
    let change = SyncChange {
        stream: stream.clone(),
        collection: collection.clone(),
        key: Some(row.key.clone()),
        op: SyncOp::Upsert,
        row: Some(row),
        cursor: cursor.clone().unwrap(),
        origin: None,
    };
    let changes = LocalChangeBatch::new(stream, collection, vec![change], cursor);

    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(changes.changes.len(), 1);
}
