// Verifier invariant tests — the security-critical bits from
// RFC-070 §5.8 / §6.1.
//
// Each test issues its own JWT against an `HS256` config (so we
// don't need a JWKS server) and exercises one rejection path:
// algorithm pinning, `alg: none`, JWT confusion, expiry, issuer/
// audience mismatch, claim-map projection, scope check, revocation.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{HeaderMap, HeaderValue, Method, Uri};
use jsonwebtoken::{encode, Algorithm as JwtAlg, EncodingKey, Header};
use pocopine_auth::{AuthProvider, RequestContext, Role};
use pocopine_auth_jwt::{
    Algorithm, ClaimMap, ClaimPath, JwtAuthError, JwtConfig, JwtIssuer, JwtVerifier, KeySource,
    RevocationCheck, SecretBytes, TokenSource,
};
use serde_json::{json, Value};

const SECRET: &[u8] = b"this-is-a-32-byte-long-test-secret-please";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn issue_token(claims: Value) -> String {
    let header = Header::new(JwtAlg::HS256);
    encode(&header, &claims, &EncodingKey::from_secret(SECRET)).unwrap()
}

fn issue_with_alg(alg: JwtAlg, claims: Value) -> String {
    let header = Header::new(alg);
    encode(&header, &claims, &EncodingKey::from_secret(SECRET)).unwrap()
}

fn standard_config(algorithms: Vec<Algorithm>) -> JwtConfig {
    JwtConfig {
        keys: KeySource::Hmac {
            secret: SecretBytes::new(SECRET.to_vec()),
        },
        issuer: Some("test-issuer".into()),
        audience: Some(vec!["test-aud".into()]),
        algorithms,
        leeway: Duration::from_secs(5),
        sources: vec![TokenSource::Bearer],
        revocation: None,
        claim_map: ClaimMap::oidc(),
        required_scopes: vec![],
    }
}

fn ctx_with_bearer(token: &str) -> RequestContext {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    RequestContext::new(Method::POST, Uri::from_static("/_pocopine/test"), headers)
}

fn standard_claims() -> Value {
    json!({
        "sub": "user-123",
        "iss": "test-issuer",
        "aud": "test-aud",
        "iat": now_secs(),
        "exp": now_secs() + 3600,
        "email": "alice@example.com",
        "name": "Alice",
    })
}

#[tokio::test]
async fn happy_path_verifies_and_projects_user() {
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let token = issue_token(standard_claims());
    let user = verifier.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "user-123");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    assert_eq!(user.name.as_deref(), Some("Alice"));
    // Raw claims preserved for provider-specific access.
    assert_eq!(user.claim("aud"), Some(&json!("test-aud")));
}

#[tokio::test]
async fn algorithm_not_in_whitelist_is_rejected() {
    // Token signed with HS512; verifier whitelists only HS256.
    let token = issue_with_alg(JwtAlg::HS512, standard_claims());
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::AlgorithmRejected { .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn alg_none_is_impossible_to_express() {
    // jsonwebtoken's `Algorithm` enum has no `None` variant either.
    // A token with `"alg": "none"` parses but fails verification:
    // jsonwebtoken returns an error during decode_header (or
    // signature check). The point is: there is NO path through
    // our enum / config that allows a none-alg token to verify.
    //
    // We assert this by hand-crafting a header `{"alg":"none","typ":"JWT"}`
    // and confirming we get a Malformed/AlgorithmRejected, never
    // a successful verification.
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let header_b64 = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload_b64 = URL_SAFE_NO_PAD.encode(br#"{"sub":"attacker"}"#);
    let token = format!("{header_b64}.{payload_b64}.");

    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(
            err,
            JwtAuthError::AlgorithmRejected { .. } | JwtAuthError::Malformed { .. }
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let mut claims = standard_claims();
    claims["exp"] = json!(now_secs() - 3600);
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::ClaimRejected { claim: "exp", .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn issuer_mismatch_is_rejected() {
    let mut claims = standard_claims();
    claims["iss"] = json!("evil-issuer");
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::ClaimRejected { claim: "iss", .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn audience_mismatch_is_rejected() {
    let mut claims = standard_claims();
    claims["aud"] = json!("wrong-aud");
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::ClaimRejected { claim: "aud", .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn signature_tamper_is_rejected() {
    let mut token = issue_token(standard_claims());
    // Flip the last character of the signature.
    let last = token.pop().unwrap();
    let replacement = if last == 'A' { 'B' } else { 'A' };
    token.push(replacement);
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::SignatureInvalid),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn missing_token_returns_anonymous_via_authprovider() {
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let ctx = RequestContext::new(
        Method::POST,
        Uri::from_static("/_pocopine/test"),
        HeaderMap::new(),
    );
    let result = verifier.authenticate(&ctx).await.unwrap();
    assert!(result.is_none(), "missing token should yield anonymous");
}

#[tokio::test]
async fn good_token_via_authprovider_returns_user() {
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let token = issue_token(standard_claims());
    let ctx = ctx_with_bearer(&token);
    let user = verifier.authenticate(&ctx).await.unwrap().unwrap();
    assert_eq!(user.id, "user-123");
}

#[tokio::test]
async fn role_claim_extraction_from_array_and_string() {
    let mut config = standard_config(vec![Algorithm::Hs256]);
    config.claim_map.roles = Some(ClaimPath::from_str("roles"));

    // Array form.
    let mut claims = standard_claims();
    claims["roles"] = json!(["admin", "editor"]);
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(config.clone()).unwrap();
    let user = verifier.verify_token(&token).await.unwrap();
    assert!(user.has_role(&Role::admin()));
    assert!(user.has_role(&Role::named("editor")));

    // String form.
    let mut claims = standard_claims();
    claims["roles"] = json!("admin");
    let token = issue_token(claims);
    let user = verifier.verify_token(&token).await.unwrap();
    assert!(user.has_role(&Role::admin()));
}

#[tokio::test]
async fn nested_claim_path_walks_dotted_segments() {
    // Simulate Firebase's `firebase.identities.email[0]`-style paths.
    let mut config = standard_config(vec![Algorithm::Hs256]);
    config.claim_map.id = ClaimPath::segments(["nested", "user_id"]);

    let claims = json!({
        "iss": "test-issuer",
        "aud": "test-aud",
        "iat": now_secs(),
        "exp": now_secs() + 3600,
        "nested": { "user_id": "deep-user" },
    });
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(config).unwrap();
    let user = verifier.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "deep-user");
}

#[tokio::test]
async fn missing_id_claim_fails_with_claim_map_failed() {
    let claims = json!({
        "iss": "test-issuer",
        "aud": "test-aud",
        "iat": now_secs(),
        "exp": now_secs() + 3600,
        // No `sub` field!
    });
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(standard_config(vec![Algorithm::Hs256])).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::ClaimMapFailed { .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn required_scope_present_passes() {
    let mut config = standard_config(vec![Algorithm::Hs256]);
    config.required_scopes = vec!["read:posts".into()];

    let mut claims = standard_claims();
    claims["scope"] = json!("read:posts write:posts");
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(config).unwrap();
    let user = verifier.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "user-123");
}

#[tokio::test]
async fn required_scope_missing_fails() {
    let mut config = standard_config(vec![Algorithm::Hs256]);
    config.required_scopes = vec!["admin:everything".into()];

    let mut claims = standard_claims();
    claims["scope"] = json!("read:posts");
    let token = issue_token(claims);
    let verifier = JwtVerifier::custom(config).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::ScopeMissing { ref required } if required == "admin:everything"),
        "got: {err:?}"
    );
}

struct AlwaysRevoked;
impl RevocationCheck for AlwaysRevoked {
    fn is_revoked(&self, _claims: &Value) -> bool {
        true
    }
}

#[tokio::test]
async fn revocation_hook_short_circuits_after_signature_verify() {
    let mut config = standard_config(vec![Algorithm::Hs256]);
    config.revocation = Some(Arc::new(AlwaysRevoked));
    let token = issue_token(standard_claims());
    let verifier = JwtVerifier::custom(config).unwrap();
    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(matches!(err, JwtAuthError::Revoked), "got: {err:?}");
}

#[tokio::test]
async fn issuer_round_trips_signed_and_verified_token() {
    // Symmetry test: issue a token via JwtIssuer, verify with the
    // matching pocopine preset.
    let secret = SecretBytes::new(SECRET.to_vec());
    let issuer = JwtIssuer::pocopine(secret.clone()).with_default_ttl(Duration::from_secs(60));
    let token = issuer.sign("user-7", json!({ "email": "x@y" })).unwrap();

    let verifier = JwtVerifier::pocopine(secret).unwrap();
    let user = verifier.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "user-7");
    assert_eq!(user.claim("email"), Some(&json!("x@y")));
}

#[tokio::test]
async fn config_validation_rejects_jwks_with_hmac_alg() {
    let bad = JwtConfig {
        keys: KeySource::Jwks {
            url: "https://example.test/jwks".into(),
            cache_ttl: Duration::from_secs(60),
            refresh_cooldown: Duration::from_secs(30),
        },
        issuer: Some("x".into()),
        audience: Some(vec!["x".into()]),
        algorithms: vec![Algorithm::Hs256],
        leeway: Duration::from_secs(5),
        sources: vec![TokenSource::Bearer],
        revocation: None,
        claim_map: ClaimMap::oidc(),
        required_scopes: vec![],
    };
    match JwtVerifier::custom(bad) {
        Ok(_) => panic!("validation should reject jwks + hmac alg"),
        Err(JwtAuthError::InvalidConfig { reason }) => {
            assert!(reason.contains("JWT-confusion"), "got: {reason}");
        }
        Err(other) => panic!("expected InvalidConfig, got: {other:?}"),
    }
}

#[tokio::test]
async fn cookie_token_source_extracts_token_from_named_cookie() {
    let mut config = standard_config(vec![Algorithm::Hs256]);
    config.sources = vec![TokenSource::Cookie(std::borrow::Cow::Borrowed(
        "pocopine_session",
    ))];
    let token = issue_token(standard_claims());
    let verifier = JwtVerifier::custom(config).unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        HeaderValue::from_str(&format!("pocopine_session={token}; other=ignored")).unwrap(),
    );
    let ctx = RequestContext::new(Method::POST, Uri::from_static("/_pocopine/test"), headers);
    let user = verifier.authenticate(&ctx).await.unwrap().unwrap();
    assert_eq!(user.id, "user-123");
}
