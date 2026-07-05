//! First-party Qwen / Alibaba Cloud Model Studio provider for
//! [Pocopine Agenkit](../../rfcs/rfc-093-pocopine-agenkit.md).
//!
//! Qwen's DashScope compatible-mode API follows OpenAI's
//! `/chat/completions` wire shape, so this crate delegates protocol handling to
//! `pocopine-agenkit-oai` while providing Qwen defaults, credentials, and typed
//! model handles.
//!
//! ```no_run
//! use pocopine_agenkit::server::Agenkit;
//! use pocopine_agenkit_qwen::{QwenProvider, models};
//!
//! let agenkit = Agenkit::builder()
//!     // Credentials are read from DASHSCOPE_API_KEY — server-only (§D10).
//!     .provider(QwenProvider::from_env("qwen").expect("DASHSCOPE_API_KEY"))
//!     .default_model(models::QWEN_PLUS)
//!     .build()
//!     .unwrap();
//! # let _ = agenkit;
//! ```
//!
//! Supports the OpenAI-compatible Agenkit contract: text, native SSE streaming,
//! function tools, `json_object` structured output, usage, retries, BYOK via
//! `ProviderContext`, and redacted provider credentials.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::time::Duration;

use pocopine_agenkit::server::{
    BoxFuture, BoxStream, GenerateRequest, GenerateResponse, Provider, ProviderCapabilities,
    ProviderContext, StreamChunk,
};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use pocopine_agenkit_oai::{MaxTokensParam, OpenAiProvider, ThinkingParam};

/// The default DashScope OpenAI-compatible API base URL.
///
/// Override with [`QwenProvider::with_base_url`] or `DASHSCOPE_BASE_URL` when
/// using a workspace-specific or regional endpoint.
pub const DEFAULT_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

/// The US-region DashScope OpenAI-compatible API base URL.
pub const US_BASE_URL: &str = "https://dashscope-us.aliyuncs.com/compatible-mode/v1";

/// Generated typed handles for known Qwen model aliases.
///
/// These are re-exported from `pocopine_agenkit::server::models::qwen`, whose
/// names are produced by `gen-model-catalog` from LiteLLM's model data. The
/// provider strips the `qwen/` namespace before sending the request, so
/// `models::QWEN_PLUS` is sent to DashScope as `qwen-plus`.
pub mod models {
    pub use pocopine_agenkit::server::models::qwen::*;
}

/// A Qwen provider backed by DashScope compatible-mode Chat Completions.
#[derive(Clone)]
pub struct QwenProvider {
    inner: OpenAiProvider,
}

impl std::fmt::Debug for QwenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QwenProvider")
            .field("inner", &self.inner)
            .finish()
    }
}

impl QwenProvider {
    /// A provider serving `alias`, authenticating with `api_key`, against the
    /// default DashScope OpenAI-compatible endpoint.
    pub fn new(alias: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            inner: OpenAiProvider::new(alias, api_key)
                .with_base_url(DEFAULT_BASE_URL)
                // DashScope compatible-mode uses the broad OpenAI-compatible
                // `max_tokens` field, not OpenAI's newer `max_completion_tokens`.
                .with_max_tokens_param(MaxTokensParam::MaxTokens)
                // DashScope's reasoning knob is the `enable_thinking` boolean
                // (Qwen3 family), not OpenAI's `reasoning_effort` string.
                .with_thinking_param(ThinkingParam::EnableThinking)
                // Prefer the widely-supported json_object path for Qwen; callers
                // can opt into strict json_schema if their model/endpoint supports it.
                .with_strict_schema(false),
        }
    }

    /// Build from the environment.
    ///
    /// Reads `DASHSCOPE_API_KEY` (preferred) or `QWEN_API_KEY`, and optionally
    /// `DASHSCOPE_BASE_URL` or `QWEN_BASE_URL`.
    pub fn from_env(alias: impl Into<String>) -> AgenkitResult<Self> {
        let api_key = std::env::var("DASHSCOPE_API_KEY")
            .or_else(|_| std::env::var("QWEN_API_KEY"))
            .map_err(|_| AgenkitError::config("DASHSCOPE_API_KEY or QWEN_API_KEY is not set"))?;
        let mut provider = Self::new(alias, api_key);
        if let Ok(base_url) =
            std::env::var("DASHSCOPE_BASE_URL").or_else(|_| std::env::var("QWEN_BASE_URL"))
        {
            provider = provider.with_base_url(base_url);
        }
        Ok(provider)
    }

    /// Point at a workspace-specific, regional, or test compatible-mode endpoint.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner = self.inner.with_base_url(base_url);
        self
    }

    /// Toggle native strict `json_schema` structured output.
    ///
    /// Qwen defaults this to `false`, using `json_object` mode plus runtime
    /// validation. Enable only for endpoints/models that support strict schemas.
    pub fn with_strict_schema(mut self, strict: bool) -> Self {
        self.inner = self.inner.with_strict_schema(strict);
        self
    }

    /// Force which output-token field is sent.
    pub fn with_max_tokens_param(mut self, param: MaxTokensParam) -> Self {
        self.inner = self.inner.with_max_tokens_param(param);
        self
    }

    /// Which reasoning-request field a thinking level maps to (defaults to
    /// [`ThinkingParam::EnableThinking`] — DashScope's dialect).
    pub fn with_thinking_param(mut self, param: ThinkingParam) -> Self {
        self.inner = self.inner.with_thinking_param(param);
        self
    }

    /// Total timeout for a non-streaming request (streaming is never capped).
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.with_request_timeout(timeout);
        self
    }

    /// Retries on transient failures (429 / 5xx / network errors). `0` disables.
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.inner = self.inner.with_max_retries(retries);
        self
    }

    /// Use a pre-configured `reqwest::Client` (timeouts, proxy, ...).
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.inner = self.inner.with_http_client(http);
        self
    }

    /// Borrow the underlying OpenAI-compatible provider.
    pub fn inner(&self) -> &OpenAiProvider {
        &self.inner
    }

    /// Consume this wrapper and return the underlying OpenAI-compatible provider.
    pub fn into_inner(self) -> OpenAiProvider {
        self.inner
    }
}

impl Provider for QwenProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
        cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        self.inner.generate(request, cx)
    }

    fn generate_stream<'a>(
        &'a self,
        request: GenerateRequest,
        cx: &'a ProviderContext,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        self.inner.generate_stream(request, cx)
    }
}
