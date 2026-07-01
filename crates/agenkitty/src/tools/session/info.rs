use std::sync::Arc;

use agenkitty_core::SessionStoreKind;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::SessionRuntime;

pub const SESSION_INFO_TOOL_ID: &str = "session.info";

#[derive(Clone)]
pub struct SessionInfoTool {
    runtime: Arc<SessionRuntime>,
}

impl SessionInfoTool {
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SessionInfoInput) -> AgenkitResult<SessionInfoOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let identity = context.identity;
        let event_count = self
            .runtime
            .store()
            .event_count(&identity.thread_id)
            .await?;
        Ok(SessionInfoOutput {
            thread_id: identity.thread_id,
            agent_id: identity.agent_id,
            model: identity.model,
            run_id: identity.run_id,
            turn_id: identity.turn_id,
            owner: identity
                .principal_key
                .unwrap_or_else(|| "anonymous".to_string()),
            tool_ids: identity.tool_ids,
            max_steps_per_turn: identity.max_steps_per_turn,
            capture_policy: identity.capture_policy,
            transcript_store: store_kind_name(identity.transcript_store).to_string(),
            metadata_store: store_kind_name(self.runtime.store().kind()).to_string(),
            event_count,
            project_id: identity.project_id,
        })
    }
}

fn store_kind_name(kind: SessionStoreKind) -> &'static str {
    match kind {
        SessionStoreKind::InMemory => "in_memory",
        SessionStoreKind::LocalJsonl => "local_jsonl",
        SessionStoreKind::Sqlite => "sqlite",
        SessionStoreKind::HostProvided => "host_provided",
        SessionStoreKind::Unknown => "unknown",
    }
}

impl AiTool for SessionInfoTool {
    const ID: &'static str = SESSION_INFO_TOOL_ID;
    type Input = SessionInfoInput;
    type Output = SessionInfoOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SESSION_INFO_TOOL_ID,
            "Return bounded metadata about the current agent session, including thread id, model, tools, limits, and event count.",
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

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct SessionInfoInput {
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SessionInfoOutput {
    pub thread_id: String,
    pub agent_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub owner: String,
    pub tool_ids: Vec<String>,
    pub max_steps_per_turn: u32,
    pub capture_policy: String,
    pub transcript_store: String,
    pub metadata_store: String,
    pub event_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session::common::{
        CurrentSessionContext, InMemorySessionMetadataStore, SessionMetadataStore,
    };
    use agenkitty_core::{SessionIdentity, SessionStoreKind};

    fn identity() -> SessionIdentity {
        SessionIdentity {
            thread_id: "thread-1".to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: Some("run-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            principal_key: Some("local".to_string()),
            tool_ids: vec![SESSION_INFO_TOOL_ID.to_string()],
            max_steps_per_turn: 8,
            capture_policy: "full".to_string(),
            transcript_store: SessionStoreKind::InMemory,
            metadata_store: SessionStoreKind::InMemory,
            created_at_ms: 1,
            last_active_at_ms: 1,
            project_id: Some("project".to_string()),
        }
    }

    #[tokio::test]
    async fn session_info_uses_injected_context() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let runtime = Arc::new(SessionRuntime::new(store.clone()));
        store.upsert_identity(identity()).await.unwrap();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({}),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let input: SessionInfoInput = serde_json::from_value(args).unwrap();
        let output = SessionInfoTool::new(runtime).run(input).await.unwrap();

        assert_eq!(output.thread_id, "thread-1");
        assert_eq!(output.agent_id, "agent");
        assert_eq!(output.tool_ids, vec![SESSION_INFO_TOOL_ID.to_string()]);
    }

    #[tokio::test]
    async fn session_info_rejects_missing_context() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let err = SessionInfoTool::new(runtime)
            .run(SessionInfoInput::default())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }
}
