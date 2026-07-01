//! `memory.forget`: tombstone a memory entry, leaving an audit record.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{MemoryRuntime, MemoryScope};

pub const MEMORY_FORGET_TOOL_ID: &str = "memory.forget";

#[derive(Clone)]
pub struct MemoryForgetTool {
    runtime: Arc<MemoryRuntime>,
}

impl MemoryForgetTool {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: MemoryForgetInput) -> AgenkitResult<MemoryForgetOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
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

        let tombstone = self
            .runtime
            .store()
            .tombstone(&input.id, input.expected_version, input.reason)
            .await?;

        tracing::info!(
            target: "pocopine.log",
            tool = MEMORY_FORGET_TOOL_ID,
            id = %tombstone.id,
            scope = tombstone.scope.as_str(),
            version = tombstone.version,
            "memory.forget tombstoned entry"
        );

        Ok(MemoryForgetOutput {
            id: tombstone.id,
            version: tombstone.version,
            scope: tombstone.scope,
            namespace: tombstone.namespace,
            reason: tombstone.reason,
            tombstoned_at_ms: tombstone.tombstoned_at_ms,
        })
    }
}

impl AiTool for MemoryForgetTool {
    const ID: &'static str = MEMORY_FORGET_TOOL_ID;
    type Input = MemoryForgetInput;
    type Output = MemoryForgetOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            MEMORY_FORGET_TOOL_ID,
            "Forget (tombstone) a memory entry. Requires the expected version and a reason. The \
             body becomes unreadable; an audit tombstone remains. Caller-owned entries only.",
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
pub struct MemoryForgetInput {
    pub id: String,
    /// The version the caller believes is current. Required.
    pub expected_version: u64,
    /// Why the entry is being forgotten. Required.
    pub reason: String,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MemoryForgetOutput {
    pub id: String,
    pub version: u64,
    pub scope: MemoryScope,
    pub namespace: String,
    pub reason: String,
    pub tombstoned_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{
        CurrentMemoryContext, MemoryEntry, MemoryKind, MemoryRetention, MemorySource,
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
            MemoryRetention::Session,
            None,
        )
        .unwrap();
        runtime.store().append(entry).await.unwrap().id
    }

    fn forget_input(runtime: &MemoryRuntime, body: serde_json::Value) -> MemoryForgetInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn forget_tombstones_entry() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let id = seed(&runtime, "proj").await;
        let out = MemoryForgetTool::new(runtime.clone())
            .run(forget_input(
                &runtime,
                serde_json::json!({ "id": id, "expected_version": 1, "reason": "stale" }),
            ))
            .await
            .unwrap();
        assert_eq!(out.version, 1);
        assert_eq!(out.namespace, "proj");
        // Body is no longer readable.
        assert!(runtime.store().read(&out.id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn forget_rejects_foreign_namespace() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let id = seed(&runtime, "other-proj").await;
        let err = MemoryForgetTool::new(runtime.clone())
            .run(forget_input(
                &runtime,
                serde_json::json!({ "id": id, "expected_version": 1, "reason": "stale" }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
