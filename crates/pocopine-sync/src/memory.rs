use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use pocopine_server::auth::RequestContext;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::{
    ClientMutation, SyncChange, SyncCollectionName, SyncConflict, SyncCursor, SyncError, SyncOp,
    SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncRejectedMutation,
    SyncResult, SyncRow, SyncStreamName,
};

use super::server::{SyncBoxFuture, SyncStreamSource};

#[derive(Debug)]
struct Inner<T> {
    version: u64,
    rows: BTreeMap<String, SyncRow<T>>,
    changes: Vec<SyncChange<T>>,
    // Unbounded by design for the reference source. Durable adapters should
    // persist mutation ids with an explicit retention policy.
    accepted_mutations: BTreeSet<String>,
}

impl<T> Default for Inner<T> {
    fn default() -> Self {
        Self {
            version: 0,
            rows: BTreeMap::new(),
            changes: Vec::new(),
            accepted_mutations: BTreeSet::new(),
        }
    }
}

/// In-memory source for tests, examples, and explicit single-process apps.
///
/// This source keeps an unbounded in-memory change log and serializes all
/// access through one lock. Accepted mutation ids are also retained without
/// bounds for idempotency. It is a reference implementation, not a durable
/// production backend.
#[derive(Clone, Debug)]
pub struct MemorySyncStream<T> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    inner: Arc<Mutex<Inner<T>>>,
}

/// Cloneable handle to the in-memory state backing a stream.
pub type MemorySyncState<T> = MemorySyncStream<T>;

impl<T> MemorySyncStream<T>
where
    T: Clone + Serialize + Send + Sync + 'static,
{
    /// Create an empty in-memory stream.
    pub fn new(stream: impl Into<String>, collection: impl Into<String>) -> SyncResult<Self> {
        Ok(Self {
            stream: SyncStreamName::new(stream.into())?,
            collection: SyncCollectionName::new(collection.into())?,
            inner: Arc::new(Mutex::new(Inner::default())),
        })
    }

    /// Insert or replace one row.
    pub fn upsert(&self, key: impl Into<String>, value: T) -> SyncResult<SyncCursor> {
        let mut inner = self.lock()?;
        inner.version = inner.version.saturating_add(1);
        let version = inner.version;
        let cursor = SyncCursor::new(version.to_string())?;
        let row = SyncRow::new(key.into(), value)?.version(format!("v{version}"))?;
        inner.rows.insert(row.key.as_str().to_string(), row.clone());
        // Unbounded by design for the reference source; production adapters
        // should retain changes according to their own cursor window policy.
        inner.changes.push(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key: None,
            op: SyncOp::Upsert,
            row: Some(row),
            cursor: cursor.clone(),
        });
        Ok(cursor)
    }

    /// Delete one row.
    pub fn delete(&self, key: impl Into<String>) -> SyncResult<SyncCursor> {
        let key = crate::RowKey::new(key.into())?;
        let mut inner = self.lock()?;
        inner.version = inner.version.saturating_add(1);
        let cursor = SyncCursor::new(inner.version.to_string())?;
        inner.rows.remove(key.as_str());
        inner.changes.push(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key: Some(key),
            op: SyncOp::Delete,
            row: None,
            cursor: cursor.clone(),
        });
        Ok(cursor)
    }

    /// Remove all rows and force clients to resnapshot.
    pub fn reset(&self) -> SyncResult<SyncCursor> {
        let mut inner = self.lock()?;
        inner.version = inner.version.saturating_add(1);
        let cursor = SyncCursor::new(inner.version.to_string())?;
        inner.rows.clear();
        inner.changes.push(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key: None,
            op: SyncOp::Reset,
            row: None,
            cursor: cursor.clone(),
        });
        Ok(cursor)
    }

    fn pull_value(&self, request: SyncPullRequest) -> SyncResult<SyncPullResponse<Value>> {
        let inner = self.lock()?;
        let cursor = Some(SyncCursor::new(inner.version.to_string())?);

        let Some(after) = request.cursor else {
            let rows = inner
                .rows
                .values()
                .cloned()
                .map(row_to_value)
                .collect::<SyncResult<Vec<_>>>()?;
            return Ok(SyncPullResponse::snapshot(
                self.stream.clone(),
                self.collection.clone(),
                rows,
                cursor,
            ));
        };

        let after = after
            .as_str()
            .parse::<u64>()
            .map_err(|_| SyncError::Gap(after.to_string()))?;
        let mut changes = Vec::new();
        for change in &inner.changes {
            let cursor = change
                .cursor
                .as_str()
                .parse::<u64>()
                .map_err(|_| SyncError::backend("memory sync change cursor is not numeric"))?;
            if cursor > after {
                changes.push(change_to_value(change.clone())?);
            }
        }

        Ok(SyncPullResponse::incremental(
            self.stream.clone(),
            self.collection.clone(),
            changes,
            cursor,
        ))
    }

    fn push_value(&self, request: SyncPushRequest<Value>) -> SyncResult<SyncPushResponse<Value>>
    where
        T: DeserializeOwned,
    {
        let mut inner = self.lock()?;
        let mut response = SyncPushResponse::new(self.stream.clone());
        response.collection = Some(self.collection.clone());

        for mutation in request.mutations {
            if inner.accepted_mutations.contains(mutation.id.as_str()) {
                response.accepted.push(mutation.id);
                continue;
            }

            let outcome = self.apply_mutation(&mut inner, mutation)?;
            match outcome {
                MemoryMutationOutcome::Accepted { id, row, cursor } => {
                    inner.accepted_mutations.insert(id.as_str().to_string());
                    response.accepted.push(id);
                    if let Some(row) = row {
                        response.rows.push(row_to_value(row)?);
                    }
                    response.cursor = Some(cursor);
                }
                MemoryMutationOutcome::Rejected(rejected) => {
                    response.rejected.push(rejected);
                }
                MemoryMutationOutcome::Conflict(conflict) => {
                    response.conflicts.push(conflict_to_value(conflict)?);
                }
            }
        }

        Ok(response)
    }

    fn apply_mutation(
        &self,
        inner: &mut Inner<T>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<MemoryMutationOutcome<T>>
    where
        T: DeserializeOwned,
    {
        match mutation.op {
            SyncOp::Upsert => self.apply_upsert(inner, mutation),
            SyncOp::Delete => self.apply_delete(inner, mutation),
            SyncOp::Reset => self.apply_reset(inner, mutation),
        }
    }

    fn apply_upsert(
        &self,
        inner: &mut Inner<T>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<MemoryMutationOutcome<T>>
    where
        T: DeserializeOwned,
    {
        let Some(key) = mutation.key.clone() else {
            return Ok(MemoryMutationOutcome::Rejected(SyncRejectedMutation {
                mutation_id: mutation.id,
                key: None,
                reason: "upsert requires a row key".to_string(),
            }));
        };

        if let Some(conflict) = self.version_conflict(inner, &mutation, key.as_str()) {
            return Ok(MemoryMutationOutcome::Conflict(conflict));
        }

        let value = match serde_json::from_value::<T>(mutation.payload) {
            Ok(value) => value,
            Err(err) => {
                return Ok(MemoryMutationOutcome::Rejected(SyncRejectedMutation {
                    mutation_id: mutation.id,
                    key: Some(key),
                    reason: format!("invalid mutation payload: {err}"),
                }));
            }
        };

        inner.version = inner.version.saturating_add(1);
        let version = inner.version;
        let cursor = SyncCursor::new(version.to_string())?;
        let row = SyncRow::new(key.as_str().to_string(), value)?.version(format!("v{version}"))?;
        inner.rows.insert(key.as_str().to_string(), row.clone());
        inner.changes.push(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key: Some(key),
            op: SyncOp::Upsert,
            row: Some(row.clone()),
            cursor: cursor.clone(),
        });

        Ok(MemoryMutationOutcome::Accepted {
            id: mutation.id,
            row: Some(row),
            cursor,
        })
    }

    fn apply_delete(
        &self,
        inner: &mut Inner<T>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<MemoryMutationOutcome<T>> {
        let Some(key) = mutation.key.clone() else {
            return Ok(MemoryMutationOutcome::Rejected(SyncRejectedMutation {
                mutation_id: mutation.id,
                key: None,
                reason: "delete requires a row key".to_string(),
            }));
        };

        if let Some(conflict) = self.version_conflict(inner, &mutation, key.as_str()) {
            return Ok(MemoryMutationOutcome::Conflict(conflict));
        }

        inner.version = inner.version.saturating_add(1);
        let cursor = SyncCursor::new(inner.version.to_string())?;
        inner.rows.remove(key.as_str());
        inner.changes.push(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key: Some(key),
            op: SyncOp::Delete,
            row: None,
            cursor: cursor.clone(),
        });

        Ok(MemoryMutationOutcome::Accepted {
            id: mutation.id,
            row: None,
            cursor,
        })
    }

    fn apply_reset(
        &self,
        inner: &mut Inner<T>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<MemoryMutationOutcome<T>> {
        inner.version = inner.version.saturating_add(1);
        let cursor = SyncCursor::new(inner.version.to_string())?;
        inner.rows.clear();
        inner.changes.push(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key: None,
            op: SyncOp::Reset,
            row: None,
            cursor: cursor.clone(),
        });

        Ok(MemoryMutationOutcome::Accepted {
            id: mutation.id,
            row: None,
            cursor,
        })
    }

    fn version_conflict(
        &self,
        inner: &Inner<T>,
        mutation: &ClientMutation<Value>,
        key: &str,
    ) -> Option<SyncConflict<T>> {
        let expected = mutation.base_version.as_ref()?;
        let server_row = inner.rows.get(key).cloned();
        let server_version = server_row.as_ref().and_then(|row| row.version.as_ref());
        if server_version == Some(expected) {
            return None;
        }

        Some(SyncConflict {
            mutation_id: mutation.id.clone(),
            key: mutation.key.clone(),
            server_row,
            reason: "base version is stale".to_string(),
        })
    }

    fn lock(&self) -> SyncResult<std::sync::MutexGuard<'_, Inner<T>>> {
        self.inner
            .lock()
            .map_err(|_| SyncError::backend("memory sync stream lock poisoned"))
    }
}

impl<T> SyncStreamSource for MemorySyncStream<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn stream(&self) -> &SyncStreamName {
        &self.stream
    }

    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }

    fn current_cursor(&self, ctx: &RequestContext) -> Option<SyncCursor> {
        let _ = ctx;
        let inner = self.inner.lock().ok()?;
        SyncCursor::new(inner.version.to_string()).ok()
    }

    fn pull<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        let _ = ctx;
        Box::pin(async move { self.pull_value(request) })
    }

    fn push<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        let _ = ctx;
        Box::pin(async move { self.push_value(request) })
    }
}

enum MemoryMutationOutcome<T> {
    Accepted {
        id: crate::MutationId,
        row: Option<SyncRow<T>>,
        cursor: SyncCursor,
    },
    Rejected(SyncRejectedMutation),
    Conflict(SyncConflict<T>),
}

fn row_to_value<T: Serialize>(row: SyncRow<T>) -> SyncResult<SyncRow<Value>> {
    Ok(SyncRow {
        key: row.key,
        version: row.version,
        value: serde_json::to_value(row.value)?,
        pending: row.pending,
        conflict: row.conflict,
    })
}

fn change_to_value<T: Serialize>(change: SyncChange<T>) -> SyncResult<SyncChange<Value>> {
    Ok(SyncChange {
        stream: change.stream,
        collection: change.collection,
        key: change.key,
        op: change.op,
        row: change.row.map(row_to_value).transpose()?,
        cursor: change.cursor,
    })
}

fn conflict_to_value<T: Serialize>(conflict: SyncConflict<T>) -> SyncResult<SyncConflict<Value>> {
    Ok(SyncConflict {
        mutation_id: conflict.mutation_id,
        key: conflict.key,
        server_row: conflict.server_row.map(row_to_value).transpose()?,
        reason: conflict.reason,
    })
}

#[cfg(test)]
mod tests {
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
}
