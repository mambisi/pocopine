#![cfg(not(target_arch = "wasm32"))]
//! Streaming: `ctx.ai().stream_text()` forwards incremental OutputDeltas to the
//! client stream, and the runtime does not duplicate the final result.

mod common;

use common::block_on;
use pocopine_agenkit::server::{Agenkit, AiFlowContext, Flow, MockProvider};
use pocopine_agenkit_core::{AgenkitResult, FlowStreamEvent, ModelRef};

const ANSWER: &str = "uploads use presigned URLs for direct browser upload";

async fn chat(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai().prompt("how do uploads work?").stream_text().await
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(MockProvider::new("local").on_prompt_text("uploads", ANSWER))
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("chat", chat).public())
        .build()
        .unwrap()
}

#[test]
fn flow_streams_incremental_output_deltas() {
    let agenkit = runtime();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result: String = block_on(async {
        let value = agenkit
            .flow("chat")
            .input(serde_json::Value::Null)
            .stream(tx)
            .await
            .unwrap();
        serde_json::from_value(value).unwrap()
    });
    assert_eq!(result, ANSWER);

    let mut deltas = Vec::new();
    let mut completions = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            FlowStreamEvent::OutputDelta { text } => deltas.push(text),
            FlowStreamEvent::OutputCompleted => completions += 1,
            _ => {}
        }
    }

    // The mock streams word-by-word, so the client sees many deltas (not one
    // buffered result), and they reassemble to the full answer with no
    // duplication from the runtime's final-result emission.
    assert!(
        deltas.len() > 3,
        "expected incremental deltas, got {deltas:?}"
    );
    assert_eq!(deltas.concat(), ANSWER);
    assert_eq!(completions, 1);
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Summary {
    title: String,
    words: u32,
}

async fn summarize(_input: (), ctx: AiFlowContext) -> AgenkitResult<Summary> {
    ctx.ai()
        .prompt("summarize the uploads doc")
        .schema::<Summary>()
        .stream_structured()
        .await
}

#[test]
fn structured_flow_streams_partial_objects() {
    // The mock streams the serialized JSON word-by-word, so the client sees the
    // object completing field-by-field (Genkit-style partial objects).
    let agenkit = Agenkit::builder()
        .provider(MockProvider::new("local").default_structured(
            serde_json::json!({"title": "Object storage uploads", "words": 12}),
        ))
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("summarize", summarize).public())
        .build()
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result: Summary = block_on(async {
        let value = agenkit
            .flow("summarize")
            .input(serde_json::Value::Null)
            .stream(tx)
            .await
            .unwrap();
        serde_json::from_value(value).unwrap()
    });
    assert_eq!(
        result,
        Summary {
            title: "Object storage uploads".to_string(),
            words: 12
        }
    );

    let mut partials = Vec::new();
    let mut text_deltas = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            FlowStreamEvent::ObjectDelta { partial } => partials.push(partial),
            FlowStreamEvent::OutputDelta { .. } => text_deltas += 1,
            _ => {}
        }
    }

    // Structured streaming emits partial *objects*, never raw text deltas, and
    // the runtime does not duplicate the final result as a text delta.
    assert_eq!(
        text_deltas, 0,
        "structured streaming must not emit text deltas"
    );
    assert!(
        partials.len() > 1,
        "expected incremental partials, got {partials:?}"
    );

    // Every partial is a JSON object, growing toward the full result: an early
    // partial has the (still-completing) title but not yet the trailing field.
    assert!(partials.iter().all(serde_json::Value::is_object));
    assert!(
        partials[0].get("words").is_none(),
        "first partial should arrive before the trailing field: {:?}",
        partials[0]
    );
    let first_title = partials[0]["title"].as_str().unwrap();
    assert!(
        "Object storage uploads".starts_with(first_title.trim_end()),
        "first partial title {first_title:?} should be a prefix of the final title"
    );

    // The last partial converges to exactly the validated result.
    let converged: Summary = serde_json::from_value(partials.last().unwrap().clone()).unwrap();
    assert_eq!(converged, result);
}

#[test]
fn non_streaming_flow_still_emits_the_result_once() {
    async fn computed(_input: (), _ctx: AiFlowContext) -> AgenkitResult<String> {
        // Returns a value without streaming it.
        Ok("computed".to_string())
    }
    let agenkit = Agenkit::builder()
        .provider(MockProvider::new("local"))
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("computed", computed).public())
        .build()
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _: String = block_on(async {
        let value = agenkit
            .flow("computed")
            .input(serde_json::Value::Null)
            .stream(tx)
            .await
            .unwrap();
        serde_json::from_value(value).unwrap()
    });

    let deltas: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|e| match e {
            FlowStreamEvent::OutputDelta { text } => Some(text),
            _ => None,
        })
        .collect();
    // Exactly one final-result delta for a flow that didn't stream.
    assert_eq!(deltas, vec!["\"computed\"".to_string()]);
}
