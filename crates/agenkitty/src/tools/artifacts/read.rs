//! `artifact.read`: read bounded artifact contents or metadata.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    ArtifactContentWindow, ArtifactMetadata, ArtifactRuntime, ArtifactScope, MAX_READ_WINDOW_BYTES,
};

pub const ARTIFACT_READ_TOOL_ID: &str = "artifact.read";

#[derive(Clone)]
pub struct ArtifactReadTool {
    runtime: Arc<ArtifactRuntime>,
}

impl ArtifactReadTool {
    pub fn new(runtime: Arc<ArtifactRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: ArtifactReadInput) -> AgenkitResult<ArtifactReadOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let accessible = context.accessible();
        if input.metadata_only.unwrap_or(false) {
            let metadata = self.runtime.store().stat(&input.id, &accessible).await?;
            return Ok(ArtifactReadOutput::from_metadata(metadata, None));
        }
        let (metadata, window) = self
            .runtime
            .store()
            .read(
                &input.id,
                &accessible,
                input.offset.unwrap_or(0),
                input
                    .max_bytes
                    .unwrap_or(MAX_READ_WINDOW_BYTES)
                    .min(MAX_READ_WINDOW_BYTES),
            )
            .await?;
        Ok(ArtifactReadOutput::from_metadata(metadata, Some(window)))
    }
}

impl AiTool for ArtifactReadTool {
    const ID: &'static str = ARTIFACT_READ_TOOL_ID;
    type Input = ArtifactReadInput;
    type Output = ArtifactReadOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            ARTIFACT_READ_TOOL_ID,
            "Read a bounded window of an artifact's contents (or metadata only) by its id. \
             Paginate with `offset`; binary windows return base64.",
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
pub struct ArtifactReadInput {
    /// The artifact id (`art-…`).
    pub id: String,
    /// Byte offset to start the window at. Defaults to 0.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Window size in bytes (capped by the read-window limit).
    #[serde(default)]
    pub max_bytes: Option<usize>,
    /// Return metadata only, no contents.
    #[serde(default)]
    pub metadata_only: Option<bool>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArtifactReadOutput {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    pub scope: ArtifactScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_path: Option<String>,
    pub created_at_ms: u64,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<ArtifactContentWindow>,
}

impl ArtifactReadOutput {
    fn from_metadata(metadata: ArtifactMetadata, window: Option<ArtifactContentWindow>) -> Self {
        Self {
            id: metadata.id,
            name: metadata.name,
            media_type: metadata.media_type,
            size: metadata.size,
            sha256: metadata.sha256,
            scope: metadata.scope,
            link_path: metadata.link_path,
            created_at_ms: metadata.created_at_ms,
            deleted: metadata.deleted,
            window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::{ArtifactEncoding, CurrentArtifactContext};
    use super::super::write::{ArtifactWriteInput, ArtifactWriteTool};
    use super::*;

    fn context() -> CurrentArtifactContext {
        CurrentArtifactContext {
            project_id: "proj".to_string(),
            thread_id: Some("thread-1".to_string()),
        }
    }

    async fn seeded_runtime() -> Arc<ArtifactRuntime> {
        let runtime = Arc::new(ArtifactRuntime::in_memory());
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "name": "notes.txt",
                    "content": "0123456789",
                    "scope": "session"
                }),
                context(),
            )
            .unwrap();
        let input: ArtifactWriteInput = serde_json::from_value(args).unwrap();
        ArtifactWriteTool::new(runtime.clone())
            .run(input)
            .await
            .unwrap();
        runtime
    }

    fn read_input(runtime: &ArtifactRuntime, body: serde_json::Value) -> ArtifactReadInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn read_windows_paginate() {
        let runtime = seeded_runtime().await;
        let out = ArtifactReadTool::new(runtime.clone())
            .run(read_input(
                &runtime,
                serde_json::json!({ "id": "art-1", "offset": 4, "max_bytes": 3 }),
            ))
            .await
            .unwrap();
        let window = out.window.unwrap();
        assert_eq!(window.content, "456");
        assert_eq!(window.encoding, ArtifactEncoding::Utf8);
        assert!(window.truncated);
    }

    #[tokio::test]
    async fn metadata_only_skips_contents() {
        let runtime = seeded_runtime().await;
        let out = ArtifactReadTool::new(runtime.clone())
            .run(read_input(
                &runtime,
                serde_json::json!({ "id": "art-1", "metadata_only": true }),
            ))
            .await
            .unwrap();
        assert!(out.window.is_none());
        assert_eq!(out.size, 10);
    }
}
