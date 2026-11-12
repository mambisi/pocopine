#![cfg(not(target_arch = "wasm32"))]
//! LIVE check of the Anthropic provider against the real Messages API: the
//! thinking shape each model generation accepts, structured output, prompt
//! caching, and an agent turn that is steered and compacted. Ignored by default:
//! needs `ANTHROPIC_API_KEY` and spends a few cents on the Haiku models. Run:
//!
//!   cargo test -p pocopine-agenkit-anthropic --test anthropic_live -- \
//!     --ignored --nocapture --test-threads=1
//!
//! Requests go through a local relay that records each body and response and
//! forwards them. For models that bind reasoning blocks to the conversation,
//! the relay asks the API to refuse a changed history
//! (`prefix_mismatch_behavior: "error"`), so an edit the history may not have
//! fails here instead of passing on an account that is not yet enforced.
//!
//! Verified 2026-11-12 on Haiku 4.5, Haiku 5.5, Sonnet 4.6, Opus 4.6, Opus
//! 4.8, Sonnet 5, Sonnet 5.5, Opus 5, Opus 5.5 and Fable 5.2 (about $5 in all):
//! - Each thinking shape is accepted: a budget on Haiku 4.5; adaptive with an
//!   effort elsewhere, summarized from Opus 4.7 on. An adaptive model may skip
//!   reasoning on an easy question, at any effort.
//! - Structured output goes through the forced tool on the 4.x models and the
//!   system prompt on the models that reason by default.
//! - The cache is written once and read by the next request, history included.
//! - In the agent turn a steer arrives after the running tool batch, the turn
//!   that fills the window compacts and goes on, and every request is
//!   accepted with the binding check enforced. Opus 5's classifier refuses
//!   some compacted turns ("reasoning_extraction"), not all; Opus 5.5 does not.
//!
//! The check found a refusal reaching the caller as an empty answer, and,
//! when turns trimmed their tool results in place, reasoning replayed after
//! the edit (refused by Haiku 5.5), trimmed turns refused outright by Opus 5,
//! and models re-reading or guessing what was cut: in-turn trimming gave way
//! to compaction. A summary without the user's own words lost the task on
//! four of ten models; it carries them now.
//!
//! `LIVE_MODELS` (comma-separated aliases) checks other models than the two
//! Haiku ones; `LIVE_BUDGET_USD` (default 2) stops sending once the run has
//! spent that much, priced from each response's usage.
//! `LIVE_CASE="anthropic/claude-haiku-5-5 High"` runs one agent case, and
//! `LIVE_DUMP=<dir>` writes each agent case's request and response bodies
//! there (bodies only: no headers, so no key).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use pocopine_agenkit::server::catalog::{self, Model};
use pocopine_agenkit::server::{
    Agenkit, AgentConfig, AgentEvent, AgentSession, AiTool, AiToolContext, BoxFuture,
    GenerateRequest, Provider, ProviderContext, StopReason, StreamChunk,
};
use pocopine_agenkit_anthropic::{AnthropicProvider, PromptCaching};
use pocopine_agenkit_core::{AgenkitResult, Message, ModelRef, ThinkingLevel, ToolDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const HAIKU_4_5: &str = "anthropic/claude-haiku-4-5";
const HAIKU_5_5: &str = "anthropic/claude-haiku-5-5";
const BETA: &str = "thinking-binding-controls-2026-08-01";

/// The models to check: `LIVE_MODELS`, else the two Haiku models.
fn live_models() -> Vec<String> {
    match std::env::var("LIVE_MODELS") {
        Ok(list) => list
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(String::from)
            .collect(),
        Err(_) => vec![HAIKU_4_5.into(), HAIKU_5_5.into()],
    }
}

/// Whether `model` takes a thinking budget (Claude 4.5 and earlier).
fn takes_budget(model: &str) -> bool {
    ["-4-5", "-4-1", "-4-2025", "-3-7"]
        .iter()
        .any(|p| model.contains(p))
}

/// USD per million tokens: input, output, 5-minute cache write, cache read.
/// An unknown model is priced high, so the budget errs on the safe side.
fn price(model: &str) -> [f64; 4] {
    const PRICES: &[(&str, [f64; 4])] = &[
        ("claude-fable-5-2", [8.0, 40.0, 10.0, 0.20]),
        ("claude-opus-5-5", [4.0, 20.0, 5.0, 0.20]),
        ("claude-opus-5", [5.0, 25.0, 6.25, 0.50]),
        ("claude-opus-4", [5.0, 25.0, 6.25, 0.50]),
        ("claude-sonnet-5-5", [2.0, 10.0, 2.5, 0.10]),
        ("claude-sonnet-5", [2.0, 10.0, 2.5, 0.20]),
        ("claude-sonnet-4", [3.0, 15.0, 3.75, 0.30]),
        ("claude-haiku-5-5", [0.10, 0.50, 0.125, 0.01]),
        ("claude-haiku-4-5", [1.0, 5.0, 1.25, 0.10]),
    ];
    PRICES
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map_or([15.0, 75.0, 18.75, 1.5], |(_, p)| *p)
}

/// What the run has spent so far, in USD.
static SPENT: Mutex<f64> = Mutex::new(0.0);

fn spent() -> f64 {
    *SPENT.lock().unwrap()
}

fn budget() -> f64 {
    std::env::var("LIVE_BUDGET_USD")
        .ok()
        .and_then(|b| b.parse().ok())
        .unwrap_or(2.0)
}

/// The cost of one response, from the usage it reports (streamed or not).
fn cost(model: &str, response: &str) -> f64 {
    let mut usage = serde_json::Map::new();
    let mut merge = |u: &Value| {
        if let Some(u) = u.as_object() {
            for (k, v) in u {
                if v.is_u64() {
                    usage.insert(k.clone(), v.clone());
                }
            }
        }
    };
    if let Ok(body) = serde_json::from_str::<Value>(response) {
        merge(&body["usage"]);
    } else {
        for line in response.lines() {
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            merge(&event["message"]["usage"]);
            merge(&event["usage"]);
        }
    }
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0) as f64;
    let [input, output, write, read] = price(model);
    (n("input_tokens") * input
        + n("output_tokens") * output
        + n("cache_creation_input_tokens") * write
        + n("cache_read_input_tokens") * read)
        / 1_000_000.0
}

#[derive(Clone, Debug)]
struct Exchange {
    request: Value,
    status: u16,
    response: String,
}

impl Exchange {
    /// Whether the API changed the replayed reasoning: a block dropped, or one
    /// that failed the check but was let through.
    fn thinking_changed(&self) -> bool {
        self.response.contains("thinking_dropped")
            || self.response.contains("thinking_mismatch_allowed")
    }
}

#[derive(Clone, Default)]
struct Relay {
    log: Arc<Mutex<Vec<Exchange>>>,
}

/// Models that reason when the request says nothing; the relay asks the API
/// to enforce the binding check on them as on any adaptive request.
fn binds_thinking(model: &str) -> bool {
    [
        "claude-haiku-5",
        "claude-sonnet-5",
        "claude-opus-5",
        "claude-fable",
        "claude-mythos",
    ]
    .iter()
    .any(|p| model.starts_with(p))
}

impl Respond for Relay {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let original: Value = serde_json::from_slice(&req.body).expect("a JSON body");
        let mut body = original.clone();
        let header = |name: &str| {
            req.headers
                .get(name)
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default()
        };
        let (key, version) = (header("x-api-key"), header("anthropic-version"));
        let model = body["model"].as_str().unwrap_or_default().to_owned();
        if spent() >= budget() {
            return ResponseTemplate::new(400).set_body_json(json!({
                "type": "error",
                "error": {"type": "invalid_request_error", "message": "live budget spent"}
            }));
        }
        let enforce = body["thinking"]["type"] == "adaptive"
            || (body.get("thinking").is_none() && binds_thinking(&model));
        if enforce {
            let thinking = body
                .as_object_mut()
                .unwrap()
                .entry("thinking")
                .or_insert_with(|| json!({"type": "adaptive"}));
            thinking["block_binding"] = json!({"prefix_mismatch_behavior": "error"});
        }
        let url = format!("https://api.anthropic.com{}", req.url.path());
        let (status, content_type, text) = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let mut request = reqwest::Client::new()
                    .post(url)
                    .header("x-api-key", key)
                    .header("anthropic-version", version)
                    .header("content-type", "application/json")
                    .timeout(Duration::from_secs(120));
                if enforce {
                    request = request.header("anthropic-beta", BETA);
                }
                let response = request
                    .body(serde_json::to_vec(&body).unwrap())
                    .send()
                    .await
                    .expect("the API answers");
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                (status, content_type, response.text().await.unwrap())
            })
        })
        .join()
        .unwrap();
        *SPENT.lock().unwrap() += cost(&model, &text);
        self.log.lock().unwrap().push(Exchange {
            request: original,
            status,
            response: text.clone(),
        });
        ResponseTemplate::new(status)
            .insert_header("content-type", content_type.as_str())
            .set_body_string(text)
    }
}

struct Live {
    _server: MockServer,
    base: String,
    relay: Relay,
}

impl Live {
    async fn start() -> Live {
        let server = MockServer::start().await;
        let relay = Relay::default();
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(relay.clone())
            .mount(&server)
            .await;
        Live {
            base: format!("{}/v1", server.uri()),
            _server: server,
            relay,
        }
    }

    fn provider(&self, caching: PromptCaching) -> AnthropicProvider {
        let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
        AnthropicProvider::new("anthropic", key)
            .with_base_url(self.base.clone())
            .with_prompt_caching(caching)
            .with_max_retries(0)
    }

    fn take(&self) -> Vec<Exchange> {
        std::mem::take(&mut *self.relay.log.lock().unwrap())
    }
}

/// What a streamed call returned.
#[derive(Debug, Default)]
struct Streamed {
    text: String,
    thinking: Vec<(usize, bool)>,
    usage: Option<pocopine_agenkit_core::Usage>,
    error: Option<String>,
}

async fn stream(p: &AnthropicProvider, req: GenerateRequest) -> Streamed {
    let cx = ProviderContext::default();
    let mut s = p.generate_stream(req, &cx);
    let mut out = Streamed::default();
    while let Some(chunk) = s.next().await {
        match chunk {
            Ok(StreamChunk::Text(t)) => out.text.push_str(&t),
            Ok(StreamChunk::Thinking { text, signature }) => {
                out.thinking.push((text.len(), signature.is_some()))
            }
            Ok(StreamChunk::Usage(u)) => out.usage = Some(u),
            Ok(_) => {}
            Err(e) => out.error = Some(e.to_string()),
        }
    }
    out
}

/// Writes `log` to `LIVE_DUMP`, when set, as `<name>.json`.
fn dump(log: &[Exchange], name: &str) {
    if let Ok(dir) = std::env::var("LIVE_DUMP") {
        let all: Vec<Value> = log
            .iter()
            .map(|e| json!({"request": e.request, "status": e.status, "response": e.response}))
            .collect();
        let file = format!("{dir}/{}.json", name.replace('/', "_"));
        std::fs::write(file, serde_json::to_string_pretty(&all).unwrap()).unwrap();
    }
}

fn shape(request: &Value) -> String {
    format!(
        "max_tokens={} thinking={} output_config={}",
        request["max_tokens"], request["thinking"], request["output_config"]
    )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn thinking_shapes() {
    let live = Live::start().await;
    let p = live.provider(PromptCaching::FiveMinutes);
    let mut cases = Vec::new();
    for model in live_models() {
        let levels: &[ThinkingLevel] = if takes_budget(&model) {
            &[ThinkingLevel::Off, ThinkingLevel::Low]
        } else {
            &[ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::High]
        };
        cases.extend(levels.iter().map(|level| (model.clone(), *level)));
    }
    println!("\n=== THINKING SHAPES ===");
    let mut failed = Vec::new();
    for (model, level) in cases {
        let model = model.as_str();
        let req = GenerateRequest {
            model: ModelRef::new(model),
            // An easy question, and at high effort one worth reasoning about.
            messages: vec![Message::user(if level == ThinkingLevel::High {
                "Compute 7^23 mod 1009. Answer with the number only."
            } else {
                "A bat and a ball cost $1.10; the bat costs $1.00 more than the ball. \
                 What does the ball cost? Answer with the amount only."
            })],
            max_tokens: Some(256),
            thinking: level,
            ..GenerateRequest::default()
        };
        let out = stream(&p, req).await;
        let ex = live.take().pop().expect("one request");
        println!(
            "  {model} {level:?}: http {} | sent {} | thinking blocks (chars, signed) {:?} | text {:?} | {:?}",
            ex.status,
            shape(&ex.request),
            out.thinking,
            out.text.trim(),
            out.usage,
        );
        if let Some(e) = &out.error {
            println!("    error: {e}");
        }
        // A budget always reasons; an adaptive model may skip an easy question
        // below high effort.
        let thinks =
            level != ThinkingLevel::Off && (takes_budget(model) || level == ThinkingLevel::High);
        let ok = ex.status == 200
            && out.error.is_none()
            && !out.text.trim().is_empty()
            && (!thinks || out.thinking.iter().any(|(len, _)| *len > 0));
        if !ok {
            failed.push(format!("{model} {level:?}"));
        }
    }
    println!("  spent so far: ${:.4}", spent());
    assert!(failed.is_empty(), "failed: {failed:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn structured_output() {
    let live = Live::start().await;
    let p = live.provider(PromptCaching::FiveMinutes);
    let schema = json!({
        "type": "object",
        "properties": {
            "city": {"type": "string"},
            "country": {"type": "string"}
        },
        "required": ["city", "country"]
    });
    println!("\n=== STRUCTURED OUTPUT ===");
    let mut failed = Vec::new();
    for model in live_models() {
        let model = model.as_str();
        let req = GenerateRequest {
            model: ModelRef::new(model),
            messages: vec![Message::user("Which city is the Eiffel Tower in?")],
            json_schema: Some(schema.clone()),
            max_tokens: Some(256),
            ..GenerateRequest::default()
        };
        let result = p.generate(req, &ProviderContext::default()).await;
        let ex = live.take().pop().expect("one request");
        let forced = ex.request["tool_choice"].clone();
        match result {
            Ok(r) => {
                let json = r.content.parts.iter().find_map(|part| match part {
                    pocopine_agenkit_core::ContentPart::Json { value } => Some(value.clone()),
                    _ => None,
                });
                let value = json.or_else(|| serde_json::from_str(r.text_output().trim()).ok());
                println!(
                    "  {model}: http {} tool_choice={forced} -> {value:?}",
                    ex.status
                );
                if value.as_ref().and_then(|v| v["city"].as_str()).is_none() {
                    failed.push(model.to_string());
                }
            }
            Err(e) => {
                println!(
                    "  {model}: http {} tool_choice={forced} -> error {e}",
                    ex.status
                );
                failed.push(model.to_string());
            }
        }
    }
    println!("  spent so far: ${:.4}", spent());
    assert!(failed.is_empty(), "failed: {failed:?}");
}

/// About 6,000 tokens: above every model's cache minimum (4,096 on Haiku 4.5).
/// Plain prose: Opus 5's classifier refused a table of item codes as cyber.
fn long_system() -> String {
    let mut s = String::from(
        "You answer questions about the garden diary below in one short sentence.\n\n",
    );
    for i in 0..400 {
        s.push_str(&format!(
            "Day {i}: we planted {} tomato seedlings, watered the beds with {} litres, \
             and counted {} bees near the lavender.\n",
            (i * 7) % 23 + 1,
            (i * 11) % 40 + 5,
            (i * 13) % 60
        ));
    }
    s
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn prompt_cache() {
    let live = Live::start().await;
    println!("\n=== PROMPT CACHE ===");
    let mut failed = Vec::new();
    for model in live_models() {
        let model = model.as_str();
        // Unique per run, so an entry an earlier run wrote is not read.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let system = format!("Session {nonce}. {}", long_system());
        let p = live.provider(PromptCaching::FiveMinutes);
        let request = |messages: Vec<Message>| GenerateRequest {
            model: ModelRef::new(model),
            system: Some(system.clone()),
            messages,
            max_tokens: Some(128),
            ..GenerateRequest::default()
        };
        let q1 = "How many tomato seedlings did we plant on day 12?".to_string();
        let first = stream(&p, request(vec![Message::user(q1.clone())])).await;
        let second = stream(&p, request(vec![Message::user(q1.clone())])).await;
        let mut history = vec![
            Message::user(q1),
            Message::assistant(first.text.clone()),
            Message::user("And on day 13?"),
        ];
        let third = stream(&p, request(history.clone())).await;
        history.push(Message::assistant(third.text.clone()));
        history.push(Message::user("And on day 14?"));
        let fourth = stream(&p, request(history.clone())).await;
        let off = live.provider(PromptCaching::Off);
        let uncached = stream(
            &off,
            request(vec![Message::user("How many bees on day 15?")]),
        )
        .await;
        let log = live.take();
        dump(&log, &format!("{model}-cache"));
        let usage = |s: &Streamed| s.usage.unwrap_or_default();
        let (u1, u2, u3, u4, u5) = (
            usage(&first),
            usage(&second),
            usage(&third),
            usage(&fourth),
            usage(&uncached),
        );
        for (name, s, u) in [
            ("1 write", &first, u1),
            ("2 same", &second, u2),
            ("3 history", &third, u3),
            ("4 longer", &fourth, u4),
            ("5 caching off", &uncached, u5),
        ] {
            println!(
                "  {model} {name:14} in={:5} write={:5} read={:5} out={:4} err={:?}",
                u.input_tokens,
                u.cache_creation_tokens,
                u.cache_read_tokens,
                u.output_tokens,
                s.error
            );
        }
        let markers: Vec<usize> = log
            .iter()
            .map(|e| e.request.to_string().matches("\"cache_control\"").count())
            .collect();
        println!(
            "  {model} cache markers per request: {markers:?}; statuses {:?}",
            log.iter().map(|e| e.status).collect::<Vec<_>>()
        );
        let ok = u1.cache_creation_tokens > 0
            && u2.cache_read_tokens >= u1.cache_creation_tokens
            && u3.cache_read_tokens > 0
            && u3.cache_creation_tokens > 0
            && u4.cache_read_tokens > u3.cache_read_tokens
            && u5.cache_read_tokens == 0
            && u5.cache_creation_tokens == 0
            && markers.last() == Some(&0);
        if !ok {
            failed.push(model.to_string());
        }
    }
    println!("  spent so far: ${:.4}", spent());
    assert!(failed.is_empty(), "failed: {failed:?}");
}

// ── an agent turn: steered while a tool runs, compacted when it fills ────────

#[derive(Deserialize, schemars::JsonSchema)]
struct PartIn {
    /// Which part, 1 to 3.
    part: u32,
}

#[derive(Serialize, schemars::JsonSchema)]
struct PartOut {
    text: String,
}

/// Returns about 1,700 tokens per part, slowly, so a steer arrives while it
/// runs.
struct ReadPart;

impl AiTool for ReadPart {
    const ID: &'static str = "read_part";
    type Input = PartIn;
    type Output = PartOut;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            "read_part",
            "Read one part of the report by number (1 to 3).",
        )
    }

    fn call(&self, input: PartIn, _ctx: AiToolContext) -> BoxFuture<'_, AgenkitResult<PartOut>> {
        Box::pin(async move {
            std::thread::sleep(Duration::from_millis(1500));
            let n = input.part;
            let mut text = String::new();
            for i in 0..120 {
                text.push_str(&format!(
                    "Part {n}, line {i}: sensor {} read {} units at minute {}.\n",
                    (i * 13 + n) % 50,
                    (i * 7 + n * 11) % 97,
                    i * 3
                ));
            }
            text.push_str(&format!(
                "Part {n} ends with the code word {}.\n",
                ["AMBER", "BIRCH", "CEDAR"][(n as usize + 2) % 3]
            ));
            Ok(PartOut { text })
        })
    }
}

async fn steered_and_compacted(model: &str, level: ThinkingLevel) -> bool {
    let live = Live::start().await;
    let agenkit = Agenkit::builder()
        .provider(live.provider(PromptCaching::FiveMinutes))
        .default_model(ModelRef::new(model))
        .tool(ReadPart)
        .build()
        .unwrap();
    let config = AgentConfig::new()
        .model(ModelRef::new(model))
        .system("You read reports with the read_part tool, one part per step, never two at once.")
        .tools(["read_part"])
        .max_tokens(2048)
        .max_steps_per_turn(10)
        .thinking(level);
    let session = AgentSession::builder(&agenkit)
        .config(config)
        .open(None)
        .await
        .unwrap();
    let mut stream = session.prompt(
        // The answer needs only the last part, which the compaction keeps;
        // the steer before it is folded into the summary and must survive.
        "Read part 1, then part 2, then part 3 of the report, one call at a time. \
         Then tell me the code word part 3 ends with.",
    );
    let mut events = Vec::new();
    let mut steered = false;
    while let Some(event) = stream.next().await {
        if !steered && matches!(event, AgentEvent::ToolStarted { .. }) {
            session.steer("Also: end your final answer with the word KITTEN.");
            steered = true;
        }
        events.push(event);
    }
    let log = live.take();
    dump(&log, &format!("{model}-{level:?}"));
    let names: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { .. } => Some("tool".into()),
            AgentEvent::ToolCompleted { .. } => Some("done".into()),
            AgentEvent::Steered { .. } => Some("STEERED".into()),
            AgentEvent::Compacted { folded } => Some(format!("COMPACTED({folded})")),
            AgentEvent::Stopped { reason } => Some(format!("stopped {reason:?}")),
            AgentEvent::Failed { error } => Some(format!("FAILED {error}")),
            _ => None,
        })
        .collect();
    let answer = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::AssistantText { text } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let usage = session.usage().await.unwrap();
    println!("  {model} {level:?}: {}", names.join(" → "));
    println!("    answer: {:?}", answer.trim());
    println!(
        "    usage: in={} write={} read={} out={}",
        usage.input_tokens,
        usage.cache_creation_tokens,
        usage.cache_read_tokens,
        usage.output_tokens
    );
    for (i, ex) in log.iter().enumerate() {
        let messages = ex.request["messages"].as_array().map_or(0, Vec::len);
        let thinking_blocks = ex
            .request
            .to_string()
            .matches("\"type\":\"thinking\"")
            .count();
        println!(
            "    request {i}: http {} messages={messages} replayed thinking={thinking_blocks} changed={} | {}",
            ex.status,
            ex.thinking_changed(),
            shape(&ex.request)
        );
        if ex.status != 200 {
            println!(
                "      {}",
                ex.response.chars().take(600).collect::<String>()
            );
        }
    }
    let order_ok = names
        .iter()
        .position(|n| n == "STEERED")
        .zip(names.iter().position(|n| n == "done"))
        .is_some_and(|(s, d)| s > d);
    log.iter().all(|e| e.status == 200 && !e.thinking_changed())
        && names.iter().any(|n| n.starts_with("COMPACTED"))
        && order_ok
        && names
            .last()
            .is_some_and(|n| n == &format!("stopped {:?}", StopReason::Idle))
        && answer.contains("KITTEN")
        && answer.contains("CEDAR")
}

/// Sizes `model` so that the third part fills the window and the turn compacts.
fn small_window(model: &str, base: Option<&Model>) {
    let mut m = base
        .cloned()
        .unwrap_or_else(|| Model::new(ModelRef::new(model), 0, 128_000));
    m.id = ModelRef::new(model);
    m.context_window = 7_000;
    m.max_output = 4_096;
    catalog::register(m).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + spends tokens"]
async fn agent_turn_steered_and_compacted() {
    println!("\n=== AGENT TURN: STEER + COMPACT ===");
    let mut cases = Vec::new();
    for model in live_models() {
        small_window(&model, catalog::lookup(&ModelRef::new(model.as_str())));
        // Reasoning on, so the kept step's reasoning meets a compaction; and
        // Haiku 5.5 with it off, which still reasons by default.
        if takes_budget(&model) {
            cases.push((model, ThinkingLevel::Low));
        } else {
            if model == HAIKU_5_5 {
                cases.push((model.clone(), ThinkingLevel::Off));
            }
            cases.push((model, ThinkingLevel::High));
        }
    }
    let mut failed = Vec::new();
    let only = std::env::var("LIVE_CASE").ok();
    for (model, level) in cases {
        let model = model.as_str();
        if only
            .as_deref()
            .is_some_and(|o| o != format!("{model} {level:?}"))
        {
            continue;
        }
        if !steered_and_compacted(model, level).await {
            failed.push(format!("{model} {level:?}"));
        }
        println!("  spent so far: ${:.4}", spent());
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}
