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
        title: String,
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
}
