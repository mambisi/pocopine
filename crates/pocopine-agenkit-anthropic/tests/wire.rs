#![cfg(not(target_arch = "wasm32"))]
//! Integration tests for the Anthropic Messages API provider, driven against a
//! `wiremock` server (no real API calls / credentials).

use futures::StreamExt;
use pocopine_agenkit::server::{
    Agenkit, AiFlowContext, Flow, GenerateRequest, Provider, StreamChunk,
};
use pocopine_agenkit_anthropic::AnthropicProvider;
use pocopine_agenkit_core::{AgenkitResult, FlowStreamEvent, Message, ModelRef, ToolDescriptor};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new("anthropic", "test-key").with_base_url(format!("{}/v1", server.uri()))
}

fn text_request(prompt: &str) -> GenerateRequest {
    GenerateRequest {
        model: ModelRef::new("anthropic/claude-opus-4-8"),
        messages: vec![Message::user(prompt)],
        ..GenerateRequest::default()
    }
}

/// Build one Anthropic SSE event (`event:` + `data:` lines, blank-line ended).
fn sse(event: &str, data: serde_json::Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap()
    )
}

#[tokio::test]
async fn maps_text_and_sends_anthropic_headers_with_required_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            // The provider namespace is stripped and max_tokens is always set.
            assert_eq!(body["model"], "claude-opus-4-8");
            assert_eq!(body["max_tokens"], 4096);
            assert_eq!(body["system"], "be brief");
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "uploads use presigned URLs"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 7, "output_tokens": 5}
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        system: Some("be brief".to_string()),
        ..text_request("how do uploads work?")
    };
    let response = provider(&server).generate(request).await.unwrap();
    assert_eq!(response.text_output(), "uploads use presigned URLs");
    assert_eq!(response.usage.unwrap().total(), 12);
}

#[tokio::test]
async fn maps_tools_and_reverse_maps_sanitized_names() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            // The dotted tool id is sanitized to Anthropic's name rule.
            assert_eq!(body["tools"][0]["name"], "weather_lookup");
            assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "weather_lookup",
                    "input": {"city": "Paris"}
                }],
                "stop_reason": "tool_use"
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        tools: vec![ToolDescriptor::new("weather.lookup", "Look up weather")],
        ..text_request("weather?")
    };
    let response = provider(&server).generate(request).await.unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    // ...and the response tool name maps back to the original id for dispatch.
    assert_eq!(response.tool_calls[0].tool_id, "weather.lookup");
    assert_eq!(response.tool_calls[0].args, json!({"city": "Paris"}));
}

#[tokio::test]
async fn forces_a_tool_for_structured_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            // No user tools + a schema → a single forced structured-output tool.
            assert_eq!(body["tool_choice"]["type"], "tool");
            assert_eq!(body["tool_choice"]["name"], "structured_output");
            // The model "calls" it with the structured answer.
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "structured_output",
                    "input": {"title": "Uploads"}
                }],
                "stop_reason": "tool_use"
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        json_schema: Some(json!({"type": "object", "properties": {"title": {"type": "string"}}})),
        ..text_request("summarize")
    };
    let response = provider(&server).generate(request).await.unwrap();
    // Surfaced as a structured value (not a tool call) for the runtime to validate.
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.text_output(), "");
    assert_eq!(
        response.structured_value().cloned(),
        Some(json!({"title": "Uploads"}))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_text_deltas_and_usage() {
    let server = MockServer::start().await;
    let body = format!(
        "{}{}{}{}{}{}",
        sse(
            "message_start",
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 3, "output_tokens": 0}}})
        ),
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "uploads "}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "use presigned URLs"}})
        ),
        sse(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}})
        ),
        sse("message_stop", json!({"type": "message_stop"})),
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let client = provider(&server);
    let mut stream = client.generate_stream(text_request("how do uploads work?"));
    let mut text = String::new();
    let mut usage = None;
    let mut deltas = 0;
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::Text(fragment) => {
                deltas += 1;
                text.push_str(&fragment);
            }
            StreamChunk::Usage(reported) => usage = Some(reported),
            StreamChunk::ToolCall(_) => {}
        }
    }
    assert_eq!(text, "uploads use presigned URLs");
    assert!(deltas >= 2, "expected incremental deltas, got {deltas}");
    assert_eq!(usage.unwrap().total(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_tool_call_from_input_json_deltas() {
    let server = MockServer::start().await;
    let body = format!(
        "{}{}{}{}{}",
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"Paris\"}"}})
        ),
        sse(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0})
        ),
        sse("message_stop", json!({"type": "message_stop"})),
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let client = provider(&server);
    let mut stream = client.generate_stream(text_request("weather?"));
    let mut tool_call = None;
    while let Some(chunk) = stream.next().await {
        if let StreamChunk::ToolCall(call) = chunk.unwrap() {
            tool_call = Some(call);
        }
    }
    let call = tool_call.expect("expected a streamed tool call");
    assert_eq!(call.tool_id, "get_weather");
    assert_eq!(call.args, json!({"city": "Paris"}));
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Summary {
    title: String,
    words: u32,
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_structured_object_deltas_via_forced_tool() {
    let server = MockServer::start().await;
    // The forced structured tool's input arrives as `input_json_delta`
    // fragments; the provider forwards them as text so the runtime parses
    // progressively-completing partial objects.
    let body = format!(
        "{}{}{}{}{}",
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "structured_output", "input": {}}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"title\":\"Obj"}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "ect storage\",\"words\":12}"}})
        ),
        sse(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0})
        ),
        sse("message_stop", json!({"type": "message_stop"})),
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    async fn summarize(_input: (), ctx: AiFlowContext) -> AgenkitResult<Summary> {
        ctx.ai()
            .prompt("summarize")
            .schema::<Summary>()
            .stream_structured()
            .await
    }

    let agenkit = Agenkit::builder()
        .provider(provider(&server))
        .default_model(ModelRef::new("anthropic/claude-opus-4-8"))
        .flow(Flow::new("summarize", summarize).public())
        .build()
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let value = agenkit
        .flow("summarize")
        .input(serde_json::Value::Null)
        .stream(tx)
        .await
        .unwrap();
    let result: Summary = serde_json::from_value(value).unwrap();
    assert_eq!(
        result,
        Summary {
            title: "Object storage".to_string(),
            words: 12
        }
    );

    let partials = {
        let mut acc = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let FlowStreamEvent::ObjectDelta { partial } = event {
                acc.push(partial);
            }
        }
        acc
    };
    assert!(
        partials.len() > 1,
        "expected incremental partials, got {partials:?}"
    );
}

#[tokio::test]
async fn http_error_does_not_leak_message_or_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "invalid x-api-key test-key"}
        })))
        .mount(&server)
        .await;

    let error = provider(&server)
        .generate(text_request("hi"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "provider");
    let rendered = error.to_string();
    assert!(rendered.contains("401"));
    assert!(rendered.contains("type=authentication_error"));
    assert!(!rendered.contains("test-key"), "leaked the key: {rendered}");
}

#[tokio::test]
async fn retries_a_transient_server_error_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(json!({
            "type": "error", "error": {"type": "overloaded_error"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "recovered"}],
            "stop_reason": "end_turn"
        })))
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request("hi"))
        .await
        .unwrap();
    assert_eq!(response.text_output(), "recovered");
}

#[test]
fn debug_redacts_the_api_key() {
    let provider = AnthropicProvider::new("anthropic", "sk-ant-SECRET");
    let rendered = format!("{provider:?}");
    assert!(!rendered.contains("sk-ant-SECRET"), "leaked: {rendered}");
    assert!(rendered.contains("<redacted>"));
}
