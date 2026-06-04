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
//! `docs/guides/auth/credentials.md` for a Postgres + `sqlx` walkthrough).

#![cfg(not(target_arch = "wasm32"))]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestTokenStore, TestUserStore};
use http_body_util::BodyExt;
use pocopine_auth::Principal;
use pocopine_auth_credentials::Credentials;
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
    assert_eq!(body["error"], "email_taken");
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
    assert_eq!(body["error"], "invalid_email");
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

#[tokio::test]
async fn custom_claims_round_trip_through_session_token() {
    pocopine_server::__reset_for_test();
    // The TestUser's `to_auth_user()` adds `email_verified` as a
    // custom claim via `with_claim(...)`. After signup mints a JWT
    // and the paired verifier extracts it, `email_verified` MUST be
    // reachable via `principal.user().claim("email_verified")` —
    // i.e. the routes flatten custom claims to top-level JWT, and
    // the verifier puts them back into `AuthUser::claims`. Codex
    // review HIGH: this used to break because the routes nested
    // app claims under `"claims"` instead of flattening.
    let creds = build_credentials();
    let mut config = creds.verifier_config();
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

    let user = verifier
        .verify_token(&token)
        .await
        .expect("verifier accepts session token");

    assert_eq!(
        user.claim("email_verified").and_then(|v| v.as_bool()),
        Some(false),
        "email_verified custom claim must round-trip at top level; got user.claims = {:?}",
        user.claims,
    );
}

#[tokio::test]
async fn verifier_config_default_excludes_cookie_source() {
    // Codex review MEDIUM: cookie auth was on by default but the
    // crate ships no cookie issuance / clearing / CSRF — apps with
    // an XSS or subdomain-takeover surface could inject a cookie
    // without ever signing in. Default is bearer-only; cookie
    // ships when the cookie lifecycle ships.
    let creds = build_credentials();
    let cfg = creds.verifier_config();
    assert_eq!(
        cfg.sources.len(),
        1,
        "default verifier config must have exactly one source (Codex MEDIUM); got {:?}",
        cfg.sources
    );
    assert!(
        matches!(cfg.sources[0], TokenSource::Bearer),
        "default verifier config must be Bearer-only (Codex MEDIUM); cookie auth ships \
         with cookie lifecycle. got: {:?}",
        cfg.sources[0]
    );
}

#[tokio::test]
async fn signup_response_body_does_not_contain_password_hash() {
    pocopine_server::__reset_for_test();
    let app = router();
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
    // Strict assertion: every leaf in the response body is checked
    // against argon2 PHC markers + the literal `password_hash` key
    // anywhere in the JSON tree. Catches both direct serialization
    // and accidental nesting.
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("password_hash"),
        "response body must not contain `password_hash`: {raw}"
    );
    assert!(
        !raw.contains("$argon2"),
        "response body must not contain an argon2 PHC string: {raw}"
    );
}

#[tokio::test]
async fn signup_with_custom_password_validator_rejects_weak() {
    use pocopine_auth_credentials::Credentials;
    pocopine_server::__reset_for_test();
    // Custom validator: must contain a digit. Mirrors what an
    // app would write when defaulting on top of the framework's
    // length-only check.
    let creds = Credentials::new(
        fixed_secret(),
        TestUserStore::default(),
        TestTokenStore::default(),
    )
    .with_password_validator(|p| {
        if !p.chars().any(|c| c.is_ascii_digit()) {
            return Err("must_contain_digit");
        }
        Ok(())
    });
    let app = Server::new(axum::Router::new())
        .plugin(creds)
        .try_finalize()
        .unwrap();

    // No digit → rejected.
    let resp = app
        .clone()
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "alphabetic",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_to_json(resp.into_body()).await;
    assert_eq!(body["error"], "weak_password");

    // Same length WITH a digit → accepted.
    let resp = app
        .oneshot(post(
            "/_pocopine/auth/signup",
            serde_json::json!({
                "email": "alice@example.com",
                "password": "alphabetic1",
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
