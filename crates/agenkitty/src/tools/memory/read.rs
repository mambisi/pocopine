//! `memory.read`: read one memory entry by id, enforcing namespace isolation.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::Deserialize;

use super::common::{MemoryEntryView, MemoryRuntime};

pub const MEMORY_READ_TOOL_ID: &str = "memory.read";

#[derive(Clone)]
pub struct MemoryReadTool {
    runtime: Arc<MemoryRuntime>,
}

impl MemoryReadTool {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: MemoryReadInput) -> AgenkitResult<MemoryEntryView> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let entry = self.runtime.store().read(&input.id, input.version).await?;
        // Foreign-namespace and tombstoned entries both look like "not found":
        // the caller cannot probe another owner's namespace by guessing ids.
        match entry {
            Some(entry) if context.can_access(entry.scope, &entry.namespace) => Ok(entry.into()),
            _ => Err(AgenkitError::not_found(format!(
                "memory entry `{}` not found",
                input.id
            ))),
        }
    }
}

impl AiTool for MemoryReadTool {
    const ID: &'static str = MEMORY_READ_TOOL_ID;
    type Input = MemoryReadInput;
    type Output = MemoryEntryView;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            MEMORY_READ_TOOL_ID,
            "Read one memory entry by id (optionally a specific version). Returns the bounded \
             entry, or not_found for unknown, forgotten, or out-of-namespace ids.",
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
pub struct MemoryReadInput {
    pub id: String,
    /// Optional historical revision. Omit for the current version.
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{
        CurrentMemoryContext, MemoryEntry, MemoryKind, MemoryRetention, MemoryScope, MemorySource,
    };

    fn context(thread: &str) -> CurrentMemoryContext {
        CurrentMemoryContext {
            project_id: "proj".to_string(),
            agent_id: "agent".to_string(),
            thread_id: Some(thread.to_string()),
        }
    }

    async fn seed(runtime: &MemoryRuntime, scope: MemoryScope, namespace: &str) -> String {
        let entry = MemoryEntry::draft(
            scope,
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

    fn read_input(runtime: &MemoryRuntime, id: &str, thread: &str) -> MemoryReadInput {
        let args = runtime
            .inject_context_args(&serde_json::json!({ "id": id }), context(thread))
            .unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn read_returns_entry_in_namespace() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let id = seed(&runtime, MemoryScope::Project, "proj").await;
        let view = MemoryReadTool::new(runtime.clone())
            .run(read_input(&runtime, &id, "thread"))
            .await
            .unwrap();
        assert_eq!(view.id, id);
        assert_eq!(view.namespace, "proj");
        assert_eq!(view.body, "body");
    }

    #[tokio::test]
    async fn read_hides_foreign_namespace_as_not_found() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        // A project entry owned by a different project id.
        let id = seed(&runtime, MemoryScope::Project, "other-proj").await;
        let err = MemoryReadTool::new(runtime.clone())
            .run(read_input(&runtime, &id, "thread"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[tokio::test]
    async fn read_unknown_id_is_not_found() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let err = MemoryReadTool::new(runtime.clone())
            .run(read_input(&runtime, "mem-999", "thread"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
