//! Shared guarded-HTTP core for the network tools.
//!
//! Every SSRF-critical mechanism lives here exactly once, so `net.fetch`
//! (renders the body as markdown/text) and `net.download` (stores the body as
//! an artifact) can never drift apart on the security path:
//!
//! - `authorize` — policy allow/scheme/port applied to the parsed target
//!   *before* DNS, then the SSRF guard resolves + validates + pins;
//! - the manual redirect loop — every hop re-runs the full guard against a
//!   client pinned to that hop's validated addresses, so a redirect can never
//!   re-resolve to a rebound internal IP;
//! - origin-only credential headers — a cross-host hop drops them;
//! - the request budget; and
//! - the capped body reader — `Content-Length` is never trusted.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use hickory_resolver::TokioResolver;
use pocopine_agenkit::server::{BoxFuture, Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use reqwest::StatusCode;
use reqwest::header::{HeaderName, LOCATION};
use reqwest::redirect::Policy;
use url::Url;

use super::error::{NetError, NetResult};
use super::policy::NetPolicy;
use super::ssrf::{Resolve, ValidatedTarget, resolve_and_pin, validate_url};
use crate::tools::secrets::{SecretRuntime, resolve_secret_headers};

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

/// The fully-read result of a guarded GET: the final URL (after redirects), the
/// status, the declared content type, the capped body bytes, and whether the
/// body was cut off at the cap. Callers decide what to do with `body` (render
/// vs store) — the SSRF/redirect/pin/budget guarantees are already applied.
pub(super) struct FetchedResponse {
    pub final_url: String,
    /// The origin host the request was made to (the allowlisted host the agent
    /// asked for) — the security-relevant field of an egress audit log.
    pub origin_host: String,
    pub status: StatusCode,
    pub content_type: String,
    pub body: Vec<u8>,
    pub truncated: bool,
}

/// The shared network engine: policy + resolver + request budget + injected
/// credential headers. Constructed once per tool registration and cloned into
/// each tool that needs it.
pub(super) struct GuardedHttp {
    policy: NetPolicy,
    resolver: Arc<dyn Resolve>,
    /// Requests performed by this registration, for the `max_requests` budget.
    requests: Arc<AtomicUsize>,
    /// Credential headers (e.g. `Authorization`) sent only to the origin host.
    secret_headers: Vec<(HeaderName, SecretString)>,
    secret_runtime: Option<Arc<SecretRuntime>>,
}

impl GuardedHttp {
    /// Build with the production hickory resolver.
    pub(super) fn new(policy: NetPolicy) -> NetResult<Self> {
        Ok(Self {
            policy,
            resolver: Arc::new(HickoryResolver::new()?),
            requests: Arc::new(AtomicUsize::new(0)),
            secret_headers: Vec::new(),
            secret_runtime: None,
        })
    }

    /// Build with an injected resolver (tests).
    pub(super) fn with_resolver(policy: NetPolicy, resolver: Arc<dyn Resolve>) -> Self {
        Self {
            policy,
            resolver,
            requests: Arc::new(AtomicUsize::new(0)),
            secret_headers: Vec::new(),
            secret_runtime: None,
        }
    }

    pub(super) fn push_secret_header(&mut self, name: HeaderName, value: SecretString) {
        self.secret_headers.push((name, value));
    }

    pub(super) fn set_secret_runtime(&mut self, runtime: Arc<SecretRuntime>) {
        self.secret_runtime = Some(runtime);
    }

    pub(super) fn policy(&self) -> &NetPolicy {
        &self.policy
    }

    /// Resolve + validate `raw` through the full guard (scheme/host/port
    /// allowlist → DNS → SSRF block → pin) WITHOUT connecting. The returned
    /// [`ValidatedTarget`] is the pre-flight net.resolve surfaces; a refused
    /// (non-allowlisted or private/metadata) target errors here exactly as it
    /// would for a fetch, and — like a fetch — never echoes a blocked IP.
    pub(super) async fn resolve(&self, raw: &str) -> NetResult<ValidatedTarget> {
        // Charge the request budget BEFORE resolving: a resolve triggers a real
        // DNS lookup, so — like `get` — it must count against
        // `NetPolicy::max_requests`, or net.resolve would be an uncapped DNS
        // channel (covert exfil / internal-name enumeration) that fetch bounds.
        self.charge_request()?;
        self.authorize(raw).await
    }

    /// GET `url` with the full guard, following redirects manually (each hop
    /// re-authorized + pinned), sending the injected + `requested` secret
    /// headers only to the origin host, and reading the body capped at `cap`
    /// bytes. `tool_id` binds resolved secret handles to the calling tool.
    ///
    /// `validate_headers(status, content_type)` runs on the **final** response
    /// *before* the body is read, so a caller (e.g. net.fetch rejecting binary)
    /// gets a deterministic classification error without downloading a body it
    /// will discard.
    pub(super) async fn get(
        &self,
        url: &str,
        requested_headers: &BTreeMap<String, String>,
        principal: &Principal,
        tool_id: &str,
        cap: usize,
        validate_headers: impl Fn(StatusCode, &str) -> NetResult<()>,
    ) -> NetResult<FetchedResponse> {
        self.charge_request()?;
        let mut target = self.authorize(url).await?;
        let origin_host = target.host().to_string();
        let dynamic_headers = self
            .resolve_dynamic_headers(requested_headers, principal, tool_id, &origin_host)
            .await
            .map_err(|err| NetError::not_allowed(err.to_string()))?;

        // Follow redirects ourselves so every hop re-runs the full guard +
        // policy (reqwest's own follower would not). Each request uses a client
        // pinned to that hop's validated addresses, so it can never re-resolve
        // to a rebound internal IP. Credential headers are sent only to the
        // origin host — a cross-host hop drops them.
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
        // Reject on the headers (e.g. binary content type) before spending
        // bandwidth on a body the caller will discard.
        validate_headers(status, &content_type)?;
        let (body, truncated) = read_capped(response, cap).await?;
        Ok(FetchedResponse {
            final_url,
            origin_host,
            status,
            content_type,
            body,
            truncated,
        })
    }

    /// The credential headers to send to `host`: the configured set on the
    /// origin host, nothing on any other host.
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

    /// Count this request against the budget, refusing once it is spent.
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

    /// Compose the allow/block policy and the SSRF guard into one gate. Policy
    /// is applied to the parsed target *before* DNS, so a disallowed host is
    /// never resolved; then the SSRF guard resolves + validates + pins.
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

    async fn resolve_dynamic_headers(
        &self,
        requested: &BTreeMap<String, String>,
        principal: &Principal,
        tool_id: &str,
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
                .resolve_handle(handle_id.as_str(), principal, tool_id, Some(origin_host))
                .await?;
            out.push((header, secret));
        }
        Ok(out)
    }
}

/// Build a client pinned to the target's validated addresses, with redirects
/// disabled (we follow them manually so each hop is re-guarded).
fn pinned_client(target: &ValidatedTarget) -> NetResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        // Disable system/env proxies (HTTPS_PROXY/ALL_PROXY): a proxy would do
        // its own DNS + connection, bypassing the resolve_to_addrs pin and
        // reopening the SSRF/DNS-rebinding hole the guard closes.
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .resolve_to_addrs(target.host(), target.addrs())
        .build()
        .map_err(|err| NetError::not_accessible(format!("HTTP client build failed: {err}")))
}

/// Decide the next hop given a response. Returns `Ok(None)` to stop
/// (non-redirect status, or a redirect with no `Location`), `Ok(Some(url))` to
/// follow (relative locations resolved against `current`), or an error once the
/// hop cap is hit. The returned URL is **not** yet validated — the caller
/// re-runs the full guard.
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

/// Read the body, stopping once `cap` bytes have been buffered. Returns the
/// bytes and whether the body was cut off. `Content-Length` is never trusted.
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

    fn engine(policy: NetPolicy, resolved: &[&str]) -> GuardedHttp {
        let addrs = resolved.iter().map(|a| a.parse().unwrap()).collect();
        GuardedHttp::with_resolver(policy, Arc::new(MockResolver(addrs)))
    }

    #[test]
    fn request_budget_is_enforced() {
        let http = engine(
            NetPolicy::allow(["example.com"]).with_max_requests(2),
            &["93.184.216.34"],
        );
        assert!(http.charge_request().is_ok());
        assert!(http.charge_request().is_ok());
        assert_eq!(
            http.charge_request().unwrap_err().code,
            NetErrorCode::TooManyRequests
        );
    }

    #[test]
    fn request_budget_unlimited_by_default() {
        let http = engine(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        for _ in 0..100 {
            assert!(http.charge_request().is_ok());
        }
    }

    #[test]
    fn credentials_only_sent_to_origin_host() {
        let mut http = engine(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        http.push_secret_header(
            reqwest::header::AUTHORIZATION,
            SecretString::new("Bearer token".to_string()),
        );
        assert_eq!(
            http.credentials_for(&[], "example.com", "example.com")
                .count(),
            1
        );
        // A cross-host redirect target gets no credentials.
        assert_eq!(
            http.credentials_for(&[], "evil.com", "example.com").count(),
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
                        .with_target_tools(["net.fetch"])
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
                    target_tool: "net.fetch".to_string(),
                    destination: Some("example.com".to_string()),
                    ttl_ms: None,
                    inheritable: None,
                },
                &principal,
            )
            .await
            .unwrap();
        let mut http = engine(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        http.set_secret_runtime(runtime);

        let headers = http
            .resolve_dynamic_headers(
                &BTreeMap::from([("authorization".to_string(), grant.handle_id)]),
                &principal,
                "net.fetch",
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
        let http = engine(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        assert!(http.authorize("https://example.com/").await.is_ok());
        assert_eq!(
            http.authorize("https://other.com/").await.unwrap_err().code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_enforces_scheme_and_port() {
        let http = engine(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        // default policy is https-only on 443
        assert_eq!(
            http.authorize("http://example.com/")
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
        assert_eq!(
            http.authorize("https://example.com:8080/")
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_blocks_ssrf_even_for_allowlisted_host() {
        // Host is allowlisted, but DNS resolves to a metadata address.
        let http = engine(NetPolicy::allow(["example.com"]), &["169.254.169.254"]);
        assert_eq!(
            http.authorize("https://example.com/")
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_does_not_resolve_disallowed_host() {
        let http = engine(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        let err = http.authorize("https://evil.test/").await.unwrap_err();
        assert_eq!(err.code, NetErrorCode::UrlNotAllowed);
        assert!(err.message.contains("allowlist"));
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn next_redirect_follows_relative_and_absolute() {
        let current = url("https://example.com/a/b");
        let next = next_redirect(&current, StatusCode::FOUND, Some("/c"), 0)
            .unwrap()
            .unwrap();
        assert_eq!(next.as_str(), "https://example.com/c");
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
}
