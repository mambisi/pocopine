//! Typed CRUD contracts for Pocopine sync.
//!
//! `pocopine-sync-crud` is an explicit extension crate. It provides the
//! resource identity, mutation payload, write-policy, and transaction-binding
//! contracts that a later proc macro will target. It does not generate SQL and
//! it is not an ORM.

use std::{fmt, hash::Hash, str::FromStr};

use pocopine_sync::{
    ClientMutationDraft, MutationId, RowKey, RowVersion, SyncError, SyncOp, SyncResult, SyncRow,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub use async_trait::async_trait;

/// Resource identity boundary for sync CRUD resources.
///
/// Implementations encode themselves through [`Display`](fmt::Display) and
/// decode through [`FromStr`]. This keeps common Rust id types usable without
/// wrapper-specific conversion methods.
pub trait ResourceId:
    Clone + Eq + Hash + fmt::Display + FromStr + Serialize + DeserializeOwned + Send + Sync + 'static
{
    /// Generate a local-first id when this type supports client-side ids.
    fn generate_local() -> SyncResult<Self> {
        Err(SyncError::unsupported(
            "this resource id does not support local generation",
        ))
    }

    /// Encode this id as a sync row key.
    fn to_row_key(&self) -> SyncResult<RowKey> {
        RowKey::new(self.to_string())
    }

    /// Decode this id from a sync row key.
    fn from_row_key(row_key: &RowKey) -> SyncResult<Self> {
        Self::from_str(row_key.as_str())
            .map_err(|_| SyncError::client(format!("invalid resource id: {}", row_key.as_str())))
    }
}

impl ResourceId for String {
    fn to_row_key(&self) -> SyncResult<RowKey> {
        RowKey::new(self.clone())
    }
}

macro_rules! integer_resource_id {
    ($($ty:ty),* $(,)?) => {
        $(
            impl ResourceId for $ty {}
        )*
    };
}

integer_resource_id!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl ResourceId for uuid::Uuid {
    fn generate_local() -> SyncResult<Self> {
        Ok(uuid::Uuid::new_v4())
    }
}

/// Whether a CRUD write may queue offline or must be confirmed by the server.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    /// Queue locally, apply optimistic UI, and replay when online.
    #[default]
    QueueOffline,
    /// Require a server round-trip before reporting success.
    RequireOnline,
}

impl WritePolicy {
    /// Return whether this policy allows local offline queueing.
    pub fn queues_offline(self) -> bool {
        matches!(self, Self::QueueOffline)
    }

    /// Return whether this policy requires server confirmation.
    pub fn requires_online(self) -> bool {
        matches!(self, Self::RequireOnline)
    }
}

/// Options shared by explicit transaction helpers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransactionOptions {
    pub write_policy: WritePolicy,
}

impl Default for TransactionOptions {
    fn default() -> Self {
        Self {
            write_policy: WritePolicy::QueueOffline,
        }
    }
}

impl TransactionOptions {
    /// Build default transaction options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow local offline queueing.
    pub fn queue_offline(mut self) -> Self {
        self.write_policy = WritePolicy::QueueOffline;
        self
    }

    /// Require server confirmation before the transaction reports success.
    pub fn require_online(mut self) -> Self {
        self.write_policy = WritePolicy::RequireOnline;
        self
    }
}

/// Create operation options.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "Row: Serialize", deserialize = "Row: Deserialize<'de>"))]
pub struct CreateOptions<Row> {
    pub optimistic: Option<Row>,
    pub write_policy: WritePolicy,
}

impl<Row> Default for CreateOptions<Row> {
    fn default() -> Self {
        Self {
            optimistic: None,
            write_policy: WritePolicy::QueueOffline,
        }
    }
}

impl<Row> CreateOptions<Row> {
    /// Build default create options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the row to show optimistically while the server confirms.
    pub fn optimistic(mut self, row: Row) -> Self {
        self.optimistic = Some(row);
        self
    }

    /// Allow local offline queueing.
    pub fn queue_offline(mut self) -> Self {
        self.write_policy = WritePolicy::QueueOffline;
        self
    }

    /// Require server confirmation before reporting success.
    pub fn require_online(mut self) -> Self {
        self.write_policy = WritePolicy::RequireOnline;
        self
    }
}

/// Save operation options.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "Row: Serialize", deserialize = "Row: Deserialize<'de>"))]
pub struct SaveOptions<Row> {
    pub base_version: Option<RowVersion>,
    pub optimistic: Option<Row>,
    pub write_policy: WritePolicy,
}

impl<Row> Default for SaveOptions<Row> {
    fn default() -> Self {
        Self {
            base_version: None,
            optimistic: None,
            write_policy: WritePolicy::QueueOffline,
        }
    }
}

impl<Row> SaveOptions<Row> {
    /// Build default save options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the row version used for conflict detection.
    pub fn base_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach the row to show optimistically while the server confirms.
    pub fn optimistic(mut self, row: Row) -> Self {
        self.optimistic = Some(row);
        self
    }

    /// Allow local offline queueing.
    pub fn queue_offline(mut self) -> Self {
        self.write_policy = WritePolicy::QueueOffline;
        self
    }

    /// Require server confirmation before reporting success.
    pub fn require_online(mut self) -> Self {
        self.write_policy = WritePolicy::RequireOnline;
        self
    }
}

/// Remove operation options.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveOptions {
    pub base_version: Option<RowVersion>,
    pub write_policy: WritePolicy,
}

impl RemoveOptions {
    /// Build default remove options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the row version used for conflict detection.
    pub fn base_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Allow local offline queueing.
    pub fn queue_offline(mut self) -> Self {
        self.write_policy = WritePolicy::QueueOffline;
        self
    }

    /// Require server confirmation before reporting success.
    pub fn require_online(mut self) -> Self {
        self.write_policy = WritePolicy::RequireOnline;
        self
    }
}

/// Payload for a create mutation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: Serialize, Draft: Serialize",
    deserialize = "Id: Deserialize<'de>, Draft: Deserialize<'de>"
))]
pub struct CreatePayload<Id, Draft> {
    pub id: Id,
    pub draft: Draft,
}

impl<Id, Draft> CreatePayload<Id, Draft> {
    pub fn new(id: Id, draft: Draft) -> Self {
        Self { id, draft }
    }
}

/// Payload for a save mutation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: Serialize, Draft: Serialize",
    deserialize = "Id: Deserialize<'de>, Draft: Deserialize<'de>"
))]
pub struct SavePayload<Id, Draft> {
    pub id: Id,
    pub draft: Draft,
}

impl<Id, Draft> SavePayload<Id, Draft> {
    pub fn new(id: Id, draft: Draft) -> Self {
        Self { id, draft }
    }
}

/// Payload for a remove mutation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "Id: Serialize", deserialize = "Id: Deserialize<'de>"))]
pub struct RemovePayload<Id> {
    pub id: Id,
}

impl<Id> RemovePayload<Id> {
    pub fn new(id: Id) -> Self {
        Self { id }
    }
}

/// Unified CRUD mutation payload sent through the sync push protocol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", content = "payload", rename_all = "snake_case")]
#[serde(bound(
    serialize = "Id: Serialize, Draft: Serialize",
    deserialize = "Id: Deserialize<'de>, Draft: Deserialize<'de>"
))]
pub enum CrudMutationPayload<Id, Draft> {
    Create(CreatePayload<Id, Draft>),
    Save(SavePayload<Id, Draft>),
    Remove(RemovePayload<Id>),
}

impl<Id, Draft> CrudMutationPayload<Id, Draft> {
    pub fn create(id: Id, draft: Draft) -> Self {
        Self::Create(CreatePayload::new(id, draft))
    }

    pub fn save(id: Id, draft: Draft) -> Self {
        Self::Save(SavePayload::new(id, draft))
    }

    pub fn remove(id: Id) -> Self {
        Self::Remove(RemovePayload::new(id))
    }

    pub fn id(&self) -> &Id {
        match self {
            Self::Create(payload) => &payload.id,
            Self::Save(payload) => &payload.id,
            Self::Remove(payload) => &payload.id,
        }
    }

    pub fn sync_op(&self) -> SyncOp {
        match self {
            Self::Create(_) | Self::Save(_) => SyncOp::Upsert,
            Self::Remove(_) => SyncOp::Delete,
        }
    }
}

impl<Id, Draft> CrudMutationPayload<Id, Draft>
where
    Id: ResourceId,
{
    /// Convert this CRUD payload into a sync mutation draft.
    pub fn into_sync_draft(self) -> SyncResult<ClientMutationDraft<Self>> {
        self.into_sync_draft_with_base_version(None)
    }

    /// Convert this CRUD payload into a sync mutation draft with a base version.
    pub fn into_sync_draft_with_base_version(
        self,
        base_version: Option<RowVersion>,
    ) -> SyncResult<ClientMutationDraft<Self>> {
        let key = self.id().to_row_key()?;
        let op = self.sync_op();
        Ok(ClientMutationDraft::new(op, self)
            .row_key(key)
            .base_row_version(base_version))
    }
}

impl<Id, Draft> From<CreatePayload<Id, Draft>> for CrudMutationPayload<Id, Draft> {
    fn from(payload: CreatePayload<Id, Draft>) -> Self {
        Self::Create(payload)
    }
}

impl<Id, Draft> From<SavePayload<Id, Draft>> for CrudMutationPayload<Id, Draft> {
    fn from(payload: SavePayload<Id, Draft>) -> Self {
        Self::Save(payload)
    }
}

impl<Id, Draft> From<RemovePayload<Id>> for CrudMutationPayload<Id, Draft> {
    fn from(payload: RemovePayload<Id>) -> Self {
        Self::Remove(payload)
    }
}

/// CRUD mutation status visible to application UI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedStatus {
    #[default]
    Queued,
    Syncing,
    Accepted,
    Rejected,
    Conflict,
}

/// A locally queued CRUD mutation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "Id: Serialize", deserialize = "Id: Deserialize<'de>"))]
pub struct Queued<Id> {
    pub mutation_id: MutationId,
    pub id: Id,
    pub status: QueuedStatus,
}

impl<Id> Queued<Id> {
    pub fn new(mutation_id: MutationId, id: Id) -> Self {
        Self {
            mutation_id,
            id,
            status: QueuedStatus::Queued,
        }
    }

    pub fn status(mut self, status: QueuedStatus) -> Self {
        self.status = status;
        self
    }
}

/// CRUD write outcome after local queueing or server reconciliation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: Serialize, Row: Serialize",
    deserialize = "Id: Deserialize<'de>, Row: Deserialize<'de>"
))]
pub enum CrudOutcome<Id, Row> {
    Queued(Queued<Id>),
    Accepted {
        id: Id,
        row: Row,
    },
    Rejected {
        id: Id,
        reason: String,
    },
    Conflict {
        id: Id,
        server_row: Option<Row>,
        reason: String,
    },
}

impl<Id, Row> CrudOutcome<Id, Row> {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Queued(_))
    }
}

/// Transaction wrapper that binds resources to an active transaction handle.
pub struct Transaction<'tx, Tx> {
    raw: &'tx mut Tx,
}

impl<'tx, Tx> Transaction<'tx, Tx> {
    pub fn new(raw: &'tx mut Tx) -> Self {
        Self { raw }
    }

    pub fn raw(&mut self) -> &mut Tx {
        self.raw
    }

    pub fn with<R>(&mut self, resource: R) -> R::Bound<'_>
    where
        R: TransactionBindable<Tx>,
    {
        resource.bind(self.raw)
    }
}

/// Internal binding contract behind `tx.with(resource)`.
pub trait TransactionBindable<Tx>: Sized {
    type Bound<'tx>
    where
        Self: 'tx,
        Tx: 'tx;

    fn bind<'tx>(self, tx: &'tx mut Tx) -> Self::Bound<'tx>;
}

/// Server-side CRUD source contract.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
pub trait CrudSource: Send + Sync + 'static {
    type Id: ResourceId;
    type Row: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;
    type Draft: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    async fn list(&self, ctx: pocopine_auth::RequestContext) -> SyncResult<Vec<Self::Row>>;

    async fn get(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
    ) -> SyncResult<Option<Self::Row>>;

    async fn create(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SyncResult<Self::Row>;

    async fn save(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SyncResult<Self::Row>;

    async fn remove(&self, ctx: pocopine_auth::RequestContext, id: Self::Id) -> SyncResult<()>;
}

/// Build an optimistic sync row for a CRUD resource row.
pub fn optimistic_row<Id, Row>(id: &Id, row: Row) -> SyncResult<SyncRow<Row>>
where
    Id: ResourceId,
{
    Ok(SyncRow {
        key: id.to_row_key()?,
        version: None,
        value: row,
        pending: true,
        conflict: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Draft {
        title: String,
    }

    #[test]
    fn resource_id_converts_to_and_from_row_key() {
        let id = 42_i64;
        let row_key = id.to_row_key().unwrap();

        assert_eq!(row_key.as_str(), "42");
        assert_eq!(i64::from_row_key(&row_key).unwrap(), id);
    }

    #[test]
    fn string_resource_id_uses_validated_row_key() {
        assert!("customer_1".to_string().to_row_key().is_ok());
        assert!("bad\nid".to_string().to_row_key().is_err());
    }

    #[test]
    fn uuid_resource_id_can_generate_local_ids() {
        let id = uuid::Uuid::generate_local().unwrap();
        let row_key = id.to_row_key().unwrap();

        assert_eq!(uuid::Uuid::from_row_key(&row_key).unwrap(), id);
    }

    #[test]
    fn write_policy_defaults_to_queue_offline() {
        let options = SaveOptions::<String>::default();

        assert_eq!(options.write_policy, WritePolicy::QueueOffline);
        assert!(options.write_policy.queues_offline());
        assert!(options.require_online().write_policy.requires_online());
    }

    #[test]
    fn crud_payload_maps_create_to_upsert_draft() {
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            Draft {
                title: "hello".to_string(),
            },
        );

        let draft = payload.into_sync_draft().unwrap();

        assert_eq!(draft.op, SyncOp::Upsert);
        assert_eq!(draft.key.unwrap().as_str(), "post_1");
        assert!(draft.base_version.is_none());
    }

    #[test]
    fn crud_payload_maps_save_base_version() {
        let base_version = RowVersion::new("row_1").unwrap();
        let payload = CrudMutationPayload::save(
            "post_1".to_string(),
            Draft {
                title: "updated".to_string(),
            },
        );

        let draft = payload
            .into_sync_draft_with_base_version(Some(base_version.clone()))
            .unwrap();

        assert_eq!(draft.op, SyncOp::Upsert);
        assert_eq!(draft.key.unwrap().as_str(), "post_1");
        assert_eq!(draft.base_version, Some(base_version));
    }

    #[test]
    fn crud_payload_maps_remove_to_delete_draft() {
        let payload: CrudMutationPayload<String, Draft> =
            CrudMutationPayload::remove("post_1".to_string());

        let draft = payload.into_sync_draft().unwrap();

        assert_eq!(draft.op, SyncOp::Delete);
        assert_eq!(draft.key.unwrap().as_str(), "post_1");
    }

    #[test]
    fn optimistic_row_marks_pending() {
        let row = optimistic_row(&"post_1".to_string(), "hello".to_string()).unwrap();

        assert_eq!(row.key.as_str(), "post_1");
        assert!(row.pending);
        assert!(!row.conflict);
        assert_eq!(row.value, "hello");
    }

    #[test]
    fn queued_and_outcome_statuses_are_explicit() {
        let queued = Queued::new(
            MutationId::new("device_abc:1").unwrap(),
            "post_1".to_string(),
        );
        let outcome: CrudOutcome<String, String> = CrudOutcome::Queued(queued.clone());

        assert_eq!(queued.status, QueuedStatus::Queued);
        assert!(!outcome.is_terminal());
        assert!(CrudOutcome::<String, String>::Rejected {
            id: "post_1".to_string(),
            reason: "invalid".to_string()
        }
        .is_terminal());
    }

    #[test]
    fn transaction_with_uses_bind_contract() {
        struct RawTx {
            log: Vec<&'static str>,
        }

        struct Audit;

        struct AuditInTx<'tx> {
            tx: &'tx mut RawTx,
        }

        impl<'tx> AuditInTx<'tx> {
            fn record(self, message: &'static str) {
                self.tx.log.push(message);
            }
        }

        impl TransactionBindable<RawTx> for Audit {
            type Bound<'tx> = AuditInTx<'tx>;

            fn bind<'tx>(self, tx: &'tx mut RawTx) -> Self::Bound<'tx> {
                AuditInTx { tx }
            }
        }

        let mut raw = RawTx { log: Vec::new() };
        let mut tx = Transaction::new(&mut raw);
        tx.with(Audit).record("created");

        assert_eq!(tx.raw().log, vec!["created"]);
    }
}
