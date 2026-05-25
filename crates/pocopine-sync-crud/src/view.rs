use pocopine_sync::{
    CollectionState, MutationId, RowVersion, SyncOp, SyncReason, SyncResult, SyncRow,
};
use serde::{Deserialize, Serialize};

use crate::ResourceId;

/// Render status for a typed resource row.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalResourceRowStatus {
    /// The row matches the latest known canonical server state.
    #[default]
    Synced,
    /// The row includes a local optimistic overlay waiting for `/push`.
    Pending,
    /// The server reported a conflict for this row.
    Conflict,
}

impl LocalResourceRowStatus {
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }

    pub fn is_conflict(self) -> bool {
        matches!(self, Self::Conflict)
    }
}

/// A typed row currently visible to application code.
///
/// `row_version` belongs to the rendered row. `base_version` is the latest
/// canonical server version known for the id and is the value generated CRUD
/// saves/removes should use for conflict detection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: Serialize, Row: Serialize",
    deserialize = "Id: Deserialize<'de>, Row: Deserialize<'de>"
))]
pub struct LocalResourceRow<Id, Row> {
    pub id: Id,
    pub value: Row,
    pub row_version: Option<RowVersion>,
    pub base_version: Option<RowVersion>,
    pub status: LocalResourceRowStatus,
}

impl<Id, Row> LocalResourceRow<Id, Row> {
    pub fn is_pending(&self) -> bool {
        self.status.is_pending()
    }

    pub fn is_conflict(&self) -> bool {
        self.status.is_conflict()
    }
}

/// A queued resource mutation, including mutations with no visible row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "Id: Serialize", deserialize = "Id: Deserialize<'de>"))]
pub struct LocalResourcePendingMutation<Id> {
    pub mutation_id: MutationId,
    pub id: Option<Id>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
}

impl<Id> LocalResourcePendingMutation<Id> {
    pub fn is_delete(&self) -> bool {
        self.op == SyncOp::Delete
    }
}

/// Typed local-first resource state ready for components or generated CRUD APIs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: Serialize, Row: Serialize",
    deserialize = "Id: Deserialize<'de>, Row: Deserialize<'de>"
))]
pub struct LocalResourceView<Id, Row> {
    pub rows: Vec<LocalResourceRow<Id, Row>>,
    pub pending_mutations: Vec<LocalResourcePendingMutation<Id>>,
    pub loading: bool,
    pub syncing: bool,
    pub stale: bool,
    pub error: String,
    pub version: u64,
    pub pending_count: u64,
    pub conflict_count: u64,
    pub rejected_count: u64,
    pub last_reason: SyncReason,
}

/// Comparable state emitted by resource-view subscriptions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: Serialize, Row: Serialize",
    deserialize = "Id: Deserialize<'de>, Row: Deserialize<'de>"
))]
pub enum LocalResourceViewState<Id, Row> {
    Ready(LocalResourceView<Id, Row>),
    Error(String),
}

impl<Id, Row> LocalResourceViewState<Id, Row> {
    pub fn ready(view: LocalResourceView<Id, Row>) -> Self {
        Self::Ready(view)
    }

    pub fn from_error(error: impl ToString) -> Self {
        Self::Error(error.to_string())
    }

    pub fn from_result(result: SyncResult<LocalResourceView<Id, Row>>) -> Self {
        match result {
            Ok(view) => Self::Ready(view),
            Err(err) => Self::Error(err.to_string()),
        }
    }

    pub fn from_collection_state(state: &CollectionState<Row>) -> Self
    where
        Id: ResourceId,
        Row: Clone,
    {
        Self::from_result(LocalResourceView::from_collection_state(state))
    }

    pub fn view(&self) -> Option<&LocalResourceView<Id, Row>> {
        match self {
            Self::Ready(view) => Some(view),
            Self::Error(_) => None,
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Ready(_) => None,
            Self::Error(error) => Some(error.as_str()),
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error_message()
    }

    pub fn has_pending(&self) -> bool {
        self.view().is_some_and(LocalResourceView::has_pending)
    }

    pub fn has_conflicts(&self) -> bool {
        self.view().is_some_and(LocalResourceView::has_conflicts)
    }
}

impl<Id, Row> LocalResourceView<Id, Row> {
    /// Build a typed resource view from the low-level sync collection state.
    pub fn from_collection_state(state: &CollectionState<Row>) -> SyncResult<Self>
    where
        Id: ResourceId,
        Row: Clone,
    {
        let rows = state
            .rows
            .iter()
            .map(|row| row_from_sync::<Id, Row>(state, row))
            .collect::<SyncResult<Vec<_>>>()?;
        let pending_mutations = state
            .pending_mutations
            .iter()
            .map(|pending| {
                let id = pending.key.as_ref().map(Id::from_row_key).transpose()?;
                let base_version = pending.key.as_ref().and_then(|key| state.base_version(key));
                Ok(LocalResourcePendingMutation {
                    mutation_id: pending.id.clone(),
                    id,
                    op: pending.op,
                    base_version,
                })
            })
            .collect::<SyncResult<Vec<_>>>()?;

        Ok(Self {
            rows,
            pending_mutations,
            loading: state.loading,
            syncing: state.syncing,
            stale: state.stale,
            error: state.error.clone(),
            version: state.version,
            pending_count: state.pending_count,
            conflict_count: state.conflict_count,
            rejected_count: state.rejected_count,
            last_reason: state.last_reason,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn has_pending(&self) -> bool {
        self.pending_count > 0 || self.rows.iter().any(LocalResourceRow::is_pending)
    }

    pub fn has_conflicts(&self) -> bool {
        self.conflict_count > 0 || self.rows.iter().any(LocalResourceRow::is_conflict)
    }

    pub fn conflicts(&self) -> Vec<&LocalResourceRow<Id, Row>> {
        self.rows.iter().filter(|row| row.is_conflict()).collect()
    }

    pub fn get(&self, id: &Id) -> Option<&LocalResourceRow<Id, Row>>
    where
        Id: Eq,
    {
        self.rows.iter().find(|row| &row.id == id)
    }

    pub fn conflict_for(&self, id: &Id) -> Option<&LocalResourceRow<Id, Row>>
    where
        Id: Eq,
    {
        self.get(id).filter(|row| row.is_conflict())
    }

    pub fn pending_for(&self, id: &Id) -> Vec<&LocalResourcePendingMutation<Id>>
    where
        Id: Eq,
    {
        self.pending_mutations
            .iter()
            .filter(|pending| pending.id.as_ref() == Some(id))
            .collect()
    }

    pub fn base_version(&self, id: &Id) -> Option<&RowVersion>
    where
        Id: Eq,
    {
        self.get(id)
            .and_then(|row| row.base_version.as_ref())
            .or_else(|| {
                self.pending_mutations
                    .iter()
                    .rev()
                    .find(|pending| pending.id.as_ref() == Some(id))
                    .and_then(|pending| pending.base_version.as_ref())
            })
    }
}

/// Build a typed resource view from the low-level sync collection state.
pub fn local_resource_view<Id, Row>(
    state: &CollectionState<Row>,
) -> SyncResult<LocalResourceView<Id, Row>>
where
    Id: ResourceId,
    Row: Clone,
{
    LocalResourceView::from_collection_state(state)
}

fn row_from_sync<Id, Row>(
    state: &CollectionState<Row>,
    row: &SyncRow<Row>,
) -> SyncResult<LocalResourceRow<Id, Row>>
where
    Id: ResourceId,
    Row: Clone,
{
    let status = if row.conflict {
        LocalResourceRowStatus::Conflict
    } else if row.pending {
        LocalResourceRowStatus::Pending
    } else {
        LocalResourceRowStatus::Synced
    };

    Ok(LocalResourceRow {
        id: Id::from_row_key(&row.key)?,
        value: row.value.clone(),
        row_version: row.version.clone(),
        base_version: state.base_version(&row.key),
        status,
    })
}

#[cfg(test)]
mod tests {
    use pocopine_sync::{
        CollectionState, MutationId, RowKey, RowVersion, SyncConflict, SyncCursor,
        SyncPullResponse, SyncPushResponse, SyncRow, SyncStreamName,
    };

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Post {
        title: String,
    }

    fn row(key: &str, title: &str, version: &str) -> SyncRow<Post> {
        SyncRow::new(
            key,
            Post {
                title: title.to_string(),
            },
        )
        .unwrap()
        .version(version)
        .unwrap()
    }

    fn initial_state() -> CollectionState<Post> {
        let mut state = CollectionState::default();
        let request = state.begin_initial();
        state.apply_pull(
            request,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                pocopine_sync::SyncCollectionName::new("posts").unwrap(),
                vec![row("post_1", "Server", "row_1")],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );
        state
    }

    #[test]
    fn resource_view_maps_synced_rows_to_typed_ids() {
        let state = initial_state();

        let view = local_resource_view::<String, _>(&state).unwrap();

        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].id, "post_1");
        assert_eq!(view.rows[0].value.title, "Server");
        assert_eq!(view.rows[0].status, LocalResourceRowStatus::Synced);
        assert_eq!(
            view.rows[0].base_version.as_ref().unwrap(),
            &RowVersion::new("row_1").unwrap()
        );
        assert!(!view.has_pending());
        assert!(!view.has_conflicts());
    }

    #[test]
    fn resource_view_exposes_rebased_pending_overlay_base_version() {
        let mut state = initial_state();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:save").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(
                SyncRow::new(
                    "post_1",
                    Post {
                        title: "Local".to_string(),
                    },
                )
                .unwrap(),
            ),
        );

        let view = local_resource_view::<String, _>(&state).unwrap();

        assert_eq!(view.rows[0].value.title, "Local");
        assert_eq!(view.rows[0].status, LocalResourceRowStatus::Pending);
        assert!(view.rows[0].is_pending());
        assert!(view.rows[0].row_version.is_none());
        assert_eq!(
            view.base_version(&"post_1".to_string()).unwrap(),
            &RowVersion::new("row_1").unwrap()
        );
        assert_eq!(view.pending_mutations.len(), 1);
        assert_eq!(view.pending_mutations[0].id.as_deref(), Some("post_1"));
        assert_eq!(view.pending_mutations[0].op, SyncOp::Upsert);
    }

    #[test]
    fn resource_view_exposes_pending_delete_without_visible_row() {
        let mut state = initial_state();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:delete").unwrap(),
            SyncOp::Delete,
            Some(RowKey::new("post_1").unwrap()),
            None,
        );

        let view = local_resource_view::<String, _>(&state).unwrap();

        assert!(view.rows.is_empty());
        assert!(view.has_pending());
        assert_eq!(view.pending_mutations.len(), 1);
        assert!(view.pending_mutations[0].is_delete());
        assert_eq!(
            view.base_version(&"post_1".to_string()).unwrap(),
            &RowVersion::new("row_1").unwrap()
        );
    }

    #[test]
    fn resource_view_marks_conflict_rows() {
        let mut state = initial_state();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:save").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(
                SyncRow::new(
                    "post_1",
                    Post {
                        title: "Local".to_string(),
                    },
                )
                .unwrap(),
            ),
        );
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:save").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(row("post_1", "Server updated", "row_2")),
            reason: "base version is stale".to_string(),
        });
        state.apply_push(response);

        let view = local_resource_view::<String, _>(&state).unwrap();

        assert_eq!(view.rows[0].value.title, "Server updated");
        assert_eq!(view.rows[0].status, LocalResourceRowStatus::Conflict);
        assert!(view.rows[0].is_conflict());
        assert!(view.has_conflicts());
        assert_eq!(view.conflicts().len(), 1);
        assert_eq!(
            view.conflict_for(&"post_1".to_string())
                .unwrap()
                .value
                .title,
            "Server updated"
        );
        assert!(view.conflict_for(&"post_2".to_string()).is_none());
        assert_eq!(view.conflict_count, 1);
        assert_eq!(view.pending_mutations.len(), 0);
    }

    #[test]
    fn resource_view_keeps_conflict_visible_under_later_pending_overlay() {
        let mut state = initial_state();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:first").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(
                SyncRow::new(
                    "post_1",
                    Post {
                        title: "Local first".to_string(),
                    },
                )
                .unwrap(),
            ),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:second").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(
                SyncRow::new(
                    "post_1",
                    Post {
                        title: "Local second".to_string(),
                    },
                )
                .unwrap(),
            ),
        );
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:first").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(row("post_1", "Server updated", "row_2")),
            reason: "base version is stale".to_string(),
        });
        state.apply_push(response);

        let view = local_resource_view::<String, _>(&state).unwrap();

        assert_eq!(view.rows[0].value.title, "Local second");
        assert_eq!(view.rows[0].status, LocalResourceRowStatus::Conflict);
        assert!(view.rows[0].is_conflict());
        assert!(view.has_pending());
        assert!(view.has_conflicts());
        assert_eq!(view.conflict_count, 1);
    }

    #[test]
    fn resource_view_base_version_uses_latest_pending_fallback() {
        let view = LocalResourceView::<String, Post> {
            rows: Vec::new(),
            pending_mutations: vec![
                LocalResourcePendingMutation {
                    mutation_id: MutationId::new("device_1:first").unwrap(),
                    id: Some("post_1".to_string()),
                    op: SyncOp::Delete,
                    base_version: Some(RowVersion::new("row_1").unwrap()),
                },
                LocalResourcePendingMutation {
                    mutation_id: MutationId::new("device_1:second").unwrap(),
                    id: Some("post_1".to_string()),
                    op: SyncOp::Delete,
                    base_version: Some(RowVersion::new("row_2").unwrap()),
                },
            ],
            loading: false,
            syncing: false,
            stale: false,
            error: String::new(),
            version: 0,
            pending_count: 2,
            conflict_count: 0,
            rejected_count: 0,
            last_reason: SyncReason::Push,
        };

        assert_eq!(
            view.base_version(&"post_1".to_string()).unwrap(),
            &RowVersion::new("row_2").unwrap()
        );
    }

    #[test]
    fn resource_view_rejects_rows_that_do_not_match_typed_id() {
        let mut state = CollectionState::default();
        let request = state.begin_initial();
        state.apply_pull(
            request,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                pocopine_sync::SyncCollectionName::new("posts").unwrap(),
                vec![row("not-an-integer", "Bad", "row_1")],
                None,
            ),
        );

        let err = local_resource_view::<i64, _>(&state).unwrap_err();

        assert!(err.to_string().contains("invalid resource id"));
    }

    #[test]
    fn view_state_wraps_ready_views_and_errors() {
        let state = initial_state();
        let view_state = LocalResourceViewState::<String, Post>::from_collection_state(&state);

        let view = view_state.view().expect("view state should be ready");
        assert_eq!(view.rows.len(), 1);
        assert!(!view_state.has_pending());
        assert!(!view_state.has_conflicts());
        assert_eq!(view_state.error(), None);

        let error_state = LocalResourceViewState::<String, Post>::from_error("already borrowed");
        assert!(error_state.view().is_none());
        assert_eq!(error_state.error(), Some("already borrowed"));
        assert!(!error_state.has_pending());
        assert!(!error_state.has_conflicts());
    }
}
