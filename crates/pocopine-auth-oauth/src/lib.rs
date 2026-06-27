//! OAuth 2.0 Authorization Code + PKCE — the flow mechanism, provider-neutral.
//!
//! The same flow drives two jobs:
//! - **login** ("Sign in with X") — the token proves *identity*; an adapter
//!   turns it into an `AuthUser`/`Principal`;
//! - **a provider credential** — the token authenticates an *outbound* API call;
//!   `pocopine-agenkit`'s `OAuthCredentials` adapter turns it into a `Bearer`.
//!
//! This crate ships only the mechanism and the [`OAuthTokenStore`] contract.
//! Pocopine mints the PKCE verifier/challenge + CSRF `state`, exchanges the
//! callback `code` for tokens, and refreshes them — but **never stores them**.
//! The app persists tokens (the §D10 no-app-secret-storage rule); the store is
//! keyed on an opaque `subject` string (a user id, a tenant id), so this crate
//! depends on no identity type.
//!
//! ```text
//!   begin_authorization ──▶ redirect user ──▶ provider login ──▶ ?code&state
//!         │ (stash verifier+state)                                    │
//!         ▼                                                           ▼
//!   app stores {verifier,state} ◀──── compare state, complete_authorization(code,verifier)
//!                                                                     │
//!                                                       OAuthToken ◀──┘  (app saves it)
//! ```
//!
//! Provider-specific endpoints (`authorize_url`/`token_url`/`client_id`) are
//! configured by the app — pocopine does not hardcode third-party OAuth URLs.
//! Host-only: the lib is empty on wasm32.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use pocopine_crypto::SecretString;
use serde::Deserialize;

/// An OAuth flow error. Carries only stable, redacted detail — never a token
/// endpoint's response body (which can echo a code or secret), so it is safe to
/// log or surface as a kind (§D10).
#[derive(Debug)]
pub enum OAuthError {
    /// The token endpoint failed (network error, or a non-2xx status). The
    /// message holds only the status/cause, never the body.
    Endpoint(String),
    /// A configuration or environment problem (e.g. no secure RNG available).
    Config(String),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OAuthError::Endpoint(m) => write!(f, "oauth token endpoint: {m}"),
            OAuthError::Config(m) => write!(f, "oauth config: {m}"),
        }
    }
}

impl std::error::Error for OAuthError {}

/// The result type for OAuth operations.
pub type OAuthResult<T> = Result<T, OAuthError>;

/// A provider's OAuth endpoints and client registration.
#[derive(Clone, Debug)]
pub struct OAuthConfig {
    /// The authorization endpoint the user is redirected to.
    pub authorize_url: String,
    /// The token endpoint where `code` is exchanged and tokens are refreshed.
    pub token_url: String,
    /// The registered client id.
    pub client_id: String,
    /// The client secret, for a *confidential* client. Public clients (the PKCE
    /// case) omit it.
    pub client_secret: Option<SecretString>,
    /// The redirect URI registered with the provider (your callback route).
    pub redirect_uri: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
}

impl OAuthConfig {
    /// A public-client config (PKCE, no client secret).
    pub fn public(
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
        client_id: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            client_id: client_id.into(),
            client_secret: None,
            redirect_uri: redirect_uri.into(),
            scopes: Vec::new(),
        }
    }

    /// Add a client secret (confidential client).
    pub fn with_client_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = Some(SecretString::new(secret));
        self
    }

    /// Set the requested scopes.
    pub fn with_scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }
}

/// OAuth tokens for one subject+provider. The **app** persists these; pocopine
/// refreshes them. Secrets are redacted ([`SecretString`]); `expires_at` is
/// absolute wall-clock so it survives a save/load round-trip.
#[derive(Clone, Debug)]
pub struct OAuthToken {
    /// The access token sent as `Authorization: Bearer`.
    pub access_token: SecretString,
    /// The refresh token, when the provider issues one.
    pub refresh_token: Option<SecretString>,
    /// Absolute expiry, when known (`expires_in` resolved against the wall clock
    /// at issue time).
    pub expires_at: Option<SystemTime>,
}

impl OAuthToken {
    /// Whether the access token is expired (or expires within `margin`). A token
    /// with no known expiry is treated as still valid.
    pub fn is_expired(&self, margin: Duration) -> bool {
        match self.expires_at {
            Some(at) => SystemTime::now() + margin >= at,
            None => false,
        }
    }
}

/// A started authorization: where to send the user, plus the PKCE verifier and
/// CSRF `state` the app must stash (keyed to the user's session) until the
/// callback.
#[derive(Debug)]
pub struct Authorization {
    /// Redirect the user here to log in and consent.
    pub authorize_url: String,
    /// Keep this server-side; it's required to complete the exchange.
    pub pkce_verifier: SecretString,
    /// Compare this against the `state` query param on the callback (CSRF).
    pub state: String,
}

/// Begin the Authorization Code + PKCE flow: mint a PKCE verifier/challenge
/// (`S256`) and a CSRF `state`, and build the authorize URL. The app redirects
/// the user to [`Authorization::authorize_url`] and stashes the verifier + state.
pub fn begin_authorization(config: &OAuthConfig) -> OAuthResult<Authorization> {
    let verifier = random_url_token(32)?;
    let challenge = pocopine_codec::base64url_encode(&pocopine_crypto::sha256(verifier.as_bytes()));
    let state = random_url_token(16)?;

    let mut params = pocopine_codec::QueryParams::new()
        .pair("response_type", "code")
        .pair("client_id", &config.client_id)
        .pair("redirect_uri", &config.redirect_uri)
        .pair("code_challenge", &challenge)
        .pair("code_challenge_method", "S256")
        .pair("state", &state);
    if !config.scopes.is_empty() {
        params = params.pair("scope", config.scopes.join(" "));
    }
    let mut url = config.authorize_url.clone();
    params.append_to(&mut url);

    Ok(Authorization {
        authorize_url: url,
        pkce_verifier: SecretString::new(verifier),
        state,
    })
}

/// Complete the flow at your callback: exchange the `code` (with the stashed
/// `pkce_verifier`) for tokens at the token endpoint. Verify `state` *before*
/// calling this.
pub async fn complete_authorization(
    config: &OAuthConfig,
    code: &str,
    pkce_verifier: &SecretString,
) -> OAuthResult<OAuthToken> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_uri.clone()),
        ("client_id", config.client_id.clone()),
        ("code_verifier", pkce_verifier.expose().to_string()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.expose().to_string()));
    }
    post_token(config, &form).await
}

/// Refresh an access token using its refresh token.
pub async fn refresh(
    config: &OAuthConfig,
    refresh_token: &SecretString,
) -> OAuthResult<OAuthToken> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.expose().to_string()),
        ("client_id", config.client_id.clone()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.expose().to_string()));
    }
    let mut token = post_token(config, &form).await?;
    // Some providers omit the refresh token on refresh — keep the existing one.
    if token.refresh_token.is_none() {
        token.refresh_token = Some(refresh_token.clone());
    }
    Ok(token)
}

/// POST a form to the token endpoint and parse the token response. A non-2xx is
/// collapsed to a stable error — the body (which can echo a code/secret) is never
/// surfaced (§D10).
async fn post_token(config: &OAuthConfig, form: &[(&str, String)]) -> OAuthResult<OAuthToken> {
    // Negotiate a JSON token response. Spec-compliant providers already return
    // JSON, but GitHub's token endpoint defaults to `application/x-www-form-
    // urlencoded` and only returns JSON when `Accept: application/json` is sent —
    // without this, `.json()` below fails with "error decoding response body".
    let response = reqwest::Client::new()
        .post(&config.token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|err| OAuthError::Endpoint(format!("request failed: {err}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(OAuthError::Endpoint(format!(
            "returned {}",
            status.as_u16()
        )));
    }
    let body: TokenResponse = response
        .json()
        .await
        .map_err(|err| OAuthError::Endpoint(format!("invalid response: {err}")))?;
    Ok(body.into_token())
}

/// The RFC 6749 token-endpoint success response (the fields we use).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl TokenResponse {
    fn into_token(self) -> OAuthToken {
        OAuthToken {
            access_token: SecretString::new(self.access_token),
            refresh_token: self.refresh_token.map(SecretString::new),
            expires_at: self
                .expires_in
                .map(|secs| SystemTime::now() + Duration::from_secs(secs)),
        }
    }
}

/// Fill `len` bytes from the OS CSPRNG and encode as base64url (no padding) — a
/// URL-safe random token for a PKCE verifier or CSRF state.
fn random_url_token(len: usize) -> OAuthResult<String> {
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes)
        .map_err(|err| OAuthError::Config(format!("no secure RNG available: {err}")))?;
    Ok(pocopine_codec::base64url_encode(&bytes))
}

/// The object-safe return shape for the async [`OAuthTokenStore`] methods.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = OAuthResult<T>> + Send + 'a>>;

/// The app's per-subject OAuth token storage. Pocopine reads the token to
/// authenticate and writes it back after a refresh; the tokens live in the app's
/// store (a DB, a secrets manager), never in pocopine. `subject` is an opaque
/// key the caller chooses — typically a stable user id.
pub trait OAuthTokenStore: Send + Sync + 'static {
    /// Load the stored token for `subject`, if any.
    fn load<'a>(&'a self, subject: &'a str) -> StoreFuture<'a, Option<OAuthToken>>;

    /// Persist a (refreshed) token for `subject`.
    fn save<'a>(&'a self, subject: &'a str, token: OAuthToken) -> StoreFuture<'a, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuthConfig {
        OAuthConfig::public(
            "https://auth.example.com/authorize",
            "https://auth.example.com/token",
            "client-123",
            "https://app.example.com/callback",
        )
        .with_scopes(["read", "write"])
    }

    #[test]
    fn begin_authorization_builds_a_pkce_challenge_url() {
        let auth = begin_authorization(&config()).unwrap();
        assert!(
            auth.authorize_url
                .starts_with("https://auth.example.com/authorize?")
        );
        assert!(auth.authorize_url.contains("response_type=code"));
        assert!(auth.authorize_url.contains("client_id=client-123"));
        assert!(auth.authorize_url.contains("code_challenge_method=S256"));
        assert!(auth.authorize_url.contains("scope=read%20write"));
        // The challenge in the URL is base64url(sha256(verifier)).
        let expected = pocopine_codec::base64url_encode(&pocopine_crypto::sha256(
            auth.pkce_verifier.expose().as_bytes(),
        ));
        assert!(
            auth.authorize_url
                .contains(&format!("code_challenge={expected}"))
        );
        assert!(!auth.pkce_verifier.expose().is_empty());
        assert!(!auth.state.is_empty());
    }

    #[test]
    fn begin_authorization_appends_query_before_fragment() {
        let cfg = OAuthConfig::public(
            "https://auth.example.com/authorize?prompt=select#login",
            "https://auth.example.com/token",
            "client 123",
            "https://app.example.com/callback?from=oauth",
        );

        let auth = begin_authorization(&cfg).unwrap();

        assert!(
            auth.authorize_url
                .starts_with("https://auth.example.com/authorize?prompt=select&response_type=code")
        );
        assert!(auth.authorize_url.ends_with("#login"));
        assert!(auth.authorize_url.contains("client_id=client%201"));
        assert!(
            auth.authorize_url
                .contains("redirect_uri=https%3A%2F%2Fapp.example.com%2Fcallback%3Ffrom%3Doauth")
        );
    }

    #[test]
    fn two_authorizations_use_distinct_verifiers_and_state() {
        let cfg = config();
        let a = begin_authorization(&cfg).unwrap();
        let b = begin_authorization(&cfg).unwrap();
        assert_ne!(a.pkce_verifier.expose(), b.pkce_verifier.expose());
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn token_is_expired_respects_margin_and_unknown_expiry() {
        let soon = OAuthToken {
            access_token: SecretString::new("a"),
            refresh_token: None,
            expires_at: Some(SystemTime::now() + Duration::from_secs(30)),
        };
        assert!(soon.is_expired(Duration::from_secs(60)));
        assert!(!soon.is_expired(Duration::from_secs(5)));

        let no_expiry = OAuthToken {
            access_token: SecretString::new("a"),
            refresh_token: None,
            expires_at: None,
        };
        assert!(!no_expiry.is_expired(Duration::from_secs(3600)));
    }
}
