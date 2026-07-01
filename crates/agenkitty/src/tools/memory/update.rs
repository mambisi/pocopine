//! `memory.update`: revise a memory entry with optimistic concurrency.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::Deserialize;

use super::common::{MemoryEntryView, MemoryKind, MemoryPatch, MemoryRetention, MemoryRuntime};

pub const MEMORY_UPDATE_TOOL_ID: &str = "memory.update";

#[derive(Clone)]
pub struct MemoryUpdateTool {
    runtime: Arc<MemoryRuntime>,
}

impl MemoryUpdateTool {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: MemoryUpdateInput) -> AgenkitResult<MemoryEntryView> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        // Verify the caller owns this entry's namespace before mutating; foreign
        // owners look like missing entries.
        let current = self.runtime.store().read(&input.id, None).await?;
        let accessible = current
            .filter(|entry| context.can_access(entry.scope, &entry.namespace))
            .is_some();
        if !accessible {
            return Err(AgenkitError::not_found(format!(
                "memory entry `{}` not found",
                input.id
            )));
        }

        let patch = MemoryPatch {
            title: input.title,
            body: input.body,
            tags: input.tags,
            kind: input.kind,
            retention: input.retention,
            confidence: input.confidence,
            reason: input.reason,
        };
        let updated = self
            .runtime
            .store()
            .update(&input.id, input.expected_version, patch)
            .await?;

        tracing::info!(
            target: "pocopine.log",
            tool = MEMORY_UPDATE_TOOL_ID,
            id = %updated.id,
            scope = updated.scope.as_str(),
            kind = ?updated.kind,
            version = updated.version,
            "memory.update revised entry"
        );

        Ok(updated.into())
    }
}

impl AiTool for MemoryUpdateTool {
    const ID: &'static str = MEMORY_UPDATE_TOOL_ID;
    type Input = MemoryUpdateInput;
    type Output = MemoryEntryView;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            MEMORY_UPDATE_TOOL_ID,
            "Revise a memory entry. Requires the expected version (optimistic concurrency) and a \
             reason. Only the caller's own entries can be updated.",
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
pub struct MemoryUpdateInput {
    pub id: String,
    /// The version the caller believes is current. Required.
    pub expected_version: u64,
    /// Why this revision is being made. Required.
    pub reason: String,
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
    #[serde(default)]
    pub context_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{
        CurrentMemoryContext, MemoryEntry, MemoryRetention as Retention, MemoryScope, MemorySource,
    };

    fn context() -> CurrentMemoryContext {
        CurrentMemoryContext {
            project_id: "proj".to_string(),
            agent_id: "agent".to_string(),
            thread_id: Some("thread".to_string()),
        }
    }

    async fn seed(runtime: &MemoryRuntime, namespace: &str) -> String {
        let entry = MemoryEntry::draft(
            MemoryScope::Project,
            namespace,
            MemoryKind::Fact,
            "title",
            "body",
            vec![],
            MemorySource::Agent,
            vec![],
            "reason",
            Retention::Session,
            None,
        )
        .unwrap();
        runtime.store().append(entry).await.unwrap().id
    }

    fn update_input(runtime: &MemoryRuntime, body: serde_json::Value) -> MemoryUpdateInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn update_revises_and_bumps_version() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let id = seed(&runtime, "proj").await;
        let view = MemoryUpdateTool::new(runtime.clone())
            .run(update_input(
                &runtime,
                serde_json::json!({
                    "id": id,
                    "expected_version": 1,
                    "reason": "clarify",
                    "body": "clearer body"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(view.version, 2);
        assert_eq!(view.body, "clearer body");
    }

    #[tokio::test]
    async fn update_rejects_foreign_namespace() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let id = seed(&runtime, "other-proj").await;
        let err = MemoryUpdateTool::new(runtime.clone())
            .run(update_input(
                &runtime,
                serde_json::json!({
                    "id": id,
                    "expected_version": 1,
                    "reason": "clarify",
                    "body": "x"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[tokio::test]
    async fn update_rejects_stale_version() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let id = seed(&runtime, "proj").await;
        let err = MemoryUpdateTool::new(runtime.clone())
            .run(update_input(
                &runtime,
                serde_json::json!({
                    "id": id,
                    "expected_version": 99,
                    "reason": "clarify",
                    "body": "x"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }
}
