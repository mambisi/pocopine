use serde::{Deserialize, Serialize};

use crate::{
    MutationId, RowKey, SyncChange, SyncCursor, SyncOp, SyncPullMode, SyncPullResponse,
    SyncPushResponse, SyncRow,
};

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

/// Opaque generation token for one pull request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncRequest {
    generation: u64,
}

impl SyncRequest {
    pub fn generation(self) -> u64 {
        self.generation
    }
}

/// Local mutation tracked until the server accepts, rejects, or conflicts it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct PendingMutation<T> {
    pub id: MutationId,
    pub op: SyncOp,
    pub key: Option<RowKey>,
    pub before: Option<SyncRow<T>>,
    #[serde(default)]
    pub before_rows: Vec<SyncRow<T>>,
    pub optimistic: Option<SyncRow<T>>,
}

/// Serializable state for one synced collection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct CollectionState<T> {
    pub rows: Vec<SyncRow<T>>,
    pub loading: bool,
    pub syncing: bool,
    pub stale: bool,
    pub error: String,
    pub cursor: Option<SyncCursor>,
    pub version: u64,
    pub refresh_count: u64,
    pub live_event_count: u64,
    pub pending_count: u64,
    pub conflict_count: u64,
    pub rejected_count: u64,
    #[serde(default)]
    pub pending_mutations: Vec<PendingMutation<T>>,
    pub last_reason: SyncReason,
    #[serde(skip)]
    request_generation: u64,
}

impl<T> Default for CollectionState<T> {
    // Manual impl avoids the `T: Default` bound that `derive(Default)` would add.
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            loading: false,
            syncing: false,
            stale: false,
            error: String::new(),
            cursor: None,
            version: 0,
            refresh_count: 0,
            live_event_count: 0,
            pending_count: 0,
            conflict_count: 0,
            rejected_count: 0,
            pending_mutations: Vec::new(),
            last_reason: SyncReason::Idle,
            request_generation: 0,
        }
    }
}

impl<T> CollectionState<T> {
    pub fn begin_initial(&mut self) -> SyncRequest {
        self.begin(SyncReason::Initial, false)
    }

    pub fn begin_pull(&mut self, reason: SyncReason) -> SyncRequest {
        self.begin(reason, false)
    }

    pub fn begin_live_pull(&mut self, reason: SyncReason) -> SyncRequest {
        if self.version == 0 {
            self.live_event_count = self.live_event_count.saturating_add(1);
            return self.begin(reason, true);
        }

        self.request_generation = self.request_generation.saturating_add(1);
        self.refresh_count = self.refresh_count.saturating_add(1);
        self.live_event_count = self.live_event_count.saturating_add(1);
        self.last_reason = reason;
        self.error.clear();
        self.loading = false;
        self.syncing = false;

        SyncRequest {
            generation: self.request_generation,
        }
    }

    pub fn apply_pull(&mut self, request: SyncRequest, response: SyncPullResponse<T>) -> bool {
        if !self.is_current(request) {
            return false;
        }

        let rows_changed = match response.mode {
            SyncPullMode::Snapshot => {
                self.rows = response.rows;
                true
            }
            SyncPullMode::Incremental => self.apply_changes(response.changes),
        };

        self.cursor = response.cursor;
        self.loading = false;
        self.syncing = false;
        self.stale = false;
        self.error.clear();
        self.version = self.version.saturating_add(1);
        if rows_changed {
            self.recount_row_flags();
        }
        true
    }

    pub fn apply_error(&mut self, request: SyncRequest, error: impl ToString) -> bool {
        if !self.is_current(request) {
            return false;
        }
        self.loading = false;
        self.syncing = false;
        self.stale = self.version > 0 || self.stale;
        self.error = error.to_string();
        self.last_reason = SyncReason::Error;
        true
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.loading = false;
        self.syncing = false;
        self.last_reason = SyncReason::Error;
        self.error = error.into();
    }

    pub fn clear_error(&mut self) {
        self.error.clear();
    }

    pub fn mark_stale(&mut self, reason: SyncReason) {
        self.stale = true;
        self.last_reason = reason;
    }

    pub fn is_current(&self, request: SyncRequest) -> bool {
        self.request_generation == request.generation
    }

    fn begin(&mut self, reason: SyncReason, stale_during_sync: bool) -> SyncRequest {
        self.request_generation = self.request_generation.saturating_add(1);
        self.refresh_count = self.refresh_count.saturating_add(1);
        self.last_reason = reason;
        self.error.clear();
        self.stale = stale_during_sync;

        if self.version == 0 {
            self.loading = true;
            self.syncing = false;
        } else {
            self.loading = false;
            self.syncing = true;
        }

        SyncRequest {
            generation: self.request_generation,
        }
    }

    fn apply_changes(&mut self, changes: Vec<SyncChange<T>>) -> bool {
        // Linear scans are deliberate for this first small-collection state
        // container; larger local stores should move to an indexed backend.
        let mut rows_changed = false;
        for change in changes {
            match change.op {
                SyncOp::Upsert => {
                    let Some(row) = change.row else {
                        continue;
                    };
                    if let Some(existing) = self
                        .rows
                        .iter_mut()
                        .find(|existing| existing.key == row.key)
                    {
                        *existing = row;
                    } else {
                        self.rows.push(row);
                    }
                    rows_changed = true;
                }
                SyncOp::Delete => {
                    let Some(key) = change.key else {
                        continue;
                    };
                    if let Some(index) = self.rows.iter().position(|row| row.key == key) {
                        self.rows.remove(index);
                        rows_changed = true;
                    }
                }
                SyncOp::Reset => match change.row {
                    Some(row) => {
                        self.rows.clear();
                        self.rows.push(row);
                        rows_changed = true;
                    }
                    None if !self.rows.is_empty() => {
                        self.rows.clear();
                        rows_changed = true;
                    }
                    None => {}
                },
            }
        }
        rows_changed
    }

    fn recount_row_flags(&mut self) {
        let row_pending = self.rows.iter().filter(|row| row.pending).count() as u64;
        self.pending_count = self.pending_mutations.len() as u64;
        if row_pending > self.pending_count {
            self.pending_count = row_pending;
        }
        self.conflict_count = self.rows.iter().filter(|row| row.conflict).count() as u64;
    }
}

impl<T> CollectionState<T>
where
    T: Clone,
{
    pub fn apply_optimistic_mutation(
        &mut self,
        id: MutationId,
        op: SyncOp,
        key: Option<RowKey>,
        optimistic: Option<SyncRow<T>>,
    ) {
        let before = key.as_ref().and_then(|key| self.row_by_key(key).cloned());
        let before_rows = if op == SyncOp::Reset {
            self.rows.clone()
        } else {
            Vec::new()
        };
        self.pending_mutations.retain(|pending| pending.id != id);

        match op {
            SyncOp::Upsert => {
                if let Some(mut row) = optimistic.clone() {
                    row.pending = true;
                    row.conflict = false;
                    self.upsert_row(row);
                }
            }
            SyncOp::Delete => {
                if let Some(key) = &key {
                    self.remove_row(key);
                }
            }
            SyncOp::Reset => {
                self.rows.clear();
                if let Some(mut row) = optimistic.clone() {
                    row.pending = true;
                    row.conflict = false;
                    self.rows.push(row);
                }
            }
        }

        self.pending_mutations.push(PendingMutation {
            id,
            op,
            key,
            before,
            before_rows,
            optimistic,
        });
        self.error.clear();
        self.last_reason = SyncReason::Push;
        self.recount_row_flags();
    }

    pub fn apply_push(&mut self, response: SyncPushResponse<T>) -> bool {
        let has_accepted = !response.accepted.is_empty();

        for rejected in response.rejected {
            self.rollback_mutation(&rejected.mutation_id);
            self.rejected_count = self.rejected_count.saturating_add(1);
            self.error = rejected.reason;
            self.last_reason = SyncReason::Error;
        }

        for conflict in response.conflicts {
            self.complete_mutation(&conflict.mutation_id);
            if let Some(mut row) = conflict.server_row {
                row.pending = false;
                row.conflict = true;
                self.upsert_row(row);
            } else if let Some(key) = conflict.key {
                self.remove_row(&key);
            }
            self.error = conflict.reason;
            self.last_reason = SyncReason::Error;
        }

        for id in response.accepted {
            self.complete_mutation(&id);
        }

        for mut row in response.rows {
            row.pending = false;
            row.conflict = false;
            self.upsert_row(row);
        }

        if let Some(cursor) = response.cursor {
            self.cursor = Some(cursor);
        }

        self.loading = false;
        self.syncing = false;
        self.stale = has_accepted;
        self.recount_row_flags();
        has_accepted
    }

    fn rollback_mutation(&mut self, id: &MutationId) {
        let Some(index) = self
            .pending_mutations
            .iter()
            .position(|pending| &pending.id == id)
        else {
            return;
        };
        let pending = self.pending_mutations.remove(index);
        match pending.op {
            SyncOp::Upsert => {
                if let Some(before) = pending.before {
                    self.upsert_row(before);
                } else if let Some(key) = pending.key {
                    self.remove_row(&key);
                }
            }
            SyncOp::Delete | SyncOp::Reset => {
                if pending.op == SyncOp::Reset {
                    self.rows = pending.before_rows;
                } else if let Some(before) = pending.before {
                    self.upsert_row(before);
                }
            }
        }
    }

    fn complete_mutation(&mut self, id: &MutationId) {
        let Some(index) = self
            .pending_mutations
            .iter()
            .position(|pending| &pending.id == id)
        else {
            return;
        };
        let pending = self.pending_mutations.remove(index);
        if let Some(key) = pending.key {
            if let Some(row) = self.row_by_key_mut(&key) {
                row.pending = false;
            }
        }
    }

    fn upsert_row(&mut self, row: SyncRow<T>) {
        if let Some(existing) = self
            .rows
            .iter_mut()
            .find(|existing| existing.key == row.key)
        {
            *existing = row;
        } else {
            self.rows.push(row);
        }
    }

    fn remove_row(&mut self, key: &RowKey) {
        if let Some(index) = self.rows.iter().position(|row| &row.key == key) {
            self.rows.remove(index);
        }
    }

    fn row_by_key(&self, key: &RowKey) -> Option<&SyncRow<T>> {
        self.rows.iter().find(|row| &row.key == key)
    }

    fn row_by_key_mut(&mut self, key: &RowKey) -> Option<&mut SyncRow<T>> {
        self.rows.iter_mut().find(|row| &row.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MutationId, RowKey, RowVersion, SyncChange, SyncCollectionName, SyncConflict, SyncCursor,
        SyncPullResponse, SyncPushResponse, SyncRejectedMutation, SyncStreamName,
    };

    #[test]
    fn initial_pull_replaces_rows_and_cursor() {
        let mut state = CollectionState::<String>::default();
        let request = state.begin_initial();
        let response = SyncPullResponse::snapshot(
            SyncStreamName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            vec![SyncRow::new("post_1", "hello".to_string()).unwrap()],
            Some(SyncCursor::new("1").unwrap()),
        );

        assert!(state.apply_pull(request, response));
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].value, "hello");
        assert_eq!(state.cursor.as_ref().unwrap().as_str(), "1");
        assert!(!state.loading);
    }

    #[test]
    fn incremental_pull_upserts_and_deletes_by_key() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "old".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        let request = state.begin_pull(SyncReason::Manual);
        let response = SyncPullResponse::incremental(
            SyncStreamName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            vec![
                SyncChange {
                    stream: SyncStreamName::new("posts").unwrap(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: None,
                    op: SyncOp::Upsert,
                    row: Some(SyncRow::new("post_2", "new".to_string()).unwrap()),
                    cursor: SyncCursor::new("2").unwrap(),
                },
                SyncChange {
                    stream: SyncStreamName::new("posts").unwrap(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: Some(RowKey::new("post_1").unwrap()),
                    op: SyncOp::Delete,
                    row: None,
                    cursor: SyncCursor::new("3").unwrap(),
                },
            ],
            Some(SyncCursor::new("3").unwrap()),
        );

        assert!(state.apply_pull(request, response));
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].key.as_str(), "post_2");
    }

    #[test]
    fn stale_response_is_ignored() {
        let mut state = CollectionState::<String>::default();
        let stale = state.begin_initial();
        let current = state.begin_pull(SyncReason::Manual);

        assert!(!state.apply_pull(
            stale,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "stale".to_string()).unwrap()],
                None,
            ),
        ));
        assert!(state.apply_pull(
            current,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_2", "current".to_string()).unwrap()],
                None,
            ),
        ));
        assert_eq!(state.rows[0].value, "current");
    }

    #[test]
    fn live_pull_after_initial_load_is_background_refresh() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "loaded".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        let request = state.begin_live_pull(SyncReason::Live);

        assert_eq!(request.generation(), 2);
        assert_eq!(state.refresh_count, 2);
        assert_eq!(state.live_event_count, 1);
        assert_eq!(state.last_reason, SyncReason::Live);
        assert!(!state.loading);
        assert!(!state.syncing);
        assert!(!state.stale);
        assert_eq!(state.rows[0].value, "loaded");
    }

    #[test]
    fn initial_live_pull_still_shows_loading() {
        let mut state = CollectionState::<String>::default();

        state.begin_live_pull(SyncReason::Live);

        assert!(state.loading);
        assert!(!state.syncing);
        assert!(state.stale);
        assert_eq!(state.live_event_count, 1);
    }

    #[test]
    fn empty_incremental_pull_keeps_existing_rows_visible() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "loaded".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        let request = state.begin_live_pull(SyncReason::Live);
        assert!(state.apply_pull(
            request,
            SyncPullResponse::incremental(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                Vec::new(),
                Some(SyncCursor::new("1").unwrap()),
            ),
        ));

        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].key.as_str(), "post_1");
        assert_eq!(state.rows[0].value, "loaded");
        assert!(!state.loading);
        assert!(!state.syncing);
        assert!(!state.stale);
    }

    #[test]
    fn optimistic_upsert_marks_row_pending() {
        let mut state = CollectionState::<String>::default();
        let row = SyncRow::new("post_1", "draft".to_string()).unwrap();

        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(row),
        );

        assert_eq!(state.rows.len(), 1);
        assert!(state.rows[0].pending);
        assert_eq!(state.pending_count, 1);
        assert_eq!(state.last_reason, SyncReason::Push);
    }

    #[test]
    fn rejected_push_rolls_back_optimistic_upsert() {
        let mut state = CollectionState::<String>::default();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "draft".to_string()).unwrap()),
        );
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "title is required".to_string(),
        });

        assert!(!state.apply_push(response));

        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 0);
        assert_eq!(state.rejected_count, 1);
        assert_eq!(state.error, "title is required");
    }

    #[test]
    fn accepted_push_applies_canonical_row_and_clears_pending() {
        let mut state = CollectionState::<String>::default();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "draft".to_string()).unwrap()),
        );
        let mut canonical = SyncRow::new("post_1", "saved".to_string())
            .unwrap()
            .version("v1")
            .unwrap();
        canonical.pending = true;
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response
            .accepted
            .push(MutationId::new("device_1:1").unwrap());
        response.rows.push(canonical);
        response.cursor = Some(SyncCursor::new("1").unwrap());

        assert!(state.apply_push(response));

        assert_eq!(state.rows[0].value, "saved");
        assert!(!state.rows[0].pending);
        assert_eq!(
            state.rows[0].version.as_ref().unwrap(),
            &RowVersion::new("v1").unwrap()
        );
        assert_eq!(state.pending_count, 0);
        assert!(state.stale);
        assert_eq!(state.cursor.as_ref().unwrap().as_str(), "1");
    }

    #[test]
    fn conflict_push_marks_server_row_conflicted() {
        let mut state = CollectionState::<String>::default();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local".to_string()).unwrap()),
        );
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(SyncRow::new("post_1", "server".to_string()).unwrap()),
            reason: "base version is stale".to_string(),
        });

        assert!(!state.apply_push(response));

        assert_eq!(state.rows[0].value, "server");
        assert!(!state.rows[0].pending);
        assert!(state.rows[0].conflict);
        assert_eq!(state.conflict_count, 1);
        assert_eq!(state.error, "base version is stale");
    }
}
