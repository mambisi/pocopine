#![cfg(not(target_arch = "wasm32"))]
//! LIVE compatibility smoke test: does our OpenAI-compatible provider work with
//! the top open-weight models, served through OpenRouter (an OpenAI-compatible
//! gateway)? Ignored by default — needs `OPENROUTER_API_KEY` and spends a tiny
//! number of tokens. Run with:
//!   cargo test -p pocopine-agenkit-oai --test openrouter_live -- --ignored --nocapture
//!
//! Verified result (2026-06-15): text, tool-calling, and json_object structured
//! output work on all five; streaming works on the non-reasoning four. Reasoning
//! models (e.g. Kimi K2.6) stream chain-of-thought in a non-standard `reasoning`
//! delta field (with `content: ""`); our provider reads only the standard
//! `content`, so it surfaces no user-visible text until the model finishes
//! reasoning and emits `content` — give them token headroom. This is by design
//! (raw reasoning is not client-safe streamed output, §D8), not a wire gap.

use futures::StreamExt;
use pocopine_agenkit::server::{GenerateRequest, Provider, StreamChunk};
use pocopine_agenkit_core::{Message, ModelRef, SchemaRef, ToolDescriptor};
use serde_json::json;

/// Top-5 open-weight families (2026 leaderboards), by confirmed OpenRouter slug.
const MODELS: &[&str] = &[
    "deepseek/deepseek-v3.2",
    "meta-llama/llama-4-maverick",
    "qwen/qwen3-235b-a22b-2507",
    "mistralai/mistral-large-2512",
    "moonshotai/kimi-k2.6",
];

fn provider() -> pocopine_agenkit_oai::OpenAiProvider {
    let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY must be set");
    pocopine_agenkit_oai::OpenAiProvider::new("openrouter", key)
        .with_base_url("https://openrouter.ai/api/v1")
}

fn base_request(model: &str) -> GenerateRequest {
    GenerateRequest {
        model: ModelRef::new(format!("openrouter/{model}")),
        system: Some("You are terse. Follow instructions exactly.".to_string()),
        max_tokens: Some(64),
        ..GenerateRequest::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn text_generation() {
    let p = provider();
    println!("\n=== TEXT (non-streaming) ===");
    for m in MODELS {
        let mut req = base_request(m);
        req.messages = vec![Message::user("Reply with exactly one word: pong")];
        match p.generate(req).await {
            Ok(r) => println!(
                "  ✓ {m:32}  {:?}  usage={:?}",
                r.text_output().trim(),
                r.usage.map(|u| u.total())
            ),
            Err(e) => println!("  ✗ {m:32}  {e}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn streaming() {
    let p = provider();
    println!("\n=== STREAMING (SSE deltas) ===");
    for m in MODELS {
        let mut req = base_request(m);
        req.messages = vec![Message::user("Count: one two three")];
        let mut stream = p.generate_stream(req);
        let (mut text, mut deltas, mut err) = (String::new(), 0u32, None);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(StreamChunk::Text(t)) => {
                    deltas += 1;
                    text.push_str(&t);
                }
                Ok(_) => {}
                Err(e) => err = Some(e),
            }
        }
        match err {
            None => println!("  ✓ {m:32}  {deltas} deltas  {:?}", text.trim()),
            Some(e) => println!("  ✗ {m:32}  {e}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn tool_calling() {
    let p = provider();
    let tool = ToolDescriptor::new("get_weather", "Get the current weather for a city").with_input(
        SchemaRef::named("WeatherIn").with_json_schema(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        })),
    );
    println!("\n=== TOOL CALLING ===");
    for m in MODELS {
        let mut req = base_request(m);
        req.tools = vec![tool.clone()];
        req.messages = vec![Message::user(
            "What is the weather in Paris? Use the get_weather tool.",
        )];
        match p.generate(req).await {
            Ok(r) if !r.tool_calls.is_empty() => println!(
                "  ✓ {m:32}  calls={:?}",
                r.tool_calls
                    .iter()
                    .map(|c| format!("{}({})", c.tool_id, c.args))
                    .collect::<Vec<_>>()
            ),
            Ok(r) => println!(
                "  ~ {m:32}  no tool call (text: {:?})",
                r.text_output().trim()
            ),
            Err(e) => println!("  ✗ {m:32}  {e}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn structured_output() {
    // json_object mode (strict json_schema is an OpenAI-specific feature most
    // open models lack); our provider degrades to this for compatible gateways.
    let p = provider().with_strict_schema(false);
    println!("\n=== STRUCTURED OUTPUT (json_object) ===");
    for m in MODELS {
        let mut req = base_request(m);
        req.json_schema = Some(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}, "temp_c": {"type": "integer"}},
            "required": ["city", "temp_c"]
        }));
        req.messages = vec![Message::user(
            "Return a JSON object for Paris at 20 degrees C with keys city and temp_c.",
        )];
        match p.generate(req).await {
            Ok(r) => {
                let raw = if r.text_output().is_empty() {
                    r.structured_value()
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                } else {
                    r.text_output()
                };
                let ok = serde_json::from_str::<serde_json::Value>(raw.trim())
                    .map(|v| v.get("city").is_some() && v.get("temp_c").is_some())
                    .unwrap_or(false);
                let mark = if ok { "✓" } else { "~" };
                println!("  {mark} {m:32}  {:?}", raw.trim());
            }
            Err(e) => println!("  ✗ {m:32}  {e}"),
        }
    }
}
