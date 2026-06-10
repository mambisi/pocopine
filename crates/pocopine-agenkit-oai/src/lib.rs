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

use futures::StreamExt;
use pocopine_agenkit::server::{
    BoxFuture, BoxStream, GenerateRequest, GenerateResponse, Provider, ProviderCapabilities,
    StreamChunk,
};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolCall};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;

use wire::{ChatRequest, ChatResponse, StreamEvent};

/// The default OpenAI API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// A provider backed by an OpenAI-compatible `/v1/chat/completions` endpoint.
#[derive(Clone)]
pub struct OpenAiProvider {
    alias: String,
    api_key: String,
    base_url: String,
    organization: Option<String>,
    /// Use native strict `json_schema` structured output (vs `json_object`).
    strict_schema: bool,
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
            // A connect timeout only: an unreachable endpoint fails fast, but
            // a long-lived SSE stream is never cut by a total-request timeout.
            // Use `with_http_client` for finer control.
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
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

    /// Use a pre-configured `reqwest::Client` (timeouts, proxy, ...).
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn post(&self, wire: &ChatRequest) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(wire);
        if let Some(organization) = &self.organization {
            builder = builder.header("OpenAI-Organization", organization);
        }
        builder
    }

    async fn chat(&self, request: GenerateRequest) -> AgenkitResult<GenerateResponse> {
        let wire = ChatRequest::from_agenkit(&request, false, self.strict_schema);
        let response = self
            .post(&wire)
            .send()
            .await
            .map_err(|err| AgenkitError::provider(format!("request failed: {err}")))?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            AgenkitError::provider(format!("reading response body failed: {err}"))
        })?;
        if !status.is_success() {
            return Err(AgenkitError::provider(openai_error_message(
                status.as_u16(),
                &body,
            )));
        }
        serde_json::from_str::<ChatResponse>(&body)
            .map_err(|err| AgenkitError::provider(format!("invalid response shape: {err}")))?
            .into_agenkit()
    }

    /// Read the SSE stream into `tx`, forwarding text deltas and accumulating
    /// tool-call fragments by index.
    async fn stream_into(
        &self,
        request: GenerateRequest,
        tx: UnboundedSender<AgenkitResult<StreamChunk>>,
    ) -> AgenkitResult<()> {
        let wire = ChatRequest::from_agenkit(&request, true, self.strict_schema);
        let response = self
            .post(&wire)
            .send()
            .await
            .map_err(|err| AgenkitError::provider(format!("request failed: {err}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AgenkitError::provider(openai_error_message(
                status.as_u16(),
                &body,
            )));
        }

        // Buffer raw bytes and only decode whole `\n`-terminated lines. `\n`
        // (0x0A) never appears inside a multi-byte UTF-8 sequence, so a line cut
        // at a newline is always valid UTF-8 — decoding per network chunk with
        // `from_utf8_lossy` would instead corrupt any codepoint split across two
        // chunks into replacement characters.
        let mut bytes = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut tools: BTreeMap<u32, ToolAccumulator> = BTreeMap::new();

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk
                .map_err(|err| AgenkitError::provider(format!("stream read failed: {err}")))?;
            buffer.extend_from_slice(&chunk);

            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                let done = apply_sse_line(&buffer[..newline], &mut tools, &tx);
                buffer.drain(..=newline);
                if done {
                    emit_tool_calls(tools, &tx);
                    return Ok(());
                }
            }
        }

        // Flush a final line that arrived without a trailing newline (a gateway
        // that closes the body right after the last `data:` and omits `[DONE]`).
        if !buffer.is_empty() {
            apply_sse_line(&buffer, &mut tools, &tx);
        }
        emit_tool_calls(tools, &tx);
        Ok(())
    }
}

/// Parse one SSE line and apply it to the accumulators. Returns `true` when the
/// terminal `data: [DONE]` sentinel was seen. Keep-alive/comment/undecodable
/// lines are ignored.
fn apply_sse_line(
    line: &[u8],
    tools: &mut BTreeMap<u32, ToolAccumulator>,
    tx: &UnboundedSender<AgenkitResult<StreamChunk>>,
) -> bool {
    let Ok(text) = std::str::from_utf8(line) else {
        return false;
    };
    let Some(data) = text.trim().strip_prefix("data:") else {
        return false;
    };
    let data = data.trim();
    if data.is_empty() {
        return false;
    }
    if data == "[DONE]" {
        return true;
    }
    // Ignore unparseable keep-alive / comment lines.
    let Ok(event) = serde_json::from_str::<StreamEvent>(data) else {
        return false;
    };
    for choice in event.choices {
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
    false
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
        let _ = tx.send(Ok(StreamChunk::ToolCall(ToolCall::new(
            acc.id, acc.name, args,
        ))));
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
        // Two SSE `data:` events whose JSON content carries multi-byte chars
        // ("café", "naïve 🚀"), fed as a byte stream that is re-sliced at
        // arbitrary boundaries — including in the middle of a multi-byte char.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"café \"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"naïve 🚀\"}}]}\n",
            "data: [DONE]\n",
        )
        .as_bytes();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        let mut buffer: Vec<u8> = Vec::new();
        // Drive the exact buffering loop from `stream_into`, chunking every 3
        // bytes so multi-byte sequences straddle chunk boundaries.
        let mut done = false;
        for chunk in sse.chunks(3) {
            buffer.extend_from_slice(chunk);
            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                if apply_sse_line(&buffer[..newline], &mut tools, &tx) {
                    done = true;
                }
                buffer.drain(..=newline);
            }
        }
        assert!(done, "should have seen [DONE]");
        assert_eq!(drain_text(&mut rx), "café naïve 🚀");
    }

    #[test]
    fn trailing_line_without_newline_is_flushed() {
        // A final `data:` event with no terminating newline and no `[DONE]`.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = BTreeMap::new();
        let line = br#"data: {"choices":[{"delta":{"content":"last"}}]}"#;
        apply_sse_line(line, &mut tools, &tx);
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
        emit_tool_calls(tools, &tx);
        let Ok(StreamChunk::ToolCall(call)) = rx.try_recv().unwrap() else {
            panic!("expected a tool call");
        };
        assert_eq!(call.args, serde_json::json!({}));
    }
}
