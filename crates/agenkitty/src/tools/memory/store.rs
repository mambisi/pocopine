//! `MemoryState` — the shared append-only index — and `InMemoryMemoryStore`.
//!
//! The CRUD logic lives on [`MemoryState`] as non-committing `plan_*` methods
//! that compute a result plus the [`MemoryRecord`]s to persist, paired with
//! [`MemoryState::apply_record`] which commits one record. The in-memory backend
//! applies directly; the local JSONL backend persists first, then applies — so
//! both share one implementation and the durable store never commits a change it
//! failed to write.

use std::collections::HashMap;
use std::sync::Mutex;

use agenkitty_core::looks_like_secret;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use serde::{Deserialize, Serialize};

use super::common::{
    CurrentMemoryContext, DEFAULT_SEARCH_LIMIT, MAX_BODY_BYTES, MAX_REASON_BYTES, MAX_SEARCH_LIMIT,
    MAX_TITLE_BYTES, MemoryCompactionReport, MemoryCompactionRequest, MemoryEntry, MemoryFuture,
    MemoryKind, MemoryPatch, MemoryScope, MemorySearchFilter, MemorySearchHit, MemorySource,
    MemoryStore, MemoryStoreKind, MemoryTombstone, bound_text, current_time_ms, lock_err,
    normalize_tags,
};

/// One durable event in the memory log. Compaction is persisted as one `Append`
/// (the merged entry) followed by N `Tombstone`s, so replay needs no special
/// case.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum MemoryRecord {
    Append { entry: MemoryEntry },
    Update { entry: MemoryEntry },
    Tombstone { tombstone: MemoryTombstone },
}

/// All retained revisions for one id plus an optional tombstone.
#[derive(Clone, Debug, Default)]
struct StoredEntry {
    revisions: Vec<MemoryEntry>,
    tombstone: Option<MemoryTombstone>,
}

impl StoredEntry {
    fn current(&self) -> Option<&MemoryEntry> {
        self.revisions.last()
    }
}

/// The materialized memory index: entries by id plus the id sequence counter.
/// Shared by both backends; the durable backend replays its log into one of
/// these on open.
#[derive(Clone, Debug, Default)]
pub(super) struct MemoryState {
    entries: HashMap<String, StoredEntry>,
    seq: u64,
}

impl MemoryState {
    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn next_id(&self) -> String {
        format!("mem-{}", self.seq.saturating_add(1))
    }

    /// Compute the stored form of a new entry without committing it.
    pub(super) fn plan_append(
        &self,
        entry: MemoryEntry,
    ) -> AgenkitResult<(MemoryEntry, MemoryRecord)> {
        if !entry.id.is_empty() {
            return Err(AgenkitError::validation(
                "memory append requires an empty id; the store assigns it",
            ));
        }
        let now = current_time_ms();
        let mut stored = entry;
        stored.id = self.next_id();
        stored.version = 1;
        stored.created_at_ms = now;
        stored.updated_at_ms = now;
        Ok((stored.clone(), MemoryRecord::Append { entry: stored }))
    }

    /// Compute the next revision of an entry without committing it.
    pub(super) fn plan_update(
        &self,
        id: &str,
        expected_version: u64,
        patch: MemoryPatch,
    ) -> AgenkitResult<(MemoryEntry, MemoryRecord)> {
        if patch.reason.trim().is_empty() {
            return Err(AgenkitError::validation("memory update reason is required"));
        }
        let current = self.require_live(id)?;
        if current.version != expected_version {
            return Err(stale_version(id, expected_version, current.version));
        }
        let mut next = current.clone();
        if let Some(title) = patch.title {
            let title = title.trim().to_string();
            if title.is_empty() {
                return Err(AgenkitError::validation("memory update title is empty"));
            }
            if looks_like_secret(&title) {
                return Err(AgenkitError::tool_policy(
                    "memory update title looks like a secret",
                ));
            }
            next.title = bound_text(&title, MAX_TITLE_BYTES);
        }
        if let Some(body) = patch.body {
            let body = body.trim().to_string();
            if body.is_empty() {
                return Err(AgenkitError::validation("memory update body is empty"));
            }
            if looks_like_secret(&body) {
                return Err(AgenkitError::tool_policy(
                    "memory update body looks like a secret",
                ));
            }
            next.body = bound_text(&body, MAX_BODY_BYTES);
        }
        if let Some(tags) = patch.tags {
            next.tags = normalize_tags(tags);
        }
        if let Some(kind) = patch.kind {
            next.kind = kind;
        }
        if let Some(retention) = patch.retention {
            next.retention = retention;
        }
        if let Some(confidence) = patch.confidence {
            next.confidence = Some(confidence.clamp(0.0, 1.0));
        }
        next.reason = bound_text(patch.reason.trim(), MAX_REASON_BYTES);
        next.version = current.version.saturating_add(1);
        next.updated_at_ms = current_time_ms();
        Ok((next.clone(), MemoryRecord::Update { entry: next }))
    }

    /// Compute a tombstone for an entry without committing it.
    pub(super) fn plan_tombstone(
        &self,
        id: &str,
        expected_version: u64,
        reason: String,
    ) -> AgenkitResult<(MemoryTombstone, MemoryRecord)> {
        if reason.trim().is_empty() {
            return Err(AgenkitError::validation("memory forget reason is required"));
        }
        let current = self.require_live(id)?;
        if current.version != expected_version {
            return Err(stale_version(id, expected_version, current.version));
        }
        let tombstone = MemoryTombstone {
            id: current.id.clone(),
            version: current.version,
            scope: current.scope,
            namespace: current.namespace.clone(),
            reason: bound_text(reason.trim(), MAX_REASON_BYTES),
            tombstoned_at_ms: current_time_ms(),
        };
        Ok((tombstone.clone(), MemoryRecord::Tombstone { tombstone }))
    }

    /// Compute a compaction (one merged entry + source tombstones) without
    /// committing. Validates every source so a bad request commits nothing.
    pub(super) fn plan_compact(
        &self,
        request: MemoryCompactionRequest,
    ) -> AgenkitResult<(MemoryCompactionReport, Vec<MemoryRecord>)> {
        if request.ids.len() < 2 {
            return Err(AgenkitError::validation(
                "memory compaction needs at least two source ids",
            ));
        }
        let kind = request.into_kind.unwrap_or(MemoryKind::TraceSummary);
        let merged_draft = MemoryEntry::draft(
            request.scope,
            request.namespace.clone(),
            kind,
            request.into_title.clone(),
            request.into_body.clone(),
            vec![],
            MemorySource::HostSystem,
            vec![],
            request.reason.clone(),
            super::common::MemoryRetention::Pinned,
            None,
        )?;
        for id in &request.ids {
            let current = self.require_live(id)?;
            if current.scope != request.scope || current.namespace != request.namespace {
                return Err(AgenkitError::validation(format!(
                    "memory entry `{id}` is not in scope {}/{}",
                    request.scope.as_str(),
                    request.namespace
                )));
            }
        }

        let now = current_time_ms();
        let mut merged = merged_draft;
        merged.id = self.next_id();
        merged.version = 1;
        merged.created_at_ms = now;
        merged.updated_at_ms = now;

        let mut records = vec![MemoryRecord::Append {
            entry: merged.clone(),
        }];
        let mut tombstoned = Vec::with_capacity(request.ids.len());
        for id in &request.ids {
            let current = self.require_live(id).expect("validated above");
            let tombstone = MemoryTombstone {
                id: current.id.clone(),
                version: current.version,
                scope: current.scope,
                namespace: current.namespace.clone(),
                reason: bound_text(&format!("compacted into {}", merged.id), MAX_REASON_BYTES),
                tombstoned_at_ms: now,
            };
            records.push(MemoryRecord::Tombstone {
                tombstone: tombstone.clone(),
            });
            tombstoned.push(tombstone);
        }
        Ok((MemoryCompactionReport { merged, tombstoned }, records))
    }

    /// Commit one record. Also used by the durable backend's replay.
    pub(super) fn apply_record(&mut self, record: MemoryRecord) {
        match record {
            MemoryRecord::Append { entry } | MemoryRecord::Update { entry } => {
                self.bump_seq_from_id(&entry.id);
                self.entries
                    .entry(entry.id.clone())
                    .or_default()
                    .revisions
                    .push(entry);
            }
            MemoryRecord::Tombstone { tombstone } => {
                if let Some(slot) = self.entries.get_mut(&tombstone.id) {
                    slot.tombstone = Some(tombstone);
                }
            }
        }
    }

    pub(super) fn read_entry(&self, id: &str, version: Option<u64>) -> Option<MemoryEntry> {
        let slot = self.entries.get(id)?;
        // Tombstoned entries carry no readable body, in any version.
        if slot.tombstone.is_some() {
            return None;
        }
        match version {
            None => slot.current().cloned(),
            Some(target) => slot
                .revisions
                .iter()
                .find(|revision| revision.version == target)
                .cloned(),
        }
    }

    pub(super) fn search_entries(&self, filter: &MemorySearchFilter) -> Vec<MemorySearchHit> {
        let tokens = query_tokens(&filter.query);
        let mut hits: Vec<MemorySearchHit> = Vec::new();
        for slot in self.entries.values() {
            if slot.tombstone.is_some() {
                continue;
            }
            let Some(entry) = slot.current() else {
                continue;
            };
            if !matches_filter(entry, filter) {
                continue;
            }
            let score = score_entry(entry, &tokens);
            // With a non-empty query, only positive matches are returned.
            if !tokens.is_empty() && score <= 0.0 {
                continue;
            }
            hits.push(entry.to_hit(score));
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.updated_at_ms.cmp(&a.updated_at_ms))
                .then(a.id.cmp(&b.id))
        });
        hits.truncate(filter.effective_limit());
        hits
    }

    fn require_live(&self, id: &str) -> AgenkitResult<&MemoryEntry> {
        let slot = self
            .entries
            .get(id)
            .ok_or_else(|| AgenkitError::not_found(format!("memory entry `{id}` not found")))?;
        if slot.tombstone.is_some() {
            return Err(AgenkitError::not_found(format!(
                "memory entry `{id}` was forgotten"
            )));
        }
        slot.current()
            .ok_or_else(|| AgenkitError::internal("memory entry has no revisions"))
    }

    fn bump_seq_from_id(&mut self, id: &str) {
        if let Some(value) = id
            .strip_prefix("mem-")
            .and_then(|rest| rest.parse::<u64>().ok())
        {
            self.seq = self.seq.max(value);
        }
    }
}

fn stale_version(id: &str, expected: u64, found: u64) -> AgenkitError {
    AgenkitError::validation(format!(
        "memory entry `{id}` expected version {expected} but found {found}"
    ))
}

/// Process-local, append-only memory store.
#[derive(Debug, Default)]
pub struct InMemoryMemoryStore {
    inner: Mutex<MemoryState>,
}

impl InMemoryMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.inner
            .lock()
            .map(|state| state.entry_count())
            .unwrap_or_default()
    }
}

impl MemoryStore for InMemoryMemoryStore {
    fn append<'a>(&'a self, entry: MemoryEntry) -> MemoryFuture<'a, MemoryEntry> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (entry, record) = state.plan_append(entry)?;
            state.apply_record(record);
            Ok(entry)
        })
    }

    fn read<'a>(
        &'a self,
        id: &'a str,
        version: Option<u64>,
    ) -> MemoryFuture<'a, Option<MemoryEntry>> {
        Box::pin(async move {
            let state = self.inner.lock().map_err(lock_err)?;
            Ok(state.read_entry(id, version))
        })
    }

    fn search<'a>(&'a self, filter: MemorySearchFilter) -> MemoryFuture<'a, Vec<MemorySearchHit>> {
        Box::pin(async move {
            let state = self.inner.lock().map_err(lock_err)?;
            Ok(state.search_entries(&filter))
        })
    }

    fn update<'a>(
        &'a self,
        id: &'a str,
        expected_version: u64,
        patch: MemoryPatch,
    ) -> MemoryFuture<'a, MemoryEntry> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (entry, record) = state.plan_update(id, expected_version, patch)?;
            state.apply_record(record);
            Ok(entry)
        })
    }

    fn tombstone<'a>(
        &'a self,
        id: &'a str,
        expected_version: u64,
        reason: String,
    ) -> MemoryFuture<'a, MemoryTombstone> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (tombstone, record) = state.plan_tombstone(id, expected_version, reason)?;
            state.apply_record(record);
            Ok(tombstone)
        })
    }

    fn compact<'a>(
        &'a self,
        request: MemoryCompactionRequest,
    ) -> MemoryFuture<'a, MemoryCompactionReport> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (report, records) = state.plan_compact(request)?;
            for record in records {
                state.apply_record(record);
            }
            Ok(report)
        })
    }

    fn kind(&self) -> MemoryStoreKind {
        MemoryStoreKind::InMemory
    }
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn matches_filter(entry: &MemoryEntry, filter: &MemorySearchFilter) -> bool {
    if !filter.scopes.is_empty() && !filter.scopes.contains(&entry.scope) {
        return false;
    }
    if let Some(namespace) = &filter.namespace
        && &entry.namespace != namespace
    {
        return false;
    }
    if !filter.kinds.is_empty() && !filter.kinds.contains(&entry.kind) {
        return false;
    }
    for required in &filter.tags {
        let required = required.to_ascii_lowercase();
        if !entry.tags.iter().any(|tag| tag == &required) {
            return false;
        }
    }
    if let Some(after) = filter.updated_after_ms
        && entry.updated_at_ms < after
    {
        return false;
    }
    true
}

fn score_entry(entry: &MemoryEntry, tokens: &[String]) -> f32 {
    if tokens.is_empty() {
        return 0.0;
    }
    let title = entry.title.to_ascii_lowercase();
    let body = entry.body.to_ascii_lowercase();
    let kind = format!("{:?}", entry.kind).to_ascii_lowercase();
    let source = format!("{:?}", entry.source).to_ascii_lowercase();
    let mut score = 0.0_f32;
    for token in tokens {
        if title.contains(token) {
            score += 3.0;
        }
        if entry.tags.iter().any(|tag| tag.contains(token)) {
            score += 2.0;
        }
        if body.contains(token) {
            score += 1.0;
        }
        if kind == *token || source == *token {
            score += 1.0;
        }
    }
    score
}

/// Derive the store namespace for a write in the given scope, from the caller's
/// resolved context. Returns an error for host-owned scopes the framework does
/// not configure by default.
pub fn namespace_for_write(
    context: &CurrentMemoryContext,
    scope: MemoryScope,
) -> AgenkitResult<String> {
    context.namespace_for(scope).ok_or_else(|| {
        AgenkitError::tool_policy(format!(
            "memory scope {} is host-owned and not configured",
            scope.as_str()
        ))
    })
}

/// Search only the namespaces the caller owns, one query per `(scope,
/// namespace)`, then merge/sort/truncate. A hit can never surface from another
/// owner's namespace. Shared by `memory.search` and `MemoryRetriever`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn search_caller_namespaces(
    store: &dyn MemoryStore,
    context: &CurrentMemoryContext,
    query: &str,
    scopes: &[MemoryScope],
    kinds: &[MemoryKind],
    tags: &[String],
    updated_after_ms: Option<u64>,
    limit: usize,
) -> AgenkitResult<Vec<MemorySearchHit>> {
    let limit = if limit == 0 {
        DEFAULT_SEARCH_LIMIT
    } else {
        limit.min(MAX_SEARCH_LIMIT)
    };
    let mut hits: Vec<MemorySearchHit> = Vec::new();
    for (scope, namespace) in context.accessible() {
        if !scopes.is_empty() && !scopes.contains(&scope) {
            continue;
        }
        let filter = MemorySearchFilter {
            query: query.to_string(),
            scopes: vec![scope],
            namespace: Some(namespace),
            kinds: kinds.to_vec(),
            tags: tags.to_vec(),
            updated_after_ms,
            limit,
        };
        hits.extend(store.search(filter).await?);
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.updated_at_ms.cmp(&a.updated_at_ms))
            .then(a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{MemoryKind, MemoryRetention, MemoryScope, MemorySource};
    use agenkitty_core::SessionSourceRef;

    fn draft(scope: MemoryScope, namespace: &str, title: &str, body: &str) -> MemoryEntry {
        MemoryEntry::draft(
            scope,
            namespace,
            MemoryKind::Fact,
            title,
            body,
            vec!["tag".to_string()],
            MemorySource::Agent,
            vec![SessionSourceRef::Event { seq: 1 }],
            "reason",
            MemoryRetention::Session,
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn append_assigns_id_and_version() {
        let store = InMemoryMemoryStore::new();
        let first = store
            .append(draft(
                MemoryScope::Project,
                "p",
                "yrs decision",
                "we chose yrs",
            ))
            .await
            .unwrap();
        assert_eq!(first.id, "mem-1");
        assert_eq!(first.version, 1);
        assert!(first.created_at_ms > 0);

        let second = store
            .append(draft(MemoryScope::Project, "p", "second", "another"))
            .await
            .unwrap();
        assert_eq!(second.id, "mem-2");
        assert_eq!(store.entry_count(), 2);
    }

    #[tokio::test]
    async fn append_rejects_preset_id() {
        let store = InMemoryMemoryStore::new();
        let mut entry = draft(MemoryScope::Project, "p", "t", "b");
        entry.id = "mem-99".to_string();
        assert!(store.append(entry).await.is_err());
    }

    #[tokio::test]
    async fn update_increments_version_with_optimistic_concurrency() {
        let store = InMemoryMemoryStore::new();
        let entry = store
            .append(draft(MemoryScope::Project, "p", "t", "b"))
            .await
            .unwrap();

        // Stale expected_version is rejected without mutation.
        assert!(
            store
                .update(
                    &entry.id,
                    99,
                    MemoryPatch {
                        reason: "x".into(),
                        ..Default::default()
                    }
                )
                .await
                .is_err()
        );

        let updated = store
            .update(
                &entry.id,
                1,
                MemoryPatch {
                    body: Some("revised body".to_string()),
                    reason: "clarify".to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.body, "revised body");

        // The old revision is still readable by explicit version.
        let v1 = store.read(&entry.id, Some(1)).await.unwrap().unwrap();
        assert_eq!(v1.body, "b");
        let current = store.read(&entry.id, None).await.unwrap().unwrap();
        assert_eq!(current.version, 2);
    }

    #[tokio::test]
    async fn update_requires_reason() {
        let store = InMemoryMemoryStore::new();
        let entry = store
            .append(draft(MemoryScope::Project, "p", "t", "b"))
            .await
            .unwrap();
        assert!(
            store
                .update(&entry.id, 1, MemoryPatch::default())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn tombstone_hides_entry_from_read_and_search() {
        let store = InMemoryMemoryStore::new();
        let entry = store
            .append(draft(
                MemoryScope::Project,
                "p",
                "secret plan",
                "the plan body",
            ))
            .await
            .unwrap();

        let tombstone = store
            .tombstone(&entry.id, 1, "no longer relevant".to_string())
            .await
            .unwrap();
        assert_eq!(tombstone.id, entry.id);
        assert_eq!(tombstone.version, 1);

        assert!(store.read(&entry.id, None).await.unwrap().is_none());
        assert!(store.read(&entry.id, Some(1)).await.unwrap().is_none());
        let hits = store
            .search(MemorySearchFilter {
                query: "plan".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(hits.is_empty());

        // Double-forget and stale version both fail.
        assert!(store.tombstone(&entry.id, 1, "again".into()).await.is_err());
    }

    #[tokio::test]
    async fn search_orders_by_score_then_recency_then_id() {
        let store = InMemoryMemoryStore::new();
        // title match scores higher than body match.
        store
            .append(draft(
                MemoryScope::Project,
                "p",
                "body only",
                "mentions yrs here",
            ))
            .await
            .unwrap();
        store
            .append(draft(
                MemoryScope::Project,
                "p",
                "yrs in title",
                "unrelated body",
            ))
            .await
            .unwrap();

        let hits = store
            .search(MemorySearchFilter {
                query: "yrs".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "yrs in title");
        assert!(hits[0].score > hits[1].score);
        // Snippets are bounded and never carry an unbounded body.
        assert!(
            hits.iter()
                .all(|hit| hit.snippet.len() <= super::super::common::MAX_SNIPPET_BYTES + 4)
        );
    }

    #[tokio::test]
    async fn search_filters_by_scope_namespace_and_tags() {
        let store = InMemoryMemoryStore::new();
        store
            .append(draft(MemoryScope::Project, "p", "alpha", "alpha body"))
            .await
            .unwrap();
        store
            .append(draft(MemoryScope::Session, "s", "alpha", "alpha body"))
            .await
            .unwrap();

        let hits = store
            .search(MemorySearchFilter {
                query: "alpha".to_string(),
                scopes: vec![MemoryScope::Session],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].scope, MemoryScope::Session);

        let by_ns = store
            .search(MemorySearchFilter {
                query: "alpha".to_string(),
                namespace: Some("p".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_ns.len(), 1);
        assert_eq!(by_ns[0].namespace, "p");
    }

    #[tokio::test]
    async fn compact_merges_sources_and_tombstones_them() {
        let store = InMemoryMemoryStore::new();
        let a = store
            .append(draft(MemoryScope::Project, "p", "fact a", "a body"))
            .await
            .unwrap();
        let b = store
            .append(draft(MemoryScope::Project, "p", "fact b", "b body"))
            .await
            .unwrap();

        let report = store
            .compact(MemoryCompactionRequest {
                scope: MemoryScope::Project,
                namespace: "p".to_string(),
                ids: vec![a.id.clone(), b.id.clone()],
                into_title: "merged facts".to_string(),
                into_body: "a and b combined".to_string(),
                reason: "reduce clutter".to_string(),
                into_kind: None,
            })
            .await
            .unwrap();

        assert_eq!(report.merged.kind, MemoryKind::TraceSummary);
        assert_eq!(report.tombstoned.len(), 2);
        assert!(store.read(&a.id, None).await.unwrap().is_none());
        assert!(store.read(&b.id, None).await.unwrap().is_none());
        assert!(store.read(&report.merged.id, None).await.unwrap().is_some());
    }

    #[test]
    fn namespace_for_write_rejects_host_owned_scopes() {
        let context = CurrentMemoryContext {
            project_id: "p".to_string(),
            agent_id: "a".to_string(),
            thread_id: None,
        };
        assert!(namespace_for_write(&context, MemoryScope::User).is_err());
        assert_eq!(
            namespace_for_write(&context, MemoryScope::Project).unwrap(),
            "p"
        );
    }
}
