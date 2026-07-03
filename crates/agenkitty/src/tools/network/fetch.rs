//! `net.fetch` — fetch one allowlisted URL (GET) and return bounded, paginated
//! text/markdown.
//!
//! The network I/O (policy + SSRF guard → pinned per-hop redirect follow →
//! capped body) lives in the shared [`GuardedHttp`](super::http) engine so it
//! never diverges from `net.download`. This file adds only the text-facing
//! layer: content-type gating, HTML→markdown (`fast_html2md`), and char-offset
//! pagination.

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture, Principal, SecretString};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use reqwest::header::HeaderName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::error::{NetError, NetResult};
use super::http::GuardedHttp;
pub use super::http::HickoryResolver;
use super::policy::NetPolicy;
use super::ssrf::Resolve;
use crate::tools::secrets::SecretRuntime;

pub const NET_FETCH_TOOL_ID: &str = "net.fetch";

/// Default content window returned to the model, in characters.
const DEFAULT_PAGE_CHARS: usize = 5_000;
/// Upper bound a caller may request for one window.
const MAX_PAGE_CHARS: usize = 50_000;

/// The `net.fetch` tool. A thin text-rendering layer over the shared
/// [`GuardedHttp`] engine (which owns the allow/block policy, DNS resolver,
/// request budget, and credential headers). Defaults to deny-everything (no
/// allowlist) — a host opts in domains.
pub struct NetFetchTool {
    http: GuardedHttp,
}

impl NetFetchTool {
    /// Build with the production hickory resolver.
    pub fn new(policy: NetPolicy) -> NetResult<Self> {
        Ok(Self {
            http: GuardedHttp::new(policy)?,
        })
    }

    /// Build with an injected resolver (tests).
    pub fn with_resolver(policy: NetPolicy, resolver: Arc<dyn Resolve>) -> Self {
        Self {
            http: GuardedHttp::with_resolver(policy, resolver),
        }
    }

    /// Attach a credential header (e.g. `AUTHORIZATION`) carried as a
    /// [`SecretString`]. It is sent **only** to the origin host — a cross-host
    /// redirect never receives it — and never appears in output, traces, or logs.
    pub fn with_secret_header(mut self, name: HeaderName, value: SecretString) -> Self {
        self.http.push_secret_header(name, value);
        self
    }

    pub fn with_secret_runtime(mut self, runtime: Arc<SecretRuntime>) -> Self {
        self.http.set_secret_runtime(runtime);
        self
    }

    async fn fetch(
        &self,
        input: NetFetchInput,
        principal: &Principal,
    ) -> NetResult<NetFetchOutput> {
        // net.fetch is a *text* surface: the header validator rejects binary
        // bodies (routed to net.download) BEFORE the body is read, so a binary
        // response returns the deterministic classification error without
        // spending bandwidth. The guard ran on every hop inside `get`.
        let response = self
            .http
            .get(
                &input.url,
                &input.secret_headers,
                principal,
                NET_FETCH_TOOL_ID,
                self.http.policy().max_response_bytes(),
                |_status, content_type| classify_content_type(content_type).map(|_| ()),
            )
            .await?;

        // Re-derive the kind from the (already-validated) content type to pick
        // the renderer.
        let kind = classify_content_type(&response.content_type)?;
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let rendered = match kind {
            // `fast_html2md` exposes its library as `html2md`.
            ContentKind::Html if !input.raw.unwrap_or(false) => html2md::rewrite_html(&text, false),
            _ => text,
        };

        let (content, page_truncated, next_start_index) = paginate(
            &rendered,
            input.start_index.unwrap_or(0),
            clamp_page(input.max_length),
        );
        let truncated = response.truncated || page_truncated;

        // One event per call (RFC-069 `pocopine.log`): host + outcome only —
        // never the full URL (a query can carry secrets), headers, or body.
        tracing::info!(
            target: "pocopine.log",
            tool = NET_FETCH_TOOL_ID,
            host = %response.origin_host,
            status = response.status.as_u16(),
            truncated,
            "net.fetch completed"
        );

        Ok(NetFetchOutput {
            final_url: response.final_url,
            status: response.status.as_u16(),
            content_type: response.content_type,
            content,
            truncated,
            next_start_index,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentKind {
    Text,
    Html,
}

/// Gate the content type: text-like and HTML are fetchable; binary (images, PDF,
/// archives, …) is rejected and routed to `net.download`.
fn classify_content_type(content_type: &str) -> NetResult<ContentKind> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "text/html" | "application/xhtml+xml" => Ok(ContentKind::Html),
        // A missing content type is treated as text (some servers omit it).
        "" | "application/json" => Ok(ContentKind::Text),
        other if other.starts_with("text/") => Ok(ContentKind::Text),
        other => Err(NetError::unsupported_content_type(format!(
            "content type `{other}` is not fetchable as text (use net.download for binary)"
        ))),
    }
}

fn clamp_page(max_length: Option<usize>) -> usize {
    max_length
        .unwrap_or(DEFAULT_PAGE_CHARS)
        .clamp(1, MAX_PAGE_CHARS)
}

/// Return a `max_chars` window of `content` starting at the `start_index`
/// character offset, plus whether more remains and the next cursor. Operating on
/// chars keeps the slice on UTF-8 boundaries.
fn paginate(content: &str, start_index: usize, max_chars: usize) -> (String, bool, Option<usize>) {
    let chars: Vec<char> = content.chars().collect();
    let total = chars.len();
    if start_index >= total {
        return (String::new(), false, None);
    }
    let end = (start_index + max_chars).min(total);
    let window: String = chars[start_index..end].iter().collect();
    let truncated = end < total;
    let next = truncated.then_some(end);
    (window, truncated, next)
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct NetFetchInput {
    /// The URL to fetch. Must be allowlisted and pass the SSRF guard; https only
    /// by default.
    pub url: String,
    /// Character offset to start the returned content window at (pagination).
    #[serde(default)]
    pub start_index: Option<usize>,
    /// Maximum characters to return in this window.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Return the raw text without HTML→markdown conversion.
    #[serde(default)]
    pub raw: Option<bool>,
    /// Map header name -> approved secret handle. Secret values are resolved by
    /// the host at request time and only sent to the origin host.
    #[serde(default)]
    pub secret_headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NetFetchOutput {
    /// The final URL (after redirects, once Phase 3 lands).
    pub final_url: String,
    pub status: u16,
    pub content_type: String,
    /// The content window (markdown for HTML, otherwise text).
    pub content: String,
    pub truncated: bool,
    /// Cursor to pass back as `start_index` to read the next window, if any.
    pub next_start_index: Option<usize>,
}

impl AiTool for NetFetchTool {
    const ID: &'static str = NET_FETCH_TOOL_ID;
    type Input = NetFetchInput;
    type Output = NetFetchOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            NET_FETCH_TOOL_ID,
            "Fetch one allowlisted URL with GET and return its content as bounded, \
             paginated markdown (HTML) or text. Private, metadata, and internal \
             network targets are always blocked. Use start_index/max_length to page \
             through long content.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let principal = ctx.principal().clone();
        Box::pin(async move { self.fetch(input, &principal).await.map_err(Into::into) })
    }
}

#[cfg(test)]
mod tests {
    use super::super::error::NetErrorCode;
    use super::*;

    #[test]
    fn classify_gates_binary_content() {
        assert_eq!(
            classify_content_type("text/html; charset=utf-8").unwrap(),
            ContentKind::Html
        );
        assert_eq!(
            classify_content_type("text/plain").unwrap(),
            ContentKind::Text
        );
        assert_eq!(classify_content_type("").unwrap(), ContentKind::Text);
        assert_eq!(
            classify_content_type("image/png").unwrap_err().code,
            NetErrorCode::UnsupportedContentType
        );
        assert_eq!(
            classify_content_type("application/pdf").unwrap_err().code,
            NetErrorCode::UnsupportedContentType
        );
    }

    #[test]
    fn paginate_windows_and_reports_cursor() {
        let (window, truncated, next) = paginate("abcdefghij", 0, 4);
        assert_eq!(window, "abcd");
        assert!(truncated);
        assert_eq!(next, Some(4));

        let (window, truncated, next) = paginate("abcdefghij", 8, 4);
        assert_eq!(window, "ij");
        assert!(!truncated);
        assert_eq!(next, None);

        let (window, truncated, next) = paginate("abc", 10, 4);
        assert_eq!(window, "");
        assert!(!truncated);
        assert_eq!(next, None);
    }

    #[test]
    fn paginate_respects_char_boundaries() {
        // Multibyte chars must not be split mid-codepoint.
        let (window, _truncated, next) = paginate("h\u{e9}llo w\u{f6}rld", 0, 5);
        assert_eq!(window, "h\u{e9}llo");
        assert_eq!(next, Some(5));
    }
}
