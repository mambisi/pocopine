#![cfg(not(target_arch = "wasm32"))]
//! Contract tests: drive the provider with **verbatim** payloads copied from
//! Anthropic's published Messages API docs/OpenAPI spec, so a drift between our
//! wire decoding and the real contract fails CI — without any network call.
//!
//! Fixtures captured from <https://platform.claude.com/docs/en/api/messages-streaming>
//! against OpenAPI spec hash `5ce93251…` (see the `reference-anthropic-openapi-spec`
//! memory). If Anthropic changes the wire shape, refresh these strings.

use futures::StreamExt;
use pocopine_agenkit::server::{GenerateRequest, Provider, ProviderContext, StreamChunk, models};
use pocopine_agenkit_anthropic::AnthropicProvider;
use pocopine_agenkit_core::Message;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new("anthropic", "test-key").with_base_url(format!("{}/v1", server.uri()))
}

fn text_request() -> GenerateRequest {
    GenerateRequest {
        model: models::anthropic::CLAUDE_OPUS_4_8,
        messages: vec![Message::user("hi")],
        ..GenerateRequest::default()
    }
}

async fn mount_sse(server: &MockServer, body: &'static str) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn drain(
    provider: &AnthropicProvider,
) -> (String, Vec<pocopine_agenkit_core::ToolCall>, Option<u64>) {
    let cx = ProviderContext::default();
    let mut stream = provider.generate_stream(text_request(), &cx);
    let (mut text, mut tools, mut usage) = (String::new(), Vec::new(), None);
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::Text(t) => text.push_str(&t),
            StreamChunk::ToolCall(c) => tools.push(c),
            StreamChunk::Usage(u) => usage = Some(u.total()),
            StreamChunk::Thinking { .. } => {}
        }
    }
    (text, tools, usage)
}

/// Verbatim "Basic streaming request" example from the Messages streaming docs.
const BASIC_TEXT_SSE: &str = r#"event: message_start
data: {"type": "message_start", "message": {"id": "msg_1nZdL29xx5MUA1yADyHTEsnR8uuvGzszyY", "type": "message", "role": "assistant", "content": [], "model": "claude-opus-4-8", "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 25, "output_tokens": 1}}}

event: content_block_start
data: {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}

event: ping
data: {"type": "ping"}

event: content_block_delta
data: {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}}

event: content_block_delta
data: {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "!"}}

event: content_block_stop
data: {"type": "content_block_stop", "index": 0}

event: message_delta
data: {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence":null}, "usage": {"output_tokens": 15}}

event: message_stop
data: {"type": "message_stop"}

"#;

#[tokio::test(flavor = "multi_thread")]
async fn decodes_the_documented_basic_text_stream() {
    let server = MockServer::start().await;
    mount_sse(&server, BASIC_TEXT_SSE).await;

    let (text, tools, usage) = drain(&provider(&server)).await;
    assert_eq!(text, "Hello!");
    assert!(tools.is_empty());
    // input_tokens (25, message_start) + output_tokens (15, cumulative message_delta).
    assert_eq!(usage, Some(40));
}

/// Verbatim "Streaming request with tool use" example: a text block (index 0)
/// followed by a `tool_use` block (index 1) whose input arrives as
/// `input_json_delta` fragments. (Trimmed only by removing `ping`-only noise.)
const TOOL_USE_SSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_014p7gG3wDgGV9EUtLvnow3U","type":"message","role":"assistant","model":"claude-opus-4-8","stop_sequence":null,"usage":{"input_tokens":472,"output_tokens":2},"content":[],"stop_reason":null}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me check"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01T1x1fJ34qAmk2tNTrN7Up6","name":"get_weather","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"location\":"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" \"San"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" Francisc"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"o,"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" CA\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":89}}

event: message_stop
data: {"type":"message_stop"}

"#;

#[tokio::test(flavor = "multi_thread")]
async fn decodes_the_documented_tool_use_stream() {
    let server = MockServer::start().await;
    mount_sse(&server, TOOL_USE_SSE).await;

    let (text, tools, usage) = drain(&provider(&server)).await;
    assert_eq!(text, "Let me check");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_id, "get_weather");
    assert_eq!(tools[0].args, json!({"location": "San Francisco, CA"}));
    assert_eq!(usage, Some(472 + 89));
}

/// Verbatim non-streaming `Message` response object from the spec.
#[tokio::test]
async fn decodes_the_documented_message_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "content": [{"type": "text", "text": "Hi! My name is Claude."}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 10, "output_tokens": 25}
        })))
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request(), &ProviderContext::default())
        .await
        .unwrap();
    assert_eq!(response.text_output(), "Hi! My name is Claude.");
    assert_eq!(response.usage.unwrap().total(), 35);
}
