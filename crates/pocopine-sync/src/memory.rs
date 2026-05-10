use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use crate::{
    SyncChange, SyncCollectionName, SyncCursor, SyncError, SyncOp, SyncPullRequest,
    SyncPullResponse, SyncResult, SyncRow, SyncShapeName,
};

use super::server::{SyncBoxFuture, SyncShapeSource};

#[derive(Debug)]
struct Inner<T> {
    version: u64,
    rows: BTreeMap<String, SyncRow<T>>,
    changes: Vec<SyncChange<T>>,
}

impl<T> Default for Inner<T> {
    fn default() -> Self {
        Self {
            version: 0,
            rows: BTreeMap::new(),
            changes: Vec::new(),
        }
    }
}

/// In-memory source for tests, examples, and explicit single-process apps.
///
/// This source keeps an unbounded in-memory change log and serializes all
/// access through one lock. It is a reference implementation, not a durable
/// production backend.
#[derive(Clone, Debug)]
pub struct MemorySyncShape<T> {
    shape: SyncShapeName,
    collection: SyncCollectionName,
    inner: Arc<Mutex<Inner<T>>>,
}

/// Cloneable handle to the in-memory state backing a shape.
pub type MemorySyncState<T> = MemorySyncShape<T>;

impl<T> MemorySyncShape<T>
where
    T: Clone + Serialize + Send + Sync + 'static,
{
    /// Create an empty in-memory shape.
    pub fn new(shape: impl Into<String>, collection: impl Into<String>) -> SyncResult<Self> {
        Ok(Self {
            shape: SyncShapeName::new(shape.into())?,
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
            shape: self.shape.clone(),
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
            shape: self.shape.clone(),
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
            shape: self.shape.clone(),
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
                self.shape.clone(),
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
            self.shape.clone(),
            self.collection.clone(),
            changes,
            cursor,
        ))
    }

    fn lock(&self) -> SyncResult<std::sync::MutexGuard<'_, Inner<T>>> {
        self.inner
            .lock()
            .map_err(|_| SyncError::backend("memory sync shape lock poisoned"))
    }
}

impl<T> SyncShapeSource for MemorySyncShape<T>
where
    T: Clone + Serialize + Send + Sync + 'static,
{
    fn shape(&self) -> &SyncShapeName {
        &self.shape
    }

    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }

    fn current_cursor(&self) -> Option<SyncCursor> {
        let inner = self.inner.lock().ok()?;
        SyncCursor::new(inner.version.to_string()).ok()
    }

    fn pull<'a>(&'a self, request: SyncPullRequest) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        Box::pin(async move { self.pull_value(request) })
    }
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
        shape: change.shape,
        collection: change.collection,
        key: change.key,
        op: change.op,
        row: change.row.map(row_to_value).transpose()?,
        cursor: change.cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyncPullMode;

    #[test]
    fn memory_shape_returns_snapshot_then_incremental_changes() {
        let shape = MemorySyncShape::<String>::new("posts_for_tenant", "posts").unwrap();
        shape.upsert("post_1", "one".to_string()).unwrap();
        let first = shape
            .pull_value(SyncPullRequest::new(
                SyncShapeName::new("posts_for_tenant").unwrap(),
            ))
            .unwrap();
        assert_eq!(first.mode, SyncPullMode::Snapshot);
        assert_eq!(first.rows.len(), 1);

        shape.upsert("post_2", "two".to_string()).unwrap();
        let second = shape
            .pull_value(
                SyncPullRequest::new(SyncShapeName::new("posts_for_tenant").unwrap())
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
    fn memory_shape_returns_delete_and_reset_changes() {
        let shape = MemorySyncShape::<String>::new("posts_for_tenant", "posts").unwrap();
        shape.upsert("post_1", "one".to_string()).unwrap();
        let first = shape
            .pull_value(SyncPullRequest::new(
                SyncShapeName::new("posts_for_tenant").unwrap(),
            ))
            .unwrap();

        shape.delete("post_1").unwrap();
        shape.reset().unwrap();

        let second = shape
            .pull_value(
                SyncPullRequest::new(SyncShapeName::new("posts_for_tenant").unwrap())
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
    fn memory_shape_reports_gap_for_non_numeric_cursor() {
        let shape = MemorySyncShape::<String>::new("posts_for_tenant", "posts").unwrap();
        let err = shape
            .pull_value(
                SyncPullRequest::new(SyncShapeName::new("posts_for_tenant").unwrap())
                    .cursor(Some(SyncCursor::new("not_numeric").unwrap())),
            )
            .unwrap_err();
        assert!(matches!(err, SyncError::Gap(cursor) if cursor == "not_numeric"));
    }
}
