use pocopine_sync::{ClientMutationDraft, RowVersion, SyncOp, SyncResult};
use serde::{Deserialize, Serialize};

use crate::ResourceId;

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
///
/// RFC 090 Phase 2b: the wire shape (`#[serde(tag = "op", content =
/// "payload")]`) is byte-for-byte identical to
/// `pocopine_sync_query::write::MutationPayload`. The TYPES stay
/// independent — Phase 6 deletes this whole crate (including this
/// type) when CRUD is removed. Until then, CRUD users keep using
/// `CrudMutationPayload`; new Source users use the canonical
/// `MutationPayload`. Clients on either side serialize compatible
/// bytes, so a CRUD client can push to a Source-backed server and
/// vice versa during the transition.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Draft {
        name: String,
    }

    #[test]
    fn create_payload_round_trips_json() {
        let payload = CrudMutationPayload::create(
            "id1".to_string(),
            Draft {
                name: "A".to_string(),
            },
        );
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "op": "create",
                "payload": {"id": "id1", "draft": {"name": "A"}}
            })
        );
        let round_trip: CrudMutationPayload<String, Draft> = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip, payload);
    }

    #[test]
    fn crud_and_sync_query_envelopes_have_intentionally_different_wire_shapes() {
        // RFC 090 Phase 2b: CrudMutationPayload kept its older
        // nested-payload wire shape (`{"op": "create", "payload":
        // {...}}`). The new sync-query `MutationPayload` uses a
        // cleaner flat shape (`{"op": "create", "id": ..., "draft":
        // ...}`) and renames Save → Update / Remove → Delete. The
        // two are NOT cross-deserializable on purpose — clients
        // hitting a Source-backed server use the new envelope,
        // clients hitting a CRUD-backed server use the old one.
        // Phase 6 deletes CRUD and the old shape with it.
        use pocopine_sync_query::write::MutationPayload;

        let crud = CrudMutationPayload::create(
            "id1".to_string(),
            Draft {
                name: "A".to_string(),
            },
        );
        let canonical: MutationPayload<String, Draft> = MutationPayload::create(
            "id1".to_string(),
            Draft {
                name: "A".to_string(),
            },
        );

        let crud_wire = serde_json::to_value(&crud).unwrap();
        let canonical_wire = serde_json::to_value(&canonical).unwrap();
        assert_eq!(
            crud_wire,
            serde_json::json!({
                "op": "create",
                "payload": {"id": "id1", "draft": {"name": "A"}}
            }),
            "CRUD keeps the legacy nested wire shape"
        );
        assert_eq!(
            canonical_wire,
            serde_json::json!({
                "op": "create",
                "id": "id1",
                "draft": {"name": "A"}
            }),
            "Source uses the flat wire shape"
        );
    }
}
