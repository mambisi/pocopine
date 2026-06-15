#![cfg(not(target_arch = "wasm32"))]
//! Contract tests: drive the provider with payloads shaped per OpenAI's
//! published OpenAPI spec (<https://github.com/openai/openai-openapi>) —
//! `chat.completion`, `chat.completion.chunk`, and the `[DONE]` terminator —
//! so a drift between our wire decoding and the real contract fails CI, with no
//! network call. Refresh these fixtures if the upstream schema changes.

use futures::StreamExt;
use pocopine_agenkit::server::{GenerateRequest, Provider, StreamChunk};
use pocopine_agenkit_core::{Message, ModelRef};
use pocopine_agenkit_oai::OpenAiProvider;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new("openai", "test-key").with_base_url(format!("{}/v1", server.uri()))
}

fn text_request() -> GenerateRequest {
    GenerateRequest {
        model: ModelRef::new("openai/gpt-4o-mini"),
        messages: vec![Message::user("hi")],
        ..GenerateRequest::default()
    }
}

async fn mount_sse(server: &MockServer, body: &'static str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn drain(
    provider: &OpenAiProvider,
) -> (String, Vec<pocopine_agenkit_core::ToolCall>, Option<u64>) {
    let mut stream = provider.generate_stream(text_request());
    let (mut text, mut tools, mut usage) = (String::new(), Vec::new(), None);
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::Text(t) => text.push_str(&t),
            StreamChunk::ToolCall(c) => tools.push(c),
            StreamChunk::Usage(u) => usage = Some(u.total()),
        }
    }
    (text, tools, usage)
}

/// `chat.completion.chunk` text stream, terminated by `[DONE]`, with the final
/// usage chunk emitted under `stream_options.include_usage` (choices empty).
const TEXT_STREAM_SSE: &str = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"content":"!"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o-mini","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":12,"total_tokens":21}}

data: [DONE]

"#;

#[tokio::test(flavor = "multi_thread")]
async fn decodes_a_spec_shaped_text_stream() {
    let server = MockServer::start().await;
    mount_sse(&server, TEXT_STREAM_SSE).await;

    let (text, tools, usage) = drain(&provider(&server)).await;
    assert_eq!(text, "Hello!");
    assert!(tools.is_empty());
    assert_eq!(usage, Some(21)); // prompt_tokens 9 + completion_tokens 12
}

/// `chat.completion.chunk` tool-call stream: `id`/`name` arrive once, then the
/// arguments stream in fragments correlated by `index`.
const TOOL_STREAM_SSE: &str = r#"data: {"id":"chatcmpl-2","object":"chat.completion.chunk","model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","model":"gpt-4o-mini","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","model":"gpt-4o-mini","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"SF\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]

"#;

#[tokio::test(flavor = "multi_thread")]
async fn decodes_a_spec_shaped_tool_call_stream() {
    let server = MockServer::start().await;
    mount_sse(&server, TOOL_STREAM_SSE).await;

    let (_text, tools, _usage) = drain(&provider(&server)).await;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_id, "get_weather");
    assert_eq!(tools[0].args, json!({"location": "SF"}));
}

/// A non-streaming `chat.completion` object carrying `tool_calls`.
#[tokio::test]
async fn decodes_a_spec_shaped_tool_call_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-3",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"location\":\"SF\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 50, "completion_tokens": 20, "total_tokens": 70}
        })))
        .mount(&server)
        .await;

    let response = provider(&server).generate(text_request()).await.unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].tool_id, "get_weather");
    assert_eq!(response.tool_calls[0].args, json!({"location": "SF"}));
    assert_eq!(response.usage.unwrap().total(), 70);
}
