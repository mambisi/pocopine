//! The long-lived, multi-turn **agent runtime** (the `kitty` runtime — Layer 2 /
//! pi's `pi-agent-core`).
//!
//! agenkit's [`AgentRun`](super::agent::AgentRun) is a *single-shot, flow-scoped*
//! loop: typed `input → run → typed output`, inside one request. This module is
//! the **conversational** counterpart: an [`AgentSession`] you `open` once and
//! [`prompt`](AgentSession::prompt) over time, watching a typed [`AgentEvent`]
//! stream and (L3) steering it with hooks. It runs on the **W7 session layer**
//! ([`super::session`]) for durability, branching, and resume.
//!
//! Layering of this build: **L1** = the loop core ([`AgentLoop`]) + the event
//! firehose; **L2** = durable [`AgentSession`] (resume / branch / compaction);
//! **L3** = steering queue + typed hooks + abort. The event/decision types are
//! shaped to **pre-image the WASM/WIT extension world** (parked) so extensions
//! later plug into a contract that already exists here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, AgentThreadId, Message, ModelRef, Role, ThreadMessage,
    ThreadRetention, ToolDescriptor,
};
use pocopine_auth::Principal;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::agenkit::{Agenkit, AgenkitInner};
use super::context::AiContext;
use super::provider::{GenerateRequest, ProviderContext};
use super::thread::{AgentThreadHandle, ThreadOwner};

/// How a [`prompt`](AgentSession::prompt) turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// The model answered with no tool calls (and no queued follow-up) — the
    /// conversation is idle, waiting for the next prompt.
    Idle,
    /// The per-turn model↔tool step budget was hit.
    MaxSteps,
    /// An [`AbortHandle`] cancelled the turn (checked between steps).
    Aborted,
}

/// A host-side firehose event from a running turn. Richer than the redacted
/// wire [`FlowStreamEvent`](pocopine_agenkit_core::FlowStreamEvent) — this is the
/// **trusted, in-process** view (a dev observing the agent). The variants mirror
/// the (parked) `pocopine:agent/extension` WIT `agent-event` so the extension
/// world is a projection of this contract.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A prompt was accepted; the loop begins.
    Started,
    /// A model text response within the turn (may precede tool calls).
    AssistantText {
        /// The assistant's text.
        text: String,
    },
    /// A tool call is about to run.
    ToolStarted {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// The call arguments.
        args: serde_json::Value,
    },
    /// A tool call returned.
    ToolCompleted {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// The tool's output.
        output: serde_json::Value,
    },
    /// A tool call failed; the error is fed back to the model (the loop
    /// continues) rather than aborting the conversation.
    ToolFailed {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// The error (stable kind/text — no provider internals).
        error: String,
    },
    /// A `before_tool_call` hook blocked a tool (e.g. an approval/trust gate);
    /// the block reason is fed back to the model instead of running the tool.
    ToolBlocked {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// Why the hook blocked it.
        reason: String,
    },
    /// The thread was compacted (L2): `folded` older messages → one summary.
    Compacted {
        /// How many messages were folded into the summary.
        folded: u64,
    },
    /// Terminal: the turn finished successfully.
    Stopped {
        /// Why the turn ended.
        reason: StopReason,
    },
    /// Terminal: the turn failed.
    Failed {
        /// The error kind/message.
        error: String,
    },
}

/// Configuration for a conversational agent: the same knobs as
/// [`AiAgentBuilder`](super::agent::AiAgentBuilder) minus typed I/O (the runtime
/// is a free-text conversation, not a typed flow unit).
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub(crate) model: Option<ModelRef>,
    pub(crate) system: Option<String>,
    pub(crate) tool_ids: Vec<String>,
    pub(crate) max_tokens: Option<u32>,
    pub(crate) max_steps_per_turn: u32,
}

impl Default for AgentConfig {
    /// No model/system/tools, an 8-step per-turn budget. Hand-written (not
    /// derived) so the step budget defaults to 8, not 0 — a derived `0` would
    /// make a loop run zero model calls and silently "succeed".
    fn default() -> Self {
        Self {
            model: None,
            system: None,
            tool_ids: Vec::new(),
            max_tokens: None,
            max_steps_per_turn: 8,
        }
    }
}

impl AgentConfig {
    /// A config with the default per-turn step budget (8).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the model (defaults to the runtime default).
    pub fn model(mut self, model: impl Into<ModelRef>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the system prompt.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Allow several tools (by registry id).
    pub fn tools<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tool_ids.extend(ids.into_iter().map(Into::into));
        self
    }

    /// Cap output tokens per model call.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Bound the model↔tool steps *within one turn* (default 8). A turn ends
    /// early when the model stops calling tools; this is the runaway guard.
    pub fn max_steps_per_turn(mut self, steps: u32) -> Self {
        self.max_steps_per_turn = steps.max(1);
        self
    }
}

/// A `before_tool_call` hook's decision for one tool call (L3). Mirrors the
/// (parked) WIT `hook-decision` so the extension world projects onto it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ToolDecision {
    /// Run the tool with its requested arguments.
    Proceed,
    /// Don't run the tool; feed `reason` back to the model (an approval/trust
    /// gate, or a policy block).
    Block {
        /// Why the call is blocked.
        reason: String,
    },
    /// Run the tool, but with these arguments instead (e.g. inject a sandbox
    /// path, redact a field).
    ReplaceArgs {
        /// The arguments to use.
        args: serde_json::Value,
    },
}

/// Typed steering hooks (L3) — host-side closures that *steer* the loop. The
/// shapes pre-image the WIT extension world so extensions later supply the same
/// decisions across the component boundary.
#[derive(Clone, Default)]
pub(crate) struct Hooks {
    #[allow(clippy::type_complexity)]
    before_tool_call: Option<Arc<dyn Fn(&str, &serde_json::Value) -> ToolDecision + Send + Sync>>,
}

/// A handle to cancel a running turn. Cloneable; calling [`abort`](AbortHandle::abort)
/// stops the loop at the next step boundary (between model/tool calls).
#[derive(Clone)]
pub struct AbortHandle(Arc<std::sync::atomic::AtomicBool>);

impl AbortHandle {
    /// Request cancellation of the in-flight turn.
    pub fn abort(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_aborted(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Per-turn steering controls passed into [`AgentLoop::run_turn`]: the mid-run
/// message queue, the abort flag, and the hooks. [`Default`] is the inert form
/// (empty queue, never aborted, no hooks) — the plain L1 loop.
#[derive(Clone, Default)]
pub(crate) struct TurnControls {
    queue: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    abort: Arc<std::sync::atomic::AtomicBool>,
    hooks: Hooks,
}

impl TurnControls {
    fn aborted(&self) -> bool {
        self.abort.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Pop the next steered follow-up message, if any.
    fn next_steer(&self) -> Option<String> {
        self.queue.lock().ok().and_then(|mut q| q.pop_front())
    }
}

/// The in-memory conversational loop core (L1). It owns no durable state — it
/// runs one model↔tool turn over a `messages` transcript, emitting
/// [`AgentEvent`]s, and returns where it stopped. [`AgentSession`] (L2) wraps it
/// with a W7 session; [`AgentLoop`] is reusable on its own for tests / embedding.
pub struct AgentLoop {
    inner: Arc<AgenkitInner>,
    principal: Principal,
    config: AgentConfig,
}

impl AgentLoop {
    pub(crate) fn new(inner: Arc<AgenkitInner>, principal: Principal, config: AgentConfig) -> Self {
        Self {
            inner,
            principal,
            config,
        }
    }

    /// The resolved model for this loop (config override, else runtime default).
    fn model(&self) -> AgenkitResult<ModelRef> {
        self.config
            .model
            .clone()
            .or_else(|| self.inner.default_model.clone())
            .ok_or_else(|| AgenkitError::config("agent runtime has no model"))
    }

    /// Run one turn over `messages` (which must already include the new user
    /// prompt as its last message): call the model, run any tool calls, re-enter,
    /// and stop when the model answers without tool calls or the step budget is
    /// hit. Appends every produced message (assistant text, tool-call turns, tool
    /// results) to `messages`, emits events, and returns the [`StopReason`].
    pub(crate) async fn run_turn(
        &self,
        messages: &mut Vec<Message>,
        events: &UnboundedSender<AgentEvent>,
        controls: &TurnControls,
    ) -> AgenkitResult<StopReason> {
        let model = self.model()?;
        self.inner.check_model_allowed(&model)?;
        let provider = self.inner.providers.resolve(&model)?;

        let tools: Vec<ToolDescriptor> = self
            .config
            .tool_ids
            .iter()
            .filter_map(|id| self.inner.tools.get(id).map(|t| t.descriptor()))
            .collect();
        if !tools.is_empty() && !provider.capabilities().tools {
            return Err(AgenkitError::config(format!(
                "provider `{}` does not support tool calling",
                provider.id()
            )));
        }

        // Tool-execution context + a credential resolved once for the turn (W6).
        let ctx = AiContext::with_principal(self.inner.state.clone(), self.principal.clone());
        let cx = {
            let credential = self
                .inner
                .credentials
                .resolve(provider.id(), &self.principal)
                .await?;
            ProviderContext::for_request(credential)
        };

        for _ in 0..self.config.max_steps_per_turn.max(1) {
            // Abort is checked between steps (cancels after the current call).
            // A dropped event receiver is treated the same: the consumer left, so
            // stop rather than keep making paid model/tool calls nobody reads.
            if controls.aborted() || events.is_closed() {
                return Ok(StopReason::Aborted);
            }

            let request = GenerateRequest {
                model: model.clone(),
                system: self.config.system.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                json_schema: None, // conversational: free text, not structured output
                max_tokens: self.config.max_tokens,
                thinking: Default::default(),
            };
            let response = provider
                .generate(request, &cx)
                .await
                .map_err(super::generate::reclassify_overflow)?;

            let text = response.text_output();
            if !text.is_empty() {
                let _ = events.send(AgentEvent::AssistantText { text: text.clone() });
            }

            if response.tool_calls.is_empty() {
                // The model answered. Persist the assistant text so the
                // conversation replays on resume.
                messages.push(Message::new(Role::Assistant, text));
                // Steering: a queued follow-up message continues the turn instead
                // of going idle ("talk to it mid-run"); otherwise the turn ends.
                match controls.next_steer() {
                    Some(steer) => {
                        messages.push(Message::user(steer));
                        continue;
                    }
                    None => return Ok(StopReason::Idle),
                }
            }

            // Record the assistant's tool-request turn (keeps the provider's
            // protocol linkage that real providers require on the next request).
            messages.push(Message::assistant_tool_calls(
                response.content.clone(),
                response.tool_calls.clone(),
            ));

            for call in &response.tool_calls {
                if !self.config.tool_ids.iter().any(|id| id == &call.tool_id) {
                    return Err(AgenkitError::tool_policy(format!(
                        "agent called non-allowlisted tool `{}`",
                        call.tool_id
                    )));
                }
                let tool = self.inner.tools.get(&call.tool_id).ok_or_else(|| {
                    AgenkitError::tool_policy(format!("tool `{}` is not registered", call.tool_id))
                })?;
                let _ = events.send(AgentEvent::ToolStarted {
                    id: call.id.clone(),
                    tool: call.tool_id.clone(),
                    args: call.args.clone(),
                });

                // `before_tool_call` hook (L3): block (approval/trust gate) or
                // replace the arguments before running.
                let decision = controls
                    .hooks
                    .before_tool_call
                    .as_ref()
                    .map(|hook| hook(&call.tool_id, &call.args))
                    .unwrap_or(ToolDecision::Proceed);

                let result_text = match decision {
                    ToolDecision::Block { reason } => {
                        let _ = events.send(AgentEvent::ToolBlocked {
                            id: call.id.clone(),
                            tool: call.tool_id.clone(),
                            reason: reason.clone(),
                        });
                        serde_json::json!({ "blocked": reason }).to_string()
                    }
                    decision => {
                        let args = match decision {
                            ToolDecision::ReplaceArgs { args } => args,
                            _ => call.args.clone(),
                        };
                        // A tool failure is fed back to the model (so it can
                        // recover), not propagated — a long conversation shouldn't
                        // die on one bad tool call (bounded by max_steps_per_turn).
                        match tool.call_json(args, ctx.clone()).await {
                            Ok(output) => {
                                let _ = events.send(AgentEvent::ToolCompleted {
                                    id: call.id.clone(),
                                    tool: call.tool_id.clone(),
                                    output: output.clone(),
                                });
                                serde_json::to_string(&output).map_err(|e| {
                                    AgenkitError::internal(format!(
                                        "tool `{}` output encode: {e}",
                                        call.tool_id
                                    ))
                                })?
                            }
                            Err(error) => {
                                let kind = error.to_string();
                                let _ = events.send(AgentEvent::ToolFailed {
                                    id: call.id.clone(),
                                    tool: call.tool_id.clone(),
                                    error: kind.clone(),
                                });
                                serde_json::json!({ "error": kind }).to_string()
                            }
                        }
                    }
                };
                messages.push(Message::tool_result(call.id.clone(), result_text));
            }
        }

        Ok(StopReason::MaxSteps)
    }
}

/// A long-lived, durable conversational agent (L2). Built on the W7 session
/// layer: `open` (or resume) once, then [`prompt`](AgentSession::prompt) over
/// time. Each prompt loads the compacted history, runs one turn via
/// [`AgentLoop`], persists the user + assistant messages, and compacts if the
/// window would overflow — all owner-scoped and forkable. Persistence is the
/// configured store's (the default is in-memory; see [`open`](AgentSessionBuilder::open)).
#[derive(Clone)]
pub struct AgentSession {
    inner: Arc<AgenkitInner>,
    principal: Principal,
    config: AgentConfig,
    thread: AgentThreadHandle,
    agent_id: String,
    /// Shared steering controls (queue + abort + hooks). Cloning the session
    /// shares these (Arc-backed), so `steer`/`abort` reach the running turn.
    controls: TurnControls,
    /// Single-active-turn guard (Arc-shared across clones): a turn holds it for
    /// its duration so a concurrent `prompt` is rejected rather than corrupting
    /// the shared thread/queue/abort.
    busy: Arc<AtomicBool>,
}

impl AgentSession {
    /// Begin building a session over `agenkit`'s runtime.
    pub fn builder(agenkit: &Agenkit) -> AgentSessionBuilder {
        AgentSessionBuilder {
            inner: agenkit.inner.clone(),
            principal: Principal::anonymous(),
            config: AgentConfig::new(),
            agent_id: "kitty".to_string(),
            hooks: Hooks::default(),
        }
    }

    /// Inject a follow-up message into the **currently running** turn: instead of
    /// going idle when the model next answers, the loop continues with this
    /// message (the "talk to it mid-run" steering). A steer queued just before a
    /// `prompt` is picked up by that turn; anything left unconsumed is cleared
    /// when the turn ends (it never leaks into a later prompt).
    pub fn steer(&self, text: impl Into<String>) {
        if let Ok(mut queue) = self.controls.queue.lock() {
            queue.push_back(text.into());
        }
    }

    /// A handle to cancel the running turn (stops at the next step boundary).
    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle(self.controls.abort.clone())
    }

    /// The durable thread id (persist this to resume the conversation later).
    pub fn id(&self) -> &AgentThreadId {
        self.thread.id()
    }

    /// The full stored conversation (every turn, pre-compaction view).
    pub async fn history(&self) -> AgenkitResult<Vec<ThreadMessage>> {
        self.thread.history().await
    }

    /// Prompt the conversation: run one turn and stream its [`AgentEvent`]s. The
    /// turn runs on a spawned task (so this **requires a Tokio runtime** — it
    /// panics otherwise); drain the returned stream to observe it. The terminal
    /// event is [`AgentEvent::Stopped`] (success) or [`AgentEvent::Failed`].
    ///
    /// **One turn at a time:** if a turn is already running on this session (or a
    /// clone of it), the prompt is rejected with a `Failed` event rather than run
    /// concurrently — concurrent turns would corrupt the shared thread and
    /// steering state. Await the stream to completion (or `abort`) before the
    /// next prompt.
    pub fn prompt(&self, text: impl Into<String>) -> UnboundedReceiverStream<AgentEvent> {
        let (tx, rx) = unbounded_channel();
        let text = text.into();

        // Single-active-turn guard: claim the turn lock, or reject.
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ = tx.send(AgentEvent::Failed {
                error: "a turn is already running on this session".to_string(),
            });
            return UnboundedReceiverStream::new(rx);
        }
        // Reset abort SYNCHRONOUSLY (before the task is spawned), so an `abort()`
        // the caller issues right after `prompt()` targets THIS turn and can't be
        // clobbered by a reset inside the spawned task.
        self.controls.abort.store(false, Ordering::Relaxed);

        let session = self.clone();
        tokio::spawn(async move {
            let _ = tx.send(AgentEvent::Started);
            let result = session.run_one_prompt(text, &tx).await;
            // Release the turn lock and drop any steer left unconsumed, so it
            // can't leak into the next prompt.
            if let Ok(mut queue) = session.controls.queue.lock() {
                queue.clear();
            }
            session.busy.store(false, Ordering::Release);
            match result {
                Ok(reason) => {
                    let _ = tx.send(AgentEvent::Stopped { reason });
                }
                Err(error) => {
                    let _ = tx.send(AgentEvent::Failed {
                        error: error.to_string(),
                    });
                }
            }
        });
        UnboundedReceiverStream::new(rx)
    }

    /// Fork the conversation into an independent branch (a `/fork`): a new
    /// session over a forked thread that inherits the full history; the original
    /// is untouched. `None` if the store can't branch.
    pub async fn fork(&self) -> AgenkitResult<Option<AgentSession>> {
        Ok(self.thread.fork().await?.map(|thread| AgentSession {
            inner: self.inner.clone(),
            principal: self.principal.clone(),
            config: self.config.clone(),
            thread,
            agent_id: self.agent_id.clone(),
            // The branch is an independent conversation: same hooks, fresh
            // queue + abort + turn lock.
            controls: TurnControls {
                hooks: self.controls.hooks.clone(),
                ..TurnControls::default()
            },
            busy: Arc::new(AtomicBool::new(false)),
        }))
    }

    /// One prompt's work: load history → persist the prompt → run the turn →
    /// persist the answer → compact.
    async fn run_one_prompt(
        &self,
        text: String,
        events: &UnboundedSender<AgentEvent>,
    ) -> AgenkitResult<StopReason> {
        // Seed the model context from the compacted history (W7), then the prompt.
        let mut messages: Vec<Message> = self
            .thread
            .active_history()
            .await?
            .into_iter()
            .map(|m| Message::new(m.role, m.content))
            .collect();
        let persist_from = messages.len();
        messages.push(Message::user(text.clone()));

        // Persist the prompt UP FRONT so it survives a mid-turn error (the turn
        // below may fail before producing an answer — the question must not be
        // lost from a "durable, resumable" session).
        self.thread
            .store
            .append(
                &self.thread.id,
                self.thread.owner(),
                vec![ThreadMessage::new(Role::User, text)],
            )
            .await?;

        let agent_loop = AgentLoop::new(
            self.inner.clone(),
            self.principal.clone(),
            self.config.clone(),
        );
        let reason = agent_loop
            .run_turn(&mut messages, events, &self.controls)
            .await?;

        // Persist what the turn produced (everything after the already-stored
        // prompt): any steered follow-up prompts + the assistant **answers** —
        // assistant messages with NO tool calls. Narration that rides a tool call,
        // and tool results, are NOT stored, so resume replays a clean alternating
        // Q&A rather than a transcript referencing tool calls that are gone.
        let to_persist: Vec<ThreadMessage> = messages[persist_from + 1..]
            .iter()
            .filter_map(|m| match m.role {
                Role::User => Some(ThreadMessage::new(Role::User, m.content.as_text())),
                Role::Assistant if m.tool_calls.is_empty() && !m.content.as_text().is_empty() => {
                    Some(ThreadMessage::new(Role::Assistant, m.content.as_text()))
                }
                _ => None,
            })
            .collect();
        if !to_persist.is_empty() {
            self.thread
                .store
                .append(&self.thread.id, self.thread.owner(), to_persist)
                .await?;
        }

        // Compact if the (now longer) history would overflow the window.
        let model = self
            .config
            .model
            .clone()
            .or_else(|| self.inner.default_model.clone())
            .ok_or_else(|| AgenkitError::config("agent runtime has no model"))?;
        let provider = self.inner.providers.resolve(&model)?;
        let cx = {
            let credential = self
                .inner
                .credentials
                .resolve(provider.id(), &self.principal)
                .await?;
            ProviderContext::for_request(credential)
        };
        let max_output = self.config.max_tokens.unwrap_or(1024);
        if let Some((folded, _kept)) =
            super::agent::compact_thread(&self.thread, &model, &provider, &cx, max_output).await?
        {
            let _ = events.send(AgentEvent::Compacted { folded });
        }
        Ok(reason)
    }
}

/// Builder for [`AgentSession`].
pub struct AgentSessionBuilder {
    inner: Arc<AgenkitInner>,
    principal: Principal,
    config: AgentConfig,
    agent_id: String,
    hooks: Hooks,
}

impl AgentSessionBuilder {
    /// Run the conversation under `principal` (owner-scopes the durable thread).
    /// Defaults to anonymous.
    pub fn principal(mut self, principal: Principal) -> Self {
        self.principal = principal;
        self
    }

    /// Set a `before_tool_call` steering hook (L3): called with the tool id and
    /// arguments before every tool runs, returning a [`ToolDecision`] to proceed,
    /// **block** it (an approval/trust gate — the reason is fed back to the
    /// model), or **replace** its arguments. This is the host-side mirror of the
    /// (parked) WIT extension hook.
    pub fn before_tool_call<F>(mut self, hook: F) -> Self
    where
        F: Fn(&str, &serde_json::Value) -> ToolDecision + Send + Sync + 'static,
    {
        self.hooks.before_tool_call = Some(Arc::new(hook));
        self
    }

    /// Set the agent config (model, system prompt, tools, limits).
    pub fn config(mut self, config: AgentConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the agent id stored on the thread (default `"kitty"`).
    pub fn agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }

    /// Open the session: resume the thread `id` if it exists **and** is owned by
    /// the principal, otherwise create a fresh thread.
    ///
    /// Persistence is the configured store's: the runtime's **default**
    /// `thread_store` is in-memory (process-local — resume works within the
    /// process, but the thread is lost on restart). For across-restart durability,
    /// build the runtime with a durable store, e.g.
    /// `Agenkit::builder().thread_store(SessionThreadStore::new(Arc::new(
    /// SqliteSessionStore::open(path)?)))`.
    pub async fn open(self, id: Option<AgentThreadId>) -> AgenkitResult<AgentSession> {
        let store = self.inner.thread_store.clone();
        let owner = ThreadOwner::from_principal(&self.principal);
        let owner_key = owner.key().map(str::to_string);

        let thread_id = match id {
            Some(id) if store.load(&id, owner).await?.is_some() => id,
            _ => {
                store
                    .create(&self.agent_id, owner, ThreadRetention::Durable)
                    .await?
            }
        };
        let thread = AgentThreadHandle {
            id: thread_id,
            store,
            owner: owner_key,
        };
        Ok(AgentSession {
            inner: self.inner,
            principal: self.principal,
            config: self.config,
            thread,
            agent_id: self.agent_id,
            controls: TurnControls {
                hooks: self.hooks,
                ..TurnControls::default()
            },
            busy: Arc::new(AtomicBool::new(false)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{Agenkit, AiTool, AiToolContext, BoxFuture, MockProvider};
    use futures::StreamExt;
    use pocopine_agenkit_core::{ModelRef, ToolDescriptor as Td};
    use serde::{Deserialize, Serialize};
    use tokio::sync::mpsc::unbounded_channel;

    fn chat(answer: &str) -> Agenkit {
        Agenkit::builder()
            .provider(MockProvider::new("local").default_text(answer))
            .default_model(ModelRef::new("local/default"))
            .build()
            .unwrap()
    }

    #[derive(Deserialize, schemars::JsonSchema)]
    struct EchoIn {
        text: String,
    }
    #[derive(Serialize, schemars::JsonSchema)]
    struct EchoOut {
        echoed: String,
    }
    struct Echo;
    impl AiTool for Echo {
        const ID: &'static str = "echo";
        type Input = EchoIn;
        type Output = EchoOut;
        fn descriptor() -> Td {
            Td::new("echo", "Echo the input")
        }
        fn call(
            &self,
            input: EchoIn,
            _ctx: AiToolContext,
        ) -> BoxFuture<'_, AgenkitResult<EchoOut>> {
            Box::pin(async move { Ok(EchoOut { echoed: input.text }) })
        }
    }

    fn runtime() -> Agenkit {
        Agenkit::builder()
            .provider(
                MockProvider::new("local")
                    // First model call → a tool call; second → a plain answer.
                    .on_prompt_tool("use the tool", "echo", serde_json::json!({ "text": "hi" }))
                    .default_text("all done"),
            )
            .default_model(ModelRef::new("local/default"))
            .tool(Echo)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn run_turn_loops_through_a_tool_then_answers() {
        let agenkit = runtime();
        let loop_ = AgentLoop::new(
            agenkit.inner.clone(),
            Principal::anonymous(),
            AgentConfig::new().tools(["echo"]),
        );
        let (tx, mut rx) = unbounded_channel();
        let mut messages = vec![Message::user("use the tool then answer")];

        let stop = loop_
            .run_turn(&mut messages, &tx, &TurnControls::default())
            .await
            .unwrap();
        drop(tx);
        assert_eq!(stop, StopReason::Idle);

        let mut kinds = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            kinds.push(match ev {
                AgentEvent::AssistantText { .. } => "text",
                AgentEvent::ToolStarted { tool, .. } if tool == "echo" => "tool_start",
                AgentEvent::ToolCompleted { .. } => "tool_done",
                AgentEvent::ToolFailed { .. } => "tool_fail",
                _ => "other",
            });
        }
        // tool runs, then the model answers (the answer text crosses too).
        assert!(kinds.contains(&"tool_start"), "events: {kinds:?}");
        assert!(kinds.contains(&"tool_done"), "events: {kinds:?}");
        assert!(kinds.contains(&"text"), "events: {kinds:?}");
        // The transcript grew: user + assistant(tool-call) + tool result + final assistant.
        assert!(messages.len() >= 4, "messages: {}", messages.len());
        assert_eq!(messages.last().unwrap().role, Role::Assistant);
    }

    #[tokio::test]
    async fn tool_failure_is_fed_back_not_fatal() {
        struct Boom;
        impl AiTool for Boom {
            const ID: &'static str = "boom";
            type Input = EchoIn;
            type Output = EchoOut;
            fn descriptor() -> Td {
                Td::new("boom", "Always fails")
            }
            fn call(&self, _i: EchoIn, _c: AiToolContext) -> BoxFuture<'_, AgenkitResult<EchoOut>> {
                Box::pin(async move { Err(AgenkitError::internal("kaboom")) })
            }
        }
        let agenkit = Agenkit::builder()
            .provider(
                MockProvider::new("local")
                    .on_prompt_tool("go", "boom", serde_json::json!({ "text": "x" }))
                    .default_text("recovered"),
            )
            .default_model(ModelRef::new("local/default"))
            .tool(Boom)
            .build()
            .unwrap();
        let loop_ = AgentLoop::new(
            agenkit.inner.clone(),
            Principal::anonymous(),
            AgentConfig::new().tools(["boom"]),
        );
        let (tx, mut rx) = unbounded_channel();
        let mut messages = vec![Message::user("go")];
        let stop = loop_
            .run_turn(&mut messages, &tx, &TurnControls::default())
            .await
            .unwrap();
        drop(tx);
        // The failed tool did NOT abort the turn — the model recovered and answered.
        assert_eq!(stop, StopReason::Idle);
        let mut saw_fail = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AgentEvent::ToolFailed { .. }) {
                saw_fail = true;
            }
        }
        assert!(saw_fail, "should have emitted ToolFailed");
    }

    // ── L2: durable AgentSession ─────────────────────────────────────────────

    #[tokio::test]
    async fn session_prompt_streams_events_and_persists_the_turn() {
        let agenkit = chat("hello back");
        let session = AgentSession::builder(&agenkit).open(None).await.unwrap();

        let events: Vec<AgentEvent> = session.prompt("hi").collect().await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { text } if text == "hello back")),
            "{events:?}"
        );
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::Stopped {
                    reason: StopReason::Idle
                })
            ),
            "{events:?}"
        );

        // The turn persisted: user + assistant.
        let history = session.history().await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[1].content.as_text(), "hello back");

        // A second prompt appends another turn (multi-turn conversation).
        let _ = session.prompt("again").collect::<Vec<_>>().await;
        assert_eq!(session.history().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn session_resumes_by_thread_id() {
        let agenkit = chat("ok");
        let id = {
            let s = AgentSession::builder(&agenkit).open(None).await.unwrap();
            let _ = s.prompt("remember this").collect::<Vec<_>>().await;
            s.id().clone()
        };
        // Reopen by id over the same store → the conversation resumes.
        let resumed = AgentSession::builder(&agenkit)
            .open(Some(id))
            .await
            .unwrap();
        let history = resumed.history().await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content.as_text(), "remember this");
    }

    #[tokio::test]
    async fn session_forks_into_an_independent_branch() {
        let agenkit = chat("done");
        let session = AgentSession::builder(&agenkit).open(None).await.unwrap();
        let _ = session.prompt("first").collect::<Vec<_>>().await;

        let forked = session.fork().await.unwrap().expect("store can branch");
        assert_ne!(forked.id().as_str(), session.id().as_str());
        // The fork inherits the parent's history; the parent is untouched.
        assert_eq!(forked.history().await.unwrap().len(), 2);
        let _ = forked.prompt("second").collect::<Vec<_>>().await;
        assert_eq!(forked.history().await.unwrap().len(), 4);
        assert_eq!(session.history().await.unwrap().len(), 2);
    }

    // ── L3: steering queue + typed hooks + abort ─────────────────────────────

    #[tokio::test]
    async fn pre_set_abort_stops_the_turn_immediately() {
        let agenkit = chat("won't run");
        let loop_ = AgentLoop::new(
            agenkit.inner.clone(),
            Principal::anonymous(),
            AgentConfig::new(),
        );
        let controls = TurnControls::default();
        controls
            .abort
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = unbounded_channel();
        let mut messages = vec![Message::user("hi")];

        let stop = loop_.run_turn(&mut messages, &tx, &controls).await.unwrap();
        drop(tx);
        assert_eq!(stop, StopReason::Aborted);
        // Aborted before the model call → no assistant text was produced.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn before_tool_call_block_prevents_execution() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Recorder(Arc<AtomicBool>);
        impl AiTool for Recorder {
            const ID: &'static str = "recorder";
            type Input = EchoIn;
            type Output = EchoOut;
            fn descriptor() -> Td {
                Td::new("recorder", "Records that it ran")
            }
            fn call(
                &self,
                input: EchoIn,
                _c: AiToolContext,
            ) -> BoxFuture<'_, AgenkitResult<EchoOut>> {
                self.0.store(true, Ordering::Relaxed);
                Box::pin(async move { Ok(EchoOut { echoed: input.text }) })
            }
        }
        let ran = Arc::new(AtomicBool::new(false));
        let agenkit = Agenkit::builder()
            .provider(
                MockProvider::new("local")
                    .on_prompt_tool("go", "recorder", serde_json::json!({ "text": "x" }))
                    .default_text("after block"),
            )
            .default_model(ModelRef::new("local/default"))
            .tool(Recorder(ran.clone()))
            .build()
            .unwrap();
        let loop_ = AgentLoop::new(
            agenkit.inner.clone(),
            Principal::anonymous(),
            AgentConfig::new().tools(["recorder"]),
        );
        let controls = TurnControls {
            hooks: Hooks {
                before_tool_call: Some(Arc::new(|_tool, _args| ToolDecision::Block {
                    reason: "needs approval".to_string(),
                })),
            },
            ..TurnControls::default()
        };
        let (tx, mut rx) = unbounded_channel();
        let mut messages = vec![Message::user("go")];
        let _ = loop_.run_turn(&mut messages, &tx, &controls).await.unwrap();
        drop(tx);

        // The hook blocked the call → the tool never executed, and a ToolBlocked
        // event was emitted instead.
        assert!(
            !ran.load(Ordering::Relaxed),
            "the blocked tool must not run"
        );
        let mut saw_block = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AgentEvent::ToolBlocked { .. }) {
                saw_block = true;
            }
        }
        assert!(saw_block, "should have emitted ToolBlocked");
    }

    #[tokio::test]
    async fn steered_follow_up_continues_the_same_turn() {
        let agenkit = chat("answer");
        let session = AgentSession::builder(&agenkit).open(None).await.unwrap();
        // Queue a follow-up *before* prompting: when the model answers the first
        // prompt and would go idle, the steered message continues the turn.
        session.steer("and another thing");
        let _ = session.prompt("first").collect::<Vec<_>>().await;
        // One prompt processed two exchanges: user/assistant ×2 = 4 messages.
        assert_eq!(session.history().await.unwrap().len(), 4);
        let history = session.history().await.unwrap();
        assert_eq!(history[0].content.as_text(), "first");
        assert_eq!(history[2].content.as_text(), "and another thing");
    }

    // ── review fixes ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_concurrent_prompt_is_rejected_not_run() {
        // current-thread runtime: the two prompt() calls run synchronously before
        // either spawned turn does, so the second sees the turn lock held.
        let agenkit = chat("ok");
        let session = AgentSession::builder(&agenkit).open(None).await.unwrap();
        let a = session.prompt("A"); // claims the turn lock (sync), spawns the turn
        let b: Vec<AgentEvent> = session.prompt("B").collect().await; // rejected
        assert!(
            matches!(b.as_slice(), [AgentEvent::Failed { .. }]),
            "second concurrent prompt must be rejected: {b:?}"
        );
        let _ = a.collect::<Vec<_>>().await; // let A finish, releasing the lock
        // A fresh prompt works once the lock is free.
        let c: Vec<AgentEvent> = session.prompt("C").collect().await;
        assert!(
            matches!(c.last(), Some(AgentEvent::Stopped { .. })),
            "{c:?}"
        );
        // A persisted (prompt+answer); B persisted nothing; C persisted.
        assert_eq!(session.history().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn the_prompt_is_persisted_even_when_the_turn_errors() {
        // The model calls a tool the agent never allow-listed → run_turn errors.
        let agenkit = Agenkit::builder()
            .provider(MockProvider::new("local").on_prompt_tool(
                "go",
                "ghost",
                serde_json::json!({}),
            ))
            .default_model(ModelRef::new("local/default"))
            .build()
            .unwrap();
        let session = AgentSession::builder(&agenkit).open(None).await.unwrap();
        let events: Vec<AgentEvent> = session.prompt("go").collect().await;
        assert!(
            matches!(events.last(), Some(AgentEvent::Failed { .. })),
            "{events:?}"
        );
        // The question survived the error (persisted up front), so resume isn't blank.
        let history = session.history().await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content.as_text(), "go");
    }

    #[tokio::test]
    async fn a_tool_using_turn_persists_only_the_question_and_final_answer() {
        let agenkit = Agenkit::builder()
            .provider(
                MockProvider::new("local")
                    .on_prompt_tool("weather", "echo", serde_json::json!({ "text": "x" }))
                    .default_text("it's sunny"),
            )
            .default_model(ModelRef::new("local/default"))
            .tool(Echo)
            .build()
            .unwrap();
        let session = AgentSession::builder(&agenkit)
            .config(AgentConfig::new().tools(["echo"]))
            .open(None)
            .await
            .unwrap();
        let _ = session.prompt("weather?").collect::<Vec<_>>().await;
        // The assistant tool-call turn and the tool result are NOT persisted —
        // only the user question and the final answer (a clean alternating Q&A).
        let history = session.history().await.unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        assert_eq!(history[0].content.as_text(), "weather?");
        assert_eq!(history[1].content.as_text(), "it's sunny");
    }

    #[test]
    fn agent_config_default_has_a_nonzero_step_budget() {
        // Regression: the derived Default gave max_steps_per_turn = 0 (a no-op turn).
        assert_eq!(AgentConfig::default().max_steps_per_turn, 8);
        assert_eq!(AgentConfig::new().max_steps_per_turn, 8);
    }
}
