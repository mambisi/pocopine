#![cfg(not(target_arch = "wasm32"))]
//! A tool call the policy refuses must never run — but refusing it should not
//! cost the user their whole turn.
//!
//! The two loops answer that differently on purpose. A typed agent's allowlist
//! sits beside its tools, so a call outside it is a programming error and
//! failing loudly is the point. The conversational runtime faces a model that
//! can name a tool it was not given — because the system prompt mentions one, or
//! because the model's profile silently lost its tools — and killing the turn
//! turns a recoverable mistake into a dead conversation.
//!
//! Both directions are pinned here: the refusal is never an execution, and the
//! recovery is never a silent one.

mod common;

use common::block_on;
use pocopine_agenkit::server::{
    Agenkit, AgentEvent, AgentSession, AiAgent, AiAgentBuilder, AiFlowContext, AiTool,
    AiToolContext, BoxFuture, BoxStream, Flow, GenerateRequest, GenerateResponse, MockProvider,
    Provider, ProviderCapabilities, ProviderContext, StreamChunk,
};
use pocopine_agenkit_core::{AgenkitResult, ModelRef, ToolCall, ToolDescriptor};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_stream::StreamExt;

const MODEL: &str = "anthropic/claude-sonnet-4-6";

#[derive(Deserialize, schemars::JsonSchema)]
struct Empty {}

#[derive(Serialize, schemars::JsonSchema)]
struct Done {
    ok: bool,
}

/// Registered and allowlisted, and counts its own invocations — so a test can
/// assert a refusal never reached execution rather than inferring it.
struct Allowed(Arc<AtomicUsize>);

impl AiTool for Allowed {
    const ID: &'static str = "allowed";
    type Input = Empty;
    type Output = Done;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("allowed", "A tool the agent is permitted to call")
    }

    fn call(&self, _input: Empty, _ctx: AiToolContext) -> BoxFuture<'_, AgenkitResult<Done>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { Ok(Done { ok: true }) })
    }
}

// ── the conversational runtime: refuse, tell the model, keep going ───────────

/// Asks for a tool it was never given, then — having been told it is not
/// available — answers anyway. `MockProvider` matches on prompt content, and the
/// needle survives into the follow-up request along with the rest of the
/// history, so it would repeat the call forever; this changes its mind exactly
/// once, which is the behaviour under test.
struct RelentsAfterRefusal {
    calls: AtomicUsize,
}

impl Provider for RelentsAfterRefusal {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: false,
            json_schema: true,
            tools: true,
        }
    }

    fn generate<'a>(
        &'a self,
        _request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        let first = self.calls.fetch_add(1, Ordering::Relaxed) == 0;
        Box::pin(async move {
            Ok(if first {
                GenerateResponse {
                    tool_calls: vec![ToolCall::new(
                        "call-1",
                        "web.search",
                        serde_json::json!({"q": "x"}),
                    )],
                    ..GenerateResponse::text("")
                }
            } else {
                GenerateResponse::text("I could not search, so here is what I know.")
            })
        })
    }

    fn generate_stream<'a>(
        &'a self,
        _request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn runtime_feeds_a_refused_tool_back_to_the_model_instead_of_killing_the_turn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agenkit = Agenkit::builder()
        .provider(RelentsAfterRefusal {
            calls: AtomicUsize::new(0),
        })
        .default_model(ModelRef::new(MODEL))
        .tool(Allowed(calls.clone()))
        .build()
        .unwrap();

    let events = block_on(async {
        let session = AgentSession::builder(&agenkit).open(None).await.unwrap();
        let mut stream = session.prompt("please search for something");
        let mut seen = Vec::new();
        while let Some(event) = stream.next().await {
            seen.push(event);
        }
        seen
    });

    // The turn survives. Before this, the refusal was a hard `Err` and the user
    // got a dead conversation instead of an answer.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Stopped { .. })),
        "the turn should finish, not fail: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Failed { .. })),
        "a refused tool must not fail the turn: {events:?}"
    );

    // Refusing is not swallowing: it stays visible to anything watching the
    // firehose, because a call outside the allowlist can be the visible edge of
    // a prompt injection.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolBlocked { tool, .. } if tool == "web.search"
        )),
        "the refusal should surface as ToolBlocked: {events:?}"
    );

    // And the model got to answer anyway.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text.contains("here is what I know")
        )),
        "the model should still produce an answer: {events:?}"
    );

    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "no tool should have executed"
    );
}

// ── the typed run: a programming error, and it still fails loudly ────────────

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct Ask {
    question: String,
}

#[derive(Serialize, Deserialize, Debug, schemars::JsonSchema)]
struct Answer {
    answer: String,
}

struct Strict;

impl AiAgent for Strict {
    const ID: &'static str = "strict";
    type Input = Ask;
    type Output = Answer;

    fn configure(builder: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        builder
            .system("Answer the question.")
            .tools(["allowed"])
            .max_steps(3)
    }
}

async fn strict_flow(input: Ask, ctx: AiFlowContext) -> AgenkitResult<Answer> {
    ctx.agent::<Strict>().input(input).run().await
}

#[test]
fn typed_run_still_fails_hard_on_a_call_outside_its_allowlist() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agenkit = Agenkit::builder()
        .provider(
            MockProvider::new("anthropic")
                .on_prompt_tool("search", "web.search", serde_json::json!({"q": "x"}))
                .default_structured(serde_json::json!({"answer": "unused"})),
        )
        .default_model(ModelRef::new(MODEL))
        .tool(Allowed(calls.clone()))
        .flow(Flow::new("strict", strict_flow))
        .build()
        .unwrap();

    let result: AgenkitResult<Answer> = block_on(
        agenkit
            .flow("strict")
            .input(Ask {
                question: "please search for something".into(),
            })
            .run(),
    );

    // A typed agent's allowlist is written next to its tools, so a call outside
    // it means the two disagree — which the author needs told, not smoothed
    // over into a plausible-looking answer.
    let error = result.expect_err("a non-allowlisted call must fail the typed run");
    assert!(
        error.to_string().contains("web.search"),
        "the error should name the offending tool: {error}"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "no tool should have executed"
    );
}
