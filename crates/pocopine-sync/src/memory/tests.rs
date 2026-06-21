use super::*;
use crate::{MutationId, RowKey, RowVersion, SyncPullMode};

#[test]
fn memory_stream_returns_snapshot_then_incremental_changes() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    stream.upsert("post_1", "one".to_string()).unwrap();
    let first = stream
        .pull_value(SyncPullRequest::new(
            SyncStreamName::new("posts_for_tenant").unwrap(),
        ))
        .unwrap();
    assert_eq!(first.mode, SyncPullMode::Snapshot);
    assert_eq!(first.rows.len(), 1);

    stream.upsert("post_2", "two".to_string()).unwrap();
    let second = stream
        .pull_value(
            SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap())
                .cursor(first.cursor.clone()),
        )
        .unwrap();
    assert_eq!(second.mode, SyncPullMode::Incremental);
    assert_eq!(second.changes.len(), 1);
    assert_eq!(
        second.changes[0].row.as_ref().unwrap().key.as_str(),
        "post_2"
    );
}

#[test]
fn memory_stream_returns_delete_and_reset_changes() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    stream.upsert("post_1", "one".to_string()).unwrap();
    let first = stream
        .pull_value(SyncPullRequest::new(
            SyncStreamName::new("posts_for_tenant").unwrap(),
        ))
        .unwrap();

    stream.delete("post_1").unwrap();
    stream.reset().unwrap();

    let second = stream
        .pull_value(
            SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap())
                .cursor(first.cursor.clone()),
        )
        .unwrap();
    assert_eq!(second.mode, SyncPullMode::Incremental);
    assert_eq!(second.changes.len(), 2);
    assert_eq!(second.changes[0].op, SyncOp::Delete);
    assert_eq!(second.changes[0].key.as_ref().unwrap().as_str(), "post_1");
    assert_eq!(second.changes[1].op, SyncOp::Reset);
}

#[test]
fn memory_stream_reports_gap_for_non_numeric_cursor() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    let err = stream
        .pull_value(
            SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap())
                .cursor(Some(SyncCursor::new("not_numeric").unwrap())),
        )
        .unwrap_err();
    assert!(matches!(err, SyncError::Gap(cursor) if cursor == "not_numeric"));
}

#[test]
fn memory_push_accepts_upsert_and_emits_incremental_change() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    let request = SyncPushRequest::new(
        SyncStreamName::new("posts_for_tenant").unwrap(),
        [ClientMutation {
            id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: Value::String("hello".to_string()),
            migration_outcome: None,
        }],
    );

    let pushed = stream.push_value(request).unwrap();
    assert_eq!(pushed.accepted[0].as_str(), "device_1:1");
    assert!(pushed.rejected.is_empty());
    assert!(pushed.conflicts.is_empty());
    assert_eq!(pushed.rows[0].value, Value::String("hello".to_string()));

    let pulled = stream
        .pull_value(
            SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap())
                .cursor(Some(SyncCursor::new("0").unwrap())),
        )
        .unwrap();
    assert_eq!(pulled.mode, SyncPullMode::Incremental);
    assert_eq!(pulled.changes.len(), 1);
    assert_eq!(pulled.changes[0].op, SyncOp::Upsert);
}

#[test]
fn memory_push_rejects_invalid_payload_without_advancing_cursor() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    let request = SyncPushRequest::new(
        SyncStreamName::new("posts_for_tenant").unwrap(),
        [ClientMutation {
            id: MutationId::new("device_1:bad").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"not": "a string"}),

            migration_outcome: None,
        }],
    );

    let pushed = stream.push_value(request).unwrap();
    assert!(pushed.accepted.is_empty());
    assert_eq!(pushed.rejected[0].mutation_id.as_str(), "device_1:bad");
    assert!(pushed.cursor.is_none());

    let pulled = stream
        .pull_value(SyncPullRequest::new(
            SyncStreamName::new("posts_for_tenant").unwrap(),
        ))
        .unwrap();
    assert!(pulled.rows.is_empty());
    assert_eq!(pulled.cursor.unwrap().as_str(), "0");
}

#[test]
fn memory_push_detects_stale_base_version_conflict() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    stream.upsert("post_1", "server".to_string()).unwrap();
    let request = SyncPushRequest::new(
        SyncStreamName::new("posts_for_tenant").unwrap(),
        [ClientMutation {
            id: MutationId::new("device_1:stale").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: Some(RowVersion::new("v0").unwrap()),
            payload: Value::String("client".to_string()),
            migration_outcome: None,
        }],
    );

    let pushed = stream.push_value(request).unwrap();
    assert!(pushed.accepted.is_empty());
    assert!(pushed.rejected.is_empty());
    assert_eq!(pushed.conflicts[0].mutation_id.as_str(), "device_1:stale");
    assert_eq!(
        pushed.conflicts[0].server_row.as_ref().unwrap().value,
        Value::String("server".to_string())
    );
}

#[test]
fn memory_push_dedupes_accepted_mutation_ids() {
    let stream = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    let mutation = ClientMutation {
        id: MutationId::new("device_1:dupe").unwrap(),
        key: Some(RowKey::new("post_1").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: Value::String("hello".to_string()),
        migration_outcome: None,
    };

    stream
        .push_value(SyncPushRequest::new(
            SyncStreamName::new("posts_for_tenant").unwrap(),
            [mutation.clone()],
        ))
        .unwrap();
    let pushed_again = stream
        .push_value(SyncPushRequest::new(
            SyncStreamName::new("posts_for_tenant").unwrap(),
            [mutation],
        ))
        .unwrap();

    assert_eq!(pushed_again.accepted[0].as_str(), "device_1:dupe");
    assert!(pushed_again.rows.is_empty());

    let pulled = stream
        .pull_value(
            SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap())
                .cursor(Some(SyncCursor::new("0").unwrap())),
        )
        .unwrap();
    assert_eq!(pulled.changes.len(), 1);
}
