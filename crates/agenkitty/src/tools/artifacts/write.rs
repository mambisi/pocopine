//! `artifact.write`: create a durable artifact from text or bytes.

use std::sync::Arc;

use agenkitty_core::SessionSourceRef;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use pocopine_codec::base64_decode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    ArtifactDraft, ArtifactEncoding, ArtifactRuntime, ArtifactScope, MAX_CONTENT_BYTES,
    current_time_ms, reject_secret_like_content, validate_artifact_name, validate_media_type,
};
use crate::tools::session::SessionSourceRefSpec;

pub const ARTIFACT_WRITE_TOOL_ID: &str = "artifact.write";

#[derive(Clone)]
pub struct ArtifactWriteTool {
    runtime: Arc<ArtifactRuntime>,
}

impl ArtifactWriteTool {
    pub fn new(runtime: Arc<ArtifactRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: ArtifactWriteInput) -> AgenkitResult<ArtifactWriteOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        // The namespace derives from the caller's context and scope, never
        // from the model — that is the isolation boundary.
        let namespace = context.namespace_for(input.scope).ok_or_else(|| {
            AgenkitError::tool_policy(format!(
                "artifact scope `{}` is not available in this run",
                input.scope.as_str()
            ))
        })?;
        let name = validate_artifact_name(&input.name)?;
        let media_type = validate_media_type(input.media_type)?;
        let encoding = input.encoding.unwrap_or_default();
        // Reject an over-cap payload BEFORE `decode_contents` allocates the
        // output buffer: the encoded input is already in memory, but a
        // multi-GB body shouldn't also allocate its decoded copy just to be
        // rejected. UTF-8 decodes 1:1; base64 decodes to at most 3/4 of its
        // input, so an encoded length past this bound cannot fit the cap.
        let max_encoded = match encoding {
            ArtifactEncoding::Utf8 => MAX_CONTENT_BYTES,
            ArtifactEncoding::Base64 => (MAX_CONTENT_BYTES / 3 + 1) * 4,
        };
        if input.content.len() > max_encoded {
            return Err(AgenkitError::validation(format!(
                "artifact content exceeds {MAX_CONTENT_BYTES} bytes"
            )));
        }
        let contents = decode_contents(&input.content, encoding)?;
        // Exact post-decode gate (base64 whitespace/padding can shrink it).
        if contents.len() > MAX_CONTENT_BYTES {
            return Err(AgenkitError::validation(format!(
                "artifact content exceeds {MAX_CONTENT_BYTES} bytes"
            )));
        }
        reject_secret_like_content(&contents)?;
        // Project-scoped writes are the plan's `Ask`: the runtime consults the
        // host approver (fail closed headless); session scope proceeds.
        self.runtime
            .authorize_scope_write(
                ARTIFACT_WRITE_TOOL_ID,
                input.scope,
                serde_json::json!({
                    "name": name,
                    "media_type": media_type,
                    "size": contents.len(),
                    "scope": input.scope.as_str(),
                }),
            )
            .await?;

        // Record the originating session on the artifact (the session side is
        // the metadata store's `link_artifact`, wired at run end).
        let mut source_refs: Vec<SessionSourceRef> = context.thread_ref().into_iter().collect();
        source_refs.extend(input.source_refs.into_iter().map(Into::into));
        let stored = self
            .runtime
            .store()
            .write(
                ArtifactDraft {
                    name,
                    media_type,
                    scope: input.scope,
                    namespace,
                    source_refs,
                    link_path: None,
                    created_at_ms: current_time_ms(),
                },
                contents,
            )
            .await?;

        // Observability per RFC-069: id/scope/size/outcome only — never the
        // content or name.
        tracing::info!(
            target: "pocopine.log",
            tool = ARTIFACT_WRITE_TOOL_ID,
            id = %stored.id,
            scope = stored.scope.as_str(),
            size = stored.size,
            "artifact.write stored artifact"
        );

        Ok(ArtifactWriteOutput {
            id: stored.id,
            name: stored.name,
            media_type: stored.media_type,
            size: stored.size,
            sha256: stored.sha256,
            scope: stored.scope,
            created_at_ms: stored.created_at_ms,
        })
    }
}

pub(super) fn decode_contents(content: &str, encoding: ArtifactEncoding) -> AgenkitResult<Vec<u8>> {
    match encoding {
        ArtifactEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        ArtifactEncoding::Base64 => base64_decode(content)
            .map_err(|err| AgenkitError::validation(format!("invalid base64 content: {err}"))),
    }
}

impl AiTool for ArtifactWriteTool {
    const ID: &'static str = ARTIFACT_WRITE_TOOL_ID;
    type Input = ArtifactWriteInput;
    type Output = ArtifactWriteOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            ARTIFACT_WRITE_TOOL_ID,
            "Store a durable run-output artifact (report, log, build product) from text or \
             base64 bytes. Returns a stable citable id. Session scope stores beside the \
             session; project scope requires host approval.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input).await })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ArtifactWriteInput {
    /// Artifact name (a single path component; used for display and citation).
    pub name: String,
    /// The content: UTF-8 text, or base64 when `encoding` is `base64`.
    pub content: String,
    /// Content encoding. Defaults to `utf8`.
    #[serde(default)]
    pub encoding: Option<ArtifactEncoding>,
    /// Declared media type (`type/subtype`). Defaults to `text/plain`.
    #[serde(default)]
    pub media_type: Option<String>,
    /// `session` (reaped with the session) or `project` (survives; requires
    /// host approval).
    pub scope: ArtifactScope,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactWriteOutput {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    pub scope: ArtifactScope,
    pub created_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::super::common::CurrentArtifactContext;
    use super::*;

    fn context() -> CurrentArtifactContext {
        CurrentArtifactContext {
            project_id: "proj".to_string(),
            thread_id: Some("thread-1".to_string()),
        }
    }

    fn input(runtime: &ArtifactRuntime, body: serde_json::Value) -> ArtifactWriteInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn session_write_stores_and_returns_a_citable_id() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        let out = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "report.md",
                    "content": "# Findings",
                    "media_type": "text/markdown",
                    "scope": "session"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(out.id, "art-1");
        assert_eq!(out.media_type, "text/markdown");
        assert_eq!(out.size, 10);
        assert_eq!(
            out.sha256,
            super::super::common::content_hash(b"# Findings")
        );
    }

    #[tokio::test]
    async fn base64_content_decodes_through_the_codec() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        let encoded = pocopine_codec::base64_encode(&[0xff, 0x00, 0x01]);
        let out = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "blob.bin",
                    "content": encoded,
                    "encoding": "base64",
                    "media_type": "application/octet-stream",
                    "scope": "session"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(out.size, 3);
    }

    #[tokio::test]
    async fn project_write_fails_closed_without_an_approver() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        let err = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "adr.md",
                    "content": "decision",
                    "scope": "project"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("no approver is configured"));
    }

    #[tokio::test]
    async fn project_write_resolves_through_the_approver() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        runtime.set_approver(Arc::new(crate::policy::StaticApprover(true)));
        let out = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "adr.md",
                    "content": "decision",
                    "scope": "project"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(out.scope, ArtifactScope::Project);

        let denied = Arc::new(ArtifactRuntime::in_memory());
        denied.set_approver(Arc::new(crate::policy::StaticApprover(false)));
        let err = ArtifactWriteTool::new(denied.clone())
            .run(input(
                &denied,
                serde_json::json!({
                    "name": "adr.md",
                    "content": "decision",
                    "scope": "project"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn secret_like_and_oversized_content_is_rejected() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        let err = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "creds.txt",
                    "content": "api_key = sk-live-12345",
                    "scope": "session"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");

        let err = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "big.txt",
                    "content": "x".repeat(MAX_CONTENT_BYTES + 1),
                    "scope": "session"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");

        // An over-cap base64 payload is rejected before decode allocates.
        let oversized_b64 = "A".repeat((MAX_CONTENT_BYTES / 3 + 1) * 4 + 8);
        let err = ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "big.bin",
                    "content": oversized_b64,
                    "encoding": "base64",
                    "scope": "session"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn write_trace_omits_content() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        ArtifactWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "name": "r.md",
                    "content": "super-sensitive-artifact-text",
                    "scope": "session"
                }),
            ))
            .await
            .unwrap();
        assert!(logs_contain("artifact.write stored artifact"));
        assert!(
            !logs_contain("super-sensitive-artifact-text"),
            "the artifact content leaked into the trace log"
        );
    }
}
