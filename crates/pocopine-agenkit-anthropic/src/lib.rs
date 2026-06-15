//! Native Anthropic **Claude (Messages API)** provider for [Pocopine
//! Agenkit](../../rfcs/rfc-093-pocopine-agenkit.md).
//!
//! Implements the Agenkit `Provider` trait against `/v1/messages`, parallel to
//! `pocopine-agenkit-oai`. This is the recommended provider when building AI
//! apps on Pocopine (default to the latest Claude models).
//!
//! ```no_run
//! use pocopine_agenkit::server::Agenkit;
//! use pocopine_agenkit_anthropic::AnthropicProvider;
//! use pocopine_agenkit_core::ModelRef;
//!
//! let agenkit = Agenkit::builder()
//!     // Credentials are read from ANTHROPIC_API_KEY — server-only (§D10).
//!     .provider(AnthropicProvider::from_env("anthropic").expect("ANTHROPIC_API_KEY"))
//!     .default_model(ModelRef::new("anthropic/claude-opus-4-8"))
//!     .build()
//!     .unwrap();
//! # let _ = agenkit;
//! ```
//!
//! Supports text + native **SSE streaming**, function **tools** (mapped to
//! Anthropic content blocks), and schema-constrained **structured output** via
//! a forced single tool (Anthropic has no `response_format`). The API key lives
//! only in the `x-api-key` header, is redacted from `Debug`, and never appears
//! in errors (§D10).
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
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolCall, ToolNameMap, Usage};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;

use wire::{
    MessagesRequest, MessagesResponse, STRUCTURED_TOOL_NAME, StreamBlockStart, StreamDelta,
    StreamEvent,
};

/// The default Anthropic API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// The Anthropic API version sent in the `anthropic-version` header.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default `max_tokens` when the neutral request omits one (Anthropic requires
/// the field). Sized for a typical completion; raise via `with_default_max_tokens`.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Default total timeout for a non-streaming request (streaming is uncapped).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Default retries on transient failures (429 / 5xx / network).
const DEFAULT_MAX_RETRIES: u32 = 2;

/// A provider backed by the Anthropic Messages API (`/v1/messages`).
#[derive(Clone)]
pub struct AnthropicProvider {
    alias: String,
    api_key: String,
    base_url: String,
    version: String,
    default_max_tokens: u32,
    request_timeout: Duration,
    max_retries: u32,
    http: reqwest::Client,
}

// Hand-rolled so the API key never lands in logs or panic messages (§D10).
impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("alias", &self.alias)
            .field("base_url", &self.base_url)
            .field("version", &self.version)
            .field("default_max_tokens", &self.default_max_tokens)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl AnthropicProvider {
    /// A provider serving `alias`, authenticating with `api_key`, against the
    /// default Anthropic base URL.
    pub fn new(alias: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            default_max_tokens: DEFAULT_MAX_TOKENS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            // Connect timeout only on the shared client; the total-request
            // timeout is applied per-call (non-streaming) so a long-lived SSE
            // stream is never cut. Use `with_http_client` for finer control.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Build from the environment: `ANTHROPIC_API_KEY` (required) and optionally
    /// `ANTHROPIC_BASE_URL`. Credentials never enter app code or client bundles.
    pub fn from_env(alias: impl Into<String>) -> AgenkitResult<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| AgenkitError::config("ANTHROPIC_API_KEY is not set"))?;
        let mut provider = Self::new(alias, api_key);
        if let Ok(base_url) = std::env::var("ANTHROPIC_BASE_URL") {
            provider.base_url = base_url;
        }
        Ok(provider)
    }

    /// Point at a compatible endpoint (e.g. a proxy or Bedrock-compatible URL).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the `anthropic-version` header.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Set the `max_tokens` used when a request omits one.
    pub fn with_default_max_tokens(mut self, max_tokens: u32) -> Self {
        self.default_max_tokens = max_tokens.max(1);
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
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }

    fn post(&self, wire: &MessagesRequest, timeout: Option<Duration>) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(wire);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        builder
    }

    /// POST with retry on transient failures (429 / 5xx / network errors).
    /// `timeout` caps each attempt (non-streaming only). On a terminal non-2xx
    /// the error carries only the status/type, never the body message (§D10).
    async fn send_with_retry(
        &self,
        wire: &MessagesRequest,
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
                    return Err(AgenkitError::provider(anthropic_error_message(
                        status, &body,
                    )));
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

    async fn message(&self, request: GenerateRequest) -> AgenkitResult<GenerateResponse> {
        let names = ToolNameMap::from_descriptors(&request.tools);
        let wire = MessagesRequest::from_agenkit(&request, false, self.default_max_tokens);
        let response = self
            .send_with_retry(&wire, Some(self.request_timeout))
            .await?;
        let body = response.text().await.map_err(|err| {
            AgenkitError::provider(format!("reading response body failed: {err}"))
        })?;
        let parsed = serde_json::from_str::<MessagesResponse>(&body)
            .map_err(|err| AgenkitError::provider(format!("invalid response shape: {err}")))?;
        Ok(parsed.into_agenkit(&names))
    }

    /// Read the SSE stream into `tx`, forwarding text deltas and accumulating
    /// tool-call fragments by content-block index.
    async fn stream_into(
        &self,
        request: GenerateRequest,
        tx: UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> AgenkitResult<()> {
        let names = ToolNameMap::from_descriptors(&request.tools);
        let wire = MessagesRequest::from_agenkit(&request, true, self.default_max_tokens);
        // No total timeout on a stream; retry only the connect/status handshake.
        let response = self.send_with_retry(&wire, None).await?;

        // Buffer raw bytes and decode whole `\n`-terminated lines. `\n` (0x0A)
        // never appears mid multi-byte UTF-8 sequence, so a line cut at a
        // newline is always valid UTF-8 — unlike per-chunk `from_utf8_lossy`.
        let mut bytes = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut decoder = SseDecoder::new(&names);

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk
                .map_err(|err| AgenkitError::provider(format!("stream read failed: {err}")))?;
            buffer.extend_from_slice(&chunk);
            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                let done = decoder.feed_line(&buffer[..newline], &tx);
                buffer.drain(..=newline);
                if done {
                    return Ok(());
                }
            }
        }
        // Flush a final event buffered with no terminating blank line. If either
        // the trailing line or the buffered event is `message_stop`, the stream
        // closed cleanly and `finish` already ran.
        if !buffer.is_empty() && decoder.feed_line(&buffer, &tx) {
            return Ok(());
        }
        if decoder.flush(&tx) {
            return Ok(());
        }
        // EOF before `message_stop`: a well-behaved Anthropic stream always ends
        // in `message_stop`, so reaching here means the body was truncated. Drain
        // any pending tool calls + usage rather than losing them silently.
        decoder.finish(&tx);
        Ok(())
    }
}

impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.alias
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Anthropic streams natively, calls tools, and owns its structured-output
        // strategy in `MessagesRequest::from_agenkit`: with no user tools it
        // forces a schema-typed `structured_output` tool; with user tools present
        // (forcing a tool would suppress them) it injects a schema instruction
        // into the system prompt. Either way the provider handles the schema, so
        // it reports `json_schema: true` to suppress the runtime's prompt
        // fallback — otherwise the schema would be sent twice (§D13).
        ProviderCapabilities {
            streaming: true,
            json_schema: true,
            tools: true,
        }
    }

    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        Box::pin(self.message(request))
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

/// An accumulating tool-use content block.
struct ToolAccumulator {
    id: String,
    name: String,
    json: String,
    /// True for the synthetic structured-output tool, whose JSON input is
    /// forwarded as text deltas (so the runtime parses partial objects) rather
    /// than surfaced as a tool call.
    structured: bool,
}

/// Reassembles Anthropic SSE events from individual lines (blank-line-delimited
/// per spec) and applies them to the chunk stream.
struct SseDecoder<'a> {
    data: Vec<String>,
    tools: BTreeMap<u32, ToolAccumulator>,
    input_tokens: u64,
    output_tokens: u64,
    name_map: &'a ToolNameMap,
}

impl<'a> SseDecoder<'a> {
    fn new(name_map: &'a ToolNameMap) -> Self {
        Self {
            data: Vec::new(),
            tools: BTreeMap::new(),
            input_tokens: 0,
            output_tokens: 0,
            name_map,
        }
    }

    /// Feed one line (without trailing `\n`). Returns `true` once the stream is
    /// complete (`message_stop`).
    fn feed_line(&mut self, line: &[u8], tx: &UnboundedSender<AgenkitResult<StreamChunk>>) -> bool {
        let Ok(text) = std::str::from_utf8(line) else {
            return false;
        };
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            return self.dispatch(tx);
        }
        if text.starts_with(':') {
            return false; // comment / keep-alive
        }
        // Anthropic sends `event:` and `data:` lines; the data payload is
        // self-describing via its `type`, so only `data:` matters here.
        if let Some(value) = text.strip_prefix("data:") {
            self.data
                .push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
        false
    }

    /// Dispatch the buffered event. Returns `true` on `message_stop`.
    fn dispatch(&mut self, tx: &UnboundedSender<AgenkitResult<StreamChunk>>) -> bool {
        if self.data.is_empty() {
            return false;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        let Ok(event) = serde_json::from_str::<StreamEvent>(data.trim()) else {
            return false;
        };
        self.apply(event, tx)
    }

    /// Flush a final event with no terminating blank line.
    fn flush(&mut self, tx: &UnboundedSender<AgenkitResult<StreamChunk>>) -> bool {
        self.dispatch(tx)
    }

    fn apply(
        &mut self,
        event: StreamEvent,
        tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> bool {
        match event {
            StreamEvent::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    self.input_tokens = usage.input_tokens;
                }
            }
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let StreamBlockStart { kind, id, name } = content_block;
                if kind == "tool_use" {
                    let name = name.unwrap_or_default();
                    let structured = name == STRUCTURED_TOOL_NAME;
                    self.tools.insert(
                        index,
                        ToolAccumulator {
                            id: id.unwrap_or_default(),
                            name,
                            json: String::new(),
                            structured,
                        },
                    );
                }
            }
            StreamEvent::ContentBlockDelta { index, delta } => match delta {
                StreamDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        let _ = tx.send(Ok(StreamChunk::Text(text)));
                    }
                }
                StreamDelta::InputJsonDelta { partial_json } => {
                    if let Some(acc) = self.tools.get_mut(&index) {
                        if acc.structured {
                            // Forward as text so the runtime parses partial
                            // structured objects and assembles the final value.
                            if !partial_json.is_empty() {
                                let _ = tx.send(Ok(StreamChunk::Text(partial_json)));
                            }
                        } else {
                            acc.json.push_str(&partial_json);
                        }
                    }
                }
                StreamDelta::Other => {}
            },
            StreamEvent::ContentBlockStop { index } => {
                self.emit_tool(index, tx);
            }
            StreamEvent::MessageDelta { usage } => {
                if let Some(usage) = usage {
                    // `message_delta` usage is cumulative, so the latest value
                    // wins. It may also restate `input_tokens` (e.g. when a
                    // server-side tool inflated the prompt); take it when given.
                    self.output_tokens = usage.output_tokens;
                    if usage.input_tokens > 0 {
                        self.input_tokens = usage.input_tokens;
                    }
                }
            }
            StreamEvent::MessageStop => {
                self.finish(tx);
                return true;
            }
            StreamEvent::Other => {}
        }
        false
    }

    /// Close out the stream: drain any tool block that never received a stop,
    /// then emit the final usage. Called on `message_stop` and — crucially — on
    /// a truncated stream (EOF before `message_stop`, e.g. a proxy that closed
    /// the body mid-response), so accumulated tool calls and usage are never
    /// silently dropped. Runs at most once per stream (the `message_stop` path
    /// returns early before the EOF path can re-run it).
    fn finish(&mut self, tx: &UnboundedSender<AgenkitResult<StreamChunk>>) {
        let indices: Vec<u32> = self.tools.keys().copied().collect();
        for index in indices {
            self.emit_tool(index, tx);
        }
        if self.input_tokens > 0 || self.output_tokens > 0 {
            let _ = tx.send(Ok(StreamChunk::Usage(Usage::new(
                self.input_tokens,
                self.output_tokens,
            ))));
        }
    }

    /// Emit a finished (non-structured) tool call, resolving its name back to
    /// the original tool id. Structured-tool blocks were already streamed as text.
    fn emit_tool(&mut self, index: u32, tx: &UnboundedSender<AgenkitResult<StreamChunk>>) {
        let Some(acc) = self.tools.remove(&index) else {
            return;
        };
        if acc.structured {
            return;
        }
        let args = if acc.json.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&acc.json).unwrap_or_else(|_| serde_json::json!({}))
        };
        let name = self.name_map.resolve(&acc.name);
        let _ = tx.send(Ok(StreamChunk::ToolCall(ToolCall::new(acc.id, name, args))));
    }
}

/// Whether a non-2xx status is worth retrying: rate limits and server errors.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || status >= 500
}

/// Backoff before the next retry: honor `retry-after` (seconds, capped) when
/// present, else exponential from 250ms.
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

/// Build a client-safe error string from an Anthropic error body. Surfaces only
/// the stable status/type — never the free-form message, which can echo request
/// content or hints (§D10).
fn anthropic_error_message(status: u16, body: &str) -> String {
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
            "Anthropic API error {status} (type={})",
            parsed.error.kind.as_deref().unwrap_or("unknown")
        ),
        Err(_) => format!("Anthropic API error {status}"),
    }
}
