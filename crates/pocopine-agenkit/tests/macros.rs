#![cfg(not(target_arch = "wasm32"))]
//! End-to-end test of the `#[ai_tool]` / `#[ai_flow]` macros against the real
//! runtime: the generated `AiTool` impl and `FlowHandler` constructor must
//! register and run, and carry the descriptor metadata derived from the
//! attribute + doc comment (#216).

use pocopine_agenkit::prelude::*;
use pocopine_agenkit::{ai_flow, ai_tool};
use pocopine_agenkit_core::ToolSideEffectPolicy;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
struct LookupIn {
    term: String,
}

#[derive(Serialize, JsonSchema)]
struct LookupOut {
    fact: String,
}

/// Look up a fact about a term.
#[ai_tool]
async fn lookup(input: LookupIn, _ctx: AiToolContext) -> AgenkitResult<LookupOut> {
    Ok(LookupOut {
        fact: format!("{} use presigned URLs", input.term),
    })
}

/// Doc ignored when an explicit description is given.
#[ai_tool(id = "writer", description = "Write a file", side_effecting)]
async fn write_file(input: LookupIn, _ctx: AiToolContext) -> AgenkitResult<LookupOut> {
    Ok(LookupOut { fact: input.term })
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Question {
    q: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, JsonSchema)]
struct Answer {
    answer: String,
}

#[ai_flow(public, tools("lookup"))]
async fn answer(input: Question, ctx: AiFlowContext) -> AgenkitResult<Answer> {
    ctx.ai()
        .prompt(input.q)
        .schema::<Answer>()
        .generate_structured()
        .await
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("local").default_structured(serde_json::json!({"answer": "ok"})),
        )
        .default_model(ModelRef::new("local/default"))
        .tool(Lookup) // generated unit struct from #[ai_tool]
        .tool(WriteFile)
        .flow(answer()) // generated `fn answer() -> impl FlowHandler`
        .build()
        .unwrap()
}

#[test]
fn ai_tool_derives_descriptor_from_fn_and_doc() {
    let d = <Lookup as AiTool>::descriptor();
    assert_eq!(<Lookup as AiTool>::ID, "lookup");
    assert_eq!(d.id, "lookup"); // defaults to the fn name
    assert_eq!(d.description, "Look up a fact about a term."); // from the doc comment
    assert_eq!(d.side_effect, ToolSideEffectPolicy::ReadOnly); // default

    let w = <WriteFile as AiTool>::descriptor();
    assert_eq!(w.id, "writer"); // attribute override
    assert_eq!(w.description, "Write a file");
    assert_eq!(w.side_effect, ToolSideEffectPolicy::SideEffecting);
}

#[tokio::test]
async fn ai_flow_registers_runs_and_is_public() {
    let agenkit = runtime();
    // run_public_flow only succeeds if `#[ai_flow(public)]` emitted `.public()`.
    let out: Answer = agenkit
        .run_public_flow(
            "answer",
            Question {
                q: "hi".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        Answer {
            answer: "ok".to_string()
        }
    );
}
