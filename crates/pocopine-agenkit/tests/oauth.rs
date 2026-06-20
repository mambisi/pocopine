#![cfg(not(target_arch = "wasm32"))]
//! The agenkit OAuth *adapter* (W6.3): `OAuthCredentials` resolves a principal's
//! stored token to a `Bearer`, refreshing on expiry. The neutral flow itself
//! (code exchange / refresh / PKCE) is tested in `pocopine-auth-oauth`; here we
//! test the bridge — principal → subject, refresh-and-persist, anonymous → none.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use pocopine_agenkit::server::{
    AuthUser, OAuthConfig, OAuthCredentials, OAuthToken, OAuthTokenStore, Principal,
    ProviderCredential, ProviderCredentials, SecretString, StoreFuture,
};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(server: &MockServer) -> OAuthConfig {
    OAuthConfig::public(
        "https://auth.example.com/authorize",
        format!("{}/token", server.uri()),
        "client-123",
        "https://app.example.com/callback",
    )
}

/// A per-subject token store backed by a shared cell so the test can inspect
/// what was persisted (and which subject it was keyed under) after a refresh.
#[derive(Clone)]
struct CellStore(Arc<Mutex<Option<(String, OAuthToken)>>>);

impl OAuthTokenStore for CellStore {
    fn load<'a>(&'a self, subject: &'a str) -> StoreFuture<'a, Option<OAuthToken>> {
        let entry = self.0.lock().unwrap().clone();
        Box::pin(async move { Ok(entry.and_then(|(s, t)| (s == subject).then_some(t))) })
    }

    fn save<'a>(&'a self, subject: &'a str, token: OAuthToken) -> StoreFuture<'a, ()> {
        *self.0.lock().unwrap() = Some((subject.to_string(), token));
        Box::pin(async move { Ok(()) })
    }
}

#[tokio::test]
async fn oauth_credentials_refresh_an_expired_token_and_persist_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access",
            "refresh_token": "fresh-refresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    // An already-expired token for user "alice", carrying a refresh token.
    let cell = Arc::new(Mutex::new(Some((
        "alice".to_string(),
        OAuthToken {
            access_token: SecretString::new("stale-access"),
            refresh_token: Some(SecretString::new("refresh-1")),
            expires_at: Some(SystemTime::now() - Duration::from_secs(10)),
        },
    ))));
    let creds = OAuthCredentials::new(config(&server), CellStore(cell.clone()));

    let principal = Principal::from_user(AuthUser::new("alice"));
    let resolved = creds.resolve("anthropic", &principal).await.unwrap();

    // The resolver refreshed and returned a Bearer with the NEW access token.
    match resolved {
        Some(ProviderCredential::Bearer(s)) => assert_eq!(s.expose(), "fresh-access"),
        other => panic!("expected a refreshed Bearer, got {other:?}"),
    }
    // ...and persisted the refreshed token, still keyed under "alice".
    let (subject, saved) = cell.lock().unwrap().clone().unwrap();
    assert_eq!(subject, "alice");
    assert_eq!(saved.access_token.expose(), "fresh-access");
    assert_eq!(
        saved.refresh_token.as_ref().unwrap().expose(),
        "fresh-refresh"
    );
}

#[tokio::test]
async fn oauth_credentials_resolve_none_for_anonymous_or_unstored() {
    let server = MockServer::start().await;

    // No stored token → None → the runtime uses the provider default.
    let empty = OAuthCredentials::new(config(&server), CellStore(Arc::new(Mutex::new(None))));
    let resolved = empty
        .resolve("anthropic", &Principal::from_user(AuthUser::new("bob")))
        .await
        .unwrap();
    assert!(resolved.is_none());

    // An anonymous caller has no subject → None (can't have a per-user token).
    let cell = Arc::new(Mutex::new(Some((
        "alice".to_string(),
        OAuthToken {
            access_token: SecretString::new("a"),
            refresh_token: None,
            expires_at: None,
        },
    ))));
    let creds = OAuthCredentials::new(config(&server), CellStore(cell));
    let resolved = creds
        .resolve("anthropic", &Principal::anonymous())
        .await
        .unwrap();
    assert!(resolved.is_none());
}

#[tokio::test]
async fn scoped_resolver_only_serves_its_own_provider() {
    let server = MockServer::start().await;
    // A valid token for "alice", obtained via the Anthropic OAuth flow.
    let cell = Arc::new(Mutex::new(Some((
        "alice".to_string(),
        OAuthToken {
            access_token: SecretString::new("anthropic-token"),
            refresh_token: None,
            expires_at: None,
        },
    ))));
    let creds = OAuthCredentials::new(config(&server), CellStore(cell)).for_provider("anthropic");
    let alice = Principal::from_user(AuthUser::new("alice"));

    // Its own provider → the Bearer resolves.
    match creds.resolve("anthropic", &alice).await.unwrap() {
        Some(ProviderCredential::Bearer(s)) => assert_eq!(s.expose(), "anthropic-token"),
        other => panic!("expected the scoped Bearer, got {other:?}"),
    }
    // Any other provider → None, so the Anthropic token is never sent elsewhere.
    assert!(creds.resolve("openai", &alice).await.unwrap().is_none());
}
