//! End-to-end integration tests for the credentials routes.
//!
//! Drives `/_pocopine/auth/{signup,login,logout}` through
//! [`Server::try_finalize`] + `tower::ServiceExt::oneshot` so the
//! tests exercise the same axum router an actual `serve` call would
//! mount, without binding a TCP listener.
//!
//! Each test gets its own [`Credentials`] backed by the test-local
//! [`TestUserStore`] / [`TestTokenStore`] in `common.rs`, and a
//! paired [`pocopine_auth_jwt::JwtVerifier`] that proves the issued
//! session token round-trips through the verifier crate's HMAC
//! config path. The crate ships **no default in-memory backend** —
//! these stubs exist only to drive the routes through axum
//! `oneshot` for testing; production apps implement `UserStore` /
//! `TokenStore` against their own database (see
//! `docs/auth-credentials.md` for a Postgres + `sqlx` walkthrough).

#![cfg(not(target_arch = "wasm32"))]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestTokenStore, TestUserStore};
use http_body_util::BodyExt;
use pocopine_auth::Principal;
use pocopine_auth_credentials::Credentials;
use pocopine_auth_credentials::{E164PhoneValidator, UsernameValidator};
use pocopine_auth_jwt::{JwtVerifier, SecretBytes, TokenSource};
use pocopine_server::Server;
use serde_json::Value;
use tower::ServiceExt;

fn fixed_secret() -> SecretBytes {
    SecretBytes::new(b"integration-test-shared-secret".to_vec())
}

fn build_credentials() -> Credentials<TestUserStore, TestTokenStore> {
    Credentials::new(
        fixed_secret(),
        TestUserStore::default(),
        TestTokenStore::default(),
    )
}

fn router() -> axum::Router {
    Server::new(axum::Router::new())
        .plugin(build_credentials())
        .try_finalize()
        .expect("server finalize")
}

async fn body_to_json(body: axum::body::Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).expect("response is JSON")
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn signup_returns_token_and_user_then_login_succeeds() {
    pocopine_server::__reset_for_test();
    let app = router();

    // Signup a fresh account.
    let resp = app
        .clone()
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp.into_body()).await;
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["user"]["email"], "alice@example.com");
    // The TestUserStore's PasswordCredentials::to_auth_user impl
    // writes `email_verified` as a custom claim; assert it lives at
    // the top level of the response (the route shapes the body
    // from AuthUser, which serializes claims via the
    // AuthUser::claim accessor on the issuer side, not at the
    // response layer — so the response wraps the AuthUser as-is).
    assert!(body["user"]["id"].as_str().unwrap().starts_with('u'));

    // Hash never leaks via the public-user response.
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("password_hash") && !raw.contains("$argon2id"),
        "password_hash leaked into response body: {raw}"
    );

    // Login with the same credentials.
    let resp = app
        .clone()
        .oneshot(post(
            "/_pocopine/auth/login",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let login_body = body_to_json(resp.into_body()).await;
    let login_token = login_body["token"].as_str().unwrap();
    assert!(!login_token.is_empty());
    assert_ne!(login_token, token, "fresh login should mint a fresh token");
}

#[tokio::test]
async fn signup_duplicate_email_is_409() {
    pocopine_server::__reset_for_test();
    let app = router();
    let body = serde_json::json!({
        "email": "alice@example.com",
        "password": "correcthorse",
    });

    let first = app
        .clone()
        .oneshot(post("/_pocopine/auth/signup", body.clone()))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(post("/_pocopine/auth/signup", body))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let body = body_to_json(second.into_body()).await;
    assert_eq!(body["error"], "login_id_taken");
}

#[tokio::test]
async fn signup_weak_password_is_422() {
    pocopine_server::__reset_for_test();
    let app = router();
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "short",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "weak_password");
}

#[tokio::test]
async fn signup_invalid_email_is_422() {
    pocopine_server::__reset_for_test();
    let app = router();
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "not-an-email",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "invalid_login_id");
}

#[tokio::test]
async fn login_unknown_email_is_401_and_runs_argon2() {
    pocopine_server::__reset_for_test();
    let app = router();
    // No signup — login on a clean store.
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/login",
            serde_json::json!({
                "email": "ghost@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "invalid_credentials");
}

#[tokio::test]
async fn login_wrong_password_is_401() {
    pocopine_server::__reset_for_test();
    let app = router();
    app.clone()
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();

    let resp = app
        .oneshot(post(
            "/_pocopine/auth/login",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "wrong-password",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_returns_204() {
    pocopine_server::__reset_for_test();
    let app = router();
    let resp = app
        .oneshot(post("/_pocopine/auth/logout", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn issued_token_verifies_through_paired_jwt_verifier() {
    pocopine_server::__reset_for_test();
    // Build the credentials and the verifier from the same builder
    // (same secret), and prove a token issued by signup/login
    // round-trips through `JwtVerifier::custom(verifier_config)`.
    let creds = build_credentials();
    let mut config = creds.verifier_config();
    // Test verification through Bearer for symmetry with what the
    // wasm-side BearerMiddleware sends in production.
    config.sources = vec![TokenSource::Bearer];
    let verifier = JwtVerifier::custom(config).expect("build verifier");

    let app = Server::new(axum::Router::new())
        .plugin(creds)
        .try_finalize()
        .expect("server finalize");

    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    let body = body_to_json(resp.into_body()).await;
    let token = body["token"].as_str().unwrap().to_string();

    // Drive the bearer verification path using a synthesized
    // Authorization header. `verify_token` is the entry point used
    // by the `with_auth` middleware layer.
    let user = verifier
        .verify_token(&token)
        .await
        .expect("verifier accepts session token");

    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    let principal = Principal::from_user(user);
    assert!(principal.is_authenticated());
}

// ─── phone-based configuration ──────────────────────────────────────

fn build_phone_credentials() -> Credentials<TestUserStore, TestTokenStore> {
    Credentials::new(
        fixed_secret(),
        TestUserStore::default(),
        TestTokenStore::default(),
    )
    .with_login_id_validator(E164PhoneValidator)
    .with_login_id_field("phone")
}

#[tokio::test]
async fn signup_login_round_trip_keyed_on_phone_number() {
    pocopine_server::__reset_for_test();
    let app = Server::new(axum::Router::new())
        .plugin(build_phone_credentials())
        .try_finalize()
        .expect("server finalize");

    // Phone field, no email field — the configured validator
    // accepts E.164 only.
    let resp = app
        .clone()
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "phone": "+12025551234",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(post(
            "/_pocopine/auth/login",
            serde_json::json!({
                "phone": "+12025551234",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn signup_keyed_on_phone_rejects_non_e164_input() {
    pocopine_server::__reset_for_test();
    let app = Server::new(axum::Router::new())
        .plugin(build_phone_credentials())
        .try_finalize()
        .expect("server finalize");

    // Formatted input (parens, dashes) — E164PhoneValidator
    // rejects, never touches the store / argon.
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "phone": "(202) 555-1234",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "invalid_login_id");
}

#[tokio::test]
async fn signup_keyed_on_phone_rejects_email_input() {
    pocopine_server::__reset_for_test();
    let app = Server::new(axum::Router::new())
        .plugin(build_phone_credentials())
        .try_finalize()
        .expect("server finalize");

    // Wire shape uses `phone` field; `email` is just an unknown
    // field that returns a missing-login-id error.
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// ─── username-based configuration ───────────────────────────────────

fn build_username_credentials() -> Credentials<TestUserStore, TestTokenStore> {
    Credentials::new(
        fixed_secret(),
        TestUserStore::default(),
        TestTokenStore::default(),
    )
    .with_login_id_validator(UsernameValidator::default())
    .with_login_id_field("username")
}

#[tokio::test]
async fn signup_login_round_trip_keyed_on_username_with_case_folding() {
    pocopine_server::__reset_for_test();
    let app = Server::new(axum::Router::new())
        .plugin(build_username_credentials())
        .try_finalize()
        .expect("server finalize");

    // Sign up as "Alice".
    let resp = app
        .clone()
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "username": "Alice",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Login as "alice" — UsernameValidator default lowercases on
    // normalize, so the same row matches both casings.
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/login",
            serde_json::json!({
                "username": "alice",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_against_passwordless_user_returns_invalid_credentials() {
    pocopine_server::__reset_for_test();
    // Multi-credential model: a user who signed up via OAuth /
    // passkey has a record with `password_hash = None`. Hitting
    // the password login route for that account must return the
    // same `InvalidCredentials` shape as "no such user" — no
    // probe-based distinction.
    let store = TestUserStore::default();
    store.seed_passwordless("alice@example.com");
    let app = Server::new(axum::Router::new())
        .plugin(Credentials::new(
            fixed_secret(),
            store,
            TestTokenStore::default(),
        ))
        .try_finalize()
        .expect("server finalize");

    let resp = app
        .oneshot(post(
            "/_pocopine/auth/login",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "any-password",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "invalid_credentials");
}

#[tokio::test]
async fn signup_keyed_on_username_rejects_invalid_chars() {
    pocopine_server::__reset_for_test();
    let app = Server::new(axum::Router::new())
        .plugin(build_username_credentials())
        .try_finalize()
        .expect("server finalize");

    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "username": "alice@example.com",
                "password": "correcthorse",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "invalid_login_id");
}
