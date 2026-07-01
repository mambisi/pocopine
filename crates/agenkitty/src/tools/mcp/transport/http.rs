//! Streamable HTTP transport (Phase 4, gated behind the network capability).
//!
//! Implements rmcp's [`StreamableHttpClient`] (`post_message` / `get_stream` /
//! `delete_session`) over the network tool's **SSRF-guarded / DNS-pinned**
//! client — never a bare reqwest client. Every request (the configured endpoint
//! *and every redirect hop*) re-runs the full guard: HTTPS-only via the
//! [`NetPolicy`], destination-allowlist + reserved/metadata deny-list, and a
//! resolve→validate→**pin** so the connection can never be rebound to an
//! internal address (T6). rmcp's worker on top of this reproduces the session-id
//! handling, `Last-Event-ID` resume, and SSE-vs-JSON negotiation; we only own
//! the per-request HTTP edge.
//!
//! Auth (D10/T7): an `Authorization` (or any other) header resolved from a
//! **secret handle** at the transport edge is carried in `custom_headers`; the
//! OAuth bearer path uses rmcp's `auth_header`. The host never forwards an
//! inbound caller token upstream and never injects its own creds downstream —
//! token audience validation + the RFC 8707 resource indicator live in
//! [`super::super::oauth`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION, WWW_AUTHENTICATE};
use reqwest::redirect::Policy;
use rmcp::model::{ClientJsonRpcMessage, JsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpClient, StreamableHttpError,
    StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse, SseStream};

use crate::tools::network::{NetPolicy, Resolve, ValidatedTarget, resolve_and_pin, validate_url};

/// MCP Streamable HTTP header + MIME constants (hardcoded so we don't depend on
/// rmcp's `pub(crate)` `http_header` helpers; they mirror the spec verbatim).
const EVENT_STREAM_MIME: &str = "text/event-stream";
const JSON_MIME: &str = "application/json";
const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
const HEADER_LAST_EVENT_ID: &str = "Last-Event-Id";

/// Reserved headers a server-/worker-supplied `custom_headers` map must not
/// override (the transport sets them itself). `MCP-Protocol-Version` is reserved
/// but allowed through because rmcp's worker injects it as a custom header after
/// the handshake.
const RESERVED_HEADERS: &[&str] = &["accept", HEADER_SESSION_ID, HEADER_LAST_EVENT_ID];

/// Maximum redirect hops followed before giving up (each hop is re-guarded).
const MAX_REDIRECTS: usize = 5;
/// Hard ceiling on a **non-streaming** HTTP response body (the POST/DELETE JSON
/// responses we hand to rmcp's parser) read at the transport edge (M1). A server
/// returning a body larger than this is refused — fail-closed — *before* the
/// whole untrusted payload is buffered and parsed by rmcp/serde, so a huge
/// `tools/list` / `tools/call` JSON body can't force a giant allocation ahead of
/// the per-result post-parse caps in the adapter / resource verbs. The long-lived
/// SSE `GET` stream is exempt (it is framed incrementally by rmcp; see the note
/// on [`GuardedHttpClient::get_stream`]).
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Total-request timeout for POST/DELETE. **Not** applied to the SSE `GET`
/// stream, which is long-lived by design.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The error surfaced from the guarded HTTP edge into rmcp's
/// [`StreamableHttpError::Client`].
#[derive(Debug)]
pub enum HttpTransportError {
    /// The SSRF guard / destination policy refused the URL (the endpoint or a
    /// redirect hop): non-HTTPS, not allowlisted, a private/metadata/internal
    /// target, or a host that resolved to a blocked address. The message never
    /// echoes a resolved internal IP — the SSRF layer already scrubs that.
    Blocked(String),
    /// A bearer/`Authorization` credential failed audience validation against the
    /// server's RFC 8707 resource indicator (a confused-deputy / passthrough
    /// token, or an opaque token not bound to this destination) — refused before
    /// it is attached to any request (D10/T7).
    CredentialRejected(String),
    /// A user/worker `custom_headers` entry collided with a reserved header.
    ReservedHeader(String),
    /// Too many redirect hops.
    TooManyRedirects,
    /// The response body exceeded the pre-parse byte ceiling (M1): refused before
    /// the whole untrusted payload is buffered/parsed. `limit` is the cap in bytes.
    BodyTooLarge { limit: usize },
    /// Building the pinned reqwest client failed.
    Build(String),
    /// The underlying HTTP request failed.
    Request(reqwest::Error),
}

impl std::fmt::Display for HttpTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpTransportError::Blocked(msg) => write!(f, "mcp http request blocked: {msg}"),
            HttpTransportError::CredentialRejected(msg) => {
                write!(f, "mcp http credential rejected: {msg}")
            }
            HttpTransportError::ReservedHeader(name) => {
                write!(f, "custom header `{name}` is reserved by the transport")
            }
            HttpTransportError::TooManyRedirects => {
                write!(f, "mcp http request exceeded {MAX_REDIRECTS} redirects")
            }
            HttpTransportError::BodyTooLarge { limit } => {
                write!(f, "mcp http response body exceeded the {limit}-byte cap")
            }
            HttpTransportError::Build(msg) => write!(f, "mcp http client build failed: {msg}"),
            HttpTransportError::Request(err) => write!(f, "mcp http request failed: {err}"),
        }
    }
}

impl std::error::Error for HttpTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HttpTransportError::Request(err) => Some(err),
            _ => None,
        }
    }
}

fn client_err(err: HttpTransportError) -> StreamableHttpError<HttpTransportError> {
    StreamableHttpError::Client(err)
}

/// Read a response body with a hard byte ceiling, streaming chunk-by-chunk and
/// failing closed the moment the accumulated size would exceed `cap` — so an
/// over-large untrusted body is rejected **before** it is fully buffered/parsed
/// (M1, defense against a memory-blowup payload). A truthful `Content-Length`
/// over `cap` is rejected up front, before the first byte is read.
async fn read_body_capped(
    response: reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, HttpTransportError> {
    if let Some(len) = response.content_length()
        && len > cap as u64
    {
        return Err(HttpTransportError::BodyTooLarge { limit: cap });
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(HttpTransportError::Request)?;
        if buf.len() + chunk.len() > cap {
            return Err(HttpTransportError::BodyTooLarge { limit: cap });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// A [`StreamableHttpClient`] that routes every request through the SSRF guard +
/// destination policy and pins the connection to the validated addresses.
/// Cheap to clone (`Arc` resolver + cloneable policy), as the trait requires.
#[derive(Clone)]
pub struct GuardedHttpClient {
    policy: NetPolicy,
    resolver: Arc<dyn Resolve>,
    /// The server's RFC 8707 resource indicator (the audience any bearer
    /// credential must name, D10/T7). Set from the configured endpoint at connect
    /// time and used to validate the rmcp worker's dynamic `auth_header` bearer
    /// before it is attached to any request.
    expected_resource: String,
}

impl GuardedHttpClient {
    /// Build a client gated by `policy` (HTTPS-only + the server's destination
    /// allowlist) using `resolver` for resolve-then-pin. `expected_resource` is
    /// the endpoint's RFC 8707 resource indicator, against which any bearer
    /// `auth_header` is audience-validated before it is sent (T7).
    pub fn new(policy: NetPolicy, resolver: Arc<dyn Resolve>, expected_resource: String) -> Self {
        Self {
            policy,
            resolver,
            expected_resource,
        }
    }

    /// Audience-validate the rmcp worker's bearer (`auth_header`) before it is
    /// attached to a request (D10/T7). This is the dynamic OAuth-flow credential,
    /// **not** an operator-bound config header, so an opaque token fails closed
    /// and a JWT must name this server's resource indicator. The configured static
    /// `Authorization` header is validated once at connect time (see
    /// [`connect_http`](super::super::client::McpConnection::connect_http)); this
    /// is the send-path gate for the dynamic channel. Runs before any DNS /
    /// request, so a mismatched bearer is never transmitted.
    fn validate_bearer(
        &self,
        auth_header: Option<&str>,
    ) -> Result<(), StreamableHttpError<HttpTransportError>> {
        if let Some(token) = auth_header {
            crate::tools::mcp::oauth::validate_bearer_credential(
                token,
                &self.expected_resource,
                false,
            )
            .map_err(|err| client_err(HttpTransportError::CredentialRejected(err.to_string())))?;
        }
        Ok(())
    }

    /// Resolve → validate → pin one URL: HTTPS + allowlist + port via the
    /// [`NetPolicy`], then the SSRF deny-list over every resolved address. Reused
    /// for the endpoint and for each redirect hop.
    async fn guard(&self, raw: &str) -> Result<ValidatedTarget, HttpTransportError> {
        let target =
            validate_url(raw).map_err(|err| HttpTransportError::Blocked(err.to_string()))?;
        let scheme = target.url.scheme();
        if !self.policy.scheme_allowed(scheme) {
            return Err(HttpTransportError::Blocked(format!(
                "scheme `{scheme}` is not allowed (https-only)"
            )));
        }
        if !self.policy.host_allowed(&target.host) {
            return Err(HttpTransportError::Blocked(format!(
                "host `{}` is not in the server destination allowlist",
                target.host
            )));
        }
        if !self.policy.port_allowed(target.port) {
            return Err(HttpTransportError::Blocked(format!(
                "port {} is not allowed by policy",
                target.port
            )));
        }
        resolve_and_pin(self.resolver.as_ref(), target)
            .await
            .map_err(|err| HttpTransportError::Blocked(err.to_string()))
    }

    /// A reqwest client pinned to the validated addresses, with auto-redirects
    /// disabled (we follow them ourselves so each hop is re-guarded) and
    /// system/env proxies disabled (a proxy would do its own DNS, bypassing the
    /// pin and reopening the rebinding hole). `streaming` omits the total-request
    /// timeout for the long-lived SSE `GET`.
    fn pinned_client(
        &self,
        target: &ValidatedTarget,
        streaming: bool,
    ) -> Result<reqwest::Client, HttpTransportError> {
        let mut builder = reqwest::Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .resolve_to_addrs(target.host(), target.addrs());
        if !streaming {
            builder = builder.timeout(REQUEST_TIMEOUT);
        }
        builder
            .build()
            .map_err(|err| HttpTransportError::Build(err.to_string()))
    }

    /// Send a request, re-guarding + re-pinning on every redirect hop, and return
    /// the final non-redirect response. `build` applies the method-specific
    /// headers/body to the per-hop request builder.
    async fn send_guarded<F>(
        &self,
        raw_uri: &str,
        method: Method,
        streaming: bool,
        build: F,
    ) -> Result<reqwest::Response, StreamableHttpError<HttpTransportError>>
    where
        F: Fn(reqwest::RequestBuilder, bool) -> reqwest::RequestBuilder,
    {
        let mut current = raw_uri.to_string();
        let mut hops = 0usize;
        let mut origin: Option<(String, String, u16)> = None;
        loop {
            let target = self.guard(&current).await.map_err(client_err)?;
            let this_origin = target_origin(&target);
            // The first hop establishes the credential origin; a later (redirect)
            // hop only carries the secret/bearer headers when its origin is
            // identical — a 3xx to a different allowlisted host (or sibling
            // subdomain) must not replay the per-server secret (mirrors reqwest's
            // default sensitive-header stripping, lost under `Policy::none()`).
            let same_origin = origin.as_ref().is_none_or(|first| *first == this_origin);
            if origin.is_none() {
                origin = Some(this_origin);
            }
            let client = self.pinned_client(&target, streaming).map_err(client_err)?;
            let request = build(
                client.request(method.clone(), target.url().clone()),
                same_origin,
            );
            let response = request
                .send()
                .await
                .map_err(|err| client_err(HttpTransportError::Request(err)))?;

            if response.status().is_redirection()
                && let Some(location) = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
            {
                if hops >= MAX_REDIRECTS {
                    return Err(client_err(HttpTransportError::TooManyRedirects));
                }
                // Resolve relative redirects against the current URL, then
                // re-guard the next hop (T6: a 3xx to an internal IP is
                // refused exactly like the original endpoint).
                let next = target.url().join(location).map_err(|err| {
                    client_err(HttpTransportError::Blocked(format!(
                        "invalid redirect location: {err}"
                    )))
                })?;
                current = next.to_string();
                hops += 1;
                continue;
            }
            return Ok(response);
        }
    }
}

/// Reject a `custom_headers` map that collides with a transport-reserved header
/// (run once before the redirect loop so the per-hop builder is infallible).
fn check_custom_headers(
    headers: &HashMap<HeaderName, HeaderValue>,
) -> Result<(), StreamableHttpError<HttpTransportError>> {
    for name in headers.keys() {
        let lower = name.as_str();
        if RESERVED_HEADERS
            .iter()
            .any(|reserved| lower.eq_ignore_ascii_case(reserved))
        {
            return Err(client_err(HttpTransportError::ReservedHeader(
                name.to_string(),
            )));
        }
    }
    Ok(())
}

/// Apply a validated `custom_headers` map to a request builder.
///
/// On a **cross-origin** redirect hop (`same_origin == false`) the credential-
/// bearing headers (`Authorization`/`Cookie`/`Proxy-Authorization`) are dropped
/// so a configured per-server secret (resolved into the `Authorization` custom
/// header in [`connect_http`](super::super::client::McpConnection::connect_http))
/// is never replayed to a different origin. We disable reqwest's own redirect
/// handling (`Policy::none()`) and follow hops ourselves, which silently dropped
/// reqwest's default cross-origin sensitive-header stripping — this restores it.
fn apply_custom_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HashMap<HeaderName, HeaderValue>,
    same_origin: bool,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if !same_origin && is_sensitive_header(name) {
            continue;
        }
        builder = builder.header(name.clone(), value.clone());
    }
    builder
}

/// Credential-bearing headers stripped on a cross-origin redirect hop (mirrors
/// reqwest's default `remove_sensitive_headers` set).
fn is_sensitive_header(name: &HeaderName) -> bool {
    use reqwest::header::{AUTHORIZATION, COOKIE, PROXY_AUTHORIZATION};
    name == AUTHORIZATION || name == COOKIE || name == PROXY_AUTHORIZATION
}

/// The origin (scheme, host, port) of a validated target — the unit a redirect
/// must match for the secret/bearer credential headers to be re-sent. Host is
/// already normalized by the SSRF layer; lowercase the scheme for a stable
/// comparison.
fn target_origin(target: &ValidatedTarget) -> (String, String, u16) {
    (
        target.url().scheme().to_ascii_lowercase(),
        target.host().to_ascii_lowercase(),
        target.port(),
    )
}

/// Best-effort `scope=` extraction from a `WWW-Authenticate` header (quoted or
/// bare), used to populate [`InsufficientScopeError`].
fn extract_scope(header: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let pos = lower.find("scope=")? + "scope=".len();
    let rest = &header[pos..];
    if let Some(stripped) = rest.strip_prefix('"') {
        stripped.find('"').map(|end| stripped[..end].to_string())
    } else {
        let end = rest
            .find(|c: char| c == ',' || c == ';' || c.is_whitespace())
            .unwrap_or(rest.len());
        (end > 0).then(|| rest[..end].to_string())
    }
}

impl StreamableHttpClient for GuardedHttpClient {
    type Error = HttpTransportError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        check_custom_headers(&custom_headers)?;
        self.validate_bearer(auth_header.as_deref())?;
        let body = serde_json::to_vec(&message).map_err(StreamableHttpError::Deserialize)?;
        let session_was_attached = session_id.is_some();

        let response = self
            .send_guarded(&uri, Method::POST, false, |builder, same_origin| {
                let mut builder = builder
                    .header(ACCEPT, format!("{EVENT_STREAM_MIME}, {JSON_MIME}"))
                    .header(CONTENT_TYPE, JSON_MIME)
                    .body(body.clone());
                if let Some(session) = &session_id {
                    builder = builder.header(HEADER_SESSION_ID, session.as_ref());
                }
                // Bearer (OAuth) credential: only on the original origin (T7).
                if let Some(token) = &auth_header
                    && same_origin
                {
                    builder = builder.bearer_auth(token);
                }
                apply_custom_headers(builder, &custom_headers, same_origin)
            })
            .await?;

        let status = response.status();
        // Auth challenges (D10): surface so the deferred OAuth flow can engage.
        if status == reqwest::StatusCode::UNAUTHORIZED
            && let Some(header) = www_authenticate(&response)
        {
            return Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                header,
            )));
        }
        if status == reqwest::StatusCode::FORBIDDEN
            && let Some(header) = www_authenticate(&response)
        {
            let scope = extract_scope(&header);
            return Err(StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new(header, scope),
            ));
        }
        if matches!(
            status,
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == reqwest::StatusCode::NOT_FOUND && session_was_attached {
            return Err(StreamableHttpError::SessionExpired);
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
        let content_length = response.content_length();
        let session_id = response
            .headers()
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        // Some servers answer notifications/responses with an empty 200 rather
        // than 202; treat that as accepted.
        if status.is_success()
            && content_length == Some(0)
            && matches!(
                message,
                ClientJsonRpcMessage::Notification(_)
                    | ClientJsonRpcMessage::Response(_)
                    | ClientJsonRpcMessage::Error(_)
            )
        {
            return Ok(StreamableHttpPostResponse::Accepted);
        }

        if !status.is_success() {
            // Cap the (untrusted) error body before buffering/parsing it (M1).
            let body = read_body_capped(response, MAX_HTTP_BODY_BYTES)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|_| "<failed to read response body>".to_owned());
            if content_type
                .as_deref()
                .is_some_and(|ct| ct.as_bytes().starts_with(JSON_MIME.as_bytes()))
                && let Ok(message @ JsonRpcMessage::Error(_)) =
                    serde_json::from_str::<ServerJsonRpcMessage>(&body)
            {
                return Ok(StreamableHttpPostResponse::Json(message, session_id));
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(
                format!("HTTP {status}").into(),
            ));
        }

        match content_type.as_deref() {
            Some(ct) if ct.as_bytes().starts_with(EVENT_STREAM_MIME.as_bytes()) => {
                let stream = SseStream::from_byte_stream(response.bytes_stream()).boxed();
                Ok(StreamableHttpPostResponse::Sse(stream, session_id))
            }
            Some(ct) if ct.as_bytes().starts_with(JSON_MIME.as_bytes()) => {
                // Cap the body BEFORE rmcp/serde parses it (M1): a huge JSON
                // `tools/list` / `tools/call` payload is refused (fail-closed)
                // rather than allocated whole and parsed.
                let body = read_body_capped(response, MAX_HTTP_BODY_BYTES)
                    .await
                    .map_err(client_err)?;
                match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
                    Ok(message) => Ok(StreamableHttpPostResponse::Json(message, session_id)),
                    Err(err) => {
                        tracing::warn!(
                            target: "pocopine.log",
                            error = %err,
                            "mcp http: 200 json body was not a JSON-RPC message; treating as accepted"
                        );
                        Ok(StreamableHttpPostResponse::Accepted)
                    }
                }
            }
            other => Err(StreamableHttpError::UnexpectedContentType(
                other.map(str::to_string),
            )),
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        check_custom_headers(&custom_headers)?;
        self.validate_bearer(auth_header.as_deref())?;
        let response = self
            .send_guarded(&uri, Method::DELETE, false, |builder, same_origin| {
                let mut builder = builder.header(HEADER_SESSION_ID, session_id.as_ref());
                if let Some(token) = &auth_header
                    && same_origin
                {
                    builder = builder.bearer_auth(token);
                }
                apply_custom_headers(builder, &custom_headers, same_origin)
            })
            .await?;
        // A server that does not support session deletion answers 405; that is
        // not an error (the worker's cleanup tolerates it).
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        if !response.status().is_success() {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                format!("HTTP {} on delete session", response.status()).into(),
            ));
        }
        Ok(())
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        check_custom_headers(&custom_headers)?;
        self.validate_bearer(auth_header.as_deref())?;
        let response = self
            .send_guarded(&uri, Method::GET, true, |builder, same_origin| {
                let mut builder = builder
                    .header(ACCEPT, format!("{EVENT_STREAM_MIME}, {JSON_MIME}"))
                    .header(HEADER_SESSION_ID, session_id.as_ref());
                if let Some(id) = &last_event_id {
                    builder = builder.header(HEADER_LAST_EVENT_ID, id.clone());
                }
                if let Some(token) = &auth_header
                    && same_origin
                {
                    builder = builder.bearer_auth(token);
                }
                apply_custom_headers(builder, &custom_headers, same_origin)
            })
            .await?;

        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                format!("HTTP {} on get stream", response.status()).into(),
            ));
        }
        match response.headers().get(CONTENT_TYPE) {
            Some(ct)
                if ct.as_bytes().starts_with(EVENT_STREAM_MIME.as_bytes())
                    || ct.as_bytes().starts_with(JSON_MIME.as_bytes()) => {}
            Some(ct) => {
                return Err(StreamableHttpError::UnexpectedContentType(Some(
                    String::from_utf8_lossy(ct.as_bytes()).to_string(),
                )));
            }
            None => return Err(StreamableHttpError::UnexpectedContentType(None)),
        }
        // Residual (M1): the long-lived SSE byte stream is framed incrementally by
        // rmcp, so no single pre-parse total-byte cap applies here (a legitimate
        // stream is unbounded over time). The per-result post-parse caps in the
        // adapter / resource verbs remain the defense-in-depth bound on what any
        // one event can deliver to the model.
        Ok(SseStream::from_byte_stream(response.bytes_stream()).boxed())
    }
}

/// The `WWW-Authenticate` header value as an owned string, if present + UTF-8.
fn www_authenticate(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Build the HTTPS-only destination policy for a remote MCP server from its
/// network capability: the allow/deny host lists gate the endpoint, https is the
/// only scheme, and the configured URL's port is admitted (default 443).
///
/// `MCP-Protocol-Version` is intentionally not reserved here (it is a header, not
/// a policy concern); see [`RESERVED_HEADERS`] for the transport-reserved set.
pub fn server_net_policy(
    allow_hosts: &[String],
    deny_hosts: &[String],
    port: u16,
) -> Result<NetPolicy, pocopine_agenkit_core::AgenkitError> {
    // Fail-closed (H3). The `NetworkCapability` contract is "empty `allow_hosts`
    // = deny all" (never an implicit allow-all). A `deny_hosts`-only config would
    // otherwise build a `NetPolicy` whose `host_allowed` permits *every* host
    // except the denied ones (allow-minus-deny), reopening egress to arbitrary
    // destinations. Reject it: a remote MCP server must name its destinations in
    // `allow_hosts`. (`allow_hosts` + `deny_hosts` together are already mutually
    // exclusive in `NetPolicy::new`, so a deny list is only ever reachable with an
    // empty allow list — exactly this fail-open case — making `deny_hosts`
    // unsupported for MCP HTTP servers. Stdio already rejects any host list.)
    if allow_hosts.is_empty() && !deny_hosts.is_empty() {
        return Err(pocopine_agenkit_core::AgenkitError::config(
            "remote mcp server network capability sets deny_hosts but no allow_hosts; \
             an empty allow_hosts denies all (the deny list would be fail-open) — \
             specify an explicit allow_hosts allowlist instead",
        ));
    }
    let policy = NetPolicy::new(allow_hosts.to_vec(), deny_hosts.to_vec())?
        .with_schemes(["https"])
        .with_ports([port, 443]);
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::network::ssrf::Resolve;
    use pocopine_agenkit::server::BoxFuture;
    use std::net::IpAddr;

    struct MockResolver(Vec<IpAddr>);
    impl Resolve for MockResolver {
        fn lookup(
            &self,
            _host: &str,
        ) -> BoxFuture<'_, crate::tools::network::NetResult<Vec<IpAddr>>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    /// A resolver that must never be consulted: the bearer-audience tests reject
    /// the credential *before* any DNS, so a lookup here is a test failure.
    struct PanicResolver;
    impl Resolve for PanicResolver {
        fn lookup(
            &self,
            _host: &str,
        ) -> BoxFuture<'_, crate::tools::network::NetResult<Vec<IpAddr>>> {
            Box::pin(async { panic!("resolver must not run: credential rejected before send") })
        }
    }

    fn jwt_with_aud(aud: &str) -> String {
        use pocopine_codec::base64url_encode;
        let header = base64url_encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = base64url_encode(serde_json::json!({ "aud": aud }).to_string().as_bytes());
        format!("{header}.{payload}.sig")
    }

    fn client(policy: NetPolicy, resolved: &[&str]) -> GuardedHttpClient {
        let addrs = resolved.iter().map(|a| a.parse().unwrap()).collect();
        // These guard tests don't attach a bearer; the resource indicator is only
        // consulted by `validate_bearer`, which they never reach.
        GuardedHttpClient::new(
            policy,
            Arc::new(MockResolver(addrs)),
            "https://mcp.example.com/mcp".to_string(),
        )
    }

    #[tokio::test]
    async fn guard_pins_allowlisted_https_host() {
        let policy = server_net_policy(&["mcp.example.com".to_string()], &[], 443).unwrap();
        let target = client(policy, &["93.184.216.34"])
            .guard("https://mcp.example.com/mcp")
            .await
            .unwrap();
        assert_eq!(target.host(), "mcp.example.com");
    }

    #[tokio::test]
    async fn guard_blocks_http_scheme() {
        let policy = server_net_policy(&["mcp.example.com".to_string()], &[], 443).unwrap();
        let err = client(policy, &["93.184.216.34"])
            .guard("http://mcp.example.com/mcp")
            .await
            .unwrap_err();
        assert!(matches!(err, HttpTransportError::Blocked(_)));
    }

    #[tokio::test]
    async fn guard_blocks_non_allowlisted_host() {
        let policy = server_net_policy(&["mcp.example.com".to_string()], &[], 443).unwrap();
        let err = client(policy, &["93.184.216.34"])
            .guard("https://evil.example.com/mcp")
            .await
            .unwrap_err();
        assert!(matches!(err, HttpTransportError::Blocked(_)));
    }

    #[tokio::test]
    async fn guard_blocks_metadata_ip_literal() {
        // A server-supplied discovery URL pointing at the cloud metadata IP (T6).
        let policy = server_net_policy(&[], &[], 443)
            .unwrap()
            .with_ports([443, 80]);
        // The deny-list catches the literal before any allowlist consideration.
        let err = client(policy, &[])
            .guard("https://169.254.169.254/latest/meta-data/")
            .await
            .unwrap_err();
        assert!(matches!(err, HttpTransportError::Blocked(_)));
    }

    #[tokio::test]
    async fn guard_blocks_allowlisted_host_that_resolves_to_private_ip() {
        // Host is allowlisted, but DNS (rebinding / a poisoned redirect target)
        // resolves to a private address — the resolved-IP deny-check fires.
        let policy = server_net_policy(&["mcp.example.com".to_string()], &[], 443).unwrap();
        let err = client(policy, &["169.254.169.254"])
            .guard("https://mcp.example.com/mcp")
            .await
            .unwrap_err();
        assert!(matches!(err, HttpTransportError::Blocked(_)));
        // The refusal must not leak the resolved internal IP.
        let HttpTransportError::Blocked(msg) = err else {
            unreachable!()
        };
        assert!(!msg.contains("169.254.169.254"));
    }

    #[test]
    fn reserved_custom_headers_are_rejected() {
        let mut headers = HashMap::new();
        headers.insert(
            HeaderName::from_static("mcp-session-id"),
            HeaderValue::from_static("forged"),
        );
        assert!(check_custom_headers(&headers).is_err());

        // Authorization + MCP-Protocol-Version are allowed through.
        let mut allowed = HashMap::new();
        allowed.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer x"),
        );
        allowed.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static("2025-11-25"),
        );
        assert!(check_custom_headers(&allowed).is_ok());
    }

    #[test]
    fn sensitive_headers_are_dropped_on_cross_origin_hops() {
        // The per-server secret rides in the `Authorization` custom header
        // (connect_http). A cross-origin redirect must NOT replay it; a
        // non-sensitive header (MCP-Protocol-Version) still rides along.
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer server-secret"),
        );
        headers.insert(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("session=abc"),
        );
        headers.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static("2025-11-25"),
        );

        // Same origin (the original endpoint): the credentials are sent.
        let same = apply_custom_headers(client.get("https://a.example.com/mcp"), &headers, true)
            .build()
            .unwrap();
        assert!(same.headers().contains_key(reqwest::header::AUTHORIZATION));
        assert!(same.headers().contains_key(reqwest::header::COOKIE));
        assert!(same.headers().contains_key("mcp-protocol-version"));

        // Cross origin (a 302 to a different allowlisted host): the secret
        // Authorization + Cookie are stripped; the non-sensitive header stays.
        let cross = apply_custom_headers(client.get("https://b.example.com/mcp"), &headers, false)
            .build()
            .unwrap();
        assert!(!cross.headers().contains_key(reqwest::header::AUTHORIZATION));
        assert!(!cross.headers().contains_key(reqwest::header::COOKIE));
        assert!(cross.headers().contains_key("mcp-protocol-version"));
    }

    #[tokio::test]
    async fn target_origin_distinguishes_allowlisted_siblings() {
        // Two allowlisted hosts (and a different port) are distinct origins, so a
        // redirect between them is treated as cross-origin (credentials dropped).
        let policy = server_net_policy(
            &["a.example.com".to_string(), "b.example.com".to_string()],
            &[],
            443,
        )
        .unwrap();
        let c = client(policy, &["93.184.216.34"]);
        let a = c.guard("https://a.example.com/mcp").await.unwrap();
        let b = c.guard("https://b.example.com/mcp").await.unwrap();
        assert_eq!(target_origin(&a), target_origin(&a));
        assert_ne!(
            target_origin(&a),
            target_origin(&b),
            "a sibling allowlisted host is a different origin"
        );
    }

    #[tokio::test]
    async fn post_message_rejects_foreign_audience_bearer_before_sending() {
        // H4/T7: the rmcp worker's dynamic `auth_header` bearer is audience-
        // validated before it is attached. A JWT minted for a DIFFERENT server is
        // refused before any DNS / request — the (panicking) resolver is never hit.
        let policy = server_net_policy(&["mcp.example.com".to_string()], &[], 443).unwrap();
        let client = GuardedHttpClient::new(
            policy,
            Arc::new(PanicResolver),
            "https://mcp.example.com/mcp".to_string(),
        );
        let foreign = jwt_with_aud("https://evil.example.com/mcp");
        let message: ClientJsonRpcMessage = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        let err = client
            .post_message(
                "https://mcp.example.com/mcp".into(),
                message,
                None,
                Some(foreign),
                HashMap::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            StreamableHttpError::Client(HttpTransportError::CredentialRejected(_))
        ));
    }

    #[tokio::test]
    async fn matching_audience_bearer_passes_validation() {
        // A bearer whose audience matches the endpoint's resource indicator is
        // accepted by the validation gate (it then proceeds to DNS — exercised
        // elsewhere). Validate the gate directly via the shared oauth helper.
        let good = jwt_with_aud("https://mcp.example.com/mcp");
        assert!(
            crate::tools::mcp::oauth::validate_bearer_credential(
                &good,
                "https://mcp.example.com/mcp",
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn deny_only_network_config_is_rejected_fail_closed() {
        // H3: a NetworkCapability with deny_hosts but empty allow_hosts must NOT
        // build an allow-minus-deny policy (which permits every host except the
        // denied ones). The contract is "empty allow_hosts = deny all", so this
        // fail-open config is rejected outright.
        let err = server_net_policy(&[], &["evil.example.com".to_string()], 443).unwrap_err();
        assert_eq!(err.kind(), "config");

        // The fail-closed contract still works: an explicit allowlist is fine,
        // and a fully-empty capability (deny all) builds a policy that admits no
        // host.
        assert!(server_net_policy(&["mcp.example.com".to_string()], &[], 443).is_ok());
        let empty = server_net_policy(&[], &[], 443).unwrap();
        assert!(!empty.host_allowed("anything.example.com"));
    }

    #[tokio::test]
    async fn read_body_capped_rejects_oversized_body_before_full_buffering() {
        // M1: a body past the cap is refused (fail-closed) rather than allocated
        // and parsed whole. A truthful Content-Length over the cap is rejected up
        // front; a chunked/unknown-length body trips the streaming guard.
        let cap = 1024;
        let big = vec![b'A'; cap * 8];
        let http_resp = http::Response::builder()
            .status(200)
            .body(reqwest::Body::from(big))
            .unwrap();
        let resp = reqwest::Response::from(http_resp);
        let err = read_body_capped(resp, cap).await.unwrap_err();
        assert!(
            matches!(err, HttpTransportError::BodyTooLarge { limit } if limit == cap),
            "an oversized body must be rejected as BodyTooLarge, got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_body_capped_passes_a_body_within_the_cap() {
        let cap = 1024;
        let body = b"a small jsonrpc response".to_vec();
        let http_resp = http::Response::builder()
            .status(200)
            .body(reqwest::Body::from(body.clone()))
            .unwrap();
        let resp = reqwest::Response::from(http_resp);
        let out = read_body_capped(resp, cap).await.unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn extract_scope_handles_quoted_and_bare() {
        assert_eq!(
            extract_scope(r#"Bearer error="insufficient_scope", scope="a b""#),
            Some("a b".to_string())
        );
        assert_eq!(
            extract_scope("Bearer scope=read:data, realm=x"),
            Some("read:data".to_string())
        );
        assert_eq!(extract_scope("Bearer realm=x"), None);
    }
}
