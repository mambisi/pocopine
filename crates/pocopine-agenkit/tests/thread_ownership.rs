#![cfg(not(target_arch = "wasm32"))]
//! Issue #214 item 1: thread resume is owner-scoped. A flow that wires
//! caller-supplied input into a thread id must not let one principal read or
//! continue another principal's conversation history.

mod common;

use common::block_on;
use pocopine_agenkit::server::{
    Agenkit, AiAgent, AiAgentBuilder, AiFlowContext, Flow, MockProvider,
};
use pocopine_agenkit_core::{AgenkitResult, AgentThreadId, ModelRef};
use pocopine_auth::{AuthUser, Principal};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct Question {
    question: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
struct Answer {
    answer: String,
}

struct Echo;

impl AiAgent for Echo {
    const ID: &'static str = "echo";
    type Input = Question;
    type Output = Answer;

    fn configure(builder: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        builder.system("Answer the question.").max_steps(1)
    }
}

#[derive(Serialize, Deserialize, Debug, schemars::JsonSchema)]
struct ThreadOut {
    thread_id: String,
    history_len: usize,
}

/// Open a fresh thread and run the agent once so it has history, returning the
/// new thread id and its message count.
async fn start(_input: (), ctx: AiFlowContext) -> AgenkitResult<ThreadOut> {
    let thread = ctx.thread::<Echo>().resume_or_create(None).await?;
    ctx.agent::<Echo>()
        .thread(thread.clone())
        .input(Question {
            question: "hi".to_string(),
        })
        .run()
        .await?;
    Ok(ThreadOut {
        thread_id: thread.id().as_str().to_string(),
        history_len: thread.history().await?.len(),
    })
}

/// Resume the supplied thread id (if owned by the caller) and report what is
/// visible — without running the agent, so it neither appends nor mutates.
async fn peek(input: String, ctx: AiFlowContext) -> AgenkitResult<ThreadOut> {
    let thread = ctx
        .thread::<Echo>()
        .resume_or_create(Some(AgentThreadId::new(input)))
        .await?;
    Ok(ThreadOut {
        thread_id: thread.id().as_str().to_string(),
        history_len: thread.history().await?.len(),
    })
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("local").default_structured(serde_json::json!({"answer": "ok"})),
        )
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("start", start).uses_agent("echo").public())
        .flow(Flow::new("peek", peek).uses_agent("echo").public())
        .build()
        .unwrap()
}

fn alice() -> Principal {
    Principal::from_user(AuthUser::new("alice"))
}

fn bob() -> Principal {
    Principal::from_user(AuthUser::new("bob"))
}

#[test]
fn principal_cannot_resume_another_principals_thread() {
    let agenkit = runtime();

    // Bob starts a thread; the agent run appends one user + one assistant msg.
    let bobs: ThreadOut = block_on(agenkit.flow("start").principal(bob()).run()).unwrap();
    assert_eq!(bobs.history_len, 2);

    // Alice tries to resume Bob's thread id: resume is rejected, she gets a
    // fresh empty thread with a different id — never Bob's history.
    let alices: ThreadOut = block_on(
        agenkit
            .flow("peek")
            .input(bobs.thread_id.clone())
            .principal(alice())
            .run(),
    )
    .unwrap();
    assert_eq!(alices.history_len, 0, "Alice must not see Bob's history");
    assert_ne!(
        alices.thread_id, bobs.thread_id,
        "Alice must not be handed Bob's thread"
    );

    // An anonymous caller is likewise denied Bob's authenticated thread.
    let anons: ThreadOut =
        block_on(agenkit.flow("peek").input(bobs.thread_id.clone()).run()).unwrap();
    assert_eq!(anons.history_len, 0, "anonymous must not see Bob's history");

    // Bob himself resumes his own thread and sees its history intact.
    let resumed: ThreadOut = block_on(
        agenkit
            .flow("peek")
            .input(bobs.thread_id.clone())
            .principal(bob())
            .run(),
    )
    .unwrap();
    assert_eq!(resumed.thread_id, bobs.thread_id);
    assert_eq!(resumed.history_len, 2, "owner must resume their own thread");
}
