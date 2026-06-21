use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ClientMutation, MutationId, SyncChange, SyncCollectionName, SyncConflict, SyncCursor,
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
    /// Application-level schema version this snapshot was cached under,
    /// captured from the server's most recent `/open` response. `None`
    /// means "the store has never observed an advertised version for
    /// this stream" — typically a fresh install or a `__pocopine_streams`
    /// row that existed before the v3→v4 storage migration added the
    /// column. The client compares this against the freshly-advertised
    /// version on every open: if they differ (and cached is `Some`),
    /// the stream is wiped via `clear_stream`; if cached is `None`, the
    /// advertised value is adopted silently on the next snapshot save.
    #[serde(default)]
    pub application_schema_version: Option<u32>,
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
            application_schema_version: None,
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
    /// Application-level schema version to record alongside this
    /// snapshot. `None` means the caller hasn't observed an advertised
    /// version yet and the store should leave the column NULL; the next
    /// open with a known advertised version will set it.
    #[serde(default)]
    pub application_schema_version: Option<u32>,
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
            application_schema_version: None,
        }
    }

    /// Fluent setter for the application schema version. Use this when
    /// the snapshot is being saved in response to a successful `/open`
    /// that advertised the version.
    pub fn with_application_schema_version(mut self, version: Option<u32>) -> Self {
        self.application_schema_version = version;
        self
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
