#![cfg(not(target_arch = "wasm32"))]
//! End-to-end test of the `#[ai_tool]` / `#[ai_flow]` macros against the real
//! runtime: the generated `AiTool` impl and flow marker (`FlowHandler` +
//! `FlowDef` + `FlowKey`) must register and run, carry the descriptor metadata
//! derived from the attribute + doc comment, and drive the typed
//! `agenkit.flow(Marker)` call (#216).

use pocopine_agenkit::prelude::*;
use pocopine_agenkit::server::FlowDef;
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

// The fn name (`answer_question`) must differ from its output type (`Answer`);
// the marker is the PascalCase of the fn name. The id is pinned to "answer" to
// show that the marker name and the registered id are independent.
#[ai_flow(public, id = "answer", tools("lookup"))]
async fn answer_question(input: Question, ctx: AiFlowContext) -> AgenkitResult<Answer> {
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
        .flow(AnswerQuestion) // generated marker struct from #[ai_flow]
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
    // `#[ai_flow(public)]` emits `.public()` — recorded in the manifest.
    assert!(agenkit.flow_is_public("answer"));
    // Typed call: `.input(..)` is checked against `Question`, `.run()` infers
    // `Answer` (no turbofish, no string id).
    let out: Answer = agenkit
        .flow(AnswerQuestion)
        .input(Question {
            q: "hi".to_string(),
        })
        .run()
        .await
        .unwrap();
    assert_eq!(
        out,
        Answer {
            answer: "ok".to_string()
        }
    );
}

#[test]
fn ai_flow_marker_exposes_typed_contract() {
    // The marker carries the id and typed I/O, so the input schema is derivable
    // from the type alone — no stringly-typed call site (#216).
    assert_eq!(<AnswerQuestion as FlowDef>::ID, "answer");
    let input_schema = serde_json::to_string(&pocopine_agenkit::schemars::schema_for!(
        <AnswerQuestion as FlowDef>::Input
    ))
    .unwrap();
    assert!(
        input_schema.contains("\"q\""),
        "input schema derivable from the marker: {input_schema}"
    );
}
