use pocopine_sync::RowVersion;
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_policy_defaults_to_queue_offline() {
        let options = SaveOptions::<String>::default();

        assert_eq!(options.write_policy, WritePolicy::QueueOffline);
        assert!(options.write_policy.queues_offline());
        assert!(options.require_online().write_policy.requires_online());
    }
}
