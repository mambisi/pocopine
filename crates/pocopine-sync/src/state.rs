//! Shared sync-state vocabulary.
//!
//! The legacy collection client's `CollectionState` / `PendingMutation`
//! / `SyncRequest` lived here until RFC-090 Phase 6; the collection
//! client was deleted once `examples/keep` migrated to
//! `pocopine-sync-query`. `SyncReason` stays — it's the transition
//! label that `pocopine_sync_query::QueryState` still carries.

use serde::{Deserialize, Serialize};

/// Reason attached to the latest sync state transition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    #[default]
    Idle,
    Initial,
    Manual,
    Live,
    Push,
    Gap,
    Error,
}
