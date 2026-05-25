use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ClientMutation, MutationId, RowKey, SyncChange, SyncCollectionName, SyncConflict, SyncCursor,
    SyncDeviceId, SyncError, SyncPushResponse, SyncRejectedMutation, SyncResult, SyncRow,
    SyncStreamName,
};

/// Boxed future returned by local sync store implementations.
pub type SyncLocalFuture<'a, T> = Pin<Box<dyn Future<Output = SyncResult<T>> + 'a>>;

/// Durable client identity persisted by a local sync store.
///
/// `next_mutation_counter` is the next counter to reserve for this device.
/// It starts at `1`; stores should persist the increment before exposing a
/// mutation id that may be sent to the server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncLocalIdentity {
    pub device_id: SyncDeviceId,
    pub next_mutation_counter: u64,
}

impl SyncLocalIdentity {
    /// Build an identity with the first mutation counter.
    pub fn new(device_id: SyncDeviceId) -> Self {
        Self {
            device_id,
            next_mutation_counter: 1,
        }
    }

    /// Build an identity with a caller-supplied next mutation counter.
    pub fn with_next_counter(
        device_id: SyncDeviceId,
        next_mutation_counter: u64,
    ) -> SyncResult<Self> {
        if next_mutation_counter == 0 {
            return Err(SyncError::invalid_value("next mutation counter", "0"));
        }
        Ok(Self {
            device_id,
            next_mutation_counter,
        })
    }

    /// Build a mutation id generator from this identity.
    pub fn mutation_id_generator(&self) -> SyncResult<MutationIdGenerator> {
        MutationIdGenerator::with_next_counter(self.device_id.clone(), self.next_mutation_counter)
    }

    /// Reserve the current mutation id and return the advanced identity.
    ///
    /// Stores use this to persist the incremented counter before exposing the
    /// id to client code. If the counter cannot advance, no id is returned.
    pub fn reserve_mutation_id(&self) -> SyncResult<(MutationId, Self)> {
        let next_mutation_counter = self
            .next_mutation_counter
            .checked_add(1)
            .ok_or_else(|| SyncError::invalid_value("next mutation counter", "overflow"))?;
        let id = MutationId::new(format!("{}:{}", self.device_id, self.next_mutation_counter))?;
        Ok((
            id,
            Self {
                device_id: self.device_id.clone(),
                next_mutation_counter,
            },
        ))
    }
}

/// Generate a fresh sync device identity.
pub fn generate_sync_device_id() -> SyncResult<SyncDeviceId> {
    SyncDeviceId::new(format!("device_{}", uuid::Uuid::new_v4().simple()))
}

/// Deterministic mutation id generator for one persisted device id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationIdGenerator {
    device_id: SyncDeviceId,
    next_counter: u64,
}

impl MutationIdGenerator {
    /// Start generating ids at counter `1`.
    pub fn new(device_id: SyncDeviceId) -> Self {
        Self {
            device_id,
            next_counter: 1,
        }
    }

    /// Resume generation from a persisted next counter.
    pub fn with_next_counter(device_id: SyncDeviceId, next_counter: u64) -> SyncResult<Self> {
        if next_counter == 0 {
            return Err(SyncError::invalid_value("next mutation counter", "0"));
        }
        Ok(Self {
            device_id,
            next_counter,
        })
    }

    /// The device id this generator belongs to.
    pub fn device_id(&self) -> &SyncDeviceId {
        &self.device_id
    }

    /// The counter that will be used by the next generated id.
    pub fn next_counter(&self) -> u64 {
        self.next_counter
    }

    /// Generate the next mutation id and advance the in-memory counter.
    pub fn next_mutation_id(&mut self) -> SyncResult<MutationId> {
        let id = MutationId::new(format!("{}:{}", self.device_id, self.next_counter))?;
        self.next_counter = self
            .next_counter
            .checked_add(1)
            .ok_or_else(|| SyncError::invalid_value("next mutation counter", "overflow"))?;
        Ok(id)
    }
}

/// Local-only queued mutation metadata.
///
/// `mutation` is the wire payload sent to `/push`. `optimistic_row` is never
/// sent to the server; it lets the client reconstruct the rendered pending
/// overlay after reload when the wire payload is not itself a row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LocalPendingMutation {
    pub mutation: ClientMutation<Value>,
    #[serde(default)]
    pub optimistic_row: Option<SyncRow<Value>>,
}

impl LocalPendingMutation {
    pub fn new(mutation: ClientMutation<Value>) -> Self {
        Self {
            mutation,
            optimistic_row: None,
        }
    }

    pub fn with_optimistic_row(mut self, optimistic_row: Option<SyncRow<Value>>) -> Self {
        self.optimistic_row = optimistic_row;
        self
    }
}

impl<'de> Deserialize<'de> for LocalPendingMutation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum PendingMutationRepr {
            Record {
                mutation: ClientMutation<Value>,
                #[serde(default)]
                optimistic_row: Option<SyncRow<Value>>,
            },
            Legacy(ClientMutation<Value>),
        }

        match PendingMutationRepr::deserialize(deserializer)? {
            PendingMutationRepr::Record {
                mutation,
                optimistic_row,
            } => Ok(Self {
                mutation,
                optimistic_row,
            }),
            PendingMutationRepr::Legacy(mutation) => Ok(Self::new(mutation)),
        }
    }
}

/// Locally cached rows and cursor for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalStreamSnapshot {
    pub stream: SyncStreamName,
    pub collection: Option<SyncCollectionName>,
    pub cursor: Option<SyncCursor>,
    #[serde(default)]
    pub rows: Vec<SyncRow<serde_json::Value>>,
    #[serde(default)]
    pub pending_mutations: Vec<LocalPendingMutation>,
}

impl LocalStreamSnapshot {
    /// Empty local state for a stream that has not been persisted yet.
    pub fn empty(stream: SyncStreamName) -> Self {
        Self {
            stream,
            collection: None,
            cursor: None,
            rows: Vec::new(),
            pending_mutations: Vec::new(),
        }
    }
}

/// Atomic snapshot replacement to persist for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalSnapshotBatch {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
    #[serde(default)]
    pub rows: Vec<SyncRow<serde_json::Value>>,
}

impl LocalSnapshotBatch {
    pub fn new(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        rows: Vec<SyncRow<serde_json::Value>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            stream,
            collection,
            cursor,
            rows,
        }
    }
}

/// Atomic incremental change batch to persist for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalChangeBatch {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
    #[serde(default)]
    pub changes: Vec<SyncChange<serde_json::Value>>,
}

impl LocalChangeBatch {
    pub fn new(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        changes: Vec<SyncChange<serde_json::Value>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            stream,
            collection,
            cursor,
            changes,
        }
    }
}

/// Push outcome persisted by the local store after the server responds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalPushResult {
    pub stream: SyncStreamName,
    pub collection: Option<SyncCollectionName>,
    #[serde(default)]
    pub accepted: Vec<MutationId>,
    #[serde(default)]
    pub rejected: Vec<SyncRejectedMutation>,
    #[serde(default)]
    pub rows: Vec<SyncRow<serde_json::Value>>,
    #[serde(default)]
    pub conflicts: Vec<SyncConflict<serde_json::Value>>,
    pub cursor: Option<SyncCursor>,
}

impl LocalPushResult {
    pub fn from_response(response: SyncPushResponse<serde_json::Value>) -> Self {
        Self {
            stream: response.stream,
            collection: response.collection,
            accepted: response.accepted,
            rejected: response.rejected,
            rows: response.rows,
            conflicts: response.conflicts,
            cursor: response.cursor,
        }
    }
}

/// Durable client-side storage contract for sync streams.
///
/// Implementations must apply snapshot, change, mutation enqueue, and push
/// result operations atomically. The local store improves durability and
/// startup latency; it is not an authorization boundary.
///
/// `reserve_mutation_id` is the safe allocation boundary for stable mutation
/// ids. Implementations must persist the incremented counter before returning
/// the id, so a reload or failed network request cannot reuse it.
pub trait SyncLocalStore {
    /// Load the persisted client identity, if this store has one.
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>>;

    /// Persist the client identity and next mutation counter.
    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()>;

    /// Reserve a durable mutation id for the local device.
    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId>;

    /// Hydrate locally cached rows and pending mutations for a stream.
    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot>;

    /// Atomically replace the locally cached stream snapshot.
    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()>;

    /// Atomically apply incremental server changes for a stream.
    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()>;

    /// Persist a mutation before it is sent to the server.
    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()>;

    /// Persist a local pending mutation and its optional optimistic row before
    /// the mutation is sent to the server.
    fn enqueue_pending_mutation(
        &self,
        stream: &SyncStreamName,
        pending: LocalPendingMutation,
    ) -> SyncLocalFuture<'_, ()> {
        self.enqueue_mutation(stream, pending.mutation)
    }

    /// Persist accepted, rejected, or conflicted mutation outcomes.
    ///
    /// Row `pending` flags persisted here describe the latest server outcome.
    /// With stacked pending mutations for the same key, hydrated rows may
    /// understate pending UI state until the client replays the queued
    /// mutations.
    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()>;

    /// Clear a persisted row conflict marker after the user resolves it.
    ///
    /// This only clears local metadata. It does not write application data to
    /// the server and does not remove any still-pending mutations for the row.
    fn clear_conflict(&self, stream: &SyncStreamName, key: &RowKey) -> SyncLocalFuture<'_, ()>;

    /// Load pending mutations that should be replayed for one stream.
    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>>;

    /// Durably remove every still-pending mutation whose `key` matches
    /// the given row, scoped to one stream. Returns the number of
    /// mutations removed.
    ///
    /// This is the storage primitive behind
    /// `CrudClientResource::discard_local`: it lets the author-facing
    /// helper back out a row's queued edits with the same durability
    /// guarantees as `enqueue_mutation` — a reload cannot resurrect
    /// the purged mutations.
    ///
    /// Implementations MUST:
    ///
    /// * Persist the removal before returning. The whole point of a
    ///   durable purge is that reload doesn't replay the dropped
    ///   mutations.
    /// * Leave mutations whose `key` is `None` or differs from `key`
    ///   untouched — purging one row must not touch another row's
    ///   queue.
    /// * Leave non-pending records (accepted / rejected / conflict
    ///   history) alone. Those are server outcomes, not local edits.
    /// * Leave the row's conflict marker untouched; clearing it is
    ///   `clear_conflict`'s job and the caller composes the two.
    ///
    /// The default implementation falls back to no-op + `0` for stores
    /// that haven't been updated yet — existing impls without this
    /// method continue to compile, but generated `discard_local`
    /// helpers will silently leave durable entries behind until the
    /// store opts in.
    fn purge_pending_for_row(
        &self,
        _stream: &SyncStreamName,
        _key: &RowKey,
    ) -> SyncLocalFuture<'_, usize> {
        Box::pin(std::future::ready(Ok(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RowKey, SyncOp};

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
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 7)
                .unwrap();

        let (id, advanced) = identity.reserve_mutation_id().unwrap();

        assert_eq!(id.as_str(), "device_abc:7");
        assert_eq!(advanced.device_id.as_str(), "device_abc");
        assert_eq!(advanced.next_mutation_counter, 8);
    }

    #[test]
    fn local_identity_rejects_counter_overflow_without_id() {
        let identity = SyncLocalIdentity::with_next_counter(
            SyncDeviceId::new("device_abc").unwrap(),
            u64::MAX,
        )
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
        assert!(MutationIdGenerator::with_next_counter(
            SyncDeviceId::new("device_abc").unwrap(),
            0
        )
        .is_err());
    }

    #[test]
    fn mutation_id_generator_rejects_counter_overflow_without_advancing() {
        let mut generator = MutationIdGenerator::with_next_counter(
            SyncDeviceId::new("device_abc").unwrap(),
            u64::MAX,
        )
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
        };
        let optimistic = SyncRow::new("post_1", serde_json::json!({"title": "Visible"})).unwrap();

        let pending = LocalPendingMutation::new(mutation.clone())
            .with_optimistic_row(Some(optimistic.clone()));
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
        };
        let changes = LocalChangeBatch::new(stream, collection, vec![change], cursor);

        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(changes.changes.len(), 1);
    }
}
