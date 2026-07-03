use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::StreamExt;
use pocopine_agenkit::prelude::{Agenkit, AgenkitBuilder, ModelRef};
use pocopine_agenkit::server::SecretString;
use pocopine_agenkit::server::session::JsonlSessionStore;
use pocopine_agenkit::server::{
    AgentConfig, AgentEvent, AgentSession, AuthUser, MockProvider, Principal, SessionThreadStore,
    StopReason, ToolCall, ToolDecision,
};
use pocopine_agenkit_core::AgentThreadId;
use pocopine_agenkit_oai::OpenAiProvider;

use crate::events::{FrameworkEvent, RunStatus};
use crate::policy::{ToolApprover, no_approver_reason};
use crate::project::load_project_config;
use crate::tools::session::{redact_json_value, redact_text_to_limit};
use crate::tools::{
    ArtifactRuntime, ArtifactScope, CurrentArtifactContext, CurrentMemoryContext,
    CurrentSessionContext, InMemoryArtifactStore, LocalArtifactStore, LocalJsonlMemoryStore,
    LocalJsonlSessionMetadataStore, MemoryRuntime, SecretRuntime, SessionRuntime,
    builtin_tool_specs, current_time_ms, known_artifact_tool_ids, known_memory_tool_ids,
    known_session_tool_ids, register_memory_tools, register_session_tools,
    register_tools_with_all_runtimes_and_artifacts, session_event_from_framework,
};
use agenkitty_core::config::PolicyConfigSection;
use agenkitty_core::{
    ApprovalDecision, ApprovalRequest, PolicyDecision, PolicyEvaluator, SessionArtifactLink,
    SessionIdentity, SessionSourceRef, SessionStoreKind,
};

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
    artifact_runtime: Arc<ArtifactRuntime>,
    /// The secret runtime registered with the tools. Retained so `with_approver`
    /// can share the runner's one approver with the secret-grant gate. Empty
    /// (no resolver) for runners that don't register secret tools.
    secret_runtime: Arc<SecretRuntime>,
    /// Stable project id for this runner, derived from the canonical project
    /// root. `None` for project-less runners (mock / bare provider). Used to seed
    /// `SessionIdentity::project_id` on a fresh run so project/agent memory has a
    /// namespace.
    project_id: Option<String>,
    transcript_store: SessionStoreKind,
    /// The central tool-call policy gate (F1): built-in specs + the project's
    /// `[policy]` overrides, consulted in `before_tool_call` before any
    /// context injection. Project-less runners evaluate under the defaults.
    policy: Arc<PolicyEvaluator>,
    /// The host approver for `Ask` decisions (M1d). `None` = headless: every
    /// Ask fails closed.
    approver: Option<Arc<dyn ToolApprover>>,
}

/// The evaluator for runners without a project config: every tool's
/// spec-declared default mode rules.
fn default_policy() -> Arc<PolicyEvaluator> {
    Arc::new(PolicyEvaluator::new(
        PolicyConfigSection::default(),
        builtin_tool_specs(),
    ))
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
            artifact_runtime: Arc::new(ArtifactRuntime::in_memory()),
            secret_runtime: Arc::new(SecretRuntime::empty()),
            project_id: None,
            transcript_store: SessionStoreKind::InMemory,
            policy: default_policy(),
            approver: None,
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
            artifact_runtime: Arc::new(ArtifactRuntime::in_memory()),
            secret_runtime: Arc::new(SecretRuntime::empty()),
            project_id: None,
            transcript_store: SessionStoreKind::InMemory,
            policy: default_policy(),
            approver: None,
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
        let policy = Arc::new(PolicyEvaluator::new(
            load_project_config(root.as_ref())?.policy,
            builtin_tool_specs(),
        ));
        let session_runtime = Arc::new(SessionRuntime::in_memory());
        let memory_runtime = Arc::new(MemoryRuntime::in_memory());
        let artifact_runtime = Arc::new(ArtifactRuntime::new(Arc::new(
            InMemoryArtifactStore::new().with_workspace_root(root.as_ref()),
        )));
        let secret_runtime = Arc::new(SecretRuntime::empty());
        let agenkit = register_tools_with_all_runtimes_and_artifacts(
            builder,
            root,
            session_runtime.clone(),
            memory_runtime.clone(),
            secret_runtime.clone(),
            artifact_runtime.clone(),
        )?
        .build()?;
        Ok(Self {
            agenkit,
            session_runtime,
            memory_runtime,
            artifact_runtime,
            secret_runtime,
            project_id,
            transcript_store: SessionStoreKind::InMemory,
            policy,
            approver: None,
        })
    }

    fn from_builder_with_repo_tools_and_session_root(
        builder: AgenkitBuilder,
        root: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let project_id = Some(project_id_from_root(root.as_ref()));
        let policy = Arc::new(PolicyEvaluator::new(
            load_project_config(root.as_ref())?.policy,
            builtin_tool_specs(),
        ));
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
        // Durable artifacts live beside the session data; linked artifacts
        // resolve against the project workspace.
        let artifact_runtime = Arc::new(ArtifactRuntime::new(Arc::new(
            LocalArtifactStore::open(session_root.join("artifacts"))?
                .with_workspace_root(root.as_ref()),
        )));
        let secret_runtime = Arc::new(SecretRuntime::empty());
        let agenkit = register_tools_with_all_runtimes_and_artifacts(
            builder.thread_store(thread_store),
            root,
            session_runtime.clone(),
            memory_runtime.clone(),
            secret_runtime.clone(),
            artifact_runtime.clone(),
        )?
        .build()?;
        Ok(Self {
            agenkit,
            session_runtime,
            memory_runtime,
            artifact_runtime,
            secret_runtime,
            project_id,
            transcript_store: SessionStoreKind::LocalJsonl,
            policy,
            approver: None,
        })
    }

    /// Install the host approver for `Ask` policy decisions. One approver is
    /// shared across every gate: the central dispatch gate (`self.approver`),
    /// the artifact runtime (project-scoped writes), and the secret runtime
    /// (Ask-mode grant requests) — so an interactive run can approve any of
    /// them, and none silently fails closed for lack of wiring.
    pub fn with_approver(mut self, approver: Arc<dyn ToolApprover>) -> Self {
        self.artifact_runtime.set_approver(approver.clone());
        self.secret_runtime.set_approver(approver.clone());
        self.approver = Some(approver);
        self
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
        let current_artifact: Arc<Mutex<Option<CurrentArtifactContext>>> =
            Arc::new(Mutex::new(None));
        let session_runtime_for_hook = self.session_runtime.clone();
        let memory_runtime_for_hook = self.memory_runtime.clone();
        let artifact_runtime_for_hook = self.artifact_runtime.clone();
        let current_session_for_hook = current_session.clone();
        let current_memory_for_hook = current_memory.clone();
        let current_artifact_for_hook = current_artifact.clone();
        let policy_for_hook = self.policy.clone();
        let approver_for_hook = self.approver.clone();
        let session = AgentSession::builder(&self.agenkit)
            .agent_id(agent_id.clone())
            .principal(principal)
            .config(config)
            .before_tool_call(move |call: &ToolCall| {
                let policy = policy_for_hook.clone();
                let approver = approver_for_hook.clone();
                let session_runtime = session_runtime_for_hook.clone();
                let memory_runtime = memory_runtime_for_hook.clone();
                let artifact_runtime = artifact_runtime_for_hook.clone();
                let current_session = current_session_for_hook.clone();
                let current_memory = current_memory_for_hook.clone();
                let current_artifact = current_artifact_for_hook.clone();
                Box::pin(async move {
                    let tool = call.tool_id.as_str();
                    // The central policy gate (F1) runs first: a denied call
                    // never reaches context injection or a runtime. `Ask`
                    // resolves through the host approver (M1d) and fails
                    // closed without one — the same semantics the secret
                    // runtime uses. The dispatch evaluator only ever returns
                    // Allow / Ask / Deny (argument rewriting is the
                    // context-injection layer's job below, not the policy
                    // decision's), so `Rewrite` is not produced here.
                    match policy.evaluate_call(call) {
                        PolicyDecision::Deny { reason } => return ToolDecision::Block { reason },
                        PolicyDecision::Ask { reason } => {
                            let Some(approver) = approver else {
                                return ToolDecision::Block {
                                    reason: no_approver_reason(&reason),
                                };
                            };
                            let request = ApprovalRequest::new(tool, reason)
                                .with_call_id(call.id.clone())
                                .with_detail(call.args.clone());
                            match approver.approve(request).await {
                                ApprovalDecision::Approved => {}
                                ApprovalDecision::Denied { reason } => {
                                    return ToolDecision::Block { reason };
                                }
                            }
                        }
                        _ => {}
                    }
                    let args = &call.args;
                    // Session and memory tools each need their own
                    // runtime-injected context_token; everything else proceeds
                    // with the approved arguments.
                    if known_session_tool_ids().contains(&tool) {
                        let context = current_session.lock().ok().and_then(|guard| guard.clone());
                        let Some(context) = context else {
                            return ToolDecision::Block {
                                reason: "session context is not available".to_string(),
                            };
                        };
                        return match session_runtime.inject_context_args(args, context) {
                            Ok(args) => ToolDecision::ReplaceArgs { args },
                            Err(reason) => ToolDecision::Block { reason },
                        };
                    }
                    if known_memory_tool_ids().contains(&tool) {
                        let context = current_memory.lock().ok().and_then(|guard| guard.clone());
                        let Some(context) = context else {
                            return ToolDecision::Block {
                                reason: "memory context is not available".to_string(),
                            };
                        };
                        return match memory_runtime.inject_context_args(args, context) {
                            Ok(args) => ToolDecision::ReplaceArgs { args },
                            Err(reason) => ToolDecision::Block { reason },
                        };
                    }
                    // net.download stores into the artifact runtime, so it takes
                    // the same caller-derived artifact context_token as the
                    // artifact.* tools (a host must also register it — it is
                    // opt-in and not in the default set).
                    if known_artifact_tool_ids().contains(&tool)
                        || tool == crate::tools::NET_DOWNLOAD_TOOL_ID
                    {
                        let context = current_artifact.lock().ok().and_then(|guard| guard.clone());
                        let Some(context) = context else {
                            return ToolDecision::Block {
                                reason: "artifact context is not available".to_string(),
                            };
                        };
                        return match artifact_runtime.inject_context_args(args, context) {
                            Ok(args) => ToolDecision::ReplaceArgs { args },
                            Err(reason) => ToolDecision::Block { reason },
                        };
                    }
                    ToolDecision::Proceed
                })
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
        if let Ok(mut current) = current_artifact.lock() {
            *current = Some(CurrentArtifactContext {
                project_id: report_identity.project_id.clone().unwrap_or_default(),
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
        if let Ok(mut current) = current_artifact.lock() {
            *current = None;
        }

        // Run-end cleanup seam: link this run's session-scoped artifacts into
        // the session metadata so a session's outputs are discoverable via
        // `SessionExport.artifact_links`. Process-handle reaping (a session-
        // aware process sandbox) will also hang here — see [`finalize_run`].
        // The run has already completed and its events are persisted, so a
        // best-effort cleanup failure (transient IO, disk full) must NOT
        // discard the report: log and return it anyway.
        if let Err(err) = self.finalize_run(&thread_id).await {
            tracing::warn!(
                target: "pocopine.log",
                thread_id = %thread_id,
                error = %err,
                "run-end artifact linking failed; report is still returned"
            );
        }

        Ok(AgentRunReport {
            thread_id,
            session: report_identity,
            events,
        })
    }

    /// The run-end cleanup seam. Today it links the run's session-scoped
    /// artifacts into the session metadata store (deduped against existing
    /// links, so a resumed session doesn't double-link). It deliberately does
    /// **not** `close_session` — a run may be resumed, and marking a resumable
    /// session closed every run is wrong; explicit close stays
    /// `SessionHost::close`. Process-handle reaping is deferred: it needs the
    /// process sandbox to become session-aware (a thread key on each handle +
    /// a reap-by-session method + exposing the table to the runner), which is
    /// its own unit rather than a change smuggled into this seam.
    async fn finalize_run(&self, thread_id: &str) -> Result<()> {
        let accessible = vec![(ArtifactScope::Session, thread_id.to_string())];
        let artifacts = self
            .artifact_runtime
            .store()
            .list(
                &accessible,
                Some(ArtifactScope::Session),
                ARTIFACT_LINK_SCAN_LIMIT,
            )
            .await?;
        if artifacts.is_empty() {
            return Ok(());
        }
        let already_linked: std::collections::HashSet<String> = self
            .session_runtime
            .store()
            .list_artifact_links(thread_id)
            .await?
            .into_iter()
            .map(|link| link.artifact_id)
            .collect();
        for artifact in artifacts {
            if already_linked.contains(&artifact.id) {
                continue;
            }
            self.session_runtime
                .store()
                .link_artifact(
                    thread_id,
                    SessionArtifactLink {
                        artifact_id: artifact.id,
                        source_refs: artifact.source_refs,
                        promotion_policy: None,
                        created_at_ms: artifact.created_at_ms,
                    },
                )
                .await?;
        }
        Ok(())
    }
}

/// Upper bound on artifacts linked into the session at run end (a safety cap on
/// a pathological run that produced an unbounded number of artifacts).
const ARTIFACT_LINK_SCAN_LIMIT: usize = 1_000;

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
            .with_call_id(id.clone())
            .with_payload(serde_json::json!({
                "call_id": id,
                "args": redact_json_value(&args, 2048),
            }))
            .with_source_ref(tool_call_ref(&id, &tool)),
        AgentEvent::ToolCompleted { id, tool, output } => {
            FrameworkEvent::tool_completed(tool.clone())
                .with_call_id(id.clone())
                .with_payload(serde_json::json!({
                    "call_id": id,
                    "output": redact_json_value(&output, 2048),
                }))
                .with_source_ref(tool_call_ref(&id, &tool))
        }
        AgentEvent::ToolFailed { id, tool, error } => {
            FrameworkEvent::tool_failed(tool.clone(), redact_text_to_limit(&error, 4096))
                .with_call_id(id.clone())
                .with_payload(serde_json::json!({ "call_id": id }))
                .with_source_ref(tool_call_ref(&id, &tool))
        }
        AgentEvent::ToolBlocked { id, tool, reason } => {
            FrameworkEvent::tool_blocked(tool.clone(), redact_text_to_limit(&reason, 4096))
                .with_call_id(id.clone())
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

    #[test]
    fn tool_output_carrying_a_secret_is_redacted_in_the_event() {
        // Every tool's output flows through map_agent_event -> the session
        // redactor, which uses the shared F3 classifier. So a secret in a
        // process/fs/patch tool result is redacted before it persists — the
        // transitive coverage for the tools that legitimately return workspace
        // content (they must not be corrupted at the source, only redacted here).
        let event = map_agent_event(AgentEvent::ToolCompleted {
            id: "call-1".to_string(),
            tool: "process.run".to_string(),
            output: serde_json::json!({ "stdout": "export TOKEN=bearer sk-live-123" }),
        });
        let payload = event.payload.expect("tool payload");
        assert_eq!(payload["output"]["stdout"], "[redacted]");
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

    async fn run_fs_write_prompt(dir: &std::path::Path) -> AgentRunReport {
        let provider = MockProvider::new("local")
            .on_prompt_tool(
                "write note",
                "fs.write",
                serde_json::json!({ "path": "note.txt", "content": "hello" }),
            )
            .default_text("done");
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"));
        let runner = FrameworkRunner::from_builder_with_repo_tools(builder, dir).unwrap();
        runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "write note".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["fs.write".to_string()],
            })
            .await
            .unwrap()
    }

    fn tool_blocked_event(report: &AgentRunReport) -> &FrameworkEvent {
        report
            .events
            .iter()
            .find(|event| matches!(event.kind, crate::events::FrameworkEventKind::ToolBlocked))
            .expect("a ToolBlocked event")
    }

    #[tokio::test]
    async fn policy_asks_fail_closed_without_an_approver() {
        // fs.write's spec defaults to Ask; with no project override and no
        // approver, the call must be blocked before the tool runs.
        let dir = tempfile::tempdir().unwrap();
        let report = run_fs_write_prompt(dir.path()).await;

        let blocked = tool_blocked_event(&report);
        assert_eq!(blocked.tool.as_deref(), Some("fs.write"));
        assert!(
            blocked.call_id.as_deref().is_some_and(|id| !id.is_empty()),
            "blocked event must carry the invocation id"
        );
        assert!(!dir.path().join("note.txt").exists(), "tool must not run");
    }

    #[tokio::test]
    async fn project_write_mode_deny_blocks_fs_write_at_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        fs::write(
            dir.path().join(".agenkitty").join("config.toml"),
            "[policy]\nwrite_mode = \"deny\"\n",
        )
        .unwrap();
        let report = run_fs_write_prompt(dir.path()).await;

        let blocked = tool_blocked_event(&report);
        assert_eq!(blocked.tool.as_deref(), Some("fs.write"));
        assert!(blocked.call_id.is_some());
        assert!(!dir.path().join("note.txt").exists(), "tool must not run");
    }

    #[tokio::test]
    async fn approver_approval_lets_an_ask_tool_run() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new("local")
            .on_prompt_tool(
                "write note",
                "fs.write",
                serde_json::json!({ "path": "note.txt", "content": "hello" }),
            )
            .default_text("done");
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"));
        let runner = FrameworkRunner::from_builder_with_repo_tools(builder, dir.path())
            .unwrap()
            .with_approver(Arc::new(crate::policy::StaticApprover(true)));
        let report = runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "write note".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["fs.write".to_string()],
            })
            .await
            .unwrap();

        assert!(report.events.iter().any(|event| {
            matches!(event.kind, crate::events::FrameworkEventKind::ToolCompleted)
                && event.tool.as_deref() == Some("fs.write")
        }));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn approver_denial_blocks_an_ask_tool() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new("local")
            .on_prompt_tool(
                "write note",
                "fs.write",
                serde_json::json!({ "path": "note.txt", "content": "hello" }),
            )
            .default_text("done");
        let builder = Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"));
        let runner = FrameworkRunner::from_builder_with_repo_tools(builder, dir.path())
            .unwrap()
            .with_approver(Arc::new(crate::policy::StaticApprover(false)));
        let report = runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "write note".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["fs.write".to_string()],
            })
            .await
            .unwrap();

        let blocked = tool_blocked_event(&report);
        assert_eq!(blocked.tool.as_deref(), Some("fs.write"));
        assert!(
            blocked
                .message
                .as_deref()
                .is_some_and(|reason| reason.contains("denied by operator"))
        );
        assert!(!dir.path().join("note.txt").exists(), "tool must not run");
    }

    #[tokio::test]
    async fn project_write_mode_allow_loosens_fs_write() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        fs::write(
            dir.path().join(".agenkitty").join("config.toml"),
            "[policy]\nwrite_mode = \"allow\"\n",
        )
        .unwrap();
        let report = run_fs_write_prompt(dir.path()).await;

        assert!(
            report.events.iter().any(|event| {
                matches!(event.kind, crate::events::FrameworkEventKind::ToolCompleted)
                    && event.tool.as_deref() == Some("fs.write")
            }),
            "fs.write must complete under write_mode = allow"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn invalid_project_config_fails_runner_construction() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        fs::write(
            dir.path().join(".agenkitty").join("config.toml"),
            "[policy]\nwrite_mode = \"yolo\"\n",
        )
        .unwrap();
        let builder = Agenkit::builder()
            .provider(MockProvider::new("local").default_text("hi"))
            .default_model(ModelRef::new("local/default"));
        assert!(FrameworkRunner::from_builder_with_repo_tools(builder, dir.path()).is_err());
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
    async fn mock_runner_executes_artifact_write_tool() {
        // artifact.write is Write-class / Allow by default (session scope), so
        // it dispatches without prompts and the context token round-trips.
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new("local")
            .on_prompt_tool(
                "save report",
                "artifact.write",
                serde_json::json!({
                    "name": "report.md",
                    "content": "# Findings",
                    "scope": "session"
                }),
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
                prompt: "save report".to_string(),
                thread_id: None,
                max_steps_per_turn: 4,
                tool_ids: vec!["artifact.write".to_string()],
            })
            .await
            .unwrap();

        assert!(report.events.iter().any(|event| {
            matches!(event.kind, crate::events::FrameworkEventKind::ToolCompleted)
                && event.tool.as_deref() == Some("artifact.write")
        }));
        // The artifact landed in the runner's store under the session namespace.
        let accessible = vec![(
            crate::tools::ArtifactScope::Session,
            report.thread_id.clone(),
        )];
        let stored = runner
            .artifact_runtime
            .store()
            .list(&accessible, None, 10)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "report.md");
        // Artifact side of the round-trip: the artifact records its session.
        assert!(stored[0].source_refs.contains(&SessionSourceRef::Thread {
            thread_id: report.thread_id.clone(),
        }));
        // Session side: finalize_run linked the artifact into session metadata,
        // so it's discoverable via the session's artifact links / export.
        let links = runner
            .session_runtime
            .store()
            .list_artifact_links(&report.thread_id)
            .await
            .unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].artifact_id, stored[0].id);
        assert!(links[0].source_refs.contains(&SessionSourceRef::Thread {
            thread_id: report.thread_id.clone(),
        }));

        // Idempotent across a resume: re-running the same thread does not
        // double-link the already-linked artifact.
        let resumed = runner
            .run_prompt(AgentRunOptions {
                agent_id: "test".to_string(),
                model: "local/default".to_string(),
                system: "Use tools when needed.".to_string(),
                prompt: "noop".to_string(),
                thread_id: Some(report.thread_id.clone()),
                max_steps_per_turn: 4,
                tool_ids: vec!["artifact.write".to_string()],
            })
            .await
            .unwrap();
        let links = runner
            .session_runtime
            .store()
            .list_artifact_links(&resumed.thread_id)
            .await
            .unwrap();
        assert_eq!(links.len(), 1, "resume must not double-link");
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
        // patch.apply is write-class (default Ask, fails closed headless);
        // this test drives the mutation itself, so its project loosens the
        // class explicitly.
        fs::write(
            project_root.join(".agenkitty").join("config.toml"),
            "[policy]\nwrite_mode = \"allow\"\n",
        )
        .unwrap();

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
