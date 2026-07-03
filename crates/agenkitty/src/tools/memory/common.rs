//! Shared memory-tool types, the `MemoryStore` trait, bounded-text/secret
//! helpers, and the `MemoryRuntime` context plumbing.
//!
//! Memory stores *semantic* knowledge derived from sessions, tools, user input,
//! and artifacts. It is deliberately separate from
//! `pocopine_agenkit::server::session::SessionStore` (transcript records) and the
//! Agenkitty `session` metadata store (notes/summaries/checkpoints). Memory
//! entries cite session provenance through `agenkitty_core::SessionSourceRef`
//! rather than copying transcript payloads.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use agenkitty_core::SessionSourceRef;
use agenkitty_core::looks_like_secret;
use pocopine_agenkit::server::session::ThreadId;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::tools::session::SessionSourceRefSpec;

/// Host async shape used by every `MemoryStore` method, mirroring
/// `SessionMetadataFuture` so memory and session share one future pattern.
pub type MemoryFuture<'a, T> = Pin<Box<dyn Future<Output = AgenkitResult<T>> + Send + 'a>>;

/// Maximum stored title size in bytes.
pub const MAX_TITLE_BYTES: usize = 256;
/// Maximum stored body size in bytes.
pub const MAX_BODY_BYTES: usize = 8 * 1024;
/// Maximum stored reason size in bytes.
pub const MAX_REASON_BYTES: usize = 1024;
/// Maximum number of tags retained per entry.
pub const MAX_TAGS: usize = 16;
/// Maximum size of a single normalized tag in bytes.
pub const MAX_TAG_BYTES: usize = 48;
/// Maximum size of a search-hit snippet in bytes.
pub const MAX_SNIPPET_BYTES: usize = 320;
/// Hard ceiling on search results returned in a single call.
pub const MAX_SEARCH_LIMIT: usize = 50;
/// Default search limit when the caller does not specify one.
pub const DEFAULT_SEARCH_LIMIT: usize = 20;

/// Storage scope for a memory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// Working memory for a single run/thread. Not the session transcript.
    Session,
    /// Durable memory for a repository/workspace identity.
    Project,
    /// Memory private to one agent or subagent inside a project.
    Agent,
    /// Host-provided cross-project memory. Disabled unless a host configures it.
    User,
    /// Host-provided organization/team memory. Disabled unless configured.
    Team,
}

impl MemoryScope {
    /// Stable serde/display name.
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryScope::Session => "session",
            MemoryScope::Project => "project",
            MemoryScope::Agent => "agent",
            MemoryScope::User => "user",
            MemoryScope::Team => "team",
        }
    }
}

/// Semantic category of a memory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Instruction,
    Fact,
    Decision,
    Procedure,
    Debugging,
    Failure,
    Preference,
    ArtifactRef,
    TraceSummary,
}

/// Origin of a memory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    User,
    Agent,
    Subagent,
    ImportedFile,
    ToolResult,
    HostSystem,
}

/// Retention policy controlling how long an entry is expected to live.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryRetention {
    /// Lives only for the current run/thread.
    #[default]
    Session,
    /// Never expires automatically.
    Pinned,
    /// Considered stale after the given host-clock timestamp (ms).
    StaleAfter { stale_after_ms: u64 },
    /// Hard expiry at the given host-clock timestamp (ms).
    ExpiresAt { expires_at_ms: u64 },
}

/// A typed metadata edge between two memory entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRelationKind {
    DerivedFrom,
    Supersedes,
    Contradicts,
    Supports,
    Mentions,
}

/// A relation edge from this entry to another memory id. A graph backend can
/// project these; the note backends store them as plain metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRelation {
    pub kind: MemoryRelationKind,
    pub target_id: String,
}

/// A single memory entry. `id` is empty and `version` is 0 until a store
/// assigns them on `append`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Stable opaque id (`mem-{seq}`), assigned by the store on append.
    #[serde(default)]
    pub id: String,
    /// Monotonic revision number, assigned/incremented by the store.
    #[serde(default)]
    pub version: u64,
    pub scope: MemoryScope,
    pub namespace: String,
    pub kind: MemoryKind,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source: MemorySource,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRef>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub retention: MemoryRetention,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
    #[serde(default)]
    pub last_read_at_ms: Option<u64>,
    #[serde(default)]
    pub relations: Vec<MemoryRelation>,
    pub reason: String,
}

impl MemoryEntry {
    /// Build a validated, normalized draft entry ready for `append`. The
    /// returned entry has an empty id, version 0, and zero timestamps; the store
    /// assigns those. Rejects empty title/body/reason and secret-like content,
    /// and bounds/normalizes title, body, reason, and tags.
    #[allow(clippy::too_many_arguments)]
    pub fn draft(
        scope: MemoryScope,
        namespace: impl Into<String>,
        kind: MemoryKind,
        title: impl Into<String>,
        body: impl Into<String>,
        tags: Vec<String>,
        source: MemorySource,
        source_refs: Vec<SessionSourceRef>,
        reason: impl Into<String>,
        retention: MemoryRetention,
        confidence: Option<f32>,
    ) -> AgenkitResult<Self> {
        let namespace = namespace.into();
        if namespace.trim().is_empty() {
            return Err(AgenkitError::validation(
                "memory entry namespace is required",
            ));
        }
        let title = require_non_empty("title", title.into())?;
        let body = require_non_empty("body", body.into())?;
        let reason = require_non_empty("reason", reason.into())?;
        reject_secret_like("title", &title)?;
        reject_secret_like("body", &body)?;
        let tags = normalize_tags(tags);
        for tag in &tags {
            reject_secret_like("tag", tag)?;
        }
        Ok(Self {
            id: String::new(),
            version: 0,
            scope,
            namespace,
            kind,
            title: bound_text(&title, MAX_TITLE_BYTES),
            body: bound_text(&body, MAX_BODY_BYTES),
            tags,
            source,
            source_refs,
            confidence: confidence.map(|value| value.clamp(0.0, 1.0)),
            retention,
            created_at_ms: 0,
            updated_at_ms: 0,
            last_read_at_ms: None,
            relations: Vec::new(),
            reason: bound_text(&reason, MAX_REASON_BYTES),
        })
    }

    /// Attach a relation edge to another memory id. Used by host lifecycle
    /// operations (e.g. promotion records `derived_from` the source).
    pub fn with_relation(mut self, kind: MemoryRelationKind, target_id: impl Into<String>) -> Self {
        self.relations.push(MemoryRelation {
            kind,
            target_id: target_id.into(),
        });
        self
    }

    /// Project this entry to a bounded search hit with a snippet body.
    pub fn to_hit(&self, score: f32) -> MemorySearchHit {
        MemorySearchHit {
            id: self.id.clone(),
            version: self.version,
            scope: self.scope,
            namespace: self.namespace.clone(),
            kind: self.kind,
            title: self.title.clone(),
            snippet: bound_text(&self.body, MAX_SNIPPET_BYTES),
            tags: self.tags.clone(),
            score,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

/// A partial revision applied to an existing entry. `None` fields are left
/// unchanged. `reason` is always required for an update.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub kind: Option<MemoryKind>,
    #[serde(default)]
    pub retention: Option<MemoryRetention>,
    #[serde(default)]
    pub confidence: Option<f32>,
    pub reason: String,
}

/// Audit record left behind when an entry is forgotten. Carries no body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTombstone {
    pub id: String,
    /// The version the entry held when it was tombstoned.
    pub version: u64,
    pub scope: MemoryScope,
    pub namespace: String,
    pub reason: String,
    pub tombstoned_at_ms: u64,
}

/// A bounded search result. Carries a snippet, never a full body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemorySearchHit {
    pub id: String,
    pub version: u64,
    pub scope: MemoryScope,
    pub namespace: String,
    pub kind: MemoryKind,
    pub title: String,
    pub snippet: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub score: f32,
    pub updated_at_ms: u64,
}

/// A JsonSchema-friendly projection of a full [`MemoryEntry`] for tool output.
/// `MemoryEntry` itself cannot derive `JsonSchema` because its provenance uses
/// the serde-only `agenkitty_core::SessionSourceRef`; this view carries the
/// schemars-friendly [`SessionSourceRefSpec`] instead.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct MemoryEntryView {
    pub id: String,
    pub version: u64,
    pub scope: MemoryScope,
    pub namespace: String,
    pub kind: MemoryKind,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub source: MemorySource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub retention: MemoryRetention,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<MemoryRelation>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub reason: String,
}

impl From<MemoryEntry> for MemoryEntryView {
    fn from(entry: MemoryEntry) -> Self {
        Self {
            id: entry.id,
            version: entry.version,
            scope: entry.scope,
            namespace: entry.namespace,
            kind: entry.kind,
            title: entry.title,
            body: entry.body,
            tags: entry.tags,
            source: entry.source,
            source_refs: entry.source_refs.into_iter().map(Into::into).collect(),
            confidence: entry.confidence,
            retention: entry.retention,
            relations: entry.relations,
            created_at_ms: entry.created_at_ms,
            updated_at_ms: entry.updated_at_ms,
            reason: entry.reason,
        }
    }
}

/// Filter and bounds for a search. Namespace isolation is enforced at the tool
/// layer by pinning `namespace`; the store treats it as a plain filter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemorySearchFilter {
    /// Free-text query tokens (lowercased, whitespace-split by the store).
    pub query: String,
    /// Allowed scopes. Empty means all scopes.
    pub scopes: Vec<MemoryScope>,
    /// Required namespace. `None` means any namespace.
    pub namespace: Option<String>,
    /// Allowed kinds. Empty means all kinds.
    pub kinds: Vec<MemoryKind>,
    /// Required tags. All listed tags must be present on a matching entry.
    pub tags: Vec<String>,
    /// Only entries updated at or after this host-clock timestamp (ms).
    pub updated_after_ms: Option<u64>,
    /// Maximum hits to return. 0 means [`DEFAULT_SEARCH_LIMIT`].
    pub limit: usize,
}

impl MemorySearchFilter {
    /// Effective limit, clamped to [`MAX_SEARCH_LIMIT`].
    pub fn effective_limit(&self) -> usize {
        if self.limit == 0 {
            DEFAULT_SEARCH_LIMIT
        } else {
            self.limit.min(MAX_SEARCH_LIMIT)
        }
    }
}

/// Request to merge several entries into one smaller summary entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryCompactionRequest {
    pub scope: MemoryScope,
    pub namespace: String,
    /// Entries to merge. Must exist, share scope+namespace, and not be tombstoned.
    pub ids: Vec<String>,
    pub into_title: String,
    pub into_body: String,
    pub reason: String,
    #[serde(default)]
    pub into_kind: Option<MemoryKind>,
}

/// Result of a compaction: the new merged entry plus the tombstoned sources.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryCompactionReport {
    pub merged: MemoryEntry,
    pub tombstoned: Vec<MemoryTombstone>,
}

/// Backend kind for diagnostics/telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStoreKind {
    InMemory,
    LocalJsonl,
}

/// The pluggable durable-context backend. Implementations must be append-only
/// and auditable: updates create revisions and forgets leave tombstones.
pub trait MemoryStore: Send + Sync + 'static {
    /// Persist a new entry. The incoming `entry.id` must be empty; the store
    /// assigns `mem-{seq}`, version 1, and timestamps, and returns the result.
    fn append<'a>(&'a self, entry: MemoryEntry) -> MemoryFuture<'a, MemoryEntry>;

    /// Read one entry. With `version = None` returns the current entry (or
    /// `None` if missing or tombstoned). With `version = Some(v)` returns that
    /// historical revision if it exists and the entry is not tombstoned.
    fn read<'a>(
        &'a self,
        id: &'a str,
        version: Option<u64>,
    ) -> MemoryFuture<'a, Option<MemoryEntry>>;

    /// Search non-tombstoned entries, returning bounded snippet hits ordered by
    /// score desc, updated_at desc, id asc.
    fn search<'a>(&'a self, filter: MemorySearchFilter) -> MemoryFuture<'a, Vec<MemorySearchHit>>;

    /// Apply a revision with optimistic concurrency. Fails if the entry is
    /// missing/tombstoned or `expected_version` does not match the current one.
    fn update<'a>(
        &'a self,
        id: &'a str,
        expected_version: u64,
        patch: MemoryPatch,
    ) -> MemoryFuture<'a, MemoryEntry>;

    /// Tombstone an entry with optimistic concurrency. Leaves an audit record
    /// but no readable body.
    fn tombstone<'a>(
        &'a self,
        id: &'a str,
        expected_version: u64,
        reason: String,
    ) -> MemoryFuture<'a, MemoryTombstone>;

    /// Merge several entries into one summary entry, tombstoning the sources.
    fn compact<'a>(
        &'a self,
        request: MemoryCompactionRequest,
    ) -> MemoryFuture<'a, MemoryCompactionReport>;

    /// Backend kind for diagnostics.
    fn kind(&self) -> MemoryStoreKind {
        MemoryStoreKind::InMemory
    }
}

/// Caller identity resolved from a runtime-injected `context_token`. Namespace
/// isolation is derived from this, not from a hidden global.
#[derive(Clone, Debug)]
pub struct CurrentMemoryContext {
    pub project_id: String,
    pub agent_id: String,
    pub thread_id: Option<String>,
}

impl CurrentMemoryContext {
    /// Namespace for a scope the public framework can derive on its own.
    /// `User`/`Team` are host-owned and return `None`. A scope whose identity is
    /// missing (e.g. no project id on a fresh run) also returns `None`, so the
    /// policy layer reports a clean "host-owned/not configured" error rather than
    /// minting an empty or half-formed namespace.
    pub fn namespace_for(&self, scope: MemoryScope) -> Option<String> {
        let project_id = non_empty(&self.project_id);
        let agent_id = non_empty(&self.agent_id);
        let thread_id = self.thread_id.as_deref().and_then(non_empty);
        match scope {
            MemoryScope::Session => thread_id.or(agent_id),
            MemoryScope::Project => project_id,
            MemoryScope::Agent => Some(format!("{}::{}", project_id?, agent_id?)),
            MemoryScope::User | MemoryScope::Team => None,
        }
    }

    /// Source ref pointing at the active thread, when known.
    pub fn thread_ref(&self) -> Option<SessionSourceRef> {
        self.thread_id
            .clone()
            .map(|thread_id| SessionSourceRef::Thread { thread_id })
    }

    /// The (scope, namespace) pairs this caller may read/write without host
    /// configuration. `User`/`Team` are excluded — they are host-owned.
    pub fn accessible(&self) -> Vec<(MemoryScope, String)> {
        [
            MemoryScope::Session,
            MemoryScope::Project,
            MemoryScope::Agent,
        ]
        .into_iter()
        .filter_map(|scope| {
            self.namespace_for(scope)
                .map(|namespace| (scope, namespace))
        })
        .collect()
    }

    /// Whether an entry in `(scope, namespace)` is visible to this caller.
    /// Mirrors Agenkit's no-existence-oracle behavior: foreign owners look like
    /// missing entries, so the caller cannot probe another namespace by id.
    pub fn can_access(&self, scope: MemoryScope, namespace: &str) -> bool {
        self.namespace_for(scope).as_deref() == Some(namespace)
    }
}

/// Holds the memory store plus the short-lived `context_token` → context map,
/// mirroring `SessionRuntime`. Tools reach the store through `store()`.
#[derive(Clone)]
pub struct MemoryRuntime {
    store: Arc<dyn MemoryStore>,
    contexts: Arc<Mutex<HashMap<String, CurrentMemoryContext>>>,
    token_seq: Arc<AtomicU64>,
}

impl MemoryRuntime {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            contexts: Arc::new(Mutex::new(HashMap::new())),
            token_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// In-memory backend for tests and session-only harnesses.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(super::store::InMemoryMemoryStore::new()))
    }

    pub fn store(&self) -> &Arc<dyn MemoryStore> {
        &self.store
    }

    /// Inject a fresh `context_token` into the tool arguments so the tool can
    /// recover the caller's `CurrentMemoryContext`.
    pub fn inject_context_args(
        &self,
        args: &Value,
        context: CurrentMemoryContext,
    ) -> Result<Value, String> {
        let mut object = match args {
            Value::Null => Map::new(),
            Value::Object(object) => object.clone(),
            _ => return Err("memory tool arguments must be a JSON object".to_string()),
        };
        let token = self.issue_context_token(context)?;
        object.insert("context_token".to_string(), Value::String(token));
        Ok(Value::Object(object))
    }

    /// Consume the context behind a token. A token is single-use.
    pub fn take_context(&self, token: &str) -> AgenkitResult<CurrentMemoryContext> {
        let mut contexts = self.contexts.lock().map_err(lock_err)?;
        contexts.remove(token).ok_or_else(|| {
            AgenkitError::validation("memory tool called without an active memory context")
        })
    }

    fn issue_context_token(&self, context: CurrentMemoryContext) -> Result<String, String> {
        let random = ThreadId::mint().map_err(|err| err.to_string())?;
        let seq = self.token_seq.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{seq}", random.as_str());
        let mut contexts = self.contexts.lock().map_err(|err| err.to_string())?;
        contexts.insert(token.clone(), context);
        Ok(token)
    }

    #[cfg(test)]
    fn context_count(&self) -> usize {
        self.contexts
            .lock()
            .map(|contexts| contexts.len())
            .unwrap_or_default()
    }
}

/// Current host wall-clock time in milliseconds.
pub fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Truncate `text` to at most `max_bytes`, never splitting a UTF-8 boundary,
/// appending an ellipsis when truncated.
pub fn bound_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Normalize tags: trim, lowercase, collapse internal whitespace to `-`, drop
/// empties, bound per-tag length, dedupe, and cap the count.
pub fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for tag in tags {
        let collapsed = tag
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .to_ascii_lowercase();
        if collapsed.is_empty() {
            continue;
        }
        let bounded = truncate_no_ellipsis(&collapsed, MAX_TAG_BYTES);
        if !normalized.iter().any(|existing| existing == &bounded) {
            normalized.push(bounded);
        }
        if normalized.len() >= MAX_TAGS {
            break;
        }
    }
    normalized
}

fn truncate_no_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// `Some(value)` when `value` is not blank, else `None`.
fn non_empty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn require_non_empty(field: &str, value: String) -> AgenkitResult<String> {
    if value.trim().is_empty() {
        return Err(AgenkitError::validation(format!(
            "memory entry {field} is required"
        )));
    }
    Ok(value)
}

fn reject_secret_like(field: &str, value: &str) -> AgenkitResult<()> {
    // Memory *rejects* secret-like content (rather than redacting): secrets
    // must never land in a durable agent store. The classifier is the shared
    // one in `agenkitty_core` (F3) — one predicate across memory / artifacts /
    // the session redactor.
    if looks_like_secret(value) {
        return Err(AgenkitError::tool_policy(format!(
            "memory entry {field} looks like a secret; secrets must not be written to memory"
        )));
    }
    Ok(())
}

pub(super) fn lock_err<T>(_: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("memory store lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> MemoryEntry {
        MemoryEntry::draft(
            MemoryScope::Project,
            "proj-1",
            MemoryKind::Decision,
            "Use yrs for collab",
            "We chose yrs over a hand-rolled CRDT.",
            vec![
                "Collab".to_string(),
                "  collab  ".to_string(),
                "CRDT".to_string(),
            ],
            MemorySource::Agent,
            vec![SessionSourceRef::Event { seq: 7 }],
            "Records an architecture decision.",
            MemoryRetention::Pinned,
            Some(0.9),
        )
        .unwrap()
    }

    #[test]
    fn draft_rejects_empty_title_and_body() {
        assert!(
            MemoryEntry::draft(
                MemoryScope::Session,
                "ns",
                MemoryKind::Fact,
                "   ",
                "body",
                vec![],
                MemorySource::Agent,
                vec![],
                "reason",
                MemoryRetention::Session,
                None,
            )
            .is_err()
        );
        assert!(
            MemoryEntry::draft(
                MemoryScope::Session,
                "ns",
                MemoryKind::Fact,
                "title",
                "",
                vec![],
                MemorySource::Agent,
                vec![],
                "reason",
                MemoryRetention::Session,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn draft_requires_reason_and_namespace() {
        assert!(
            MemoryEntry::draft(
                MemoryScope::Session,
                "  ",
                MemoryKind::Fact,
                "title",
                "body",
                vec![],
                MemorySource::Agent,
                vec![],
                "reason",
                MemoryRetention::Session,
                None,
            )
            .is_err()
        );
        assert!(
            MemoryEntry::draft(
                MemoryScope::Session,
                "ns",
                MemoryKind::Fact,
                "title",
                "body",
                vec![],
                MemorySource::Agent,
                vec![],
                "   ",
                MemoryRetention::Session,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn draft_normalizes_and_dedupes_tags() {
        let entry = draft();
        assert_eq!(entry.tags, vec!["collab".to_string(), "crdt".to_string()]);
        assert_eq!(entry.id, "");
        assert_eq!(entry.version, 0);
        assert_eq!(entry.confidence, Some(0.9));
    }

    #[test]
    fn draft_rejects_secret_like_bodies() {
        let err = MemoryEntry::draft(
            MemoryScope::Project,
            "ns",
            MemoryKind::Fact,
            "creds",
            "api_key = sk-livetoken-0001",
            vec![],
            MemorySource::Agent,
            vec![],
            "should fail",
            MemoryRetention::Session,
            None,
        );
        assert!(err.is_err());

        let bearer = MemoryEntry::draft(
            MemoryScope::Project,
            "ns",
            MemoryKind::Fact,
            "auth header",
            "Authorization: Bearer abc.def.ghi",
            vec![],
            MemorySource::Agent,
            vec![],
            "should fail",
            MemoryRetention::Session,
            None,
        );
        assert!(bearer.is_err());
    }

    #[test]
    fn bound_text_respects_utf8_boundaries() {
        let text = "héllo wörld with multibyte ☃ characters";
        let bounded = bound_text(text, 6);
        assert!(bounded.len() <= 6 + "…".len());
        assert!(bounded.ends_with('…'));
        // No panic, and the prefix is valid UTF-8 by construction.
        assert!(bounded.starts_with("hé"));
    }

    #[test]
    fn scope_kind_source_serde_names_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&MemoryScope::Project).unwrap(),
            "\"project\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryScope::Team).unwrap(),
            "\"team\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryKind::TraceSummary).unwrap(),
            "\"trace_summary\""
        );
        assert_eq!(
            serde_json::to_string(&MemorySource::ImportedFile).unwrap(),
            "\"imported_file\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryRetention::StaleAfter { stale_after_ms: 5 }).unwrap(),
            "{\"kind\":\"stale_after\",\"stale_after_ms\":5}"
        );
    }

    #[test]
    fn runtime_context_token_round_trips_once() {
        let runtime = MemoryRuntime::in_memory();
        let context = CurrentMemoryContext {
            project_id: "proj-1".to_string(),
            agent_id: "agent-1".to_string(),
            thread_id: Some("thread-1".to_string()),
        };
        let args = runtime
            .inject_context_args(&serde_json::json!({"query": "yrs"}), context)
            .unwrap();
        let token = args
            .get("context_token")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_eq!(runtime.context_count(), 1);

        let resolved = runtime.take_context(&token).unwrap();
        assert_eq!(resolved.project_id, "proj-1");
        assert_eq!(
            resolved.namespace_for(MemoryScope::Agent).as_deref(),
            Some("proj-1::agent-1")
        );
        assert_eq!(runtime.context_count(), 0);
        // A token is single-use.
        assert!(runtime.take_context(&token).is_err());
    }

    #[test]
    fn namespace_for_user_team_is_host_owned() {
        let context = CurrentMemoryContext {
            project_id: "p".to_string(),
            agent_id: "a".to_string(),
            thread_id: None,
        };
        assert_eq!(context.namespace_for(MemoryScope::User), None);
        assert_eq!(context.namespace_for(MemoryScope::Team), None);
        assert_eq!(
            context.namespace_for(MemoryScope::Session).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn namespace_for_missing_project_is_unavailable() {
        // A fresh run with no project id: project/agent scopes are unavailable
        // (a clean policy error), but session memory still works off the thread.
        let context = CurrentMemoryContext {
            project_id: String::new(),
            agent_id: "a".to_string(),
            thread_id: Some("t".to_string()),
        };
        assert_eq!(context.namespace_for(MemoryScope::Project), None);
        assert_eq!(context.namespace_for(MemoryScope::Agent), None);
        assert_eq!(
            context.namespace_for(MemoryScope::Session).as_deref(),
            Some("t")
        );
    }
}
