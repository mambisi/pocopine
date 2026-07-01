//! `memory.write`: create a durable memory entry in a caller-derived namespace.

use std::sync::Arc;

use agenkitty_core::SessionSourceRef;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    MemoryEntry, MemoryKind, MemoryRetention, MemoryRuntime, MemoryScope, MemorySource,
};
use super::store::namespace_for_write;
use crate::tools::session::SessionSourceRefSpec;

pub const MEMORY_WRITE_TOOL_ID: &str = "memory.write";

#[derive(Clone)]
pub struct MemoryWriteTool {
    runtime: Arc<MemoryRuntime>,
}

impl MemoryWriteTool {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: MemoryWriteInput) -> AgenkitResult<MemoryWriteOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        // The namespace is derived from the caller's context and scope, never
        // taken from the model — that is the isolation boundary.
        let namespace = namespace_for_write(&context, input.scope)?;
        let source = input.source.unwrap_or(MemorySource::Agent);
        let retention = input.retention.unwrap_or_default();
        let source_refs: Vec<SessionSourceRef> =
            input.source_refs.into_iter().map(Into::into).collect();

        let entry = MemoryEntry::draft(
            input.scope,
            namespace,
            input.kind,
            input.title,
            input.body,
            input.tags,
            source,
            source_refs,
            input.reason,
            retention,
            input.confidence,
        )?;
        let stored = self.runtime.store().append(entry).await?;

        // Observability per RFC-069: id/scope/kind/outcome only — never the
        // title, body, tags, or reason.
        tracing::info!(
            target: "pocopine.log",
            tool = MEMORY_WRITE_TOOL_ID,
            id = %stored.id,
            scope = stored.scope.as_str(),
            kind = ?stored.kind,
            version = stored.version,
            "memory.write created entry"
        );

        Ok(MemoryWriteOutput {
            id: stored.id,
            version: stored.version,
            scope: stored.scope,
            namespace: stored.namespace,
            kind: stored.kind,
            title: stored.title,
            tags: stored.tags,
            created_at_ms: stored.created_at_ms,
        })
    }
}

impl AiTool for MemoryWriteTool {
    const ID: &'static str = MEMORY_WRITE_TOOL_ID;
    type Input = MemoryWriteInput;
    type Output = MemoryWriteOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            MEMORY_WRITE_TOOL_ID,
            "Write a durable memory entry (fact, decision, procedure, …) in the caller's scope. \
             Requires a reason. Namespace is derived from the caller; secrets are rejected.",
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
pub struct MemoryWriteInput {
    /// Storage scope. `user`/`team` require a host-configured store.
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Why this entry should exist. Required.
    pub reason: String,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRefSpec>,
    /// Origin of the entry. Defaults to `agent`.
    #[serde(default)]
    pub source: Option<MemorySource>,
    /// Retention policy. Defaults to `session`.
    #[serde(default)]
    pub retention: Option<MemoryRetention>,
    /// Optional 0.0–1.0 confidence.
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct MemoryWriteOutput {
    pub id: String,
    pub version: u64,
    pub scope: MemoryScope,
    pub namespace: String,
    pub kind: MemoryKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub created_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::CurrentMemoryContext;

    fn context() -> CurrentMemoryContext {
        CurrentMemoryContext {
            project_id: "proj".to_string(),
            agent_id: "agent".to_string(),
            thread_id: Some("thread".to_string()),
        }
    }

    fn input(runtime: &MemoryRuntime, body: serde_json::Value) -> MemoryWriteInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn write_derives_namespace_from_scope() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let out = MemoryWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "scope": "project",
                    "kind": "decision",
                    "title": "Use yrs",
                    "body": "We chose yrs.",
                    "reason": "architecture decision",
                    "tags": ["collab"]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(out.id, "mem-1");
        assert_eq!(out.namespace, "proj");
        assert_eq!(out.scope, MemoryScope::Project);

        let agent_scoped = MemoryWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "scope": "agent",
                    "kind": "fact",
                    "title": "t",
                    "body": "b",
                    "reason": "r"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(agent_scoped.namespace, "proj::agent");
    }

    #[tokio::test]
    async fn write_rejects_host_owned_scope_and_secrets() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let err = MemoryWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "scope": "user",
                    "kind": "fact",
                    "title": "t",
                    "body": "b",
                    "reason": "r"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");

        let secret = MemoryWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "scope": "session",
                    "kind": "fact",
                    "title": "creds",
                    "body": "api_key = sk-live-123",
                    "reason": "r"
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(secret.kind(), "tool_policy");
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn write_trace_omits_body() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        MemoryWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "scope": "session",
                    "kind": "fact",
                    "title": "title",
                    "body": "super-sensitive-body-text",
                    "reason": "r"
                }),
            ))
            .await
            .unwrap();
        assert!(logs_contain("memory.write created entry"));
        assert!(logs_contain("mem-1"));
        assert!(
            !logs_contain("super-sensitive-body-text"),
            "the memory body leaked into the trace log"
        );
    }

    #[tokio::test]
    async fn write_requires_reason_and_context() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        let err = MemoryWriteTool::new(runtime.clone())
            .run(input(
                &runtime,
                serde_json::json!({
                    "scope": "session",
                    "kind": "fact",
                    "title": "t",
                    "body": "b",
                    "reason": "  "
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");

        // No injected context token.
        let no_ctx = MemoryWriteTool::new(runtime)
            .run(MemoryWriteInput {
                scope: MemoryScope::Session,
                kind: MemoryKind::Fact,
                title: "t".to_string(),
                body: "b".to_string(),
                tags: vec![],
                reason: "r".to_string(),
                source_refs: vec![],
                source: None,
                retention: None,
                confidence: None,
                context_token: None,
            })
            .await
            .unwrap_err();
        assert_eq!(no_ctx.kind(), "validation");
    }
}
