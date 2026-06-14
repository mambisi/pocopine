use serde::{Deserialize, Serialize};

use crate::{
    MutationId, RowKey, RowVersion, SyncChange, SyncCursor, SyncOp, SyncPullMode, SyncPullResponse,
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
    #[serde(default)]
    canonical_rows: Vec<SyncRow<T>>,
    pub rows: Vec<SyncRow<T>>,
    pub loading: bool,
    pub syncing: bool,
    pub stale: bool,
    pub error: String,
    pub cursor: Option<SyncCursor>,
    pub version: u64,
    pub refresh_count: u64,
    pub live_event_count: u64,
    /// Number of in-flight mutations or pending row flags. This includes
    /// mutations like optimistic deletes where no pending row remains visible.
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
            canonical_rows: Vec::new(),
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

    pub fn apply_pull(&mut self, request: SyncRequest, response: SyncPullResponse<T>) -> bool
    where
        T: Clone,
    {
        if !self.is_current(request) {
            return false;
        }

        match response.mode {
            SyncPullMode::Snapshot => {
                self.canonical_rows = response.rows;
            }
            SyncPullMode::Incremental => self.apply_canonical_changes(response.changes),
        }
        self.rebase_rows_from_canonical();

        self.cursor = response.cursor;
        self.loading = false;
        self.syncing = false;
        self.stale = false;
        self.error.clear();
        self.version = self.version.saturating_add(1);
        true
    }

    pub fn apply_local_snapshot(
        &mut self,
        rows: Vec<SyncRow<T>>,
        cursor: Option<SyncCursor>,
        pending_count: u64,
    ) -> bool
    where
        T: Clone,
    {
        self.apply_local_snapshot_inner(rows, cursor, Vec::new(), pending_count)
    }

    pub fn apply_local_snapshot_with_pending(
        &mut self,
        rows: Vec<SyncRow<T>>,
        cursor: Option<SyncCursor>,
        pending_mutations: Vec<PendingMutation<T>>,
    ) -> bool
    where
        T: Clone,
    {
        let pending_count = pending_mutations.len() as u64;
        self.apply_local_snapshot_inner(rows, cursor, pending_mutations, pending_count)
    }

    fn apply_local_snapshot_inner(
        &mut self,
        rows: Vec<SyncRow<T>>,
        cursor: Option<SyncCursor>,
        pending_mutations: Vec<PendingMutation<T>>,
        pending_count: u64,
    ) -> bool
    where
        T: Clone,
    {
        if rows.is_empty() && cursor.is_none() && pending_count == 0 {
            return false;
        }

        self.canonical_rows = rows;
        self.pending_mutations = pending_mutations;
        self.rebase_rows_from_canonical();
        self.cursor = cursor;
        self.loading = false;
        self.syncing = false;
        self.stale = true;
        self.error.clear();
        self.version = self.version.saturating_add(1);
        self.last_reason = SyncReason::Initial;
        if pending_count > self.pending_count {
            self.pending_count = pending_count;
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

    /// Hard reset of the in-memory state for a schema-version invalidation.
    ///
    /// Used by `start_open_then_pull` when the server's advertised
    /// `schema_version` differs from the cached value: after the durable
    /// `clear_stream` succeeds, the in-memory `CollectionState` must
    /// observe the same wipe so the upcoming pull rebuilds canonical
    /// state under the new shape. Unlike `apply_local_snapshot_*`, this
    /// method unconditionally drops every field — rows, canonical_rows,
    /// pending_mutations, cursor, counters, errors — and bumps
    /// `request_generation` so any in-flight pull/push token issued
    /// before the reset is invalidated and its `apply_*` calls become
    /// no-ops via `is_current`.
    ///
    /// Note: this is intentionally NOT `*self = CollectionState::default()`
    /// because `version` is preserved at its current value — the
    /// collection has "loaded once", just with a now-empty canonical
    /// set. The next pull will write fresh rows via `apply_pull`.
    pub fn reset_for_schema_invalidation(&mut self) {
        self.canonical_rows.clear();
        self.rows.clear();
        self.pending_mutations.clear();
        self.cursor = None;
        self.pending_count = 0;
        self.conflict_count = 0;
        self.rejected_count = 0;
        self.error.clear();
        self.stale = false;
        self.loading = false;
        self.syncing = false;
        self.last_reason = SyncReason::Initial;
        // Invalidate any token issued before the reset so a stale
        // `apply_pull` (e.g. a pull that was already in flight when the
        // schema mismatch was detected) becomes a no-op via
        // `is_current`. Spawned tasks must call `begin_initial` /
        // `begin_pull` again after the reset to get a fresh token.
        self.request_generation = self.request_generation.saturating_add(1);
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

    /// Return whether this collection currently has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Return the row with the given sync row key.
    pub fn row(&self, key: &RowKey) -> Option<&SyncRow<T>> {
        self.rows.iter().find(|row| &row.key == key)
    }

    /// Return the mutable row with the given sync row key.
    pub fn row_mut(&mut self, key: &RowKey) -> Option<&mut SyncRow<T>> {
        self.rows.iter_mut().find(|row| &row.key == key)
    }

    /// Return the current server-issued row version, if one is known.
    pub fn row_version(&self, key: &RowKey) -> Option<&RowVersion> {
        self.canonical_row(key)
            .or_else(|| self.row(key))
            .and_then(|row| row.version.as_ref())
    }

    /// Clone the current row version for use as a mutation base version.
    pub fn base_version(&self, key: &RowKey) -> Option<RowVersion> {
        self.row_version(key).cloned()
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

    fn apply_canonical_changes(&mut self, changes: Vec<SyncChange<T>>) {
        // Linear scans are deliberate for this first small-collection state
        // container; larger local stores should move to an indexed backend.
        for change in changes {
            match change.op {
                SyncOp::Upsert => {
                    let Some(row) = change.row else {
                        continue;
                    };
                    self.upsert_canonical_row(row);
                }
                SyncOp::Delete => {
                    let Some(key) = change.key else {
                        continue;
                    };
                    self.remove_canonical_row(&key);
                }
                SyncOp::Reset => match change.row {
                    Some(row) => {
                        self.canonical_rows.clear();
                        self.canonical_rows.push(row);
                    }
                    None => self.canonical_rows.clear(),
                },
            }
        }
    }

    fn canonical_row(&self, key: &RowKey) -> Option<&SyncRow<T>> {
        self.canonical_rows.iter().find(|row| &row.key == key)
    }

    fn canonical_row_mut(&mut self, key: &RowKey) -> Option<&mut SyncRow<T>> {
        self.canonical_rows.iter_mut().find(|row| &row.key == key)
    }

    fn upsert_canonical_row(&mut self, row: SyncRow<T>) {
        if let Some(existing) = self
            .canonical_rows
            .iter_mut()
            .find(|existing| existing.key == row.key)
        {
            *existing = row;
        } else {
            self.canonical_rows.push(row);
        }
    }

    fn remove_canonical_row(&mut self, key: &RowKey) {
        if let Some(index) = self.canonical_rows.iter().position(|row| &row.key == key) {
            self.canonical_rows.remove(index);
        }
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
    fn rebase_rows_from_canonical(&mut self) {
        self.rows = self.canonical_rows.clone();
        for pending in self.pending_mutations.clone() {
            self.apply_pending_overlay(&pending);
        }
        self.recount_row_flags();
    }

    fn apply_pending_overlay(&mut self, pending: &PendingMutation<T>) {
        match pending.op {
            SyncOp::Upsert => {
                if let Some(mut row) = pending.optimistic.clone() {
                    row.pending = true;
                    row.conflict = self.canonical_conflict(&row.key);
                    self.upsert_row(row);
                } else if let Some(key) = &pending.key
                    && let Some(row) = self.row_mut(key)
                {
                    row.pending = true;
                }
            }
            SyncOp::Delete => {
                if let Some(key) = &pending.key {
                    if self.canonical_conflict(key) {
                        if let Some(row) = self.row_mut(key) {
                            row.pending = true;
                            row.conflict = true;
                        }
                    } else {
                        self.remove_row(key);
                    }
                }
            }
            SyncOp::Reset => {
                self.rows.clear();
                if let Some(mut row) = pending.optimistic.clone() {
                    row.pending = true;
                    row.conflict = self.canonical_conflict(&row.key);
                    self.rows.push(row);
                }
            }
        }
    }

    fn canonical_conflict(&self, key: &RowKey) -> bool {
        self.canonical_row(key)
            .map(|row| row.conflict)
            .unwrap_or(false)
    }

    fn bootstrap_canonical_rows_from_legacy_rows(&mut self) {
        if !self.canonical_rows.is_empty() || self.rows.is_empty() {
            return;
        }

        let mut rows = self.rows.clone();
        for pending in self.pending_mutations.iter().rev() {
            match pending.op {
                SyncOp::Upsert => {
                    if let Some(before) = pending.before.clone() {
                        upsert_row_in(&mut rows, before);
                    } else if let Some(key) = &pending.key {
                        remove_row_from(&mut rows, key);
                    }
                }
                SyncOp::Delete => {
                    if let Some(before) = pending.before.clone() {
                        upsert_row_in(&mut rows, before);
                    }
                }
                SyncOp::Reset => {
                    if !pending.before_rows.is_empty() {
                        rows = pending.before_rows.clone();
                    }
                }
            }
        }

        for row in &mut rows {
            row.pending = false;
        }
        self.canonical_rows = rows;
    }

    pub fn apply_optimistic_mutation(
        &mut self,
        id: MutationId,
        op: SyncOp,
        key: Option<RowKey>,
        optimistic: Option<SyncRow<T>>,
    ) {
        self.bootstrap_canonical_rows_from_legacy_rows();
        self.pending_mutations.retain(|pending| pending.id != id);
        self.rebase_rows_from_canonical();

        let before = key.as_ref().and_then(|key| self.row(key).cloned());
        let before_rows = if op == SyncOp::Reset {
            self.rows.clone()
        } else {
            Vec::new()
        };

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
        self.rebase_rows_from_canonical();
    }

    pub fn apply_push(&mut self, response: SyncPushResponse<T>) -> bool {
        let has_accepted = !response.accepted.is_empty();
        let rejected_total = response.rejected.len();
        let conflict_total = response.conflicts.len();
        let mut first_failure = None;

        for rejected in response.rejected {
            if first_failure.is_none() {
                first_failure = Some(rejected.reason.clone());
            }
            self.remove_pending_mutation(&rejected.mutation_id);
            self.rejected_count = self.rejected_count.saturating_add(1);
        }

        for conflict in response.conflicts {
            if first_failure.is_none() {
                first_failure = Some(conflict.reason.clone());
            }
            let pending = self.remove_pending_mutation(&conflict.mutation_id);
            if let Some(mut row) = conflict.server_row {
                row.pending = false;
                row.conflict = true;
                self.upsert_canonical_row(row);
            } else if let Some(key) = conflict.key {
                let fallback = pending
                    .and_then(|pending| pending.optimistic)
                    .or_else(|| self.row(&key).cloned());
                if let Some(mut row) = fallback {
                    row.pending = false;
                    row.conflict = true;
                    self.upsert_canonical_row(row);
                } else if let Some(row) = self.canonical_row_mut(&key) {
                    row.pending = false;
                    row.conflict = true;
                }
            }
        }

        for id in response.accepted {
            self.accept_mutation(&id);
        }

        for mut row in response.rows {
            row.pending = false;
            row.conflict = false;
            self.upsert_canonical_row(row);
        }

        if let Some(cursor) = response.cursor {
            self.cursor = Some(cursor);
        }

        if let Some(reason) = first_failure {
            self.error = push_failure_message(rejected_total, conflict_total, reason);
            self.last_reason = SyncReason::Error;
        }

        self.loading = false;
        self.syncing = false;
        self.stale = has_accepted;
        self.rebase_rows_from_canonical();
        has_accepted
    }

    pub fn apply_push_error(&mut self, id: &MutationId, error: impl ToString) -> bool {
        self.remove_pending_mutation(id);
        self.rebase_rows_from_canonical();
        self.loading = false;
        self.syncing = false;
        self.stale = self.version > 0 || self.stale;
        self.error = error.to_string();
        self.last_reason = SyncReason::Error;
        true
    }

    /// Drop every pending mutation targeting one row and clear its conflict
    /// marker, then rebase the rendered view from canonical.
    ///
    /// This is the client-side half of `discard_local`: the user chose to
    /// throw away their queued local edits for the row. Unkeyed pending
    /// mutations (`key == None`) and mutations for other rows are left
    /// intact. Returns `true` if the in-memory view changed.
    pub fn discard_local(&mut self, key: &RowKey) -> bool {
        let before = self.pending_mutations.len();
        self.pending_mutations
            .retain(|pending| pending.key.as_ref() != Some(key));
        let dropped_pending = self.pending_mutations.len() != before;

        let mut cleared_conflict = false;
        if let Some(row) = self.canonical_row_mut(key)
            && row.conflict
        {
            row.conflict = false;
            cleared_conflict = true;
        }
        if let Some(row) = self.row_mut(key)
            && row.conflict
        {
            row.conflict = false;
            cleared_conflict = true;
        }

        let changed = dropped_pending || cleared_conflict;
        if changed {
            self.last_reason = SyncReason::Manual;
            self.rebase_rows_from_canonical();
            if self.conflict_count == 0 && self.rejected_count == 0 {
                self.error.clear();
            }
        }
        changed
    }

    /// Clear the local conflict marker for a row and rebase the rendered view.
    ///
    /// This is the client-side half of a user choosing the current server row
    /// as the winner. Pending mutations are left intact; if a later pending
    /// overlay still targets the row, the rendered row remains pending after
    /// the conflict marker is cleared.
    pub fn clear_conflict(&mut self, key: &RowKey) -> bool {
        let mut changed = false;

        if let Some(row) = self.canonical_row_mut(key)
            && row.conflict
        {
            row.conflict = false;
            changed = true;
        }

        if let Some(row) = self.row_mut(key)
            && row.conflict
        {
            row.conflict = false;
            changed = true;
        }

        if changed {
            self.last_reason = SyncReason::Manual;
            self.rebase_rows_from_canonical();
            if self.conflict_count == 0 && self.rejected_count == 0 {
                self.error.clear();
            }
        }

        changed
    }

    fn accept_mutation(&mut self, id: &MutationId) {
        let Some(pending) = self.remove_pending_mutation(id) else {
            return;
        };
        match pending.op {
            SyncOp::Delete => {
                if let Some(key) = pending.key {
                    self.remove_canonical_row(&key);
                }
            }
            SyncOp::Reset => self.canonical_rows.clear(),
            SyncOp::Upsert => {}
        }
    }

    fn remove_pending_mutation(&mut self, id: &MutationId) -> Option<PendingMutation<T>> {
        let index = self
            .pending_mutations
            .iter()
            .position(|pending| &pending.id == id)?;
        Some(self.pending_mutations.remove(index))
    }

    fn upsert_row(&mut self, row: SyncRow<T>) {
        upsert_row_in(&mut self.rows, row);
    }

    fn remove_row(&mut self, key: &RowKey) {
        remove_row_from(&mut self.rows, key);
    }
}

fn upsert_row_in<T>(rows: &mut Vec<SyncRow<T>>, row: SyncRow<T>) {
    if let Some(existing) = rows.iter_mut().find(|existing| existing.key == row.key) {
        *existing = row;
    } else {
        rows.push(row);
    }
}

fn remove_row_from<T>(rows: &mut Vec<SyncRow<T>>, key: &RowKey) {
    if let Some(index) = rows.iter().position(|row| &row.key == key) {
        rows.remove(index);
    }
}

fn push_failure_message(rejected: usize, conflicted: usize, first_reason: String) -> String {
    if rejected + conflicted == 1 {
        return first_reason;
    }
    format!("{rejected} rejected, {conflicted} conflicted; first: {first_reason}")
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
    fn local_snapshot_marks_rows_stale_until_network_pull() {
        let mut state = CollectionState::<String>::default();
        let row = SyncRow::new("post_1", "cached".to_string()).unwrap();

        assert!(state.apply_local_snapshot(
            vec![row.clone()],
            Some(SyncCursor::new("cursor_1").unwrap()),
            2,
        ));

        assert_eq!(state.rows, vec![row]);
        assert_eq!(state.cursor.unwrap().as_str(), "cursor_1");
        assert!(state.stale);
        assert!(!state.loading);
        assert!(!state.syncing);
        assert_eq!(state.version, 1);
        assert_eq!(state.pending_count, 2);
        assert_eq!(state.last_reason, SyncReason::Initial);
    }

    #[test]
    fn local_snapshot_replays_hydrated_pending_overlay() {
        let mut state = CollectionState::<String>::default();
        let canonical = SyncRow::new("post_1", "cached".to_string())
            .unwrap()
            .version("row_1")
            .unwrap();
        let pending = PendingMutation {
            id: MutationId::new("device_1:1").unwrap(),
            op: SyncOp::Upsert,
            key: Some(RowKey::new("post_1").unwrap()),
            before: None,
            before_rows: Vec::new(),
            optimistic: Some(SyncRow::new("post_1", "queued local".to_string()).unwrap()),
        };

        assert!(state.apply_local_snapshot_with_pending(
            vec![canonical],
            Some(SyncCursor::new("cursor_1").unwrap()),
            vec![pending],
        ));

        assert_eq!(state.rows[0].value, "queued local");
        assert!(state.rows[0].pending);
        assert_eq!(state.pending_count, 1);
        assert_eq!(
            state.base_version(&RowKey::new("post_1").unwrap()).unwrap(),
            RowVersion::new("row_1").unwrap()
        );
    }

    #[test]
    fn local_snapshot_replays_hydrated_pending_delete() {
        let mut state = CollectionState::<String>::default();
        let canonical = SyncRow::new("post_1", "cached".to_string())
            .unwrap()
            .version("row_1")
            .unwrap();
        let pending = PendingMutation {
            id: MutationId::new("device_1:delete").unwrap(),
            op: SyncOp::Delete,
            key: Some(RowKey::new("post_1").unwrap()),
            before: None,
            before_rows: Vec::new(),
            optimistic: None,
        };

        assert!(state.apply_local_snapshot_with_pending(
            vec![canonical],
            Some(SyncCursor::new("cursor_1").unwrap()),
            vec![pending],
        ));

        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 1);
        assert_eq!(
            state.base_version(&RowKey::new("post_1").unwrap()).unwrap(),
            RowVersion::new("row_1").unwrap()
        );

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:delete").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "cannot delete".to_string(),
        });
        state.apply_push(rejected);

        assert_eq!(state.rows[0].value, "cached");
        assert!(!state.rows[0].pending);
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn row_helpers_expose_current_row_and_base_version() {
        let mut state = CollectionState::<String>::default();
        let key = RowKey::new("post_1").unwrap();
        let version = RowVersion::new("row_1").unwrap();
        let row = SyncRow::new("post_1", "cached".to_string())
            .unwrap()
            .row_version(version.clone());

        assert!(state.is_empty());
        state.apply_local_snapshot(vec![row], Some(SyncCursor::new("cursor_1").unwrap()), 0);

        assert!(!state.is_empty());
        assert_eq!(state.row(&key).unwrap().value, "cached");
        assert_eq!(state.row_version(&key), Some(&version));
        assert_eq!(state.base_version(&key), Some(version));

        state.row_mut(&key).unwrap().pending = true;
        assert!(state.row(&key).unwrap().pending);
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
    fn duplicate_pending_mutation_id_replaces_previous_overlay() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "server".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:dup").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "first draft".to_string()).unwrap()),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:dup").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "second draft".to_string()).unwrap()),
        );

        assert_eq!(state.pending_mutations.len(), 1);
        assert_eq!(state.pending_count, 1);
        assert_eq!(state.rows[0].value, "second draft");
        assert!(state.rows[0].pending);

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:dup").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "draft rejected".to_string(),
        });
        state.apply_push(rejected);

        assert_eq!(state.rows[0].value, "server");
        assert!(!state.rows[0].pending);
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn pull_rebases_pending_upsert_over_new_canonical_row() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", "server v1".to_string())
                        .unwrap()
                        .version("row_1")
                        .unwrap(),
                ],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local draft".to_string()).unwrap()),
        );

        let request = state.begin_live_pull(SyncReason::Live);
        state.apply_pull(
            request,
            SyncPullResponse::incremental(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncChange {
                    stream: SyncStreamName::new("posts").unwrap(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: Some(RowKey::new("post_1").unwrap()),
                    op: SyncOp::Upsert,
                    row: Some(
                        SyncRow::new("post_1", "server v2".to_string())
                            .unwrap()
                            .version("row_2")
                            .unwrap(),
                    ),
                    cursor: SyncCursor::new("2").unwrap(),
                }],
                Some(SyncCursor::new("2").unwrap()),
            ),
        );

        assert_eq!(state.rows[0].value, "local draft");
        assert!(state.rows[0].pending);
        assert_eq!(
            state.base_version(&RowKey::new("post_1").unwrap()).unwrap(),
            RowVersion::new("row_2").unwrap()
        );

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "stale draft".to_string(),
        });
        state.apply_push(rejected);

        assert_eq!(state.rows[0].value, "server v2");
        assert!(!state.rows[0].pending);
        assert_eq!(
            state.rows[0].version.as_ref().unwrap(),
            &RowVersion::new("row_2").unwrap()
        );
    }

    #[test]
    fn pull_delete_keeps_pending_upsert_visible_until_rejected() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", "server v1".to_string())
                        .unwrap()
                        .version("row_1")
                        .unwrap(),
                ],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:edit").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local edit".to_string()).unwrap()),
        );

        let request = state.begin_live_pull(SyncReason::Live);
        state.apply_pull(
            request,
            SyncPullResponse::incremental(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncChange {
                    stream: SyncStreamName::new("posts").unwrap(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: Some(RowKey::new("post_1").unwrap()),
                    op: SyncOp::Delete,
                    row: None,
                    cursor: SyncCursor::new("2").unwrap(),
                }],
                Some(SyncCursor::new("2").unwrap()),
            ),
        );

        assert_eq!(state.rows[0].value, "local edit");
        assert!(state.rows[0].pending);
        assert!(
            state
                .base_version(&RowKey::new("post_1").unwrap())
                .is_none()
        );

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:edit").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "row was deleted".to_string(),
        });
        state.apply_push(rejected);

        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn pull_snapshot_rebases_pending_delete_over_updated_canonical_row() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", "server v1".to_string())
                        .unwrap()
                        .version("row_1")
                        .unwrap(),
                ],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:delete").unwrap(),
            SyncOp::Delete,
            Some(RowKey::new("post_1").unwrap()),
            None,
        );

        let request = state.begin_pull(SyncReason::Manual);
        state.apply_pull(
            request,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", "server v2".to_string())
                        .unwrap()
                        .version("row_2")
                        .unwrap(),
                ],
                Some(SyncCursor::new("2").unwrap()),
            ),
        );

        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 1);
        assert_eq!(
            state.base_version(&RowKey::new("post_1").unwrap()).unwrap(),
            RowVersion::new("row_2").unwrap()
        );

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:delete").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "cannot delete".to_string(),
        });
        state.apply_push(rejected);

        assert_eq!(state.rows[0].value, "server v2");
        assert_eq!(
            state.rows[0].version.as_ref().unwrap(),
            &RowVersion::new("row_2").unwrap()
        );
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn pull_reset_keeps_pending_upsert_visible_until_rejected() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "server".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:new").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_2").unwrap()),
            Some(SyncRow::new("post_2", "local new".to_string()).unwrap()),
        );

        let request = state.begin_live_pull(SyncReason::Live);
        state.apply_pull(
            request,
            SyncPullResponse::incremental(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncChange {
                    stream: SyncStreamName::new("posts").unwrap(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: None,
                    op: SyncOp::Reset,
                    row: None,
                    cursor: SyncCursor::new("2").unwrap(),
                }],
                Some(SyncCursor::new("2").unwrap()),
            ),
        );

        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].key.as_str(), "post_2");
        assert_eq!(state.rows[0].value, "local new");
        assert!(state.rows[0].pending);

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:new").unwrap(),
            key: Some(RowKey::new("post_2").unwrap()),
            reason: "server reset won".to_string(),
        });
        state.apply_push(rejected);

        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn accepting_one_stacked_mutation_keeps_later_overlay_rebased() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "server v1".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local first".to_string()).unwrap()),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:2").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local second".to_string()).unwrap()),
        );

        let mut accepted = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        accepted
            .accepted
            .push(MutationId::new("device_1:1").unwrap());
        accepted.rows.push(
            SyncRow::new("post_1", "server accepted first".to_string())
                .unwrap()
                .version("row_2")
                .unwrap(),
        );
        state.apply_push(accepted);

        assert_eq!(state.rows[0].value, "local second");
        assert!(state.rows[0].pending);
        assert_eq!(state.pending_count, 1);

        let mut rejected = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        rejected.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:2").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "second rejected".to_string(),
        });
        state.apply_push(rejected);

        assert_eq!(state.rows[0].value, "server accepted first");
        assert!(!state.rows[0].pending);
        assert_eq!(
            state.rows[0].version.as_ref().unwrap(),
            &RowVersion::new("row_2").unwrap()
        );
    }

    #[test]
    fn conflicted_stacked_mutation_keeps_later_overlay_marked_conflicted() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", "server v1".to_string())
                        .unwrap()
                        .version("row_1")
                        .unwrap(),
                ],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );

        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local first".to_string()).unwrap()),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:2").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local second".to_string()).unwrap()),
        );

        let mut conflicted = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        conflicted.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(
                SyncRow::new("post_1", "server v2".to_string())
                    .unwrap()
                    .version("row_2")
                    .unwrap(),
            ),
            reason: "base version is stale".to_string(),
        });
        state.apply_push(conflicted);

        assert_eq!(state.rows[0].value, "local second");
        assert!(state.rows[0].pending);
        assert!(state.rows[0].conflict);
        assert_eq!(state.pending_count, 1);
        assert_eq!(state.conflict_count, 1);
        assert_eq!(
            state.base_version(&RowKey::new("post_1").unwrap()).unwrap(),
            RowVersion::new("row_2").unwrap()
        );
    }

    #[test]
    fn legacy_rendered_rows_with_pending_without_optimistic_do_not_get_wiped() {
        let mut state = CollectionState::<String>::default();
        let key = RowKey::new("post_1").unwrap();
        let canonical = SyncRow::new("post_1", "server".to_string())
            .unwrap()
            .version("row_1")
            .unwrap();
        state.rows = vec![canonical.clone()];
        state.pending_mutations.push(PendingMutation {
            id: MutationId::new("device_1:existing").unwrap(),
            op: SyncOp::Upsert,
            key: Some(key.clone()),
            before: Some(canonical),
            before_rows: Vec::new(),
            optimistic: None,
        });

        state.apply_optimistic_mutation(
            MutationId::new("device_1:new").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_2").unwrap()),
            Some(SyncRow::new("post_2", "new local".to_string()).unwrap()),
        );

        assert_eq!(state.rows.len(), 2);
        assert_eq!(state.row(&key).unwrap().value, "server");
        assert!(state.row(&key).unwrap().pending);
        assert_eq!(
            state.base_version(&key).unwrap(),
            RowVersion::new("row_1").unwrap()
        );
        assert_eq!(
            state.row(&RowKey::new("post_2").unwrap()).unwrap().value,
            "new local"
        );
    }

    #[test]
    fn accepted_delete_removes_canonical_row_after_overlay() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "server".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:delete").unwrap(),
            SyncOp::Delete,
            Some(RowKey::new("post_1").unwrap()),
            None,
        );

        let mut accepted = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        accepted
            .accepted
            .push(MutationId::new("device_1:delete").unwrap());
        state.apply_push(accepted);

        assert!(state.rows.is_empty());
        assert!(state.row(&RowKey::new("post_1").unwrap()).is_none());
        assert_eq!(state.pending_count, 0);
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

    #[test]
    fn discard_local_drops_pending_overlay_and_clears_conflict() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "server v1".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local v1".to_string()).unwrap()),
        );
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(SyncRow::new("post_1", "server v2".to_string()).unwrap()),
            reason: "base version is stale".to_string(),
        });
        state.apply_push(response);

        assert!(state.discard_local(&RowKey::new("post_1").unwrap()));

        assert_eq!(state.rows[0].value, "server v2");
        assert!(!state.rows[0].pending);
        assert!(!state.rows[0].conflict);
        assert_eq!(state.pending_count, 0);
        assert_eq!(state.conflict_count, 0);
        assert!(state.error.is_empty());
        assert_eq!(state.last_reason, SyncReason::Manual);
    }

    #[test]
    fn discard_local_leaves_other_rows_and_unkeyed_pending_alone() {
        let mut state = CollectionState::<String>::default();
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local 1".to_string()).unwrap()),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:2").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_2").unwrap()),
            Some(SyncRow::new("post_2", "local 2".to_string()).unwrap()),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:3").unwrap(),
            SyncOp::Reset,
            None,
            None,
        );

        assert!(state.discard_local(&RowKey::new("post_1").unwrap()));

        let surviving_keys: Vec<_> = state
            .pending_mutations
            .iter()
            .map(|p| p.key.clone())
            .collect();
        assert_eq!(
            surviving_keys,
            vec![Some(RowKey::new("post_2").unwrap()), None]
        );
    }

    #[test]
    fn discard_local_unknown_key_returns_false() {
        let mut state = CollectionState::<String>::default();
        assert!(!state.discard_local(&RowKey::new("nope").unwrap()));
    }

    #[test]
    fn clear_conflict_keeps_server_row_and_recounts_flags() {
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
        state.apply_push(response);

        assert!(state.clear_conflict(&RowKey::new("post_1").unwrap()));

        assert_eq!(state.rows[0].value, "server");
        assert!(!state.rows[0].conflict);
        assert_eq!(state.conflict_count, 0);
        assert!(state.error.is_empty());
        assert_eq!(state.last_reason, SyncReason::Manual);
    }

    #[test]
    fn clear_conflict_keeps_later_pending_overlay() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "server v1".to_string()).unwrap()],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local first".to_string()).unwrap()),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:2").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local second".to_string()).unwrap()),
        );
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(SyncRow::new("post_1", "server v2".to_string()).unwrap()),
            reason: "base version is stale".to_string(),
        });
        state.apply_push(response);

        assert!(state.clear_conflict(&RowKey::new("post_1").unwrap()));

        assert_eq!(state.rows[0].value, "local second");
        assert!(state.rows[0].pending);
        assert!(!state.rows[0].conflict);
        assert_eq!(state.pending_count, 1);
        assert_eq!(state.conflict_count, 0);
    }

    #[test]
    fn reset_for_schema_invalidation_drops_state_and_invalidates_in_flight_tokens() {
        // Seed a populated state: canonical rows, rendered rows,
        // pending mutations, cursor, counters, last error, and an
        // outstanding pull token. The wipe must drop them all AND
        // invalidate the token so a late `apply_pull` becomes a no-op.
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "v1".to_string()).unwrap()],
                Some(SyncCursor::new("cursor_v1").unwrap()),
            ),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "local".to_string()).unwrap()),
        );
        let stale_token = state.begin_pull(SyncReason::Manual);
        state.set_error("transient");

        assert!(!state.rows.is_empty());
        assert!(!state.pending_mutations.is_empty());
        assert!(state.cursor.is_some());
        assert!(state.pending_count > 0);

        state.reset_for_schema_invalidation();

        // Hard reset observed everywhere.
        assert!(state.rows.is_empty(), "rows not cleared");
        assert!(
            state.canonical_rows.is_empty(),
            "canonical_rows not cleared"
        );
        assert!(
            state.pending_mutations.is_empty(),
            "pending_mutations not cleared"
        );
        assert!(state.cursor.is_none(), "cursor not cleared");
        assert_eq!(state.pending_count, 0);
        assert_eq!(state.conflict_count, 0);
        assert_eq!(state.rejected_count, 0);
        assert!(state.error.is_empty(), "error not cleared");
        assert!(!state.stale);
        assert!(!state.loading);
        assert!(!state.syncing);
        // The stale token from before the reset must NOT apply.
        let response = SyncPullResponse::snapshot(
            SyncStreamName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            vec![SyncRow::new("ghost", "from_stale_token".to_string()).unwrap()],
            Some(SyncCursor::new("ghost").unwrap()),
        );
        let applied = state.apply_pull(stale_token, response);
        assert!(
            !applied,
            "stale token should no-op via is_current after reset"
        );
        assert!(
            state.rows.is_empty(),
            "stale apply_pull must not write rows after reset"
        );
    }

    #[test]
    fn reset_for_schema_invalidation_preserves_version_for_post_reset_pulls() {
        // After the reset, `version` stays at its pre-reset value so
        // `start_open_then_pull` can re-issue `begin_initial` (which
        // requires version == 0 to take the Initial path) OR
        // `begin_pull` if it's incrementing on an already-loaded
        // collection. The reset bumps `request_generation`, not
        // `version`.
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", "v1".to_string()).unwrap()],
                Some(SyncCursor::new("cursor_v1").unwrap()),
            ),
        );
        let version_before_reset = state.version;
        assert!(version_before_reset > 0);

        state.reset_for_schema_invalidation();

        assert_eq!(state.version, version_before_reset);
        // begin_pull post-reset issues a fresh request_generation token.
        let post_reset = state.begin_pull(SyncReason::Manual);
        // Confirm the token is fresh: apply_pull with it works.
        let response = SyncPullResponse::snapshot(
            SyncStreamName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            vec![SyncRow::new("post_1", "v2".to_string()).unwrap()],
            Some(SyncCursor::new("cursor_v2").unwrap()),
        );
        let applied = state.apply_pull(post_reset, response);
        assert!(applied);
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].value, "v2");
    }

    #[test]
    fn clear_conflict_preserves_error_when_rejections_remain() {
        let mut state = CollectionState::<String>::default();
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:reject").unwrap(),
            key: Some(RowKey::new("post_0").unwrap()),
            reason: "first rejection".to_string(),
        });
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:conflict").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(SyncRow::new("post_1", "server".to_string()).unwrap()),
            reason: "base version is stale".to_string(),
        });
        state.apply_push(response);

        assert!(state.clear_conflict(&RowKey::new("post_1").unwrap()));

        assert_eq!(state.conflict_count, 0);
        assert_eq!(state.rejected_count, 1);
        assert_eq!(
            state.error,
            "1 rejected, 1 conflicted; first: first rejection"
        );
    }

    #[test]
    fn conflict_without_server_row_keeps_local_row_visible() {
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
            server_row: None,
            reason: "base version is stale".to_string(),
        });

        assert!(!state.apply_push(response));

        assert_eq!(state.rows[0].value, "local");
        assert!(!state.rows[0].pending);
        assert!(state.rows[0].conflict);
        assert_eq!(state.conflict_count, 1);
    }

    #[test]
    fn multiple_push_failures_report_counts_and_first_reason() {
        let mut state = CollectionState::<String>::default();
        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "first rejection".to_string(),
        });
        response.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:2").unwrap(),
            key: Some(RowKey::new("post_2").unwrap()),
            reason: "second rejection".to_string(),
        });
        response.conflicts.push(SyncConflict {
            mutation_id: MutationId::new("device_1:3").unwrap(),
            key: Some(RowKey::new("post_3").unwrap()),
            server_row: None,
            reason: "conflict".to_string(),
        });

        assert!(!state.apply_push(response));

        assert_eq!(
            state.error,
            "2 rejected, 1 conflicted; first: first rejection"
        );
        assert_eq!(state.rejected_count, 2);
    }

    #[test]
    fn rejected_delete_rolls_back_removed_row() {
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
        state.apply_optimistic_mutation(
            MutationId::new("device_1:delete").unwrap(),
            SyncOp::Delete,
            Some(RowKey::new("post_1").unwrap()),
            None,
        );
        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 1);

        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:delete").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            reason: "cannot delete".to_string(),
        });
        state.apply_push(response);

        assert_eq!(state.rows[0].value, "loaded");
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn rejected_reset_restores_previous_rows() {
        let mut state = CollectionState::<String>::default();
        let initial = state.begin_initial();
        state.apply_pull(
            initial,
            SyncPullResponse::snapshot(
                SyncStreamName::new("posts").unwrap(),
                SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", "one".to_string()).unwrap(),
                    SyncRow::new("post_2", "two".to_string()).unwrap(),
                ],
                Some(SyncCursor::new("1").unwrap()),
            ),
        );
        state.apply_optimistic_mutation(
            MutationId::new("device_1:reset").unwrap(),
            SyncOp::Reset,
            None,
            None,
        );
        assert!(state.rows.is_empty());

        let mut response = SyncPushResponse::new(SyncStreamName::new("posts").unwrap());
        response.rejected.push(SyncRejectedMutation {
            mutation_id: MutationId::new("device_1:reset").unwrap(),
            key: None,
            reason: "cannot reset".to_string(),
        });
        state.apply_push(response);

        assert_eq!(state.rows.len(), 2);
        assert_eq!(state.rows[0].value, "one");
        assert_eq!(state.rows[1].value, "two");
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn push_error_rolls_back_optimistic_mutation() {
        let mut state = CollectionState::<String>::default();
        let mutation_id = MutationId::new("device_1:online").unwrap();
        state.apply_optimistic_mutation(
            mutation_id.clone(),
            SyncOp::Upsert,
            Some(RowKey::new("post_1").unwrap()),
            Some(SyncRow::new("post_1", "draft".to_string()).unwrap()),
        );
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.pending_count, 1);

        assert!(state.apply_push_error(&mutation_id, "network error: offline"));

        assert!(state.rows.is_empty());
        assert_eq!(state.pending_count, 0);
        assert_eq!(state.error, "network error: offline");
        assert_eq!(state.last_reason, SyncReason::Error);
    }
}
