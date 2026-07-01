//! Host-facing memory lifecycle API.
//!
//! These operations are for the harness/host, not the model: promote an entry to
//! a wider scope, write a compaction `trace_summary`, and build a byte-budgeted
//! bootstrap index for startup context. The model uses the `memory.*` tools
//! (`crate::tools::memory`) instead. Both sit on the same `MemoryStore`.

use std::sync::Arc;

use agenkitty_core::SessionSourceRef;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

use crate::tools::memory::{
    MemoryEntry, MemoryEntryView, MemoryKind, MemoryRelationKind, MemoryRetention, MemoryScope,
    MemorySearchFilter, MemorySource, MemoryStore,
};

/// Host-facing lifecycle API over a [`MemoryStore`].
#[derive(Clone)]
pub struct MemoryHost {
    store: Arc<dyn MemoryStore>,
}

impl MemoryHost {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<dyn MemoryStore> {
        &self.store
    }

    /// Promote an entry into a wider scope/namespace, copying its content and
    /// recording a `derived_from` relation back to the source. The source is
    /// left in place (forget it separately if the host wants a move, not a copy).
    pub async fn promote(
        &self,
        source_id: &str,
        to_scope: MemoryScope,
        to_namespace: impl Into<String>,
        reason: impl Into<String>,
    ) -> AgenkitResult<MemoryEntryView> {
        let source = self.store.read(source_id, None).await?.ok_or_else(|| {
            AgenkitError::not_found(format!("memory entry `{source_id}` not found"))
        })?;
        let entry = MemoryEntry::draft(
            to_scope,
            to_namespace,
            source.kind,
            source.title,
            source.body,
            source.tags,
            source.source,
            source.source_refs,
            reason,
            source.retention,
            source.confidence,
        )?
        .with_relation(MemoryRelationKind::DerivedFrom, source.id);
        let stored = self.store.append(entry).await?;
        Ok(stored.into())
    }

    /// Write a `trace_summary` entry for session compaction, citing the folded
    /// session records via `source_refs`. Lets the agent loop keep a durable note
    /// of what was compressed out of the active context.
    pub async fn write_trace_summary(
        &self,
        scope: MemoryScope,
        namespace: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        source_refs: Vec<SessionSourceRef>,
        reason: impl Into<String>,
    ) -> AgenkitResult<MemoryEntryView> {
        let entry = MemoryEntry::draft(
            scope,
            namespace,
            MemoryKind::TraceSummary,
            title,
            body,
            vec![],
            MemorySource::HostSystem,
            source_refs,
            reason,
            MemoryRetention::Pinned,
            None,
        )?;
        let stored = self.store.append(entry).await?;
        Ok(stored.into())
    }

    /// Build a compact, byte-budgeted index of a scope/namespace for startup
    /// context: one `- <id> [<kind>] <title>` line per entry, newest first,
    /// truncated to whole lines within `byte_budget`. This is the always-on
    /// memory hint; detailed entries are fetched on demand with `memory.read`.
    pub async fn bootstrap_index(
        &self,
        scope: MemoryScope,
        namespace: impl Into<String>,
        byte_budget: usize,
    ) -> AgenkitResult<String> {
        let hits = self
            .store
            .search(MemorySearchFilter {
                scopes: vec![scope],
                namespace: Some(namespace.into()),
                ..Default::default()
            })
            .await?;

        let mut index = String::new();
        for hit in hits {
            let line = format!("- {} [{}] {}\n", hit.id, kind_label(hit.kind), hit.title);
            if index.len() + line.len() > byte_budget {
                break;
            }
            index.push_str(&line);
        }
        Ok(index)
    }
}

fn kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Instruction => "instruction",
        MemoryKind::Fact => "fact",
        MemoryKind::Decision => "decision",
        MemoryKind::Procedure => "procedure",
        MemoryKind::Debugging => "debugging",
        MemoryKind::Failure => "failure",
        MemoryKind::Preference => "preference",
        MemoryKind::ArtifactRef => "artifact_ref",
        MemoryKind::TraceSummary => "trace_summary",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::{InMemoryMemoryStore, MemoryKind, MemoryRelationKind};

    async fn seed(
        store: &Arc<dyn MemoryStore>,
        scope: MemoryScope,
        namespace: &str,
        kind: MemoryKind,
        title: &str,
    ) -> String {
        let entry = MemoryEntry::draft(
            scope,
            namespace,
            kind,
            title,
            "body",
            vec![],
            MemorySource::Agent,
            vec![],
            "reason",
            MemoryRetention::Session,
            None,
        )
        .unwrap();
        store.append(entry).await.unwrap().id
    }

    fn host() -> (MemoryHost, Arc<dyn MemoryStore>) {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        (MemoryHost::new(store.clone()), store)
    }

    #[tokio::test]
    async fn promote_copies_and_records_derived_from() {
        let (host, store) = host();
        let source = seed(
            &store,
            MemoryScope::Session,
            "thread-1",
            MemoryKind::Decision,
            "use yrs",
        )
        .await;

        let promoted = host
            .promote(
                &source,
                MemoryScope::Project,
                "proj",
                "stable enough to keep",
            )
            .await
            .unwrap();
        assert_eq!(promoted.scope, MemoryScope::Project);
        assert_eq!(promoted.namespace, "proj");
        assert_eq!(promoted.title, "use yrs");
        assert_eq!(promoted.relations.len(), 1);
        assert_eq!(promoted.relations[0].kind, MemoryRelationKind::DerivedFrom);
        assert_eq!(promoted.relations[0].target_id, source);
        // The source is left in place.
        assert!(store.read(&source, None).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn promote_unknown_id_is_not_found() {
        let (host, _store) = host();
        let err = host
            .promote("mem-999", MemoryScope::Project, "proj", "r")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[tokio::test]
    async fn write_trace_summary_preserves_source_refs() {
        let (host, _store) = host();
        let summary = host
            .write_trace_summary(
                MemoryScope::Session,
                "thread-1",
                "folded turns 1-9",
                "the agent set up the project and ran the tests",
                vec![SessionSourceRef::RecordRange {
                    thread_id: "thread-1".to_string(),
                    start_seq: 1,
                    end_seq: 9,
                }],
                "compaction",
            )
            .await
            .unwrap();
        assert_eq!(summary.kind, MemoryKind::TraceSummary);
        assert_eq!(summary.source_refs.len(), 1);
    }

    #[tokio::test]
    async fn bootstrap_index_respects_byte_budget() {
        let (host, store) = host();
        for i in 0..10 {
            seed(
                &store,
                MemoryScope::Project,
                "proj",
                MemoryKind::Fact,
                &format!("fact number {i}"),
            )
            .await;
        }
        let full = host
            .bootstrap_index(MemoryScope::Project, "proj", 10_000)
            .await
            .unwrap();
        assert_eq!(full.lines().count(), 10);

        let budgeted = host
            .bootstrap_index(MemoryScope::Project, "proj", 60)
            .await
            .unwrap();
        assert!(budgeted.len() <= 60);
        // Only whole lines, and at least one fit.
        assert!(budgeted.ends_with('\n'));
        assert!(budgeted.lines().count() < 10);

        // A foreign namespace yields nothing.
        let other = host
            .bootstrap_index(MemoryScope::Project, "other", 10_000)
            .await
            .unwrap();
        assert!(other.is_empty());
    }
}
