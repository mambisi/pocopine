//! OpenAI-compatible provider for [Pocopine Agenkit](../../rfcs/rfc-093-pocopine-agenkit.md).
//!
//! Implements the Agenkit `Provider` trait against any
//! `/v1/chat/completions` endpoint — OpenAI itself, or compatible gateways
//! (OpenRouter, Together, Groq, Fireworks, Ollama, vLLM, LM Studio, ...) — by
//! pointing [`OpenAiProvider::with_base_url`] at the target.
//!
//! ```no_run
//! use pocopine_agenkit::server::Agenkit;
//! use pocopine_agenkit_oai::OpenAiProvider;
//! use pocopine_agenkit_core::ModelRef;
//!
//! let agenkit = Agenkit::builder()
//!     // Credentials are read from OPENAI_API_KEY — server-only (§D10).
//!     .provider(OpenAiProvider::from_env("openai").expect("OPENAI_API_KEY"))
//!     .default_model(ModelRef::new("openai/gpt-4o-mini"))
//!     .build()
//!     .unwrap();
//! # let _ = agenkit;
//! ```
//!
//! Supports the full Agenkit contract: text + native **SSE streaming**,
//! schema-constrained **structured output** (strict `json_schema`), function
//! tools, and usage. Provider credentials live only in the `Authorization`
//! header and are never echoed in errors (§D10).
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

mod wire;

use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt;
use pocopine_agenkit::server::{
    BoxFuture, BoxStream, GenerateRequest, GenerateResponse, Provider, ProviderCapabilities,
    StreamChunk,
};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolCall, ToolNameMap};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;

use wire::{ChatRequest, ChatResponse, StreamEvent};

/// The default OpenAI API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Default total timeout for a non-streaming request (streaming is never
/// capped — an SSE stream is long-lived by design).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Default number of retries on transient failures (429 / 5xx / network).
const DEFAULT_MAX_RETRIES: u32 = 2;

/// Which output-token-limit field a Chat Completions request sends.
///
/// OpenAI's o-series / gpt-5-class models reject the legacy `max_tokens` and
/// require `max_completion_tokens`; most compatible gateways still want
/// `max_tokens`. The provider picks the right one automatically by base URL
/// (see [`OpenAiProvider::with_max_tokens_param`] to override).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxTokensParam {
    /// Send legacy `max_tokens` (broad gateway compatibility).
    MaxTokens,
    /// Send `max_completion_tokens` (required by newer OpenAI models).
    MaxCompletionTokens,
}

/// A provider backed by an OpenAI-compatible `/v1/chat/completions` endpoint.
#[derive(Clone)]
pub struct OpenAiProvider {
    alias: String,
    api_key: String,
    base_url: String,
    organization: Option<String>,
    /// Use native strict `json_schema` structured output (vs `json_object`).
    strict_schema: bool,
    /// Which output-token field to send; `None` auto-derives from `base_url`.
    max_tokens_param: Option<MaxTokensParam>,
    /// Total timeout for a non-streaming request.
    request_timeout: Duration,
    /// Retries on transient failures (429 / 5xx / network errors).
    max_retries: u32,
    http: reqwest::Client,
}

impl OpenAiProvider {
    /// A provider serving `alias`, authenticating with `api_key`, against the
    /// default OpenAI base URL.
    pub fn new(alias: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            organization: None,
            strict_schema: true,
            max_tokens_param: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            // A connect timeout only on the shared client: an unreachable
            // endpoint fails fast. The total-request timeout is applied
            // per-call (non-streaming only) so a long-lived SSE stream is never
            // cut. Use `with_http_client` for finer control.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Build from the environment: `OPENAI_API_KEY` (required) and optionally
    /// `OPENAI_BASE_URL`. Credentials never enter app code or client bundles.
    pub fn from_env(alias: impl Into<String>) -> AgenkitResult<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| AgenkitError::config("OPENAI_API_KEY is not set"))?;
        let mut provider = Self::new(alias, api_key);
        if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
            provider.base_url = base_url;
        }
        Ok(provider)
    }

    /// Point at a compatible endpoint (e.g. `https://openrouter.ai/api/v1`).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Set the `OpenAI-Organization` header.
    pub fn with_organization(mut self, organization: impl Into<String>) -> Self {
        self.organization = Some(organization.into());
        self
    }

    /// Toggle native strict `json_schema` structured output. Disable for
    /// endpoints/models that don't support it (falls back to `json_object`).
    pub fn with_strict_schema(mut self, strict: bool) -> Self {
        self.strict_schema = strict;
        self
    }

    /// Force which output-token field is sent, overriding base-URL detection.
    pub fn with_max_tokens_param(mut self, param: MaxTokensParam) -> Self {
        self.max_tokens_param = Some(param);
        self
    }

    /// Total timeout for a non-streaming request (streaming is never capped).
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Retries on transient failures (429 / 5xx / network errors). `0` disables.
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Use a pre-configured `reqwest::Client` (timeouts, proxy, ...).
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// The output-token field to send: an explicit override, else
    /// `max_completion_tokens` for the official OpenAI host and the legacy
    /// `max_tokens` for any other (gateway) base URL. (Newer OpenAI models
    /// require `max_completion_tokens`; most gateways still want `max_tokens`.)
    fn max_tokens_param(&self) -> MaxTokensParam {
        self.max_tokens_param.unwrap_or_else(|| {
            if self.base_url.contains("api.openai.com") {
                MaxTokensParam::MaxCompletionTokens
            } else {
                MaxTokensParam::MaxTokens
            }
        })
    }

    fn post(&self, wire: &ChatRequest, timeout: Option<Duration>) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(wire);
        if let Some(organization) = &self.organization {
            builder = builder.header("OpenAI-Organization", organization);
        }
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        builder
    }

    /// POST with retry on transient failures (429 / 5xx / network errors),
    /// returning the successful response. `timeout` caps each attempt (set only
    /// for non-streaming calls). On a terminal non-2xx, the error carries only
    /// the stable status/type, never the body message (§D10).
    async fn send_with_retry(
        &self,
        wire: &ChatRequest,
        timeout: Option<Duration>,
    ) -> AgenkitResult<reqwest::Response> {
        let mut attempt: u32 = 0;
        loop {
            match self.post(wire, timeout).send().await {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status().as_u16();
                    if is_retryable_status(status) && attempt < self.max_retries {
                        let delay = retry_delay(attempt, response.headers());
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    let body = response.text().await.unwrap_or_default();
                    return Err(AgenkitError::provider(openai_error_message(status, &body)));
                }
                Err(err) => {
                    if attempt < self.max_retries {
                        let delay = retry_delay(attempt, &reqwest::header::HeaderMap::new());
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(AgenkitError::provider(format!("request failed: {err}")));
                }
            }
        }
    }

    async fn chat(&self, request: GenerateRequest) -> AgenkitResult<GenerateResponse> {
        let names = ToolNameMap::from_descriptors(&request.tools);
        let wire =
            ChatRequest::from_agenkit(&request, false, self.strict_schema, self.max_tokens_param());
        let response = self
            .send_with_retry(&wire, Some(self.request_timeout))
            .await?;
        let body = response.text().await.map_err(|err| {
            AgenkitError::provider(format!("reading response body failed: {err}"))
        })?;
        serde_json::from_str::<ChatResponse>(&body)
            .map_err(|err| AgenkitError::provider(format!("invalid response shape: {err}")))?
            .into_agenkit(&names)
    }

    /// Read the SSE stream into `tx`, forwarding text deltas and accumulating
    /// tool-call fragments by index.
    async fn stream_into(
        &self,
        request: GenerateRequest,
        tx: UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> AgenkitResult<()> {
        let names = ToolNameMap::from_descriptors(&request.tools);
        let wire =
            ChatRequest::from_agenkit(&request, true, self.strict_schema, self.max_tokens_param());
        // No total timeout on a stream; retry only the connect/status handshake.
        let response = self.send_with_retry(&wire, None).await?;

        // Buffer raw bytes and only decode whole `\n`-terminated lines. `\n`
        // (0x0A) never appears inside a multi-byte UTF-8 sequence, so a line cut
        // at a newline is always valid UTF-8 — decoding per network chunk with
        // `from_utf8_lossy` would instead corrupt any codepoint split across two
        // chunks into replacement characters.
        let mut bytes = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut tools: BTreeMap<u32, ToolAccumulator> = BTreeMap::new();
        let mut decoder = SseDecoder::default();

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk
                .map_err(|err| AgenkitError::provider(format!("stream read failed: {err}")))?;
            buffer.extend_from_slice(&chunk);

            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                let done = decoder.feed_line(&buffer[..newline], &mut tools, &tx);
                buffer.drain(..=newline);
                if done {
                    emit_tool_calls(tools, &tx, &names);
                    return Ok(());
                }
            }
        }

        // A final line that arrived without a trailing newline, then flush any
        // event still buffered with no terminating blank line (a gateway that
        // closes the body right after the last `data:` and omits `[DONE]`).
        if !buffer.is_empty() {
            decoder.feed_line(&buffer, &mut tools, &tx);
        }
        decoder.flush(&mut tools, &tx);
        emit_tool_calls(tools, &tx, &names);
        Ok(())
    }
}

/// Reassembles SSE events from individual lines. Per the spec an event is
/// terminated by a blank line and may carry several `data:` fields that join
/// with `\n` — so a gateway that splits one JSON payload across multiple
/// `data:` lines is parsed correctly instead of being silently dropped.
#[derive(Default)]
struct SseDecoder {
    /// `data:` field values accumulated for the in-progress event.
    data: Vec<String>,
}

impl SseDecoder {
    /// Feed one line (without its trailing `\n`). Returns `true` when the
    /// terminal `[DONE]` sentinel was dispatched.
    fn feed_line(
        &mut self,
        line: &[u8],
        tools: &mut BTreeMap<u32, ToolAccumulator>,
        tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> bool {
        let Ok(text) = std::str::from_utf8(line) else {
            return false;
        };
        // Tolerate CRLF: strip a `\r` left by splitting a `\r\n` stream on `\n`.
        let text = text.strip_suffix('\r').unwrap_or(text);
        // A blank line is the event boundary: dispatch what we've buffered.
        if text.is_empty() {
            return self.dispatch(tools, tx);
        }
        // A comment / keep-alive line (starts with `:`).
        if text.starts_with(':') {
            return false;
        }
        // Accumulate `data:` fields; ignore other SSE fields (event:, id:, ...).
        if let Some(value) = text.strip_prefix("data:") {
            // SSE strips exactly one optional leading space after the colon.
            let value = value.strip_prefix(' ').unwrap_or(value);
            self.data.push(value.to_string());
        }
        false
    }

    /// Dispatch the buffered event: join its `data:` fields with `\n` and apply.
    /// Returns `true` if it was `[DONE]`. Clears the buffer either way.
    fn dispatch(
        &mut self,
        tools: &mut BTreeMap<u32, ToolAccumulator>,
        tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> bool {
        if self.data.is_empty() {
            return false;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        let data = data.trim();
        if data == "[DONE]" {
            return true;
        }
        // Ignore unparseable keep-alive / comment payloads.
        if let Ok(event) = serde_json::from_str::<StreamEvent>(data) {
            apply_event(event, tools, tx);
        }
        false
    }

    /// Flush an event buffered with no terminating blank line. Returns `true`
    /// if it was `[DONE]`.
    fn flush(
        &mut self,
        tools: &mut BTreeMap<u32, ToolAccumulator>,
        tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> bool {
        self.dispatch(tools, tx)
    }
}

/// Apply one decoded stream event: forward text deltas, accumulate tool-call
/// fragments by index, and forward usage.
fn apply_event(
    event: StreamEvent,
    tools: &mut BTreeMap<u32, ToolAccumulator>,
    tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
) {
    // Single completion only: the runtime never requests `n > 1`, and the neutral
    // `StreamChunk` model has no choice dimension. Process only the first choice —
    // iterating all of them would interleave their text into one stream and key
    // every choice's tool calls into the same per-choice `index` accumulator,
    // corrupting both.
    if let Some(choice) = event.choices.into_iter().next() {
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            let _ = tx.send(Ok(StreamChunk::Text(content)));
        }
        for delta in choice.delta.tool_calls {
            let acc = tools.entry(delta.index).or_default();
            if let Some(id) = delta.id {
                acc.id = id;
            }
            if let Some(function) = delta.function {
                if let Some(name) = function.name {
                    acc.name = name;
                }
                if let Some(arguments) = function.arguments {
                    acc.arguments.push_str(&arguments);
                }
            }
        }
    }
    if let Some(usage) = event.usage {
        let _ = tx.send(Ok(StreamChunk::Usage(usage.into_usage())));
    }
}

/// Whether a non-2xx status is worth retrying: rate limits and server errors.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || status >= 500
}

/// Backoff before the next retry: honor `Retry-After` (seconds, capped) when
/// present, otherwise exponential from 250ms (250ms, 500ms, 1s, ...).
fn retry_delay(attempt: u32, headers: &reqwest::header::HeaderMap) -> Duration {
    if let Some(secs) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return Duration::from_secs(secs.min(30));
    }
    Duration::from_millis(250u64.saturating_mul(1u64 << attempt.min(5)))
}

impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.alias
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::all()
    }

    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        Box::pin(self.chat(request))
    }

    fn generate_stream<'a>(
        &'a self,
        request: GenerateRequest,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let provider = self.clone();
        let forward = tx.clone();
        tokio::spawn(async move {
            if let Err(error) = provider.stream_into(request, forward.clone()).await {
                let _ = forward.send(Err(error));
            }
        });
        Box::pin(UnboundedReceiverStream::new(rx))
    }
}

#[derive(Default)]
struct ToolAccumulator {
    id: String,
    name: String,
    arguments: String,
}

fn emit_tool_calls(
    tools: BTreeMap<u32, ToolAccumulator>,
    tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
    names: &ToolNameMap,
) {
    for (_, acc) in tools {
        // Empty (zero-argument tool) or truncated args default to `{}`, not
        // `null` — a struct-typed tool input deserializes from `{}` but not from
        // `null`, so `null` would turn a valid no-arg call into a cryptic
        // "expected object" validation error downstream.
        let args = if acc.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&acc.arguments).unwrap_or_else(|_| serde_json::json!({}))
        };
        // Reverse the outbound name sanitization so dispatch finds the real tool.
        let name = names.resolve(&acc.name);
        let _ = tx.send(Ok(StreamChunk::ToolCall(ToolCall::new(acc.id, name, args))));
    }
}

/// Build a client-safe error string from an OpenAI error body. Deliberately
/// surfaces only the stable status/type — never the free-form message, which
/// can echo a masked key on a 401 (§D10).
fn openai_error_message(status: u16, body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: ErrorDetail,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        #[serde(rename = "type", default)]
        kind: Option<String>,
    }

    match serde_json::from_str::<ErrorBody>(body) {
        Ok(parsed) => format!(
            "OpenAI API error {status} (type={})",
            parsed.error.kind.as_deref().unwrap_or("unknown")
        ),
        Err(_) => format!("OpenAI API error {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_body_does_not_echo_the_message() {
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-proj-LEAK","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        let rendered = openai_error_message(401, body);
        assert!(rendered.contains("type=invalid_request_error"));
        assert!(!rendered.contains("sk-proj-LEAK"));
    }

    fn drain_text(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgenkitResult<StreamChunk>>,
    ) -> String {
        let mut text = String::new();
        while let Ok(Ok(StreamChunk::Text(fragment))) = rx.try_recv() {
            text.push_str(&fragment);
        }
        text
    }

    #[test]
    fn buffered_lines_reassemble_a_codepoint_split_across_chunks() {
        // Two SSE events (blank-line delimited, per spec) whose JSON content
        // carries multi-byte chars ("café", "naïve 🚀"), fed as a byte stream
        // re-sliced at arbitrary boundaries — including mid multi-byte char.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"café \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"naïve 🚀\"}}]}\n\n",
            "data: [DONE]\n\n",
        )
        .as_bytes();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        let mut decoder = SseDecoder::default();
        let mut buffer: Vec<u8> = Vec::new();
        // Drive the exact buffering loop from `stream_into`, chunking every 3
        // bytes so multi-byte sequences straddle chunk boundaries.
        let mut done = false;
        for chunk in sse.chunks(3) {
            buffer.extend_from_slice(chunk);
            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                if decoder.feed_line(&buffer[..newline], &mut tools, &tx) {
                    done = true;
                }
                buffer.drain(..=newline);
            }
        }
        assert!(done, "should have seen [DONE]");
        assert_eq!(drain_text(&mut rx), "café naïve 🚀");
    }

    #[test]
    fn multi_line_data_fields_are_joined_before_parsing() {
        // A spec-conformant gateway split one JSON payload across two `data:`
        // lines; they must join with `\n` and parse as a single event.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        let mut decoder = SseDecoder::default();
        decoder.feed_line(br#"data: {"choices":[{"delta":"#, &mut tools, &tx);
        decoder.feed_line(br#"data: {"content":"joined"}}]}"#, &mut tools, &tx);
        let done = decoder.feed_line(b"", &mut tools, &tx); // blank line dispatches
        assert!(!done);
        assert_eq!(drain_text(&mut rx), "joined");
    }

    #[test]
    fn trailing_event_without_blank_line_is_flushed() {
        // A final `data:` event with no terminating blank line and no `[DONE]`.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        let mut decoder = SseDecoder::default();
        decoder.feed_line(
            br#"data: {"choices":[{"delta":{"content":"last"}}]}"#,
            &mut tools,
            &tx,
        );
        decoder.flush(&mut tools, &tx);
        assert_eq!(drain_text(&mut rx), "last");
    }

    #[test]
    fn empty_tool_arguments_default_to_object_not_null() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        tools.insert(
            0,
            ToolAccumulator {
                id: "call_1".to_string(),
                name: "now".to_string(),
                arguments: String::new(), // a zero-argument tool call
            },
        );
        emit_tool_calls(tools, &tx, &ToolNameMap::default());
        let Ok(StreamChunk::ToolCall(call)) = rx.try_recv().unwrap() else {
            panic!("expected a tool call");
        };
        assert_eq!(call.args, serde_json::json!({}));
    }

    #[test]
    fn streamed_tool_name_is_mapped_back_to_the_original_id() {
        // The model replies with the sanitized name; the reverse map restores
        // the original tool id so dispatch finds the real tool.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        tools.insert(
            0,
            ToolAccumulator {
                id: "call_1".to_string(),
                name: "weather_lookup".to_string(),
                arguments: "{}".to_string(),
            },
        );
        let names = ToolNameMap::from_descriptors(&[pocopine_agenkit_core::ToolDescriptor::new(
            "weather.lookup",
            "w",
        )]);
        emit_tool_calls(tools, &tx, &names);
        let Ok(StreamChunk::ToolCall(call)) = rx.try_recv().unwrap() else {
            panic!("expected a tool call");
        };
        assert_eq!(call.tool_id, "weather.lookup");
    }

    #[test]
    fn retry_delay_honors_retry_after_then_falls_back_to_exponential() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_delay(0, &headers), Duration::from_secs(2));

        let empty = reqwest::header::HeaderMap::new();
        assert_eq!(retry_delay(0, &empty), Duration::from_millis(250));
        assert_eq!(retry_delay(1, &empty), Duration::from_millis(500));
        assert_eq!(retry_delay(2, &empty), Duration::from_millis(1000));
    }

    #[test]
    fn only_rate_limit_and_server_errors_are_retryable() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
    }
}
