//! OAuth 2.1 helpers for the remote (HTTP) MCP transport (D10 / T7).
//!
//! MCP servers reached over Streamable HTTP are OAuth 2.1 Resource Servers. The
//! spec (2025-11-25) requires the client to be **RFC 8707-aware**: every
//! authorization and token request carries a `resource` indicator naming the
//! exact server, and the client **audience-validates** the access token it
//! receives so a token minted for one server can never be replayed against
//! another (the confused-deputy / token-passthrough guard, T7). The host's own
//! credentials are never injected downstream and an inbound caller token is
//! never forwarded upstream — the only `Authorization` we send is the
//! per-server secret/OAuth token resolved at the transport edge (D6/D10).
//!
//! **Scope note (v1):** this module ships the *security-load-bearing* pieces of
//! the flow — the RFC 8707 resource indicator, the request-parameter shapes
//! (PKCE + `resource` on both the authorization and token request), and token
//! **audience validation**. The *interactive execution* of the flow (RFC 9728
//! protected-resource-metadata discovery, RFC 8414 AS-metadata discovery, the
//! browser-redirect authorization-code round trip, and Dynamic Client
//! Registration) is **deferred** — it needs a real authorization server + a
//! redirect listener.
//! What lands here is what the deferred executor will call, and what the static
//! `Authorization`-header (secret-handle) path is validated against today.

use serde_json::Value;
use url::Url;

use pocopine_agenkit_core::AgenkitError;
use pocopine_codec::base64url_decode;

/// The reason an OAuth credential was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthError {
    /// The access token is not a well-formed JWT (`header.payload.signature`
    /// with a base64url-decodable, JSON payload).
    MalformedToken,
    /// The token carries no `aud` (audience) claim, so it cannot be confirmed to
    /// have been minted for this server — refused (fail closed, T7).
    MissingAudience,
    /// The token's audience does not name this server's resource indicator: a
    /// token issued for a *different* audience (a passthrough / confused-deputy
    /// attempt) — refused (T7).
    AudienceMismatch,
}

impl OAuthError {
    /// Stable snake_case code (asserted in tests / surfaced in traces).
    pub fn as_str(self) -> &'static str {
        match self {
            OAuthError::MalformedToken => "malformed_token",
            OAuthError::MissingAudience => "missing_audience",
            OAuthError::AudienceMismatch => "audience_mismatch",
        }
    }
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = match self {
            OAuthError::MalformedToken => "access token is not a well-formed JWT",
            OAuthError::MissingAudience => "access token has no audience claim",
            OAuthError::AudienceMismatch => "access token audience does not match this server",
        };
        write!(f, "oauth: {detail}")
    }
}

impl std::error::Error for OAuthError {}

impl From<OAuthError> for AgenkitError {
    fn from(err: OAuthError) -> Self {
        // An audience/passthrough failure is a policy denial, not a config error.
        AgenkitError::tool_policy(err.to_string())
    }
}

/// The RFC 8707 **resource indicator** for an MCP server URL: the canonical
/// `scheme://host[:port]/path` form, lowercased scheme + host, fragment and
/// query stripped, no trailing slash (so `https://Mcp.Example.com/api/` and
/// `https://mcp.example.com/api` produce the same indicator). This is the value
/// sent as `resource` on every authorization + token request and the audience a
/// returned access token must name.
pub fn resource_indicator(url: &Url) -> String {
    let scheme = url.scheme().to_ascii_lowercase();
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let mut indicator = format!("{scheme}://{host}");
    // Keep an explicit non-default port; the WHATWG parser already elides the
    // default port for the scheme.
    if let Some(port) = url.port() {
        indicator.push(':');
        indicator.push_str(&port.to_string());
    }
    let path = url.path();
    if !path.is_empty() && path != "/" {
        indicator.push_str(path.trim_end_matches('/'));
    }
    indicator
}

/// Normalize a resource string for comparison (lowercase, trailing slash
/// stripped). Applied to both the expected indicator and a token's audience so
/// a cosmetic difference never widens or narrows the match.
fn normalize_resource(resource: &str) -> String {
    resource.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// The PKCE + RFC 8707 parameters for the **authorization request** (the browser
/// redirect to the AS's authorization endpoint). `resource` binds the eventual
/// token to this server (RFC 8707); `code_challenge` is the S256 PKCE challenge.
///
/// Returned as ordered key/value pairs for the deferred flow executor to encode
/// into the authorization URL's query.
pub fn authorization_request_params(
    resource: &str,
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    scopes: &[String],
) -> Vec<(String, String)> {
    let mut params = vec![
        ("response_type".to_string(), "code".to_string()),
        ("client_id".to_string(), client_id.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("code_challenge".to_string(), code_challenge.to_string()),
        ("code_challenge_method".to_string(), "S256".to_string()),
        // RFC 8707: name the exact protected resource on every request.
        ("resource".to_string(), resource.to_string()),
    ];
    if !scopes.is_empty() {
        params.push(("scope".to_string(), scopes.join(" ")));
    }
    params
}

/// The PKCE + RFC 8707 parameters for the **token request** (the POST to the
/// AS's token endpoint that exchanges the authorization code). `resource` is
/// repeated here (RFC 8707 requires it on the token request too) and
/// `code_verifier` completes the PKCE proof.
pub fn token_request_params(
    resource: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Vec<(String, String)> {
    vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("client_id".to_string(), client_id.to_string()),
        ("code_verifier".to_string(), code_verifier.to_string()),
        // RFC 8707: the token request restates the resource so the AS mints a
        // token whose `aud` names exactly this server.
        ("resource".to_string(), resource.to_string()),
    ]
}

/// Extract the `aud` (audience) claim from a JWT access token's payload.
///
/// The signature is **not** verified here: signature verification is the
/// resource/authorization server's job and part of the deferred full flow. This
/// is the *binding* check — does the token name this server? — which is what
/// defeats token passthrough (T7) at the transport edge regardless of signature.
fn decode_jwt_audiences(token: &str) -> Result<Vec<String>, OAuthError> {
    let mut parts = token.split('.');
    let _header = parts.next().ok_or(OAuthError::MalformedToken)?;
    let payload = parts.next().ok_or(OAuthError::MalformedToken)?;
    // A JWT has exactly three segments; reject anything else (an opaque token is
    // not audience-checkable here and must go through the deferred introspection
    // path rather than be trusted).
    let signature = parts.next().ok_or(OAuthError::MalformedToken)?;
    if signature.is_empty() || parts.next().is_some() {
        return Err(OAuthError::MalformedToken);
    }
    let bytes = base64url_decode(payload).map_err(|_| OAuthError::MalformedToken)?;
    let claims: Value = serde_json::from_slice(&bytes).map_err(|_| OAuthError::MalformedToken)?;
    match claims.get("aud") {
        Some(Value::String(single)) => Ok(vec![single.clone()]),
        Some(Value::Array(many)) => {
            let auds: Vec<String> = many
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            if auds.is_empty() {
                Err(OAuthError::MissingAudience)
            } else {
                Ok(auds)
            }
        }
        _ => Err(OAuthError::MissingAudience),
    }
}

/// Validate that a JWT access token's audience names `expected_resource` (the
/// server's RFC 8707 resource indicator). This is the inbound-token audience
/// check (D10) and the no-passthrough guard (T7): a token issued for any other
/// audience is refused before it is ever attached to an upstream request.
pub fn validate_audience(access_token: &str, expected_resource: &str) -> Result<(), OAuthError> {
    let audiences = decode_jwt_audiences(access_token)?;
    let expected = normalize_resource(expected_resource);
    if audiences
        .iter()
        .any(|aud| normalize_resource(aud) == expected)
    {
        Ok(())
    } else {
        Err(OAuthError::AudienceMismatch)
    }
}

/// Gate a configured/acquired bearer (or `Authorization`) credential **on the
/// send path** before it is attached to an upstream MCP request (D10/T7).
///
/// `token` is the bearer value (the part after `Bearer `, or the raw token);
/// `expected_resource` is the server's RFC 8707 [`resource_indicator`].
///
/// - A **JWT** access token must name `expected_resource` in its `aud` claim, or
///   it is refused ([`AudienceMismatch`](OAuthError::AudienceMismatch)) — the
///   confused-deputy / token-passthrough guard. A **JWT with no `aud`** is
///   refused ([`MissingAudience`](OAuthError::MissingAudience)): it cannot be
///   confirmed to have been minted for this server. Both fail closed regardless
///   of binding.
/// - An **opaque** (non-JWT) token cannot be locally audience-validated. It is
///   accepted only when `operator_bound` is `true` — i.e. the operator explicitly
///   configured this exact credential for this exact server (a per-server
///   `Authorization` secret header in [`McpServerConfig`](super::config::McpServerConfig)).
///   A token obtained through the (deferred) interactive flow is **not**
///   operator-bound and fails closed ([`MalformedToken`](OAuthError::MalformedToken)).
pub fn validate_bearer_credential(
    token: &str,
    expected_resource: &str,
    operator_bound: bool,
) -> Result<(), OAuthError> {
    match validate_audience(token, expected_resource) {
        Ok(()) => Ok(()),
        // `MalformedToken` means "not a parseable JWT" → an opaque token. Accept
        // it only when the operator explicitly bound it to this destination.
        Err(OAuthError::MalformedToken) if operator_bound => Ok(()),
        // A JWT with a wrong/absent audience, or an unbound opaque token: refused.
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_codec::base64url_encode;

    fn jwt_with_aud(aud: Value) -> String {
        let header = base64url_encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = base64url_encode(
            serde_json::json!({ "aud": aud, "sub": "x" })
                .to_string()
                .as_bytes(),
        );
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn resource_indicator_canonicalizes() {
        let url = Url::parse("https://Mcp.Example.com/api/").unwrap();
        assert_eq!(resource_indicator(&url), "https://mcp.example.com/api");
        // Default port elided; explicit non-default port kept.
        let url = Url::parse("https://mcp.example.com:8443/mcp").unwrap();
        assert_eq!(resource_indicator(&url), "https://mcp.example.com:8443/mcp");
        let root = Url::parse("https://mcp.example.com/").unwrap();
        assert_eq!(resource_indicator(&root), "https://mcp.example.com");
    }

    #[test]
    fn authorization_and_token_requests_carry_resource() {
        let auth = authorization_request_params(
            "https://mcp.example.com/mcp",
            "client-1",
            "http://127.0.0.1:0/callback",
            "challenge",
            &["mcp:tools".to_string()],
        );
        assert!(
            auth.iter()
                .any(|(k, v)| k == "resource" && v == "https://mcp.example.com/mcp")
        );
        // PKCE is present (S256), never plain.
        assert!(
            auth.iter()
                .any(|(k, v)| k == "code_challenge_method" && v == "S256")
        );

        let token = token_request_params(
            "https://mcp.example.com/mcp",
            "client-1",
            "auth-code",
            "http://127.0.0.1:0/callback",
            "verifier",
        );
        assert!(
            token
                .iter()
                .any(|(k, v)| k == "resource" && v == "https://mcp.example.com/mcp")
        );
    }

    #[test]
    fn audience_validation_accepts_matching_string_and_array() {
        let resource = "https://mcp.example.com/mcp";
        let single = jwt_with_aud(Value::String(resource.to_string()));
        assert!(validate_audience(&single, resource).is_ok());

        // Trailing-slash difference must still match (normalized).
        let slashed = jwt_with_aud(Value::String("https://mcp.example.com/mcp/".to_string()));
        assert!(validate_audience(&slashed, resource).is_ok());

        let array = jwt_with_aud(serde_json::json!(["https://other.example.com", resource]));
        assert!(validate_audience(&array, resource).is_ok());
    }

    #[test]
    fn audience_validation_rejects_passthrough_token() {
        // A token minted for a DIFFERENT server (confused-deputy / passthrough).
        let foreign = jwt_with_aud(Value::String("https://evil.example.com/mcp".to_string()));
        assert_eq!(
            validate_audience(&foreign, "https://mcp.example.com/mcp").unwrap_err(),
            OAuthError::AudienceMismatch
        );
    }

    #[test]
    fn audience_validation_rejects_tokens_without_audience() {
        let header = base64url_encode(br#"{"alg":"RS256"}"#);
        let payload = base64url_encode(br#"{"sub":"x"}"#);
        let no_aud = format!("{header}.{payload}.sig");
        assert_eq!(
            validate_audience(&no_aud, "https://mcp.example.com/mcp").unwrap_err(),
            OAuthError::MissingAudience
        );
    }

    #[test]
    fn bearer_credential_validates_jwt_audience_regardless_of_binding() {
        let resource = "https://mcp.example.com/mcp";
        let good = jwt_with_aud(Value::String(resource.to_string()));
        assert!(validate_bearer_credential(&good, resource, true).is_ok());
        assert!(validate_bearer_credential(&good, resource, false).is_ok());

        // A JWT minted for a DIFFERENT server is refused even when operator-bound
        // (a confused-deputy / passthrough token configured against the wrong
        // server, T7).
        let foreign = jwt_with_aud(Value::String("https://evil.example.com/mcp".to_string()));
        assert_eq!(
            validate_bearer_credential(&foreign, resource, true).unwrap_err(),
            OAuthError::AudienceMismatch
        );
        assert_eq!(
            validate_bearer_credential(&foreign, resource, false).unwrap_err(),
            OAuthError::AudienceMismatch
        );
    }

    #[test]
    fn bearer_credential_jwt_without_audience_fails_closed() {
        let header = base64url_encode(br#"{"alg":"RS256"}"#);
        let payload = base64url_encode(br#"{"sub":"x"}"#);
        let no_aud = format!("{header}.{payload}.sig");
        assert_eq!(
            validate_bearer_credential(&no_aud, "https://mcp.example.com/mcp", true).unwrap_err(),
            OAuthError::MissingAudience
        );
    }

    #[test]
    fn bearer_credential_opaque_only_when_operator_bound() {
        // An opaque (non-JWT) token can't be audience-validated locally: it is
        // accepted only when the operator explicitly bound it to this server in
        // config, and fails closed otherwise.
        let opaque = "ghp_aVeryOpaqueTokenWithNoDots";
        assert!(validate_bearer_credential(opaque, "https://mcp.example.com/mcp", true).is_ok());
        assert_eq!(
            validate_bearer_credential(opaque, "https://mcp.example.com/mcp", false).unwrap_err(),
            OAuthError::MalformedToken
        );
    }

    #[test]
    fn audience_validation_rejects_malformed_tokens() {
        assert_eq!(
            validate_audience("not-a-jwt", "https://mcp.example.com/mcp").unwrap_err(),
            OAuthError::MalformedToken
        );
        // Two-segment token (missing signature) is not a JWT.
        assert_eq!(
            validate_audience("header.payload", "https://mcp.example.com").unwrap_err(),
            OAuthError::MalformedToken
        );
    }
}
