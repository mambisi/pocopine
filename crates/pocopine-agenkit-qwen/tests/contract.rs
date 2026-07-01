#![cfg(not(target_arch = "wasm32"))]
//! Contract tests for the Qwen first-party wrapper. The protocol decode/encode
//! behavior is covered by `pocopine-agenkit-oai`; these tests assert the Qwen
//! defaults, endpoint path, auth, and namespace stripping.

use futures::StreamExt;
use pocopine_agenkit::server::{GenerateRequest, Provider, ProviderContext, StreamChunk};
use pocopine_agenkit_core::Message;
use pocopine_agenkit_qwen::{DEFAULT_BASE_URL, QwenProvider, models};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn provider(server: &MockServer) -> QwenProvider {
    QwenProvider::new("qwen", "test-key")
        .with_base_url(format!("{}/compatible-mode/v1", server.uri()))
}

fn text_request(prompt: &str) -> GenerateRequest {
    GenerateRequest {
        model: models::QWEN_PLUS,
        messages: vec![Message::user(prompt)],
        max_tokens: Some(32),
        ..GenerateRequest::default()
    }
}

#[tokio::test]
async fn maps_text_completion_and_sends_dashscope_bearer_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/compatible-mode/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["model"], "qwen-plus");
            assert_eq!(body["max_tokens"], 32);
            assert!(
                body.get("max_completion_tokens").is_none(),
                "Qwen compatible-mode should receive max_tokens by default"
            );
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "qwen is ready"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 4, "completion_tokens": 3, "total_tokens": 7}
            }))
        })
        .mount(&server)
        .await;

    let response = provider(&server)
        .generate(text_request("ping"), &ProviderContext::default())
        .await
        .unwrap();
    assert_eq!(response.text_output(), "qwen is ready");
    assert_eq!(response.usage.unwrap().total(), 7);
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_openai_compatible_chat_chunks() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ni\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" hao\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/compatible-mode/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let client = provider(&server);
    let cx = ProviderContext::default();
    let mut stream = client.generate_stream(text_request("hello"), &cx);
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        if let StreamChunk::Text(fragment) = chunk.unwrap() {
            text.push_str(&fragment);
        }
    }
    assert_eq!(text, "ni hao");
}

#[test]
fn exposes_qwen_defaults_and_redacts_debug() {
    assert_eq!(
        DEFAULT_BASE_URL,
        "https://dashscope.aliyuncs.com/compatible-mode/v1"
    );
    assert_eq!(models::QWEN_PLUS.as_str(), "qwen/qwen-plus");
    assert_eq!(models::QWEN_PLUS.model(), "qwen-plus");

    let rendered = format!("{:?}", QwenProvider::new("qwen", "sk-SECRET"));
    assert!(rendered.contains("QwenProvider"));
    assert!(!rendered.contains("sk-SECRET"));
}
