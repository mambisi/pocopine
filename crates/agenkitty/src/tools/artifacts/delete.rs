//! `artifact.delete`: tombstone an artifact (metadata survives for audit,
//! contents are removed). Policy-gated `Ask` at the dispatch layer — the
//! central evaluator prompts the host approver before this tool ever runs.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{ArtifactRuntime, ArtifactScope};

pub const ARTIFACT_DELETE_TOOL_ID: &str = "artifact.delete";

#[derive(Clone)]
pub struct ArtifactDeleteTool {
    runtime: Arc<ArtifactRuntime>,
}

impl ArtifactDeleteTool {
    pub fn new(runtime: Arc<ArtifactRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: ArtifactDeleteInput) -> AgenkitResult<ArtifactDeleteOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let accessible = context.accessible();
        let deleted = self.runtime.store().delete(&input.id, &accessible).await?;

        tracing::info!(
            target: "pocopine.log",
            tool = ARTIFACT_DELETE_TOOL_ID,
            id = %deleted.id,
            scope = deleted.scope.as_str(),
            "artifact.delete tombstoned artifact"
        );

        Ok(ArtifactDeleteOutput {
            id: deleted.id,
            scope: deleted.scope,
            deleted: true,
        })
    }
}

impl AiTool for ArtifactDeleteTool {
    const ID: &'static str = ARTIFACT_DELETE_TOOL_ID;
    type Input = ArtifactDeleteInput;
    type Output = ArtifactDeleteOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            ARTIFACT_DELETE_TOOL_ID,
            "Delete an artifact by id: contents are removed, the metadata row survives for \
             audit. Requires approval by default.",
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
pub struct ArtifactDeleteInput {
    /// The artifact id (`art-…`).
    pub id: String,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactDeleteOutput {
    pub id: String,
    pub scope: ArtifactScope,
    pub deleted: bool,
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
    async fn delete_tombstones_within_the_callers_namespaces() {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        let args = runtime
            .inject_context_args(
                &serde_json::json!({ "name": "tmp.txt", "content": "x", "scope": "session" }),
                context(),
            )
            .unwrap();
        let input: ArtifactWriteInput = serde_json::from_value(args).unwrap();
        ArtifactWriteTool::new(runtime.clone())
            .run(input)
            .await
            .unwrap();

        let args = runtime
            .inject_context_args(&serde_json::json!({ "id": "art-1" }), context())
            .unwrap();
        let input: ArtifactDeleteInput = serde_json::from_value(args).unwrap();
        let out = ArtifactDeleteTool::new(runtime.clone())
            .run(input)
            .await
            .unwrap();
        assert!(out.deleted);

        // A foreign caller cannot delete (and cannot learn the id exists).
        let foreign = CurrentArtifactContext {
            project_id: "other".to_string(),
            thread_id: Some("thread-9".to_string()),
        };
        let args = runtime
            .inject_context_args(&serde_json::json!({ "id": "art-1" }), foreign)
            .unwrap();
        let input: ArtifactDeleteInput = serde_json::from_value(args).unwrap();
        let err = ArtifactDeleteTool::new(runtime)
            .run(input)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
