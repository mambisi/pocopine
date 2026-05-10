use serde::{Deserialize, Serialize};

use crate::{SyncChange, SyncCursor, SyncOp, SyncPullMode, SyncPullResponse, SyncRow};

/// Reason attached to the latest sync state transition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    #[default]
    Idle,
    Initial,
    Manual,
    Live,
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
        self.pending_count = self.rows.iter().filter(|row| row.pending).count() as u64;
        self.conflict_count = self.rows.iter().filter(|row| row.conflict).count() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RowKey, SyncChange, SyncCollectionName, SyncCursor, SyncPullResponse, SyncShapeName,
    };

    #[test]
    fn initial_pull_replaces_rows_and_cursor() {
        let mut state = CollectionState::<String>::default();
        let request = state.begin_initial();
        let response = SyncPullResponse::snapshot(
            SyncShapeName::new("posts").unwrap(),
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
                SyncShapeName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "old".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        let request = state.begin_pull(SyncReason::Manual);
        let response = SyncPullResponse::incremental(
            SyncShapeName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            vec![
                SyncChange {
                    shape: SyncShapeName::new("posts").unwrap(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: None,
                    op: SyncOp::Upsert,
                    row: Some(SyncRow::new("post_2", "new".to_string()).unwrap()),
                    cursor: SyncCursor::new("2").unwrap(),
                },
                SyncChange {
                    shape: SyncShapeName::new("posts").unwrap(),
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
                SyncShapeName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "stale".to_string()).unwrap()],
                None,
            ),
        ));
        assert!(state.apply_pull(
            current,
            SyncPullResponse::snapshot(
                SyncShapeName::new("posts").unwrap(),
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
                SyncShapeName::new("posts").unwrap(),
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
                SyncShapeName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "loaded".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        let request = state.begin_live_pull(SyncReason::Live);
        assert!(state.apply_pull(
            request,
            SyncPullResponse::incremental(
                SyncShapeName::new("posts").unwrap(),
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
}
