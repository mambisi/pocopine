#![cfg(not(target_arch = "wasm32"))]
//! Phase 2.5 exit gate: a registered flow runs `ctx.retrieve` + `ctx.step` +
//! `ctx.ai()` and emits a correlated trace tree through `pocopine.trace`.

mod common;

use common::{block_on, capture};
use pocopine_agenkit::server::{
    Agenkit, AiFlowContext, AiRetriever, BoxFuture, Flow, MockProvider, RetrievalContext,
};
use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ModelRef, RetrievalHit, RetrievalSet,
    RetrieverDescriptor, SourceRef,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct QuestionInput {
    question: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
struct Answer {
    text: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct DocsQuery {
    question: String,
}

#[derive(Default)]
struct ProjectDocs;

impl AiRetriever for ProjectDocs {
    const ID: &'static str = "project_docs";
    type Query = DocsQuery;

    fn descriptor() -> RetrieverDescriptor {
        RetrieverDescriptor::new("project_docs", "Search project docs").with_source_kinds(["doc"])
    }

    fn retrieve(
        &self,
        query: DocsQuery,
        _ctx: RetrievalContext,
    ) -> BoxFuture<'_, AgenkitResult<RetrievalSet>> {
        Box::pin(async move {
            Ok(RetrievalSet::new(vec![RetrievalHit {
                source: SourceRef::new("doc", "uploads.md"),
                score: Some(0.9),
                content: Content::text(format!("uploads use presigned URLs ({})", query.question)),
                citation: None,
            }]))
        })
    }
}

async fn answer_question(input: QuestionInput, ctx: AiFlowContext) -> AgenkitResult<Answer> {
    let docs = ctx
        .retrieve::<ProjectDocs>()
        .query(DocsQuery {
            question: input.question,
        })
        .top_k(3)
        .run()
        .await?;

    let context = ctx
        .step("assemble_context", async move {
            Ok::<_, AgenkitError>(
                docs.hits
                    .iter()
                    .map(|hit| hit.content.as_text())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .await?;

    ctx.ai()
        .system("Answer using the retrieved docs.")
        .prompt(context)
        .schema::<Answer>()
        .generate_structured()
        .await
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("local")
                .default_structured(serde_json::json!({"text": "uploads use presigned URLs"})),
        )
        .default_model(ModelRef::new("local/default"))
        .retriever(ProjectDocs)
        .flow(
            Flow::new("answer_question", answer_question)
                .public()
                .uses_retriever("project_docs"),
        )
        .build()
        .unwrap()
}

#[test]
fn flow_runs_and_emits_correlated_trace_tree() {
    let trace = capture(|| {
        let answer: Answer = block_on(runtime().run_flow(
            "answer_question",
            QuestionInput {
                question: "how do uploads work?".to_string(),
            },
        ))
        .unwrap();
        assert_eq!(
            answer,
            Answer {
                text: "uploads use presigned URLs".to_string()
            }
        );
    });

    for name in [
        "ai_flow_started",
        "ai_retrieval_started",
        "ai_retrieval_completed",
        "ai_step_started",
        "ai_step_completed",
        "ai_model_request",
        "ai_model_response",
        "ai_flow_completed",
    ] {
        assert!(
            trace.contains(name),
            "missing trace event `{name}`; got {:?}",
            trace.names()
        );
    }
}

#[test]
fn undeclared_retriever_emits_advisory_context_gap() {
    let agenkit = Agenkit::builder()
        .provider(MockProvider::new("local").default_structured(serde_json::json!({"text": "x"})))
        .default_model(ModelRef::new("local/default"))
        .retriever(ProjectDocs)
        // NOTE: flow does NOT declare `.uses_retriever("project_docs")`.
        .flow(Flow::new("answer_question", answer_question))
        .build()
        .unwrap();

    let trace = capture(|| {
        let _: Answer = block_on(agenkit.run_flow(
            "answer_question",
            QuestionInput {
                question: "q".to_string(),
            },
        ))
        .unwrap();
    });

    // The flow still runs (advisory only), but a context-gap event is emitted.
    assert!(
        trace.contains("ai_context_gap"),
        "expected ai_context_gap; got {:?}",
        trace.names()
    );
}
