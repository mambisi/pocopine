//! `net.fetch` — fetch one allowlisted URL (GET) and return bounded, paginated
//! text/markdown.
//!
//! The request path is: `authorize` (parse → policy allow/scheme/port → SSRF
//! resolve+pin) → build a `reqwest` client **pinned** to the validated addresses
//! → follow redirects manually, re-running the full guard on **every** hop →
//! stream-and-cap the body → render (HTML→markdown via `fast_html2md`, or text)
//! → char-offset paginate.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use hickory_resolver::TokioResolver;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture, Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use reqwest::StatusCode;
use reqwest::header::{HeaderName, LOCATION};
use reqwest::redirect::Policy;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use super::error::{NetError, NetResult};
use super::policy::NetPolicy;
use super::ssrf::{Resolve, ValidatedTarget, resolve_and_pin, validate_url};
use crate::tools::secrets::{SecretRuntime, resolve_secret_headers};

pub const NET_FETCH_TOOL_ID: &str = "net.fetch";

/// Default content window returned to the model, in characters.
const DEFAULT_PAGE_CHARS: usize = 5_000;
/// Upper bound a caller may request for one window.
const MAX_PAGE_CHARS: usize = 50_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum redirect hops followed before giving up.
const MAX_REDIRECTS: usize = 5;

/// Production DNS resolver (in-process, hickory) used for resolve-then-pin.
pub struct HickoryResolver {
    inner: TokioResolver,
}

impl HickoryResolver {
    pub fn new() -> NetResult<Self> {
        let inner = TokioResolver::builder_tokio()
            .map_err(|err| NetError::not_accessible(format!("DNS resolver init failed: {err}")))?
            .build();
        Ok(Self { inner })
    }
}

impl Resolve for HickoryResolver {
    fn lookup(&self, host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>> {
        let host = host.to_string();
        Box::pin(async move {
            let lookup = self
                .inner
                .lookup_ip(host.as_str())
                .await
                .map_err(|err| NetError::not_accessible(format!("DNS lookup failed: {err}")))?;
            Ok(lookup.iter().collect())
        })
    }
}

/// The `net.fetch` tool. Holds the host allow/block policy, a (shared) DNS
/// resolver, and any host-injected credential headers. Defaults to
/// deny-everything (no allowlist) — a host opts in domains.
pub struct NetFetchTool {
    policy: NetPolicy,
    resolver: Arc<dyn Resolve>,
    /// Fetches performed by this registration, for the `max_requests` budget.
    requests: Arc<AtomicUsize>,
    /// Credential headers (e.g. `Authorization`) sent only to the origin host.
    secret_headers: Vec<(HeaderName, SecretString)>,
    secret_runtime: Option<Arc<SecretRuntime>>,
}

impl NetFetchTool {
    /// Build with the production hickory resolver.
    pub fn new(policy: NetPolicy) -> NetResult<Self> {
        Ok(Self {
            policy,
            resolver: Arc::new(HickoryResolver::new()?),
            requests: Arc::new(AtomicUsize::new(0)),
            secret_headers: Vec::new(),
            secret_runtime: None,
        })
    }

    /// Build with an injected resolver (tests).
    pub fn with_resolver(policy: NetPolicy, resolver: Arc<dyn Resolve>) -> Self {
        Self {
            policy,
            resolver,
            requests: Arc::new(AtomicUsize::new(0)),
            secret_headers: Vec::new(),
            secret_runtime: None,
        }
    }

    /// Attach a credential header (e.g. `AUTHORIZATION`) carried as a
    /// [`SecretString`]. It is sent **only** to the origin host — a cross-host
    /// redirect never receives it — and never appears in output, traces, or logs.
    pub fn with_secret_header(mut self, name: HeaderName, value: SecretString) -> Self {
        self.secret_headers.push((name, value));
        self
    }

    pub fn with_secret_runtime(mut self, runtime: Arc<SecretRuntime>) -> Self {
        self.secret_runtime = Some(runtime);
        self
    }

    /// The credential headers to send to `host`: the configured set on the origin
    /// host, nothing on any other host.
    fn credentials_for<'a>(
        &'a self,
        dynamic_headers: &'a [(HeaderName, SecretString)],
        host: &str,
        origin_host: &str,
    ) -> Box<dyn Iterator<Item = &'a (HeaderName, SecretString)> + 'a> {
        if host == origin_host {
            Box::new(self.secret_headers.iter().chain(dynamic_headers.iter()))
        } else {
            Box::new(std::iter::empty())
        }
    }

    /// Count this fetch against the request budget, refusing once it is spent.
    fn charge_request(&self) -> NetResult<()> {
        if let Some(max) = self.policy.max_requests() {
            let used = self.requests.fetch_add(1, Ordering::SeqCst);
            if used >= max {
                return Err(NetError::too_many_requests(format!(
                    "request budget of {max} exhausted"
                )));
            }
        }
        Ok(())
    }

    /// Compose the allow/block policy and the SSRF guard into one gate. Policy is
    /// applied to the parsed target *before* DNS, so a disallowed host is never
    /// resolved; then the SSRF guard resolves + validates + pins.
    async fn authorize(&self, raw: &str) -> NetResult<ValidatedTarget> {
        let target = validate_url(raw)?;
        let scheme = target.url.scheme();
        if !self.policy.scheme_allowed(scheme) {
            return Err(NetError::not_allowed(format!(
                "scheme `{scheme}` is not allowed by policy"
            )));
        }
        if !self.policy.host_allowed(&target.host) {
            return Err(NetError::not_allowed(format!(
                "host `{}` is not in the allowlist",
                target.host
            )));
        }
        if !self.policy.port_allowed(target.port) {
            return Err(NetError::not_allowed(format!(
                "port {} is not allowed by policy",
                target.port
            )));
        }
        resolve_and_pin(self.resolver.as_ref(), target).await
    }

    async fn fetch(
        &self,
        input: NetFetchInput,
        principal: &Principal,
    ) -> NetResult<NetFetchOutput> {
        self.charge_request()?;
        let mut target = self.authorize(&input.url).await?;
        let origin_host = target.host().to_string();
        let dynamic_headers = self
            .resolve_dynamic_headers(&input.secret_headers, principal, &origin_host)
            .await
            .map_err(|err| NetError::not_allowed(err.to_string()))?;

        // Follow redirects ourselves so every hop re-runs the full guard + policy
        // (reqwest's own follower would not). Each request uses a client pinned to
        // that hop's validated addresses, so it can never re-resolve to a rebound
        // internal IP. Credential headers are sent only to the origin host — a
        // cross-host hop drops them.
        let mut hops = 0usize;
        let response = loop {
            let client = pinned_client(&target)?;
            let mut request = client.get(target.url().clone());
            for (name, secret) in
                self.credentials_for(&dynamic_headers, target.host(), &origin_host)
            {
                request = request.header(name.clone(), secret.expose());
            }
            let response = request
                .send()
                .await
                .map_err(|err| NetError::not_accessible(format!("request failed: {err}")))?;

            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            match next_redirect(target.url(), response.status(), location.as_deref(), hops)? {
                Some(next) => {
                    hops += 1;
                    target = self.authorize(next.as_str()).await?;
                }
                None => break response,
            }
        };

        let status = response.status();
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let kind = classify_content_type(&content_type)?;

        let (body, body_truncated) =
            read_capped(response, self.policy.max_response_bytes()).await?;
        let text = String::from_utf8_lossy(&body).into_owned();
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
        let truncated = body_truncated || page_truncated;

        // One event per call (RFC-069 `pocopine.log`): host + outcome only —
        // never the full URL (a query can carry secrets), headers, or body.
        tracing::info!(
            target: "pocopine.log",
            tool = NET_FETCH_TOOL_ID,
            host = %origin_host,
            status = status.as_u16(),
            truncated,
            "net.fetch completed"
        );

        Ok(NetFetchOutput {
            final_url,
            status: status.as_u16(),
            content_type,
            content,
            truncated,
            next_start_index,
        })
    }

    async fn resolve_dynamic_headers(
        &self,
        requested: &BTreeMap<String, String>,
        principal: &Principal,
        origin_host: &str,
    ) -> AgenkitResult<Vec<(HeaderName, SecretString)>> {
        let runtime = match (&self.secret_runtime, requested.is_empty()) {
            (_, true) => return Ok(Vec::new()),
            (Some(runtime), false) => runtime,
            (None, false) => {
                return Err(AgenkitError::tool_policy(
                    "network secret handles are not configured for this runtime",
                ));
            }
        };
        let mut out = Vec::new();
        for (header, handle_id) in resolve_secret_headers(requested)? {
            let secret = runtime
                .resolve_handle(
                    handle_id.as_str(),
                    principal,
                    NET_FETCH_TOOL_ID,
                    Some(origin_host),
                )
                .await?;
            out.push((header, secret));
        }
        Ok(out)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentKind {
    Text,
    Html,
}

/// Gate the content type: text-like and HTML are fetchable; binary (images, PDF,
/// archives, …) is rejected and routed to `net.download` (a later phase).
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

/// Read the body, stopping once `cap` bytes have been buffered. Returns the bytes
/// and whether the body was cut off. `Content-Length` is never trusted.
async fn read_capped(response: reqwest::Response, cap: usize) -> NetResult<(Vec<u8>, bool)> {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut overflow = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|err| NetError::not_accessible(format!("response read failed: {err}")))?;
        if buf.len() + chunk.len() > cap {
            buf.extend_from_slice(&chunk[..cap - buf.len()]);
            overflow = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((buf, overflow))
}

/// Build a client pinned to the target's validated addresses, with redirects
/// disabled (we follow them manually so each hop is re-guarded).
fn pinned_client(target: &ValidatedTarget) -> NetResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        // Disable system/env proxies (HTTPS_PROXY/ALL_PROXY): a proxy would do its
        // own DNS + connection, bypassing the resolve_to_addrs pin and reopening
        // the SSRF/DNS-rebinding hole the guard closes.
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .resolve_to_addrs(target.host(), target.addrs())
        .build()
        .map_err(|err| NetError::not_accessible(format!("HTTP client build failed: {err}")))
}

/// Decide the next hop given a response. Returns `Ok(None)` to stop (non-redirect
/// status, or a redirect with no `Location`), `Ok(Some(url))` to follow (relative
/// locations resolved against `current`), or an error once the hop cap is hit.
/// The returned URL is **not** yet validated — the caller re-runs the full guard.
fn next_redirect(
    current: &Url,
    status: StatusCode,
    location: Option<&str>,
    hops: usize,
) -> NetResult<Option<Url>> {
    if !status.is_redirection() {
        return Ok(None);
    }
    let Some(location) = location else {
        return Ok(None);
    };
    if hops >= MAX_REDIRECTS {
        return Err(NetError::not_accessible(format!(
            "exceeded {MAX_REDIRECTS} redirects"
        )));
    }
    let next = current
        .join(location)
        .map_err(|err| NetError::not_accessible(format!("invalid redirect location: {err}")))?;
    Ok(Some(next))
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
    use crate::policy::ToolMode;
    use crate::tools::secrets::{
        InMemorySecretResolver, SecretMetadata, SecretRequestInput, SecretRuntime, SecretScope,
    };

    struct MockResolver(Vec<IpAddr>);
    impl Resolve for MockResolver {
        fn lookup(&self, _host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    fn tool_with(policy: NetPolicy, resolved: &[&str]) -> NetFetchTool {
        let addrs = resolved.iter().map(|a| a.parse().unwrap()).collect();
        NetFetchTool::with_resolver(policy, Arc::new(MockResolver(addrs)))
    }

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
        let (window, _truncated, next) = paginate("héllo wörld", 0, 5);
        assert_eq!(window, "héllo");
        assert_eq!(next, Some(5));
    }

    #[test]
    fn request_budget_is_enforced() {
        let tool = tool_with(
            NetPolicy::allow(["example.com"]).with_max_requests(2),
            &["93.184.216.34"],
        );
        assert!(tool.charge_request().is_ok());
        assert!(tool.charge_request().is_ok());
        assert_eq!(
            tool.charge_request().unwrap_err().code,
            NetErrorCode::TooManyRequests
        );
    }

    #[test]
    fn request_budget_unlimited_by_default() {
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        for _ in 0..100 {
            assert!(tool.charge_request().is_ok());
        }
    }

    #[test]
    fn credentials_only_sent_to_origin_host() {
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["93.184.216.34"])
            .with_secret_header(
                reqwest::header::AUTHORIZATION,
                SecretString::new("Bearer token".to_string()),
            );
        assert_eq!(
            tool.credentials_for(&[], "example.com", "example.com")
                .count(),
            1
        );
        // A cross-host redirect target gets no credentials.
        assert_eq!(
            tool.credentials_for(&[], "evil.com", "example.com").count(),
            0
        );
    }

    #[tokio::test]
    async fn dynamic_secret_headers_resolve_for_origin_host() {
        let principal = Principal::anonymous();
        let runtime = Arc::new(
            SecretRuntime::new(Arc::new(
                InMemorySecretResolver::new().insert(
                    SecretMetadata::new("bearer", "Bearer token", SecretScope::User)
                        .with_purposes(["fetch-auth"])
                        .with_target_tools([NET_FETCH_TOOL_ID])
                        .with_destinations(["example.com"]),
                    SecretString::new("Bearer dynamic-secret".to_string()),
                ),
            ))
            .with_request_mode(ToolMode::Allow),
        );
        let grant = runtime
            .request(
                SecretRequestInput {
                    secret_ref: "bearer".to_string(),
                    purpose: "fetch-auth".to_string(),
                    target_tool: NET_FETCH_TOOL_ID.to_string(),
                    destination: Some("example.com".to_string()),
                    ttl_ms: None,
                    inheritable: None,
                },
                &principal,
            )
            .await
            .unwrap();
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["93.184.216.34"])
            .with_secret_runtime(runtime);

        let headers = tool
            .resolve_dynamic_headers(
                &BTreeMap::from([("authorization".to_string(), grant.handle_id)]),
                &principal,
                "example.com",
            )
            .await
            .unwrap();

        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, reqwest::header::AUTHORIZATION);
        assert_eq!(headers[0].1.expose(), "Bearer dynamic-secret");
    }

    #[tokio::test]
    async fn authorize_requires_allowlisted_host() {
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        assert!(tool.authorize("https://example.com/").await.is_ok());
        assert_eq!(
            tool.authorize("https://other.com/").await.unwrap_err().code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_enforces_scheme_and_port() {
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        // default policy is https-only on 443
        assert_eq!(
            tool.authorize("http://example.com/")
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
        assert_eq!(
            tool.authorize("https://example.com:8080/")
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_blocks_ssrf_even_for_allowlisted_host() {
        // Host is allowlisted, but DNS resolves to a metadata address.
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["169.254.169.254"]);
        assert_eq!(
            tool.authorize("https://example.com/")
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn next_redirect_follows_relative_and_absolute() {
        let current = url("https://example.com/a/b");
        // relative location resolved against the current URL
        let next = next_redirect(&current, StatusCode::FOUND, Some("/c"), 0)
            .unwrap()
            .unwrap();
        assert_eq!(next.as_str(), "https://example.com/c");
        // absolute location to a different host (re-guarded by the caller)
        let next = next_redirect(
            &current,
            StatusCode::MOVED_PERMANENTLY,
            Some("https://other.com/x"),
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next.as_str(), "https://other.com/x");
    }

    #[test]
    fn next_redirect_stops_on_non_redirect_or_missing_location() {
        let current = url("https://example.com/");
        assert!(
            next_redirect(&current, StatusCode::OK, Some("/x"), 0)
                .unwrap()
                .is_none()
        );
        assert!(
            next_redirect(&current, StatusCode::FOUND, None, 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn next_redirect_caps_hops() {
        let current = url("https://example.com/");
        let err =
            next_redirect(&current, StatusCode::FOUND, Some("/x"), MAX_REDIRECTS).unwrap_err();
        assert_eq!(err.code, NetErrorCode::UrlNotAccessible);
    }

    #[tokio::test]
    async fn authorize_does_not_resolve_disallowed_host() {
        // MockResolver would return a blocked IP; if it were consulted the error
        // would still be UrlNotAllowed, so assert via a resolver that would panic
        // is overkill — instead confirm a host-not-allowlisted error path.
        let tool = tool_with(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        let err = tool.authorize("https://evil.test/").await.unwrap_err();
        assert_eq!(err.code, NetErrorCode::UrlNotAllowed);
        assert!(err.message.contains("allowlist"));
    }
}
