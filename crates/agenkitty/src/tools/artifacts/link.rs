//! `artifact.link`: attach an existing workspace file as an artifact
//! reference — metadata + provenance without copying bytes.

use std::sync::Arc;

use agenkitty_core::SessionSourceRef;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    ArtifactDraft, ArtifactRuntime, ArtifactScope, current_time_ms, validate_artifact_name,
    validate_media_type,
};
use crate::tools::session::SessionSourceRefSpec;

pub const ARTIFACT_LINK_TOOL_ID: &str = "artifact.link";

#[derive(Clone)]
pub struct ArtifactLinkTool {
    runtime: Arc<ArtifactRuntime>,
}

impl ArtifactLinkTool {
    pub fn new(runtime: Arc<ArtifactRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: ArtifactLinkInput) -> AgenkitResult<ArtifactLinkOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let namespace = context.namespace_for(input.scope).ok_or_else(|| {
            AgenkitError::tool_policy(format!(
                "artifact scope `{}` is not available in this run",
                input.scope.as_str()
            ))
        })?;
        // Default the artifact name to the file's own name.
        let name = match &input.name {
            Some(name) => name.clone(),
            None => input
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&input.path)
                .to_string(),
        };
        let name = validate_artifact_name(&name)?;
        let media_type = validate_media_type(input.media_type)?;
        // Same scope policy as `artifact.write`: project links need approval.
        self.runtime
            .authorize_scope_write(
                ARTIFACT_LINK_TOOL_ID,
                input.scope,
                serde_json::json!({
                    "name": name,
                    "path": input.path,
                    "scope": input.scope.as_str(),
                }),
            )
            .await?;

        let source_refs: Vec<SessionSourceRef> =
            input.source_refs.into_iter().map(Into::into).collect();
        // The store resolves + confines the path and derives size/hash from
        // the live file; a link never owns stored bytes.
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
                    link_path: Some(input.path),
                    created_at_ms: current_time_ms(),
                },
                Vec::new(),
            )
            .await?;

        tracing::info!(
            target: "pocopine.log",
            tool = ARTIFACT_LINK_TOOL_ID,
            id = %stored.id,
            scope = stored.scope.as_str(),
            size = stored.size,
            "artifact.link attached reference"
        );

        Ok(ArtifactLinkOutput {
            id: stored.id,
            name: stored.name,
            size: stored.size,
            sha256: stored.sha256,
            scope: stored.scope,
            created_at_ms: stored.created_at_ms,
        })
    }
}

impl AiTool for ArtifactLinkTool {
    const ID: &'static str = ARTIFACT_LINK_TOOL_ID;
    type Input = ArtifactLinkInput;
    type Output = ArtifactLinkOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            ARTIFACT_LINK_TOOL_ID,
            "Attach an existing workspace file as an artifact reference (metadata + hash, no \
             copy). Reads go through the live file, confined to the workspace.",
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
pub struct ArtifactLinkInput {
    /// Workspace-relative path of the file to reference.
    pub path: String,
    /// Artifact name. Defaults to the file's own name.
    #[serde(default)]
    pub name: Option<String>,
    /// Declared media type. Defaults to `text/plain`.
    #[serde(default)]
    pub media_type: Option<String>,
    /// `session` or `project` (project requires host approval).
    pub scope: ArtifactScope,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactLinkOutput {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub scope: ArtifactScope,
    pub created_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::super::common::CurrentArtifactContext;
    use super::super::store::InMemoryArtifactStore;
    use super::*;

    fn context() -> CurrentArtifactContext {
        CurrentArtifactContext {
            project_id: "proj".to_string(),
            thread_id: Some("thread-1".to_string()),
        }
    }

    fn link_input(runtime: &ArtifactRuntime, body: serde_json::Value) -> ArtifactLinkInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn link_derives_metadata_from_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("build.log"), "ok\n").unwrap();
        let runtime = Arc::new(ArtifactRuntime::new(Arc::new(
            InMemoryArtifactStore::new().with_workspace_root(dir.path()),
        )));
        let out = ArtifactLinkTool::new(runtime.clone())
            .run(link_input(
                &runtime,
                serde_json::json!({ "path": "build.log", "scope": "session" }),
            ))
            .await
            .unwrap();
        assert_eq!(out.name, "build.log");
        assert_eq!(out.size, 3);
        assert_eq!(out.sha256, super::super::common::content_hash(b"ok\n"));
    }

    #[tokio::test]
    async fn link_rejects_escaping_paths_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Arc::new(ArtifactRuntime::new(Arc::new(
            InMemoryArtifactStore::new().with_workspace_root(dir.path()),
        )));
        let err = ArtifactLinkTool::new(runtime.clone())
            .run(link_input(
                &runtime,
                serde_json::json!({ "path": "../outside.txt", "scope": "session", "name": "outside.txt" }),
            ))
            .await
            .unwrap_err();
        // A `..` path is rejected up front (workspace-relative contract).
        assert!(matches!(
            err.kind(),
            "tool_policy" | "not_found" | "validation"
        ));
    }
}
