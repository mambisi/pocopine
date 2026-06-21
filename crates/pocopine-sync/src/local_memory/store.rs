use std::{
    collections::BTreeMap,
    future,
    sync::{Arc, Mutex},
};

use crate::{
    ClientMutation, LocalChangeBatch, LocalPendingMutation, LocalPushResult, LocalSnapshotBatch,
    LocalStreamSnapshot, MutationId, RowKey, SyncCollectionName, SyncError, SyncLocalFuture,
    SyncLocalIdentity, SyncLocalStore, SyncOp, SyncResult, SyncRow, SyncStreamName,
    generate_sync_device_id,
};

use super::changes::changes_after_last_reset;

/// In-memory [`SyncLocalStore`] implementation for tests and demos.
///
/// This store is process-local and not durable. It pins the local-store
/// semantics before browser SQLite persistence is added.
#[derive(Clone, Debug, Default)]
pub struct MemoryLocalStore {
    inner: Arc<Mutex<MemoryLocalStoreInner>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryLocalStoreInner {
    identity: Option<SyncLocalIdentity>,
    streams: BTreeMap<SyncStreamName, MemoryStreamState>,
}

#[derive(Clone, Debug, Default)]
struct MemoryStreamState {
    collection: Option<SyncCollectionName>,
    cursor: Option<crate::SyncCursor>,
    rows: BTreeMap<RowKey, SyncRow<serde_json::Value>>,
    pending: Vec<LocalPendingMutation>,
    application_schema_version: Option<u32>,
}

impl MemoryLocalStore {
    /// Build an empty in-memory local store.
    pub fn new() -> Self {
        Self::default()
    }

    fn with_inner<T>(
        &self,
        f: impl FnOnce(&mut MemoryLocalStoreInner) -> SyncResult<T>,
    ) -> SyncResult<T> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| SyncError::backend("memory local sync store lock poisoned"))?;
        f(&mut inner)
    }

    fn ready<T: 'static>(result: SyncResult<T>) -> SyncLocalFuture<'static, T> {
        Box::pin(future::ready(result))
    }
}

impl SyncLocalStore for MemoryLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        Self::ready(self.with_inner(|inner| Ok(inner.identity.clone())))
    }

    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            inner.identity = Some(identity);
            Ok(())
        }))
    }

    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId> {
        Self::ready(self.with_inner(|inner| {
            let identity = match inner.identity.clone() {
                Some(identity) => identity,
                None => SyncLocalIdentity::new(generate_sync_device_id()?),
            };
            let (id, advanced) = identity.reserve_mutation_id()?;
            inner.identity = Some(advanced);
            Ok(id)
        }))
    }

    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            let Some(state) = inner.streams.get(&stream) else {
                return Ok(LocalStreamSnapshot::empty(stream));
            };
            Ok(LocalStreamSnapshot {
                stream,
                collection: state.collection.clone(),
                cursor: state.cursor.clone(),
                rows: state.rows.values().cloned().collect(),
                pending_mutations: state.pending.clone(),
                application_schema_version: state.application_schema_version,
            })
        }))
    }

    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(snapshot.stream).or_default();
            state.collection = Some(snapshot.collection);
            state.cursor = snapshot.cursor;
            state.rows = snapshot
                .rows
                .into_iter()
                .map(|row| (row.key.clone(), row))
                .collect();
            // Only overwrite when the caller carries a value; passing
            // `None` from a code path that hasn't observed the
            // advertised version yet must not clobber a previously
            // recorded value.
            if let Some(version) = snapshot.application_schema_version {
                state.application_schema_version = Some(version);
            }
            Ok(())
        }))
    }

    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(changes.stream).or_default();
            state.collection = Some(changes.collection);
            state.cursor = changes.cursor;
            let changes = changes_after_last_reset(changes.changes);
            if changes.had_reset {
                state.rows.clear();
            }

            for change in changes.items {
                match change.op {
                    SyncOp::Upsert => {
                        if let Some(row) = change.row {
                            state.rows.insert(row.key.clone(), row);
                        }
                    }
                    SyncOp::Delete => {
                        if let Some(key) = change.key {
                            state.rows.remove(&key);
                        }
                    }
                    SyncOp::Reset => {
                        if let Some(row) = change.row {
                            state.rows.insert(row.key.clone(), row);
                        }
                    }
                }
            }
            Ok(())
        }))
    }

    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()> {
        self.enqueue_pending_mutation(stream, LocalPendingMutation::new(mutation))
    }

    fn enqueue_pending_mutation(
        &self,
        stream: &SyncStreamName,
        pending: LocalPendingMutation,
    ) -> SyncLocalFuture<'_, ()> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(stream).or_default();
            if let Some(existing) = state
                .pending
                .iter_mut()
                .find(|existing| existing.mutation.id == pending.mutation.id)
            {
                *existing = pending;
            } else {
                state.pending.push(pending);
            }
            Ok(())
        }))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            let state = inner.streams.entry(result.stream).or_default();
            if let Some(collection) = result.collection {
                state.collection = Some(collection);
            }
            if let Some(cursor) = result.cursor {
                state.cursor = Some(cursor);
            }

            for id in result.accepted {
                state.pending.retain(|pending| pending.mutation.id != id);
            }

            for rejected in result.rejected {
                state
                    .pending
                    .retain(|pending| pending.mutation.id != rejected.mutation_id);
            }

            for conflict in result.conflicts {
                state
                    .pending
                    .retain(|pending| pending.mutation.id != conflict.mutation_id);
                if let Some(mut row) = conflict.server_row {
                    row.pending = false;
                    row.conflict = true;
                    state.rows.insert(row.key.clone(), row);
                } else if let Some(key) = conflict.key
                    && let Some(row) = state.rows.get_mut(&key)
                {
                    row.pending = false;
                    row.conflict = true;
                }
            }

            for mut row in result.rows {
                row.pending = false;
                row.conflict = false;
                state.rows.insert(row.key.clone(), row);
            }

            Ok(())
        }))
    }

    fn clear_conflict(&self, stream: &SyncStreamName, key: &RowKey) -> SyncLocalFuture<'_, ()> {
        let stream = stream.clone();
        let key = key.clone();
        Self::ready(self.with_inner(|inner| {
            if let Some(state) = inner.streams.get_mut(&stream)
                && let Some(row) = state.rows.get_mut(&key)
            {
                row.conflict = false;
            }
            Ok(())
        }))
    }

    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            Ok(inner
                .streams
                .get(&stream)
                .map(|state| {
                    state
                        .pending
                        .iter()
                        .map(|pending| pending.mutation.clone())
                        .collect()
                })
                .unwrap_or_default())
        }))
    }

    fn purge_pending_for_row(
        &self,
        stream: &SyncStreamName,
        key: &RowKey,
    ) -> SyncLocalFuture<'_, usize> {
        let stream = stream.clone();
        let key = key.clone();
        Self::ready(self.with_inner(|inner| {
            let Some(state) = inner.streams.get_mut(&stream) else {
                return Ok(0);
            };
            let before = state.pending.len();
            // Retain pendings that do NOT target `key`. Mutations with
            // `key = None` (no row anchor) and mutations for any
            // OTHER row are left alone — the contract is scoped to
            // this one row.
            state
                .pending
                .retain(|pending| pending.mutation.key.as_ref() != Some(&key));
            Ok(before - state.pending.len())
        }))
    }

    fn clear_all_streams(&self) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_inner(|inner| {
            inner.streams.clear();
            Ok(())
        }))
    }

    fn clear_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, ()> {
        let stream = stream.clone();
        Self::ready(self.with_inner(|inner| {
            // Removing the entry drops every cached row, pending
            // mutation, conflict marker, cursor, and the cached
            // schema_version in one step. Other streams + the device
            // identity are untouched. Safe to call on a never-seen
            // stream: BTreeMap::remove returns None and we ignore it.
            inner.streams.remove(&stream);
            Ok(())
        }))
    }
}
