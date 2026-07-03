//! Shared artifact-tool types, the `ArtifactStore` trait, name/content
//! validation, and the `ArtifactRuntime` context plumbing.
//!
//! Artifacts are **durable run outputs** — reports, logs, build products,
//! command outputs — separated from arbitrary workspace edits (`fs.*` /
//! `patch.*`) and from semantic memory (`memory.*`). Each artifact carries a
//! stable citable id, name, media type, size, content hash, provenance
//! (`SessionSourceRef`s), and a scope. Contents are stored out-of-band by the
//! backend and read back through bounded windows.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use agenkitty_core::SessionSourceRef;
use pocopine_agenkit::server::session::ThreadId;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::policy::{ApprovalDecision, ApprovalRequest, ToolApprover, no_approver_reason};

pub use crate::tools::memory::current_time_ms;

/// Host async shape used by every `ArtifactStore` method, mirroring
/// `MemoryFuture` so the tool families share one future pattern.
pub type ArtifactFuture<'a, T> = Pin<Box<dyn Future<Output = AgenkitResult<T>> + Send + 'a>>;

/// Maximum artifact name size in bytes.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum stored content size in bytes per artifact.
pub const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum bytes returned by one `artifact.read` window.
pub const MAX_READ_WINDOW_BYTES: usize = 64 * 1024;
/// Maximum artifacts returned by one `artifact.list` call.
pub const MAX_LIST_LIMIT: usize = 100;
/// Maximum media-type string size in bytes.
pub const MAX_MEDIA_TYPE_BYTES: usize = 128;

/// Where an artifact lives and how long it outlasts the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactScope {
    /// Owned by the current session; reaped with session cleanup.
    Session,
    /// Owned by the project; survives across sessions. Writes require host
    /// approval (the plan's `Ask` default).
    Project,
}

impl ArtifactScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Project => "project",
        }
    }
}

/// How `artifact.write` content is encoded on the wire. Text is primary;
/// binary rides as base64 (decoded through `pocopine-codec` at the tool edge).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactEncoding {
    #[default]
    Utf8,
    Base64,
}

/// The metadata record every backend stores beside the out-of-band contents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    /// Stable citable id (`art-{seq}`).
    pub id: String,
    /// Validated artifact name (no path traversal; display + dedupe key).
    pub name: String,
    /// Declared media type (defaults to `text/plain`).
    pub media_type: String,
    /// Content size in bytes (the linked file's size for references).
    pub size: u64,
    /// Lowercase hex SHA-256 of the contents (via `pocopine-crypto`).
    pub sha256: String,
    pub scope: ArtifactScope,
    /// Caller-derived isolation namespace — never taken from the model.
    pub namespace: String,
    /// Provenance: which session/tool activity produced this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRef>,
    /// For `artifact.link`: the workspace-relative path this artifact
    /// references instead of owning stored bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_path: Option<String>,
    pub created_at_ms: u64,
    /// Tombstone flag: a deleted artifact keeps its audit row, never contents.
    #[serde(default)]
    pub deleted: bool,
}

/// A bounded read window of an artifact's contents.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactContentWindow {
    /// Byte offset this window starts at.
    pub offset: u64,
    /// The window's content: UTF-8 text when the bytes are valid UTF-8 at the
    /// window boundaries, otherwise base64.
    pub content: String,
    pub encoding: ArtifactEncoding,
    /// Whether more bytes remain past this window.
    pub truncated: bool,
}

/// A draft passed to [`ArtifactStore::write`] — everything but the
/// store-assigned id.
#[derive(Clone, Debug)]
pub struct ArtifactDraft {
    pub name: String,
    pub media_type: String,
    pub scope: ArtifactScope,
    pub namespace: String,
    pub source_refs: Vec<SessionSourceRef>,
    /// `Some(path)` for `artifact.link` references; `None` for owned bytes.
    pub link_path: Option<String>,
    pub created_at_ms: u64,
}

/// Backend contract for artifact storage. Contents are stored out-of-band by
/// the backend and addressed through the metadata's id; namespaces isolate
/// callers (a foreign artifact looks like a missing one — no existence
/// oracle).
pub trait ArtifactStore: Send + Sync {
    /// Store an artifact's contents + metadata; assigns the stable id. For a
    /// link draft (`link_path` set) the backend reads the linked workspace
    /// file itself to derive size/hash and ignores `contents` — a link never
    /// owns stored bytes.
    fn write<'a>(
        &'a self,
        draft: ArtifactDraft,
        contents: Vec<u8>,
    ) -> ArtifactFuture<'a, ArtifactMetadata>;

    /// Metadata for one artifact within the caller's namespaces.
    fn stat<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
    ) -> ArtifactFuture<'a, ArtifactMetadata>;

    /// A bounded window of an artifact's contents (`link` references read the
    /// linked file through the backend).
    fn read<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
        offset: u64,
        max_bytes: usize,
    ) -> ArtifactFuture<'a, (ArtifactMetadata, ArtifactContentWindow)>;

    /// Non-deleted artifacts in the caller's namespaces, newest first.
    fn list<'a>(
        &'a self,
        accessible: &'a [(ArtifactScope, String)],
        scope: Option<ArtifactScope>,
        limit: usize,
    ) -> ArtifactFuture<'a, Vec<ArtifactMetadata>>;

    /// Tombstone an artifact: metadata survives (audit), contents are removed.
    fn delete<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
    ) -> ArtifactFuture<'a, ArtifactMetadata>;

    /// Backend kind for diagnostics.
    fn kind(&self) -> ArtifactStoreKind {
        ArtifactStoreKind::InMemory
    }
}

/// Which backend an [`ArtifactRuntime`] is using.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStoreKind {
    InMemory,
    LocalFs,
}

/// Caller identity resolved from a runtime-injected `context_token`.
/// Namespace isolation is derived from this, never from the model.
#[derive(Clone, Debug)]
pub struct CurrentArtifactContext {
    pub project_id: String,
    pub thread_id: Option<String>,
}

impl CurrentArtifactContext {
    /// Namespace for a scope. A scope whose identity is missing returns
    /// `None`, so the tool reports a clean "not configured" error rather than
    /// minting a half-formed namespace.
    pub fn namespace_for(&self, scope: ArtifactScope) -> Option<String> {
        match scope {
            ArtifactScope::Session => self
                .thread_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .map(str::to_string),
            ArtifactScope::Project => {
                (!self.project_id.trim().is_empty()).then(|| self.project_id.clone())
            }
        }
    }

    /// A provenance ref pointing at the active session thread, when known — the
    /// artifact side of the session↔artifact round-trip. Prepended to an
    /// artifact's `source_refs` so the artifact records which session produced
    /// it (the session side is the metadata store's `link_artifact`).
    pub fn thread_ref(&self) -> Option<SessionSourceRef> {
        self.thread_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .map(|id| SessionSourceRef::Thread {
                thread_id: id.to_string(),
            })
    }

    /// The (scope, namespace) pairs this caller may access.
    pub fn accessible(&self) -> Vec<(ArtifactScope, String)> {
        [ArtifactScope::Session, ArtifactScope::Project]
            .into_iter()
            .filter_map(|scope| {
                self.namespace_for(scope)
                    .map(|namespace| (scope, namespace))
            })
            .collect()
    }
}

/// Holds the artifact store, the short-lived `context_token` → context map
/// (mirroring `MemoryRuntime`), and the optional host approver consulted for
/// project-scoped writes. The approver slot is interior-mutable because the
/// runner installs its approver *after* the tools were registered against
/// this (shared) runtime.
#[derive(Clone)]
pub struct ArtifactRuntime {
    store: Arc<dyn ArtifactStore>,
    approver: Arc<Mutex<Option<Arc<dyn ToolApprover>>>>,
    contexts: Arc<Mutex<HashMap<String, CurrentArtifactContext>>>,
    token_seq: Arc<AtomicU64>,
}

impl ArtifactRuntime {
    pub fn new(store: Arc<dyn ArtifactStore>) -> Self {
        Self {
            store,
            approver: Arc::new(Mutex::new(None)),
            contexts: Arc::new(Mutex::new(HashMap::new())),
            token_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// In-memory backend for tests and session-only harnesses.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(super::store::InMemoryArtifactStore::new()))
    }

    pub fn store(&self) -> &Arc<dyn ArtifactStore> {
        &self.store
    }

    /// Install the host approver consulted for project-scoped writes/links
    /// (the plan's `Ask` default). Without one they fail closed. Settable
    /// after registration — the registered tools share this runtime.
    pub fn set_approver(&self, approver: Arc<dyn ToolApprover>) {
        if let Ok(mut slot) = self.approver.lock() {
            *slot = Some(approver);
        }
    }

    fn approver(&self) -> Option<Arc<dyn ToolApprover>> {
        self.approver.lock().ok().and_then(|slot| slot.clone())
    }

    /// Resolve the plan's scope policy for a mutation: session-scope writes
    /// are allowed; project-scope writes consult the host approver and fail
    /// closed without one. `detail` is what the operator judges (bounded +
    /// redacted by the approver's renderer).
    pub async fn authorize_scope_write(
        &self,
        tool_id: &str,
        scope: ArtifactScope,
        detail: Value,
    ) -> AgenkitResult<()> {
        if scope == ArtifactScope::Session {
            return Ok(());
        }
        let reason = format!("{tool_id} to project scope requires approval");
        let Some(approver) = self.approver() else {
            return Err(AgenkitError::tool_policy(no_approver_reason(&reason)));
        };
        let request = ApprovalRequest::new(tool_id, reason).with_detail(detail);
        match approver.approve(request).await {
            ApprovalDecision::Approved => Ok(()),
            ApprovalDecision::Denied { reason } => Err(AgenkitError::tool_policy(format!(
                "{tool_id} denied: {reason}"
            ))),
        }
    }

    /// Inject a fresh `context_token` into the tool arguments so the tool can
    /// recover the caller's `CurrentArtifactContext`.
    pub fn inject_context_args(
        &self,
        args: &Value,
        context: CurrentArtifactContext,
    ) -> Result<Value, String> {
        let mut object = match args {
            Value::Null => Map::new(),
            Value::Object(object) => object.clone(),
            _ => return Err("artifact tool arguments must be a JSON object".to_string()),
        };
        let token = self.issue_context_token(context)?;
        object.insert("context_token".to_string(), Value::String(token));
        Ok(Value::Object(object))
    }

    /// Consume the context behind a token. A token is single-use.
    pub fn take_context(&self, token: &str) -> AgenkitResult<CurrentArtifactContext> {
        let mut contexts = self.contexts.lock().map_err(lock_err)?;
        contexts.remove(token).ok_or_else(|| {
            AgenkitError::validation("artifact tool called without an active artifact context")
        })
    }

    fn issue_context_token(&self, context: CurrentArtifactContext) -> Result<String, String> {
        let random = ThreadId::mint().map_err(|err| err.to_string())?;
        let seq = self.token_seq.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{seq}", random.as_str());
        let mut contexts = self.contexts.lock().map_err(|err| err.to_string())?;
        contexts.insert(token.clone(), context);
        Ok(token)
    }
}

fn lock_err<T>(_err: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("artifact runtime lock poisoned")
}

/// Validate an artifact name: non-empty, bounded, a single path component
/// (no separators, no traversal), no control characters.
pub fn validate_artifact_name(name: &str) -> AgenkitResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AgenkitError::validation("artifact name is required"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(AgenkitError::validation(format!(
            "artifact name exceeds {MAX_NAME_BYTES} bytes"
        )));
    }
    if name == "." || name == ".." {
        return Err(AgenkitError::validation("artifact name traverses paths"));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(AgenkitError::validation(
            "artifact name must be a single path component",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(AgenkitError::validation(
            "artifact name contains control characters",
        ));
    }
    Ok(name.to_string())
}

/// Validate a declared media type: bounded, `type/subtype` shaped, printable.
pub fn validate_media_type(media_type: Option<String>) -> AgenkitResult<String> {
    let media_type = media_type.unwrap_or_else(|| "text/plain".to_string());
    let media_type = media_type.trim().to_ascii_lowercase();
    if media_type.len() > MAX_MEDIA_TYPE_BYTES
        || !media_type.split_once('/').is_some_and(|(t, s)| {
            !t.is_empty()
                && !s.is_empty()
                && media_type
                    .chars()
                    .all(|c| c.is_ascii_graphic() && c != '\\')
        })
    {
        return Err(AgenkitError::validation(format!(
            "invalid media type `{}`",
            crate::tools::memory::bound_text(&media_type, 64)
        )));
    }
    Ok(media_type)
}

/// Reject text content that looks like credential material. Artifacts are
/// durable and citable; secrets belong to the secrets tool. Uses the shared
/// [`agenkitty_core::body_looks_like_secret`] classifier (F3) — true binary
/// (non-UTF-8) is never flagged.
pub fn reject_secret_like_content(contents: &[u8]) -> AgenkitResult<()> {
    if agenkitty_core::body_looks_like_secret(contents) {
        return Err(AgenkitError::tool_policy(
            "artifact content looks like credential material; store secrets through the \
             secrets tool, never as artifacts",
        ));
    }
    Ok(())
}

/// Compute the canonical artifact content hash (lowercase hex SHA-256).
pub fn content_hash(contents: &[u8]) -> String {
    pocopine_crypto::sha256_hex(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names_reject_traversal_and_separators() {
        assert!(validate_artifact_name("report.md").is_ok());
        assert!(validate_artifact_name("  padded.txt ").is_ok());
        for bad in ["", "..", ".", "a/b", "a\\b", "a\0b", "a\nb"] {
            assert!(validate_artifact_name(bad).is_err(), "`{bad}` must fail");
        }
        assert!(validate_artifact_name(&"x".repeat(MAX_NAME_BYTES + 1)).is_err());
    }

    #[test]
    fn media_types_default_and_validate() {
        assert_eq!(validate_media_type(None).unwrap(), "text/plain");
        assert_eq!(
            validate_media_type(Some("Text/Markdown".to_string())).unwrap(),
            "text/markdown"
        );
        for bad in ["", "noslash", "/x", "x/", "a b/c"] {
            assert!(
                validate_media_type(Some(bad.to_string())).is_err(),
                "`{bad}` must fail"
            );
        }
    }

    #[test]
    fn secret_like_content_is_rejected() {
        assert!(reject_secret_like_content(b"plain report text").is_ok());
        assert!(reject_secret_like_content(b"api_key = sk-live-12345").is_err());
        // Binary (non-UTF-8) content is not text-scanned.
        assert!(reject_secret_like_content(&[0xff, 0xfe, 0x00, 0x01]).is_ok());
    }

    #[test]
    fn namespaces_derive_from_context_only() {
        let context = CurrentArtifactContext {
            project_id: "proj".to_string(),
            thread_id: Some("thread-1".to_string()),
        };
        assert_eq!(
            context.namespace_for(ArtifactScope::Session).as_deref(),
            Some("thread-1")
        );
        assert_eq!(
            context.namespace_for(ArtifactScope::Project).as_deref(),
            Some("proj")
        );

        let bare = CurrentArtifactContext {
            project_id: String::new(),
            thread_id: None,
        };
        assert!(bare.namespace_for(ArtifactScope::Session).is_none());
        assert!(bare.namespace_for(ArtifactScope::Project).is_none());
        assert!(bare.accessible().is_empty());
    }

    #[tokio::test]
    async fn context_token_round_trips_once() {
        let runtime = ArtifactRuntime::in_memory();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({ "name": "r.md" }),
                CurrentArtifactContext {
                    project_id: "proj".to_string(),
                    thread_id: Some("t".to_string()),
                },
            )
            .unwrap();
        let token = args["context_token"].as_str().unwrap().to_string();
        assert!(runtime.take_context(&token).is_ok());
        // Single use.
        assert!(runtime.take_context(&token).is_err());
    }

    #[tokio::test]
    async fn project_scope_write_fails_closed_without_an_approver() {
        let runtime = ArtifactRuntime::in_memory();
        assert!(
            runtime
                .authorize_scope_write(
                    "artifact.write",
                    ArtifactScope::Session,
                    serde_json::json!({})
                )
                .await
                .is_ok()
        );
        let err = runtime
            .authorize_scope_write(
                "artifact.write",
                ArtifactScope::Project,
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("no approver is configured"));
    }
}
