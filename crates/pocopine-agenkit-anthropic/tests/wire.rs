#![cfg(not(target_arch = "wasm32"))]
//! Integration tests for the Anthropic Messages API provider, driven against a
//! `wiremock` server (no real API calls / credentials).

use futures::StreamExt;
use pocopine_agenkit::server::{
    Agenkit, AiFlowContext, AuthUser, BoxFuture, Flow, GenerateRequest, Principal, Provider,
    ProviderContext, ProviderCredential, ProviderCredentials, StreamChunk, models, with_principal,
};
use pocopine_agenkit_anthropic::AnthropicProvider;
use pocopine_agenkit_core::{
    AgenkitResult, Content, ContentPart, FlowStreamEvent, Message, StreamMode, ThinkingLevel,
    ToolDescriptor,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new("anthropic", "test-key").with_base_url(format!("{}/v1", server.uri()))
}

fn text_request(prompt: &str) -> GenerateRequest {
    GenerateRequest {
        model: models::anthropic::CLAUDE_OPUS_4_8,
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
    let response = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
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
    let response = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
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
    let response = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
    // Surfaced as a structured value (not a tool call) for the runtime to validate.
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.text_output(), "");
    assert_eq!(
        response.structured_value().cloned(),
        Some(json!({"title": "Uploads"}))
    );
}

#[tokio::test]
async fn structured_with_user_tools_injects_prompt_not_forced_tool() {
    // Anthropic forces at most one tool per turn, so with user tools present the
    // provider must NOT force the structured tool (that would suppress them) —
    // it injects the schema as a system-prompt instruction instead. This is the
    // path the agent loop (tools + json_schema) relies on for structured output.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert!(
                body.get("tool_choice").is_none(),
                "must not force a tool when user tools are present"
            );
            let names: Vec<&str> = body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect();
            assert_eq!(
                names,
                vec!["weather_lookup"],
                "no synthetic structured tool"
            );
            let system = body["system"].as_str().unwrap_or_default();
            assert!(
                system.contains("conforms to this JSON Schema") && system.contains("title"),
                "system prompt should carry the schema instruction: {system}"
            );
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "{\"title\":\"Uploads\"}"}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        tools: vec![ToolDescriptor::new("weather.lookup", "Look up weather")],
        json_schema: Some(json!({"type": "object", "properties": {"title": {"type": "string"}}})),
        ..text_request("summarize")
    };
    // The model emits the JSON as text; the runtime parses it downstream.
    let response = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
    assert_eq!(response.text_output(), "{\"title\":\"Uploads\"}");
}

#[tokio::test]
async fn tool_use_with_missing_input_defaults_to_empty_object() {
    // A `tool_use` block with no `input` must surface as `{}`, never `null` —
    // struct-typed tool inputs deserialize from `{}`, not from `null`.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "ping"}],
            "stop_reason": "tool_use"
        })))
        .mount(&server)
        .await;

    let request = GenerateRequest {
        tools: vec![ToolDescriptor::new("ping", "Ping")],
        ..text_request("ping")
    };
    let response = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].args, json!({}));
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
    let cx = ProviderContext::default();
    let mut stream = client.generate_stream(text_request("how do uploads work?"), &cx);
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
            StreamChunk::ToolCall(_) | StreamChunk::Thinking { .. } => {}
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
    let cx = ProviderContext::default();
    let mut stream = client.generate_stream(text_request("weather?"), &cx);
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

#[tokio::test(flavor = "multi_thread")]
async fn truncated_stream_still_emits_tool_call_and_usage() {
    // A stream that ends (EOF) before `content_block_stop`/`message_stop` — e.g.
    // a proxy that closed the body mid-response — must still surface the
    // accumulated tool call and the token usage, not silently drop them.
    let server = MockServer::start().await;
    let body = format!(
        "{}{}{}{}",
        sse(
            "message_start",
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 3, "output_tokens": 0}}})
        ),
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\"Paris\"}"}})
        ),
        // Usage restated, then the body ends — NO content_block_stop / message_stop.
        sse(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}})
        ),
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
    let cx = ProviderContext::default();
    let mut stream = client.generate_stream(text_request("weather?"), &cx);
    let mut tool_call = None;
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::ToolCall(call) => tool_call = Some(call),
            StreamChunk::Usage(reported) => usage = Some(reported),
            StreamChunk::Text(_) | StreamChunk::Thinking { .. } => {}
        }
    }
    let call = tool_call.expect("truncated stream must still emit the tool call");
    assert_eq!(call.tool_id, "get_weather");
    assert_eq!(call.args, json!({"city": "Paris"}));
    assert_eq!(
        usage
            .expect("truncated stream must still emit usage")
            .total(),
        8
    );
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
        .default_model(models::anthropic::CLAUDE_OPUS_4_8)
        .flow(Flow::new("summarize", summarize).public())
        .build()
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let value = agenkit
        .flow("summarize")
        .input(serde_json::Value::Null)
        .stream_into(tx)
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
        .generate(text_request("hi"), &ProviderContext::default())
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
        .generate(text_request("hi"), &ProviderContext::default())
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

// ── W4: reasoning ("thinking") content ───────────────────────────────────────

#[tokio::test]
async fn thinking_request_enables_budget_for_reasoning_model() {
    // A reasoning-capable model (opus-4-8) with `ThinkingLevel::High` sends an
    // `enabled` thinking block and bumps `max_tokens` above the budget so the
    // model still has output room (Anthropic requires max_tokens > budget_tokens).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["thinking"]["type"], "enabled");
            assert_eq!(body["thinking"]["budget_tokens"], 16384);
            assert_eq!(body["max_tokens"], 16384 + 4096);
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        thinking: ThinkingLevel::High,
        ..text_request("think hard")
    };
    let response = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
    assert_eq!(response.text_output(), "ok");
}

#[tokio::test]
async fn thinking_adds_its_budget_on_top_of_an_explicit_max_tokens() {
    // A caller's explicit `max_tokens` is their OUTPUT budget; thinking is added
    // on top, not overriding it (review #12): 500 output + 16384 thinking.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["max_tokens"], 500 + 16384);
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        thinking: ThinkingLevel::High,
        max_tokens: Some(500),
        ..text_request("think hard")
    };
    provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn thinking_request_omitted_for_non_reasoning_model() {
    // A non-reasoning model ignores the thinking level entirely: no `thinking`
    // block, and `max_tokens` keeps its default (not bumped).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert!(
                body.get("thinking").is_none(),
                "non-reasoning model must not request thinking"
            );
            assert_eq!(body["max_tokens"], 4096);
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        model: models::anthropic::CLAUDE_3_HAIKU_20240307,
        thinking: ThinkingLevel::High,
        ..text_request("hi")
    };
    let _ = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn parses_a_thinking_block_into_a_server_side_part() {
    // A response carrying a `thinking` block surfaces a Thinking content part
    // (with its signature) that `text_output()` skips — server-side only (§D10).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                {"type": "thinking", "thinking": "step one, step two", "signature": "sig-xyz"},
                {"type": "text", "text": "the answer is 42"}
            ],
            "stop_reason": "end_turn"
        })))
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request("q"), &ProviderContext::default())
        .await
        .unwrap();
    assert_eq!(response.text_output(), "the answer is 42");
    let thinking = response.content.parts.iter().find_map(|p| p.as_thinking());
    assert_eq!(thinking, Some(("step one, step two", Some("sig-xyz"))));
}

#[tokio::test]
async fn replays_an_assistant_thinking_block_verbatim() {
    // A stored assistant turn that carries a Thinking part must replay it — text
    // first preceded by the thinking block with its signature verbatim, or
    // Anthropic 400s a follow-up after extended thinking.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let blocks = &body["messages"][1]["content"];
            assert_eq!(blocks[0]["type"], "thinking");
            assert_eq!(blocks[0]["thinking"], "step one, step two");
            assert_eq!(blocks[0]["signature"], "sig-xyz");
            assert_eq!(blocks[1]["type"], "text");
            assert_eq!(blocks[1]["text"], "the answer is 42");
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let assistant = Message::assistant(Content::from_parts(vec![
        ContentPart::thinking("step one, step two", Some("sig-xyz".to_string())),
        ContentPart::text("the answer is 42"),
    ]));
    let request = GenerateRequest {
        messages: vec![Message::user("q"), assistant, Message::user("again")],
        ..text_request("again")
    };
    let _ = provider(&server)
        .generate(request, &ProviderContext::default())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_thinking_block_as_an_internal_chunk() {
    // The decoder accumulates `thinking_delta` text + `signature_delta` and emits
    // a single internal Thinking chunk on `content_block_stop` (W4).
    let server = MockServer::start().await;
    let body = format!(
        "{}{}{}{}{}{}",
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "let me "}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "reason"}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-stream"}})
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
    let cx = ProviderContext::default();
    let mut stream = client.generate_stream(text_request("q"), &cx);
    let mut thinking = None;
    while let Some(chunk) = stream.next().await {
        if let StreamChunk::Thinking { text, signature } = chunk.unwrap() {
            thinking = Some((text, signature));
        }
    }
    assert_eq!(
        thinking,
        Some(("let me reason".to_string(), Some("sig-stream".to_string())))
    );
}

/// Mount the canonical "reasoning then answer" Anthropic SSE: one thinking block
/// (`SECRET REASONING` + a signature) followed by the text answer `the answer`.
async fn mount_thinking_then_answer(server: &MockServer) {
    let body = format!(
        "{}{}{}{}{}{}{}{}",
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "SECRET REASONING"}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-1"}})
        ),
        sse(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0})
        ),
        sse(
            "content_block_start",
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}})
        ),
        sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "the answer"}})
        ),
        sse(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 1})
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
        .mount(server)
        .await;
}

async fn answer(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai().prompt("q").stream_text().await
}

/// Collect every client event a `.stream()` on the `answer` flow produces.
/// `request_reasoning` is the caller half of the reasoning gate.
async fn client_events(agenkit: &Agenkit, request_reasoning: bool) -> Vec<FlowStreamEvent> {
    agenkit
        .flow("answer")
        .input(serde_json::Value::Null)
        .request_reasoning(request_reasoning)
        .stream()
        .expect("public flow yields a stream")
        .map(|item| item.expect("each streamed item is Ok"))
        .collect()
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_thinking_text_stays_server_side_by_default() {
    // The redaction default (§D10): a flow that did NOT opt into reasoning must
    // not leak the reasoning text in any client event — only the answer crosses.
    let server = MockServer::start().await;
    mount_thinking_then_answer(&server).await;

    let agenkit = Agenkit::builder()
        .provider(provider(&server))
        .default_model(models::anthropic::CLAUDE_OPUS_4_8)
        // public, default mode, NO `.expose_reasoning()`.
        .flow(Flow::new("answer", answer).public())
        .build()
        .unwrap();

    let events = client_events(&agenkit, true).await;
    for event in &events {
        let json = serde_json::to_string(event).unwrap();
        assert!(
            !json.contains("SECRET REASONING"),
            "reasoning leaked into a client stream event: {json}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn thinking_activity_crosses_as_a_redacted_count_at_progress() {
    // W5: under Progress visibility, a non-exposing flow surfaces the thinking
    // block as a `ThinkingDelta` carrying a CHARACTER COUNT only — enough for a
    // "thinking…" indicator, never the reasoning text itself (§D10).
    let server = MockServer::start().await;
    mount_thinking_then_answer(&server).await;

    let agenkit = Agenkit::builder()
        .provider(provider(&server))
        .default_model(models::anthropic::CLAUDE_OPUS_4_8)
        .flow(
            Flow::new("answer", answer)
                .public()
                .stream_mode(StreamMode::Progress),
        )
        .build()
        .unwrap();

    let events = client_events(&agenkit, true).await;

    // The thinking block crosses as a count == len("SECRET REASONING"), no text.
    let thinking: Vec<(u32, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            FlowStreamEvent::ThinkingDelta { chars, text } => Some((*chars, text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        thinking,
        vec![("SECRET REASONING".chars().count() as u32, None)]
    );
    for event in &events {
        let json = serde_json::to_string(event).unwrap();
        assert!(
            !json.contains("SECRET REASONING"),
            "reasoning leaked into a client stream event: {json}"
        );
    }

    // The user-visible output is still just the answer.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            FlowStreamEvent::OutputDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "the answer");
}

#[tokio::test(flavor = "multi_thread")]
async fn opted_in_flow_streams_the_reasoning_text_to_the_client() {
    // The opt-in (§D10): a flow that calls `.expose_reasoning()` streams the
    // reasoning text in `ThinkingDelta { text }`, alongside the output.
    let server = MockServer::start().await;
    mount_thinking_then_answer(&server).await;

    let agenkit = Agenkit::builder()
        .provider(provider(&server))
        .default_model(models::anthropic::CLAUDE_OPUS_4_8)
        .flow(
            Flow::new("answer", answer)
                .public()
                .expose_reasoning()
                .stream_mode(StreamMode::OutputDeltas),
        )
        .build()
        .unwrap();

    let events = client_events(&agenkit, true).await;

    // The reasoning text crosses verbatim now that the author opted in.
    let thinking: Vec<(u32, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            FlowStreamEvent::ThinkingDelta { chars, text } => Some((*chars, text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        thinking,
        vec![(
            "SECRET REASONING".chars().count() as u32,
            Some("SECRET REASONING".to_string())
        )]
    );

    // And the answer still streams as normal output.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            FlowStreamEvent::OutputDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "the answer");
}

/// A credentials store that hands each principal its own API key (BYOK).
struct PerUserKeys;

impl ProviderCredentials for PerUserKeys {
    fn resolve<'a>(
        &'a self,
        _provider: &'a str,
        principal: &'a Principal,
    ) -> BoxFuture<'a, AgenkitResult<Option<ProviderCredential>>> {
        // Each user's key is `key-<id>`; an anonymous caller gets nothing (→ the
        // provider's built-in credential).
        let credential = principal
            .user()
            .map(|user| ProviderCredential::api_key(format!("key-{}", user.id)));
        Box::pin(async move { Ok(credential) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn byok_resolves_a_per_principal_key_to_the_wire() {
    // W6.2: with a per-principal `ProviderCredentials` store, two users' requests
    // carry two DIFFERENT API keys — the runtime resolves the credential for the
    // current principal and the provider sends it. The mock echoes the received
    // `x-api-key` back as the answer, so the assertion proves which key hit the wire.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let key = req
                .headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<none>")
                .to_string();
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": key}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let agenkit = Agenkit::builder()
        // The provider is built with a baked "test-key"; the store overrides it
        // per principal, so "test-key" must never reach the wire here.
        .provider(provider(&server))
        .credentials(std::sync::Arc::new(PerUserKeys))
        .default_model(models::anthropic::CLAUDE_OPUS_4_8)
        .build()
        .unwrap();

    // Alice's request authenticates with her key...
    let alice = with_principal(
        Principal::from_user(AuthUser::new("alice")),
        agenkit.ai().prompt("hi").generate_text(),
    )
    .await
    .unwrap();
    assert_eq!(alice, "key-alice");

    // ...and Bob's with his — a distinct credential on the same provider.
    let bob = with_principal(
        Principal::from_user(AuthUser::new("bob")),
        agenkit.ai().prompt("hi").generate_text(),
    )
    .await
    .unwrap();
    assert_eq!(bob, "key-bob");
    assert_ne!(alice, bob);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_store_match_falls_back_to_the_provider_credential() {
    // The store returns None for an anonymous caller → the runtime passes no
    // override and the provider uses its built-in "test-key" (W6.2 fallback).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(|req: &Request| {
            let key = req
                .headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<none>")
                .to_string();
            ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": key}],
                "stop_reason": "end_turn"
            }))
        })
        .mount(&server)
        .await;

    let agenkit = Agenkit::builder()
        .provider(provider(&server))
        .credentials(std::sync::Arc::new(PerUserKeys))
        .default_model(models::anthropic::CLAUDE_OPUS_4_8)
        .build()
        .unwrap();

    // No principal in scope → anonymous → store None → baked "test-key".
    let answer = agenkit.ai().prompt("hi").generate_text().await.unwrap();
    assert_eq!(answer, "test-key");
}
