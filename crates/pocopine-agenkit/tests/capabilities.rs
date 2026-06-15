#![cfg(not(target_arch = "wasm32"))]
//! Issue #214 item 3: the runtime honors `ProviderCapabilities`. Here: an agent
//! that declares tools on a provider that can't call them fails fast with a
//! config error (there is no graceful degrade for tool calling). The streaming
//! and json_schema degrade paths are covered by `generate.rs` unit tests.

mod common;

use common::block_on;
use pocopine_agenkit::server::{
    Agenkit, AiAgent, AiAgentBuilder, AiFlowContext, AiTool, AiToolContext, BoxFuture, BoxStream,
    Flow, GenerateRequest, GenerateResponse, Provider, ProviderCapabilities, StreamChunk,
};
use pocopine_agenkit_core::{AgenkitResult, ModelRef, ToolDescriptor};
use serde::{Deserialize, Serialize};

/// A provider that supports plain generation but not tool calling.
struct NoToolsProvider;

impl Provider for NoToolsProvider {
    fn id(&self) -> &str {
        "local"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            json_schema: true,
            tools: false,
        }
    }

    fn generate<'a>(
        &'a self,
        _request: GenerateRequest,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        Box::pin(async move { Ok(GenerateResponse::text("unused")) })
    }

    fn generate_stream<'a>(
        &'a self,
        _request: GenerateRequest,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        Box::pin(futures::stream::empty())
    }
}

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
        ToolDescriptor::new("lookup", "Look up a fact")
    }

    fn call(
        &self,
        input: LookupIn,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<LookupOut>> {
        Box::pin(async move { Ok(LookupOut { fact: input.term }) })
    }
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct Question {
    question: String,
}

#[derive(Serialize, Deserialize, Debug, schemars::JsonSchema)]
struct Answer {
    answer: String,
}

struct Researcher;

impl AiAgent for Researcher {
    const ID: &'static str = "researcher";
    type Input = Question;
    type Output = Answer;

    fn configure(builder: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        builder
            .system("Use the tool.")
            .tools(["lookup"])
            .max_steps(2)
    }
}

async fn research(input: Question, ctx: AiFlowContext) -> AgenkitResult<Answer> {
    ctx.agent::<Researcher>().input(input).run().await
}

#[test]
fn agent_with_tools_on_a_toolless_provider_fails_fast() {
    let agenkit = Agenkit::builder()
        .provider(NoToolsProvider)
        .default_model(ModelRef::new("local/default"))
        .tool(Lookup)
        .flow(Flow::new("research", research).uses_agent("researcher"))
        .build()
        .unwrap();

    let result: Result<Answer, _> = block_on(agenkit.run_flow_typed(
        "research",
        Question {
            question: "hi".to_string(),
        },
    ));
    let error = result.expect_err("a toolless provider must reject a tool-using agent");
    assert_eq!(error.kind(), "config", "error: {error}");
}
