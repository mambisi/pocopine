use pocopine_sync::MutationId;
use serde::{Deserialize, Serialize};

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
    Removed {
        id: Id,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(CrudOutcome::<String, String>::Removed {
            id: "post_1".to_string()
        }
        .is_terminal());
    }
}
