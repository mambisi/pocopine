#![cfg(not(target_arch = "wasm32"))]
//! OAuth flow (code exchange + refresh) driven against a `wiremock` token
//! endpoint — no network, no real credentials.

use pocopine_auth_oauth::{OAuthConfig, complete_authorization, refresh};
use pocopine_crypto::SecretString;
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

#[tokio::test]
async fn complete_authorization_exchanges_the_code_with_pkce() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=auth-code-xyz"))
        .and(body_string_contains("code_verifier=the-verifier"))
        .and(body_string_contains("client_id=client-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "expires_in": 3600,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    let token = complete_authorization(
        &config(&server),
        "auth-code-xyz",
        &SecretString::new("the-verifier"),
    )
    .await
    .unwrap();

    assert_eq!(token.access_token.expose(), "access-1");
    assert_eq!(token.refresh_token.as_ref().unwrap().expose(), "refresh-1");
    assert!(token.expires_at.is_some());
    assert!(!token.is_expired(std::time::Duration::from_secs(60)));
}

#[tokio::test]
async fn refresh_exchanges_the_refresh_token_and_retains_an_omitted_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-2",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let token = refresh(&config(&server), &SecretString::new("refresh-1"))
        .await
        .unwrap();
    assert_eq!(token.access_token.expose(), "access-2");
    // The provider omitted a new refresh token → the old one is retained.
    assert_eq!(token.refresh_token.as_ref().unwrap().expose(), "refresh-1");
}

#[tokio::test]
async fn token_endpoint_error_does_not_leak_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "code expired; secret was sk-LEAK"
        })))
        .mount(&server)
        .await;

    let err = refresh(&config(&server), &SecretString::new("refresh-1"))
        .await
        .unwrap_err();
    let rendered = err.to_string();
    assert!(rendered.contains("400"));
    assert!(!rendered.contains("LEAK"), "leaked token body: {rendered}");
}
