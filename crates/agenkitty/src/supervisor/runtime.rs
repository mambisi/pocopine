use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::StreamExt;
use pocopine_agenkit::prelude::{Agenkit, AgenkitBuilder, ModelRef};
use pocopine_agenkit::server::SecretString;
use pocopine_agenkit::server::session::JsonlSessionStore;
use pocopine_agenkit::server::{
    AgentConfig, AgentEvent, AgentSession, AuthUser, MockProvider, Principal, SessionThreadStore,
    StopReason, ToolDecision,
};
use pocopine_agenkit_core::AgentThreadId;
use pocopine_agenkit_oai::OpenAiProvider;

use crate::events::{FrameworkEvent, RunStatus};
use crate::tools::session::{redact_json_value, redact_text_to_limit};
use crate::tools::{
    CurrentMemoryContext, CurrentSessionContext, LocalJsonlMemoryStore,
    LocalJsonlSessionMetadataStore, MemoryRuntime, SessionRuntime, current_time_ms,
    known_memory_tool_ids, known_session_tool_ids, register_memory_tools, register_session_tools,
    register_tools_with_runtimes, session_event_from_framework,
};
use agenkitty_core::{SessionIdentity, SessionSourceRef, SessionStoreKind};

#[derive(Clone, Debug)]
pub struct AgentRunOptions {
    pub agent_id: String,
    pub model: String,
    pub system: String,
    pub prompt: String,
    pub thread_id: Option<String>,
    pub max_steps_per_turn: u32,
    pub tool_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AgentRunReport {
    pub thread_id: String,
    pub session: SessionIdentity,
    pub events: Vec<FrameworkEvent>,
}

impl AgentRunReport {
    pub fn failed(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(&event.kind, crate::events::FrameworkEventKind::Failed))
    }
}

#[derive(Clone)]
pub struct FrameworkRunner {
    agenkit: Agenkit,
    session_runtime: Arc<SessionRuntime>,
    memory_runtime: Arc<MemoryRuntime>,
    /// Stable project id for this runner, derived from the canonical project
    /// root. `None` for project-less runners (mock / bare provider). Used to seed
    /// `SessionIdentity::project_id` on a fresh run so project/agent memory has a
    /// namespace.
    project_id: Option<String>,
    transcript_store: SessionStoreKind,
}

#[derive(Clone)]
pub struct QwenProviderConfig {
    pub api_key: SecretString,
    pub base_url: String,
    pub default_model: String,
}

impl QwenProviderConfig {
    pub fn from_env(default_model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("QWEN_API_KEY").context("QWEN_API_KEY is not set")?;
        let base_url = std::env::var("QWEN_BASE_URL").context("QWEN_BASE_URL is not set")?;
        Ok(Self {
            api_key: SecretString::new(api_key),
            base_url,
            default_model: default_model.into(),
        })
    }
}

impl FrameworkRunner {
    pub fn mock() -> Self {
        let session_runtime = Arc::new(SessionRuntime::in_memory());
        let memory_runtime = Arc::new(MemoryRuntime::in_memory());
        let agenkit = register_memory_tools(
            register_session_tools(
                Agenkit::builder()
                    .provider(MockProvider::new("local").default_text("hello from agenkitty mock"))
                    .default_model(ModelRef::new("local/default")),
                session_runtime.clone(),
            ),
            memory_runtime.clone(),
        )
        .build()
        .expect("mock runtime is valid");
        Self {
            agenkit,
            session_runtime,
            memory_runtime,
            project_id: None,
            transcript_store: SessionStoreKind::InMemory,
        }
    }

    pub fn mock_for_project(root: impl AsRef<Path>) -> Result<Self> {
        Self::from_builder_with_repo_tools(mock_builder(), root)
    }

    pub fn mock_for_project_with_session_root(
        root: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::from_builder_with_repo_tools_and_session_root(mock_builder(), root, session_root)
    }

    pub fn qwen_from_env(default_model: impl Into<String>) -> Result<Self> {
        Self::qwen(QwenProviderConfig::from_env(default_model)?)
    }

    pub fn qwen(config: QwenProviderConfig) -> Result<Self> {
        Self::openai_compatible(
            "qwen",
            config.api_key.expose().to_string(),
            config.base_url,
            config.default_model,
        )
    }

    pub fn qwen_for_project(config: QwenProviderConfig, root: impl AsRef<Path>) -> Result<Self> {
        Self::openai_compatible_for_project(
            "qwen",
            config.api_key.expose().to_string(),
            config.base_url,
            config.default_model,
            root,
        )
    }

    pub fn qwen_for_project_with_session_root(
        config: QwenProviderConfig,
        root: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::openai_compatible_for_project_with_session_root(
            "qwen",
            config.api_key.expose().to_string(),
            config.base_url,
            config.default_model,
            root,
            session_root,
        )
    }

    pub fn openai_compatible(
        alias: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Result<Self> {
        let provider = OpenAiProvider::new(alias, api_key).with_base_url(base_url);
        let session_runtime = Arc::new(SessionRuntime::in_memory());
        let memory_runtime = Arc::new(MemoryRuntime::in_memory());
        let agenkit = register_memory_tools(
            register_session_tools(
                Agenkit::builder()
                    .provider(provider)
                    .default_model(ModelRef::new(default_model)),
                session_runtime.clone(),
            ),
            memory_runtime.clone(),
        )
        .build()?;
        Ok(Self {
            agenkit,
            session_runtime,
            memory_runtime,
            project_id: None,
            transcript_store: SessionStoreKind::InMemory,
        })
    }

    pub fn openai_compatible_for_project(
        alias: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
        root: impl AsRef<Path>,
    ) -> Result<Self> {
        let provider = OpenAiProvider::new(alias, api_key).with_base_url(base_url);
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new(default_model));
        Self::from_builder_with_repo_tools(builder, root)
    }

    pub fn openai_compatible_for_project_with_session_root(
        alias: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
        root: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let provider = OpenAiProvider::new(alias, api_key).with_base_url(base_url);
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new(default_model));
        Self::from_builder_with_repo_tools_and_session_root(builder, root, session_root)
    }

    fn from_builder_with_repo_tools(
        builder: AgenkitBuilder,
        root: impl AsRef<Path>,
    ) -> Result<Self> {
        let project_id = Some(project_id_from_root(root.as_ref()));
        let session_runtime = Arc::new(SessionRuntime::in_memory());
        let memory_runtime = Arc::new(MemoryRuntime::in_memory());
        let agenkit = register_tools_with_runtimes(
            builder,
            root,
            session_runtime.clone(),
            memory_runtime.clone(),
        )?
        .build()?;
        Ok(Self {
            agenkit,
            session_runtime,
            memory_runtime,
            project_id,
            transcript_store: SessionStoreKind::InMemory,
        })
    }

    fn from_builder_with_repo_tools_and_session_root(
        builder: AgenkitBuilder,
        root: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let project_id = Some(project_id_from_root(root.as_ref()));
        let session_root = session_root.as_ref();
        let transcript_root = session_root.join("threads");
        let metadata_root = session_root.join("metadata");
        let memory_root = session_root.join("memory");
        let thread_store =
            SessionThreadStore::new(Arc::new(JsonlSessionStore::new(transcript_root)));
        let session_runtime = Arc::new(SessionRuntime::new(Arc::new(
            LocalJsonlSessionMetadataStore::open(metadata_root)?,
        )));
        let memory_runtime = Arc::new(MemoryRuntime::new(Arc::new(LocalJsonlMemoryStore::open(
            memory_root,
        )?)));
        let agenkit = register_tools_with_runtimes(
            builder.thread_store(thread_store),
            root,
            session_runtime.clone(),
            memory_runtime.clone(),
        )?
        .build()?;
        Ok(Self {
            agenkit,
            session_runtime,
            memory_runtime,
            project_id,
            transcript_store: SessionStoreKind::LocalJsonl,
        })
    }

    pub async fn run_prompt(&self, options: AgentRunOptions) -> Result<AgentRunReport> {
        let agent_id = options.agent_id.clone();
        let model = options.model.clone();
        let system = options.system.clone();
        let tool_ids = options.tool_ids.clone();
        let max_steps_per_turn = options.max_steps_per_turn;
        let principal = Principal::from_user(AuthUser::new("local:agenkitty"));
        let config = AgentConfig::new()
            .model(ModelRef::new(model.clone()))
            .system(system)
            .tools(tool_ids.clone())
            .max_steps_per_turn(max_steps_per_turn);
        let resume_id = options.thread_id.map(AgentThreadId::new);
        let current_session: Arc<Mutex<Option<CurrentSessionContext>>> = Arc::new(Mutex::new(None));
        let current_memory: Arc<Mutex<Option<CurrentMemoryContext>>> = Arc::new(Mutex::new(None));
        let session_runtime_for_hook = self.session_runtime.clone();
        let memory_runtime_for_hook = self.memory_runtime.clone();
        let current_session_for_hook = current_session.clone();
        let current_memory_for_hook = current_memory.clone();
        let session = AgentSession::builder(&self.agenkit)
            .agent_id(agent_id.clone())
            .principal(principal)
            .config(config)
            .before_tool_call(move |tool, args| {
                // Session and memory tools each need their own runtime-injected
                // context_token; everything else proceeds untouched.
                if known_session_tool_ids().contains(&tool) {
                    let context = current_session_for_hook
                        .lock()
                        .ok()
                        .and_then(|guard| guard.clone());
                    let Some(context) = context else {
                        return ToolDecision::Block {
                            reason: "session context is not available".to_string(),
                        };
                    };
                    return match session_runtime_for_hook.inject_context_args(args, context) {
                        Ok(args) => ToolDecision::ReplaceArgs { args },
                        Err(reason) => ToolDecision::Block { reason },
                    };
                }
                if known_memory_tool_ids().contains(&tool) {
                    let context = current_memory_for_hook
                        .lock()
                        .ok()
                        .and_then(|guard| guard.clone());
                    let Some(context) = context else {
                        return ToolDecision::Block {
                            reason: "memory context is not available".to_string(),
                        };
                    };
                    return match memory_runtime_for_hook.inject_context_args(args, context) {
                        Ok(args) => ToolDecision::ReplaceArgs { args },
                        Err(reason) => ToolDecision::Block { reason },
                    };
                }
                ToolDecision::Proceed
            })
            .open(resume_id)
            .await?;
        let thread_id = session.id().as_str().to_string();
        let now = current_time_ms();
        let existing_identity = self.session_runtime.store().identity(&thread_id).await?;
        let created_at_ms = existing_identity
            .as_ref()
            .map_or(now, |identity| identity.created_at_ms);
        // Prefer a resumed identity's project id; otherwise seed it from the
        // runner's project so project/agent memory has a namespace on a fresh run.
        let project_id = existing_identity
            .as_ref()
            .and_then(|identity| identity.project_id.clone())
            .or_else(|| self.project_id.clone());
        let identity = SessionIdentity {
            thread_id: thread_id.clone(),
            agent_id,
            model,
            run_id: None,
            turn_id: None,
            principal_key: Some("local:agenkitty".to_string()),
            tool_ids,
            max_steps_per_turn,
            capture_policy: "full".to_string(),
            transcript_store: self.transcript_store,
            metadata_store: self.session_runtime.store().kind(),
            created_at_ms,
            last_active_at_ms: now,
            project_id,
        };
        self.session_runtime
            .store()
            .upsert_identity(identity.clone())
            .await?;
        let report_identity = identity.clone();
        if let Ok(mut current) = current_session.lock() {
            *current = Some(CurrentSessionContext { identity });
        }
        if let Ok(mut current) = current_memory.lock() {
            *current = Some(CurrentMemoryContext {
                project_id: report_identity.project_id.clone().unwrap_or_default(),
                agent_id: report_identity.agent_id.clone(),
                thread_id: Some(thread_id.clone()),
            });
        }

        let started = FrameworkEvent::started(format!("started session {thread_id}"));
        self.session_runtime
            .store()
            .append_event(
                &thread_id,
                session_event_from_framework(&started, current_time_ms()),
            )
            .await?;
        let mut events = vec![started];
        let mut stream = session.prompt(options.prompt);
        while let Some(event) = stream.next().await {
            let event = map_agent_event(event);
            self.session_runtime
                .store()
                .append_event(
                    &thread_id,
                    session_event_from_framework(&event, current_time_ms()),
                )
                .await?;
            events.push(event);
        }
        if let Ok(mut current) = current_session.lock() {
            *current = None;
        }
        if let Ok(mut current) = current_memory.lock() {
            *current = None;
        }
        Ok(AgentRunReport {
            thread_id,
            session: report_identity,
            events,
        })
    }
}

fn mock_builder() -> AgenkitBuilder {
    Agenkit::builder()
        .provider(MockProvider::new("local").default_text("hello from agenkitty mock"))
        .default_model(ModelRef::new("local/default"))
}

/// Derive a stable project id from a project root: the canonical path (so two
/// worktrees of the same checkout, or relative vs absolute invocations, agree),
/// falling back to the given path if it cannot be canonicalized.
fn project_id_from_root(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn map_agent_event(event: AgentEvent) -> FrameworkEvent {
    match event {
        AgentEvent::Started => FrameworkEvent::started("turn started"),
        AgentEvent::AssistantText { text } => {
            FrameworkEvent::assistant_text(redact_text_to_limit(&text, 4096))
        }
        AgentEvent::ToolStarted { id, tool, args } => FrameworkEvent::tool_started(tool.clone())
            .with_payload(serde_json::json!({
                "call_id": id,
                "args": redact_json_value(&args, 2048),
            }))
            .with_source_ref(tool_call_ref(&id, &tool)),
        AgentEvent::ToolCompleted { id, tool, output } => {
            FrameworkEvent::tool_completed(tool.clone())
                .with_payload(serde_json::json!({
                    "call_id": id,
                    "output": redact_json_value(&output, 2048),
                }))
                .with_source_ref(tool_call_ref(&id, &tool))
        }
        AgentEvent::ToolFailed { id, tool, error } => {
            FrameworkEvent::tool_failed(tool.clone(), redact_text_to_limit(&error, 4096))
                .with_payload(serde_json::json!({ "call_id": id }))
                .with_source_ref(tool_call_ref(&id, &tool))
        }
        AgentEvent::ToolBlocked { id, tool, reason } => {
            FrameworkEvent::tool_blocked(tool.clone(), redact_text_to_limit(&reason, 4096))
                .with_payload(serde_json::json!({ "call_id": id }))
                .with_source_ref(tool_call_ref(&id, &tool))
        }
        AgentEvent::Compacted { folded } => FrameworkEvent::compacted(folded),
        AgentEvent::Stopped { reason } => FrameworkEvent::stopped(match reason {
            StopReason::Idle => RunStatus::Idle,
            StopReason::MaxSteps => RunStatus::MaxSteps,
            StopReason::Aborted => RunStatus::Aborted,
            _ => RunStatus::Unknown,
        }),
        AgentEvent::Failed { error } => FrameworkEvent::failed(redact_text_to_limit(&error, 4096)),
        _ => FrameworkEvent::unknown("unknown agenkit event"),
    }
}

fn tool_call_ref(call_id: &str, tool_id: &str) -> SessionSourceRef {
    SessionSourceRef::ToolCall {
        call_id: call_id.to_string(),
        tool_id: tool_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn map_agent_event_redacts_assistant_text_in_report_events() {
        let event = map_agent_event(AgentEvent::AssistantText {
            text: "api_key=secret".to_string(),
        });

        assert_eq!(event.message.as_deref(), Some("[redacted]"));
    }

    #[tokio::test]
    async fn mock_runner_produces_assistant_text() {
        let report = FrameworkRunner::mock()
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Answer briefly.".to_string(),
                prompt: "hello".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: Vec::new(),
            })
            .await
            .unwrap();
        assert!(
            report
                .events
                .iter()
                .any(|event| event.message.as_deref() == Some("hello from agenkitty mock"))
        );
    }

    #[tokio::test]
    async fn mock_runner_executes_fs_read_tool() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "hello from repo\n").unwrap();
        let provider = MockProvider::new("local")
            .on_prompt_tool(
                "read note",
                "fs.read",
                serde_json::json!({ "path": "note.txt", "start_line": 1, "max_lines": 1 }),
            )
            .default_text("done");
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"));
        let runner = FrameworkRunner::from_builder_with_repo_tools(builder, dir.path()).unwrap();

        let report = runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "read note".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["fs.read".to_string()],
            })
            .await
            .unwrap();

        assert!(
            report
                .events
                .iter()
                .any(|event| event.tool.as_deref() == Some("fs.read"))
        );
        let persisted = runner
            .session_runtime
            .store()
            .list_events(
                &report.thread_id,
                crate::tools::SessionEventFilter {
                    after_seq: None,
                    start_seq: None,
                    end_seq: None,
                    limit: 20,
                    kinds: vec![agenkitty_core::SessionEventKind::ToolCompleted],
                },
            )
            .await
            .unwrap();
        assert!(
            persisted
                .iter()
                .any(|event| event.tool.as_deref() == Some("fs.read"))
        );
    }

    #[tokio::test]
    async fn mock_runner_executes_session_info_tool() {
        let provider = MockProvider::new("local")
            .on_prompt_tool("inspect session", "session.info", serde_json::json!({}))
            .default_text("done");
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"));
        let dir = tempfile::tempdir().unwrap();
        let runner = FrameworkRunner::from_builder_with_repo_tools(builder, dir.path()).unwrap();

        let report = runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "inspect session".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["session.info".to_string()],
            })
            .await
            .unwrap();

        assert!(
            report
                .events
                .iter()
                .any(|event| event.tool.as_deref() == Some("session.info"))
        );
        assert!(!report.failed());
    }

    #[tokio::test]
    async fn durable_runner_resumes_thread_and_metadata_by_id() {
        let project = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let first_runner =
            FrameworkRunner::mock_for_project_with_session_root(project.path(), sessions.path())
                .unwrap();
        let first = first_runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Answer briefly.".to_string(),
                prompt: "first".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: Vec::new(),
            })
            .await
            .unwrap();
        let first_identity = first_runner
            .session_runtime
            .store()
            .identity(&first.thread_id)
            .await
            .unwrap()
            .unwrap();
        let first_event_count = first_runner
            .session_runtime
            .store()
            .event_count(&first.thread_id)
            .await
            .unwrap();

        let second_runner =
            FrameworkRunner::mock_for_project_with_session_root(project.path(), sessions.path())
                .unwrap();
        let second = second_runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Answer briefly.".to_string(),
                prompt: "second".to_string(),
                thread_id: Some(first.thread_id.clone()),
                max_steps_per_turn: 4,
                tool_ids: Vec::new(),
            })
            .await
            .unwrap();
        let second_identity = second_runner
            .session_runtime
            .store()
            .identity(&first.thread_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(second.thread_id, first.thread_id);
        assert_eq!(first_identity.created_at_ms, second_identity.created_at_ms);
        assert_eq!(
            second_identity.transcript_store,
            SessionStoreKind::LocalJsonl
        );
        assert_eq!(second_identity.metadata_store, SessionStoreKind::LocalJsonl);
        assert_eq!(second.session.metadata_store, SessionStoreKind::LocalJsonl);
        assert_eq!(
            second.session.principal_key.as_deref(),
            Some("local:agenkitty")
        );
        assert!(
            second_runner
                .session_runtime
                .store()
                .event_count(&first.thread_id)
                .await
                .unwrap()
                > first_event_count
        );
    }

    #[tokio::test]
    async fn mock_runner_executes_patch_apply_tool_in_qwen_example_copy() {
        let temp = tempfile::tempdir().unwrap();
        let example_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/agenkitty-qwen")
            .canonicalize()
            .unwrap();
        let project_root = temp.path().join("agenkitty-qwen");
        copy_dir_all(&example_root, &project_root).unwrap();

        let provider = MockProvider::new("local")
            .on_prompt_tool(
                "patch example",
                "patch.apply",
                serde_json::json!({
                    "patch": "*** Begin Patch\n*** Add File: PATCH_SMOKE.md\n+patched from mock provider\n*** End Patch\n"
                }),
            )
            .default_text("done");
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"));
        let runner = FrameworkRunner::from_builder_with_repo_tools(builder, &project_root).unwrap();

        let report = runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "patch example".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["patch.apply".to_string()],
            })
            .await
            .unwrap();

        assert!(
            report
                .events
                .iter()
                .any(|event| event.tool.as_deref() == Some("patch.apply"))
        );
        let persisted = runner
            .session_runtime
            .store()
            .list_events(
                &report.thread_id,
                crate::tools::SessionEventFilter {
                    after_seq: None,
                    start_seq: None,
                    end_seq: None,
                    limit: 20,
                    kinds: vec![agenkitty_core::SessionEventKind::ToolCompleted],
                },
            )
            .await
            .unwrap();
        assert!(
            persisted
                .iter()
                .any(|event| event.tool.as_deref() == Some("patch.apply"))
        );
        assert_eq!(
            fs::read_to_string(project_root.join("PATCH_SMOKE.md")).unwrap(),
            "patched from mock provider\n"
        );
    }

    fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let target = destination.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir_all(&entry.path(), &target)?;
            } else {
                fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }
}
