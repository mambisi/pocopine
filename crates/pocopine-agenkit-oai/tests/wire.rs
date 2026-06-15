#![cfg(not(target_arch = "wasm32"))]
//! Integration tests for the OpenAI-compatible provider, driven against a
//! `wiremock` server (no real API calls / credentials).

use futures::StreamExt;
use pocopine_agenkit::server::{
    Agenkit, AiFlowContext, Flow, GenerateRequest, Provider, StreamChunk,
};
use pocopine_agenkit_core::{AgenkitResult, FlowStreamEvent, Message, ModelRef, ToolDescriptor};
use pocopine_agenkit_oai::{MaxTokensParam, OpenAiProvider};
use serde::Deserialize;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new("openai", "test-key").with_base_url(format!("{}/v1", server.uri()))
}

/// Build one SSE `data:` line carrying a content delta (JSON-escaped via serde).
fn sse_content_chunk(content: &str) -> String {
    let body = json!({ "choices": [{ "delta": { "content": content } }] });
    format!("data: {}\n\n", serde_json::to_string(&body).unwrap())
}

fn text_request(prompt: &str) -> GenerateRequest {
    GenerateRequest {
        model: ModelRef::new("openai/gpt-4o-mini"),
        messages: vec![Message::user(prompt)],
        ..GenerateRequest::default()
    }
}

#[tokio::test]
async fn maps_text_completion_and_sends_bearer_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "uploads use presigned URLs"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 5}
        })))
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request("how do uploads work?"))
        .await
        .unwrap();
    assert_eq!(response.text_output(), "uploads use presigned URLs");
    assert_eq!(response.usage.unwrap().total(), 12);
}

#[tokio::test]
async fn structured_request_sets_response_format_and_strips_namespace() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &Request| {
            #[derive(Deserialize)]
            struct Body {
                model: String,
                response_format: Option<serde_json::Value>,
            }
            let body: Body = serde_json::from_slice(&req.body).unwrap();
            // The provider namespace is stripped and structured mode is set.
            assert_eq!(body.model, "gpt-4o-mini");
            assert_eq!(body.response_format, Some(json!({"type": "json_object"})));
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{\"title\":\"Uploads\"}"}, "finish_reason": "stop"}]
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        json_schema: Some(json!({"type": "object"})),
        ..text_request("summarize")
    };
    let response = provider(&server).generate(request).await.unwrap();
    assert_eq!(response.text_output(), "{\"title\":\"Uploads\"}");
}

#[tokio::test]
async fn maps_tool_calls_from_the_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "search_docs", "arguments": "{\"query\":\"uploads\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request("find docs"))
        .await
        .unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].tool_id, "search_docs");
    assert_eq!(response.tool_calls[0].args, json!({"query": "uploads"}));
}

#[tokio::test]
async fn gateway_base_url_sends_legacy_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            // A non-official base URL defaults to the legacy field.
            assert_eq!(body["max_tokens"], 128);
            assert!(body.get("max_completion_tokens").is_none());
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "ok"}}]}))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        max_tokens: Some(128),
        ..text_request("hi")
    };
    provider(&server).generate(request).await.unwrap();
}

#[tokio::test]
async fn max_completion_tokens_override_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            // The override forces the new field (what o-series / gpt-5 require).
            assert_eq!(body["max_completion_tokens"], 128);
            assert!(body.get("max_tokens").is_none());
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "ok"}}]}))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        max_tokens: Some(128),
        ..text_request("hi")
    };
    provider(&server)
        .with_max_tokens_param(MaxTokensParam::MaxCompletionTokens)
        .generate(request)
        .await
        .unwrap();
}

#[tokio::test]
async fn retries_a_transient_server_error_then_succeeds() {
    let server = MockServer::start().await;
    // First a 503 (retryable), then a 200. `up_to_n_times` + ordering: mount the
    // 503 mock with a hit cap so the second attempt falls through to the 200.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"type": "server_error"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "recovered"}, "finish_reason": "stop"}]
        })))
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request("hi"))
        .await
        .unwrap();
    assert_eq!(response.text_output(), "recovered");
}

#[tokio::test]
async fn gives_up_after_max_retries_without_leaking_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "internal boom sk-proj-LEAK", "type": "server_error"}
        })))
        .mount(&server)
        .await;

    let error = provider(&server)
        .with_max_retries(1)
        .generate(text_request("hi"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "provider");
    let rendered = error.to_string();
    assert!(rendered.contains("500"));
    assert!(
        !rendered.contains("sk-proj-LEAK"),
        "leaked body: {rendered}"
    );
}

#[tokio::test]
async fn sanitizes_tool_names_outbound_and_maps_them_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            // The dotted tool id was sanitized for the wire.
            assert_eq!(body["tools"][0]["function"]["name"], "weather_lookup");
            // The model replies using that sanitized name.
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "weather_lookup", "arguments": "{}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }))
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        tools: vec![ToolDescriptor::new("weather.lookup", "Look up weather")],
        ..text_request("weather?")
    };
    let response = provider(&server).generate(request).await.unwrap();
    // ...and the response maps it back to the real tool id so dispatch works.
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].tool_id, "weather.lookup");
}

#[tokio::test]
async fn http_error_maps_to_provider_error_without_leaking_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "message": "Incorrect API key provided: sk-proj-LEAK",
                "type": "invalid_request_error",
                "code": "invalid_api_key"
            }
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
    assert!(
        !rendered.contains("sk-proj-LEAK"),
        "error leaked the key: {rendered}"
    );
}

#[tokio::test]
async fn integrates_as_an_agenkit_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "hello from gpt"}, "finish_reason": "stop"}]
        })))
        .mount(&server)
        .await;

    let agenkit = Agenkit::builder()
        .provider(provider(&server))
        .default_model(ModelRef::new("openai/gpt-4o-mini"))
        .build()
        .unwrap();

    let answer = agenkit.ai().prompt("hi").generate_text().await.unwrap();
    assert_eq!(answer, "hello from gpt");
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_text_deltas_and_usage_from_sse() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"uploads \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"use presigned URLs\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let client = provider(&server);
    let mut stream = client.generate_stream(text_request("how do uploads work?"));
    let mut text = String::new();
    let mut usage = None;
    let mut delta_count = 0;
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::Text(fragment) => {
                delta_count += 1;
                text.push_str(&fragment);
            }
            StreamChunk::Usage(reported) => usage = Some(reported),
            StreamChunk::ToolCall(_) => {}
        }
    }
    assert_eq!(text, "uploads use presigned URLs");
    assert!(
        delta_count >= 2,
        "expected incremental deltas, got {delta_count}"
    );
    assert_eq!(usage.unwrap().total(), 5);
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Summary {
    title: String,
    words: u32,
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_structured_object_deltas_across_sse_chunks() {
    let server = MockServer::start().await;
    // The model streams the JSON object split across SSE chunks mid-token; the
    // facade parses each accumulated prefix into a partial object (ObjectDelta).
    // Reassembled: {"title":"Object storage","words":12}
    let sse = format!(
        "{}{}{}data: [DONE]\n\n",
        sse_content_chunk("{\"title\":\"Obj"),
        sse_content_chunk("ect storage\",\""),
        sse_content_chunk("words\":12}"),
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
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
        .default_model(ModelRef::new("openai/gpt-4o-mini"))
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

    let mut partials = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let FlowStreamEvent::ObjectDelta { partial } = event {
            partials.push(partial);
        }
    }
    // The SSE arrives in several chunks, so the client sees the object
    // progressively complete and converge to the validated result.
    assert!(
        partials.len() > 1,
        "expected incremental partials, got {partials:?}"
    );
    assert!(
        partials[0].get("words").is_none(),
        "trailing field should arrive after the title: {:?}",
        partials[0]
    );
    let converged: Summary = serde_json::from_value(partials.last().unwrap().clone()).unwrap();
    assert_eq!(converged, result);
}

#[tokio::test]
async fn real_schema_uses_strict_json_schema_mode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let format = &body["response_format"];
            assert_eq!(format["type"], "json_schema");
            assert_eq!(format["json_schema"]["strict"], true);
            // The strict transform locked the schema down.
            assert_eq!(
                format["json_schema"]["schema"]["additionalProperties"],
                false
            );
            assert_eq!(
                format["json_schema"]["schema"]["required"],
                json!(["title"])
            );
            ResponseTemplate::new(200).set_body_json(
                json!({"choices": [{"message": {"content": "{\"title\":\"Uploads\"}"}}]}),
            )
        })
        .mount(&server)
        .await;

    let request = GenerateRequest {
        json_schema: Some(json!({"type": "object", "properties": {"title": {"type": "string"}}})),
        ..text_request("summarize")
    };
    let response = provider(&server).generate(request).await.unwrap();
    assert_eq!(response.text_output(), "{\"title\":\"Uploads\"}");
}
