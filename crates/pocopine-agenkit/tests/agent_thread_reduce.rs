#![cfg(not(target_arch = "wasm32"))]
//! Phase 2.6a: typed agents (with a tool-call loop), thread persistence, and
//! reducers (deterministic fold + model judge).

mod common;

use common::{block_on, capture};
use pocopine_agenkit::server::{
    Agenkit, AiAgent, AiAgentBuilder, AiFlowContext, AiTool, AiToolContext, BoxFuture, Flow,
    MockProvider,
};
use pocopine_agenkit_core::{AgenkitResult, ModelRef, ToolDescriptor};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, schemars::JsonSchema)]
struct LookupIn {
    term: String,
}

#[derive(Serialize, schemars::JsonSchema)]
struct LookupOut {
    fact: String,
}

struct Lookup;

impl AiTool for Lookup {
    const ID: &'static str = "lookup";
    type Input = LookupIn;
    type Output = LookupOut;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("lookup", "Look up a fact about a term")
    }

    fn call(
        &self,
        input: LookupIn,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<LookupOut>> {
        Box::pin(async move {
            Ok(LookupOut {
                fact: format!("{} use presigned URLs", input.term),
            })
        })
    }
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct Question {
    question: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
struct AgentAnswer {
    answer: String,
}

struct Researcher;

impl AiAgent for Researcher {
    const ID: &'static str = "researcher";
    type Input = Question;
    type Output = AgentAnswer;

    fn configure(builder: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        builder
            .system("Use the lookup tool, then answer.")
            .tools(["lookup"])
            .max_steps(3)
    }
}

async fn research_flow(input: Question, ctx: AiFlowContext) -> AgenkitResult<AgentAnswer> {
    ctx.agent::<Researcher>().input(input).run().await
}

async fn threaded_flow(input: Question, ctx: AiFlowContext) -> AgenkitResult<usize> {
    let thread = ctx.thread::<Researcher>().create().await?;
    ctx.agent::<Researcher>()
        .thread(thread.clone())
        .input(input)
        .run()
        .await?;
    Ok(thread.history().await?.len())
}

async fn reduce_fold_flow(_input: (), ctx: AiFlowContext) -> AgenkitResult<u32> {
    let candidates = vec![1_u32, 5, 3];
    ctx.reduce("pick_max", candidates)
        .fold(|values| Ok(values.into_iter().max().unwrap_or(0)))
        .await
}

async fn judge_flow(_input: (), ctx: AiFlowContext) -> AgenkitResult<AgentAnswer> {
    let candidates = vec!["candidate a".to_string(), "candidate b".to_string()];
    ctx.reduce("judge", candidates)
        .system("Pick the best.")
        .schema::<AgentAnswer>()
        .await
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("local")
                .on_prompt_structured(
                    "fact",
                    serde_json::json!({"answer": "uploads use presigned URLs"}),
                )
                .on_prompt_tool("uploads", "lookup", serde_json::json!({"term": "uploads"}))
                .default_structured(serde_json::json!({"answer": "best"})),
        )
        .default_model(ModelRef::new("local/default"))
        .tool(Lookup)
        .flow(
            Flow::new("research", research_flow)
                .uses_tool("lookup")
                .uses_agent("researcher"),
        )
        .flow(Flow::new("threaded", threaded_flow))
        .flow(Flow::new("reduce_fold", reduce_fold_flow))
        .flow(Flow::new("judge", judge_flow))
        .build()
        .unwrap()
}

#[test]
fn agent_runs_tool_loop_then_answers() {
    let trace = capture(|| {
        let answer: AgentAnswer = block_on(runtime().run_flow(
            "research",
            Question {
                question: "how do uploads work?".to_string(),
            },
        ))
        .unwrap();
        assert_eq!(
            answer,
            AgentAnswer {
                answer: "uploads use presigned URLs".to_string()
            }
        );
    });

    for name in [
        "ai_tool_started",
        "ai_tool_completed",
        "ai_step_completed",
        "ai_model_request",
    ] {
        assert!(
            trace.contains(name),
            "missing `{name}`; got {:?}",
            trace.names()
        );
    }
    // Two model calls: the tool-requesting call and the final structured call.
    assert!(trace.count("ai_model_request") >= 2);
}

#[test]
fn threaded_agent_persists_exchange() {
    let count: usize = block_on(runtime().run_flow(
        "threaded",
        Question {
            question: "how do uploads work?".to_string(),
        },
    ))
    .unwrap();
    // The run appends one user + one assistant message to the thread.
    assert_eq!(count, 2);
}

#[test]
fn deterministic_reduce_folds_candidates() {
    let trace = capture(|| {
        let max: u32 = block_on(runtime().run_flow("reduce_fold", ())).unwrap();
        assert_eq!(max, 5);
    });
    assert!(trace.contains("ai_reducer_started"));
    assert!(trace.contains("ai_reducer_completed"));
}

#[test]
fn model_judge_reduce_returns_typed_answer() {
    let answer: AgentAnswer = block_on(runtime().run_flow("judge", ())).unwrap();
    assert_eq!(
        answer,
        AgentAnswer {
            answer: "best".to_string()
        }
    );
}
