//! `session.search` — full-text search across the current session's recorded
//! events (which include note/summary/checkpoint creations), returning the
//! most-recent matches as bounded, redacted event views.
//!
//! It scans the thread's events (already redacted by the store) and keeps those
//! whose message, tool id, or payload contains the (case-insensitive) query. It
//! is a read-only lookup over the same event stream `session.events` pages.

use std::collections::VecDeque;
use std::sync::Arc;

use agenkitty_core::SessionEvent;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SessionEventFilter, SessionRuntime};
use super::events::{SessionEventKindFilter, SessionEventView, bounded_event_views};

pub const SESSION_SEARCH_TOOL_ID: &str = "session.search";

const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 100;
const MAX_SEARCH_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct SessionSearchTool {
    runtime: Arc<SessionRuntime>,
}

impl SessionSearchTool {
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SessionSearchInput) -> AgenkitResult<SessionSearchOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let query = input.query.trim();
        if query.is_empty() {
            return Err(AgenkitError::validation(
                "session.search: `query` must not be empty",
            ));
        }
        let needle = query.to_lowercase();
        let limit = input
            .limit
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT);
        let kinds = input.kinds.into_iter().map(Into::into).collect();
        // Scan the whole thread (limit 0 = no truncation) and filter to matches;
        // the returned window is bounded by `limit` and output byte size below.
        let events = self
            .runtime
            .store()
            .list_events(
                &context.identity.thread_id,
                SessionEventFilter {
                    kinds,
                    ..Default::default()
                },
            )
            .await?;
        // Count every match but retain only the most-recent `limit` in a bounded
        // ring, so this tool's own footprint is O(limit), not O(matches). The
        // store still materializes the full event Vec above — bounding *that*
        // needs a reverse / recent-N store API (a follow-up); today sessions are
        // bounded by compaction and the output is byte-bounded, so a full scan
        // stays safe in practice.
        let mut match_count = 0usize;
        let mut recent: VecDeque<SessionEvent> = VecDeque::new();
        for event in events {
            if event_matches(&event, &needle) {
                match_count += 1;
                recent.push_back(event);
                if recent.len() > limit {
                    recent.pop_front();
                }
            }
        }
        // `recent` holds the last ≤`limit` matches oldest-first; reverse for the
        // most-recent-first window.
        let mut matches: Vec<SessionEvent> = recent.into_iter().collect();
        matches.reverse();
        let (views, byte_truncated) = bounded_event_views(matches, MAX_SEARCH_OUTPUT_BYTES);
        Ok(SessionSearchOutput {
            thread_id: context.identity.thread_id,
            query: query.to_string(),
            match_count,
            matches: views,
            truncated: byte_truncated || match_count > limit,
        })
    }
}

/// Whether any of an event's searchable text (message, tool id, or JSON
/// payload — all already redacted by the store) contains `needle`.
fn event_matches(event: &SessionEvent, needle: &str) -> bool {
    if event
        .message
        .as_deref()
        .is_some_and(|message| message.to_lowercase().contains(needle))
    {
        return true;
    }
    if event
        .tool
        .as_deref()
        .is_some_and(|tool| tool.to_lowercase().contains(needle))
    {
        return true;
    }
    event
        .payload
        .as_ref()
        .and_then(|payload| serde_json::to_string(payload).ok())
        .is_some_and(|json| json.to_lowercase().contains(needle))
}

impl AiTool for SessionSearchTool {
    const ID: &'static str = SESSION_SEARCH_TOOL_ID;
    type Input = SessionSearchInput;
    type Output = SessionSearchOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SESSION_SEARCH_TOOL_ID,
            "Search the current agent session's events (messages, tool calls, notes, \
             summaries) for a query string, returning the most-recent matches as a \
             bounded, redacted window.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input).await })
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct SessionSearchInput {
    /// The (case-insensitive) text to find across the session's events.
    pub query: String,
    /// Maximum matches to return (default 20, max 100), most recent first.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Restrict the search to these event kinds (default: all).
    #[serde(default)]
    pub kinds: Vec<SessionEventKindFilter>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SessionSearchOutput {
    pub thread_id: String,
    pub query: String,
    /// Total events that matched (before the `limit`/byte window was applied).
    pub match_count: usize,
    pub matches: Vec<SessionEventView>,
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session::common::{
        CurrentSessionContext, InMemorySessionMetadataStore, SessionMetadataStore,
    };
    use agenkitty_core::{SessionEventKind, SessionIdentity, SessionStoreKind};

    fn identity() -> SessionIdentity {
        SessionIdentity {
            thread_id: "thread-1".to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: None,
            turn_id: None,
            principal_key: None,
            tool_ids: vec![SESSION_SEARCH_TOOL_ID.to_string()],
            max_steps_per_turn: 8,
            capture_policy: "full".to_string(),
            transcript_store: SessionStoreKind::InMemory,
            metadata_store: SessionStoreKind::InMemory,
            created_at_ms: 1,
            last_active_at_ms: 1,
            project_id: None,
        }
    }

    async fn seed(store: &impl SessionMetadataStore) {
        store
            .append_event(
                "thread-1",
                SessionEvent::new(SessionEventKind::AssistantText, 1)
                    .with_message("investigating the flaky timeout bug"),
            )
            .await
            .unwrap();
        store
            .append_event(
                "thread-1",
                SessionEvent::new(SessionEventKind::ToolCompleted, 2).with_tool("fs.read"),
            )
            .await
            .unwrap();
        store
            .append_event(
                "thread-1",
                SessionEvent::new(SessionEventKind::AssistantText, 3)
                    .with_message("the TIMEOUT was a race in the reaper"),
            )
            .await
            .unwrap();
    }

    async fn run(runtime: Arc<SessionRuntime>, args: serde_json::Value) -> SessionSearchOutput {
        let injected = runtime
            .inject_context_args(
                &args,
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let input: SessionSearchInput = serde_json::from_value(injected).unwrap();
        SessionSearchTool::new(runtime).run(input).await.unwrap()
    }

    #[tokio::test]
    async fn search_is_case_insensitive_and_most_recent_first() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        seed(store.as_ref()).await;
        let runtime = Arc::new(SessionRuntime::new(store));
        let out = run(runtime, serde_json::json!({ "query": "timeout" })).await;
        assert_eq!(out.match_count, 2);
        // Most-recent match first (seq 3 before seq 1).
        assert_eq!(
            out.matches.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![3, 1]
        );
    }

    #[tokio::test]
    async fn search_matches_tool_ids_and_reports_no_hits() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        seed(store.as_ref()).await;
        let runtime = Arc::new(SessionRuntime::new(store));
        let by_tool = run(runtime.clone(), serde_json::json!({ "query": "fs.read" })).await;
        assert_eq!(by_tool.match_count, 1);
        assert_eq!(by_tool.matches[0].seq, 2);

        let miss = run(runtime, serde_json::json!({ "query": "nonexistent" })).await;
        assert_eq!(miss.match_count, 0);
        assert!(miss.matches.is_empty());
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let injected = runtime
            .inject_context_args(
                &serde_json::json!({ "query": "   " }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let input: SessionSearchInput = serde_json::from_value(injected).unwrap();
        let err = SessionSearchTool::new(runtime)
            .run(input)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tokio::test]
    async fn search_scans_every_event_on_the_durable_backend() {
        // Regression: session.search passes limit=0 ("no cap"); the durable
        // LocalJsonl store must honor that (it once coerced 0 -> 1, so search
        // only saw the first event). Seed 3, match only the last one.
        use crate::tools::session::LocalJsonlSessionMetadataStore;
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        seed(&store).await; // the "timeout" match is at seq 1 and seq 3
        let runtime = Arc::new(SessionRuntime::new(Arc::new(store)));
        let out = run(runtime, serde_json::json!({ "query": "reaper" })).await;
        // "reaper" appears only in the third event — unreachable if the scan
        // stopped at the first.
        assert_eq!(out.match_count, 1);
        assert_eq!(out.matches[0].seq, 3);
    }

    #[tokio::test]
    async fn search_rejects_missing_context() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let err = SessionSearchTool::new(runtime)
            .run(SessionSearchInput {
                query: "x".to_string(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }
}
