//! `artifact.list`: list artifacts in the caller's namespaces.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{ArtifactRuntime, ArtifactScope, MAX_LIST_LIMIT};

pub const ARTIFACT_LIST_TOOL_ID: &str = "artifact.list";

#[derive(Clone)]
pub struct ArtifactListTool {
    runtime: Arc<ArtifactRuntime>,
}

impl ArtifactListTool {
    pub fn new(runtime: Arc<ArtifactRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: ArtifactListInput) -> AgenkitResult<ArtifactListOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let accessible = context.accessible();
        let limit = input
            .limit
            .unwrap_or(MAX_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);
        let artifacts = self
            .runtime
            .store()
            .list(&accessible, input.scope, limit)
            .await?
            .into_iter()
            .map(|metadata| ArtifactListEntry {
                id: metadata.id,
                name: metadata.name,
                media_type: metadata.media_type,
                size: metadata.size,
                scope: metadata.scope,
                linked: metadata.link_path.is_some(),
                created_at_ms: metadata.created_at_ms,
            })
            .collect();
        Ok(ArtifactListOutput { artifacts })
    }
}

impl AiTool for ArtifactListTool {
    const ID: &'static str = ARTIFACT_LIST_TOOL_ID;
    type Input = ArtifactListInput;
    type Output = ArtifactListOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            ARTIFACT_LIST_TOOL_ID,
            "List the artifacts stored for this session/project, newest first.",
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ArtifactListInput {
    /// Restrict to one scope; omitted lists both.
    #[serde(default)]
    pub scope: Option<ArtifactScope>,
    /// Maximum entries returned.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactListEntry {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub scope: ArtifactScope,
    /// Whether this artifact is a workspace-file reference (`artifact.link`).
    pub linked: bool,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactListOutput {
    pub artifacts: Vec<ArtifactListEntry>,
}

#[cfg(test)]
mod tests {
    use super::super::common::CurrentArtifactContext;
    use super::super::write::{ArtifactWriteInput, ArtifactWriteTool};
    use super::*;

    fn context() -> CurrentArtifactContext {
        CurrentArtifactContext {
            project_id: "proj".to_string(),
            thread_id: Some("thread-1".to_string()),
        }
    }

    #[tokio::test]
    async fn list_returns_newest_first_within_the_callers_namespaces() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        for name in ["a.txt", "b.txt"] {
            let args = runtime
                .inject_context_args(
                    &serde_json::json!({ "name": name, "content": "x", "scope": "session" }),
                    context(),
                )
                .unwrap();
            let input: ArtifactWriteInput = serde_json::from_value(args).unwrap();
            ArtifactWriteTool::new(runtime.clone())
                .run(input)
                .await
                .unwrap();
        }
        let args = runtime
            .inject_context_args(&serde_json::json!({}), context())
            .unwrap();
        let input: ArtifactListInput = serde_json::from_value(args).unwrap();
        let out = ArtifactListTool::new(runtime.clone())
            .run(input)
            .await
            .unwrap();
        assert_eq!(out.artifacts.len(), 2);
        assert_eq!(out.artifacts[0].name, "b.txt");

        // A foreign caller sees nothing (no existence oracle).
        let foreign = CurrentArtifactContext {
            project_id: "other".to_string(),
            thread_id: Some("thread-9".to_string()),
        };
        let args = runtime
            .inject_context_args(&serde_json::json!({}), foreign)
            .unwrap();
        let input: ArtifactListInput = serde_json::from_value(args).unwrap();
        let out = ArtifactListTool::new(runtime).run(input).await.unwrap();
        assert!(out.artifacts.is_empty());
    }
}
