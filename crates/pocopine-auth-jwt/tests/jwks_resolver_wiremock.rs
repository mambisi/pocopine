// Live JWKS-fetch tests against a wiremock HTTP server.
//
// These tests cover the live HTTP axis of `JwksResolver`: fetch,
// cache, and refresh behavior.
//
// Scenarios:
//
// 1. Happy path — first verify fetches JWKS, signs and verifies a
//    token end-to-end through the live HTTP path.
// 2. Cache hit — a second verify against a cached JWKS does NOT
//    re-fetch (asserted via wiremock's request count).
// 3. `kid` rotation — first JWKS serves kid-A; the verifier
//    receives a token signed with kid-B; the resolver refetches
//    JWKS (now serving both kids) and succeeds.
// 4. Refresh cooldown — rapid `kid`-miss requests within the
//    configured cooldown only trigger one network refresh; the
//    second request short-circuits with a rate-limited error.
// 5. JWKS server error — a 500 from the JWKS endpoint surfaces as
//    `KeyResolutionFailed`, never as a successful verify.

#![cfg(not(target_arch = "wasm32"))]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::{encode, Algorithm as JwtAlg, EncodingKey, Header};
use pocopine_auth_jwt::{
    Algorithm, ClaimMap, JwtAuthError, JwtConfig, JwtVerifier, KeySource, TokenSource,
};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISS: &str = "https://test-issuer.example";
const AUD: &str = "test-audience";
const JWKS_PATH: &str = "/.well-known/jwks.json";

/// One `(private_pem, public_key, kid)` triple.
struct TestKey {
    private_pem: String,
    public_key: RsaPublicKey,
    kid: &'static str,
}

fn synth_key(kid: &'static str) -> TestKey {
    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
    let public_key = RsaPublicKey::from(&private_key);
    let private_pem = private_key
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("to pkcs1 pem")
        .to_string();
    TestKey {
        private_pem,
        public_key,
        kid,
    }
}

fn jwks_doc(keys: &[&TestKey]) -> String {
    let entries: Vec<Value> = keys
        .iter()
        .map(|k| {
            let n = URL_SAFE_NO_PAD.encode(k.public_key.n().to_bytes_be());
            let e = URL_SAFE_NO_PAD.encode(k.public_key.e().to_bytes_be());
            json!({
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": k.kid,
                "n": n,
                "e": e,
            })
        })
        .collect();
    json!({ "keys": entries }).to_string()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sign_token(key: &TestKey, sub: &str) -> String {
    let mut header = Header::new(JwtAlg::RS256);
    header.kid = Some(key.kid.to_string());
    let claims = json!({
        "sub": sub,
        "iss": ISS,
        "aud": AUD,
        "iat": now_secs(),
        "exp": now_secs() + 3600,
    });
    encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(key.private_pem.as_bytes()).unwrap(),
    )
    .unwrap()
}

fn verifier(jwks_url: &str, cache_ttl: Duration, refresh_cooldown: Duration) -> JwtVerifier {
    JwtVerifier::custom(JwtConfig {
        keys: KeySource::Jwks {
            url: jwks_url.to_string(),
            cache_ttl,
            refresh_cooldown,
        },
        issuer: Some(ISS.to_string()),
        audience: Some(vec![AUD.to_string()]),
        algorithms: vec![Algorithm::Rs256],
        leeway: Duration::from_secs(60),
        sources: vec![TokenSource::Bearer],
        revocation: None,
        claim_map: ClaimMap::oidc(),
        required_scopes: vec![],
    })
    .expect("verifier build")
}

#[tokio::test]
async fn jwks_fetch_and_verify_round_trip() {
    let server = MockServer::start().await;
    let key = synth_key("kid-A");
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key])))
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_secs(30));
    let token = sign_token(&key, "user-1");

    let user = v.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "user-1");
}

#[tokio::test]
async fn jwks_cache_hit_does_not_refetch() {
    let server = MockServer::start().await;
    let key = synth_key("kid-A");
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key])))
        .expect(1) // wiremock asserts exact count at server drop time.
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_secs(30));

    // Two verifications in quick succession against the same kid.
    // The second must hit the resolver's cache and NOT re-fetch.
    let token = sign_token(&key, "user-1");
    v.verify_token(&token).await.unwrap();
    v.verify_token(&token).await.unwrap();
    // `expect(1)` on the mock fails the test at drop if it was
    // called more or fewer times than configured.
}

#[tokio::test]
async fn kid_miss_triggers_jwks_refresh_after_cooldown() {
    let server = MockServer::start().await;
    let key_a = synth_key("kid-A");
    let key_b = synth_key("kid-B");

    // First stub: serves only kid-A.
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key_a])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // After the first response, serve both keys (simulating a
    // key rotation that introduced kid-B).
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key_a, &key_b])))
        .mount(&server)
        .await;

    // Cooldown is well below test duration so the kid-B miss
    // can trigger a real refresh.
    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_millis(1));

    // First verify with kid-A — fetches the first JWKS.
    let token_a = sign_token(&key_a, "user-1");
    v.verify_token(&token_a).await.unwrap();

    // Sleep past the cooldown window so the kid-B miss is allowed
    // to re-fetch instead of returning rate-limited.
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Now verify with kid-B — kid not in cached JWKS → resolver
    // refetches → second stub serves both keys → succeeds.
    let token_b = sign_token(&key_b, "user-2");
    let user = v.verify_token(&token_b).await.unwrap();
    assert_eq!(user.id, "user-2");
}

#[tokio::test]
async fn refresh_cooldown_rate_limits_kid_thrash() {
    let server = MockServer::start().await;
    let key_a = synth_key("kid-A");
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key_a])))
        // Rate-limited refresh: only ONE fetch should ever happen,
        // even though we issue two `kid`-misses below.
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    // Long cooldown so the second kid miss is squarely inside it.
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_secs(60));

    // First request triggers a real fetch (kid-A is in JWKS).
    let token_a = sign_token(&key_a, "user-1");
    v.verify_token(&token_a).await.unwrap();

    // Two kid-misses in quick succession. Both should hit the
    // resolver's cooldown gate (since `last_attempt` was set by
    // the kid-A fetch above) and short-circuit with
    // `KeyResolutionFailed`. Critically, neither triggers a
    // network call — `expect(1)` on the mock would fail otherwise.
    let attacker_key = synth_key("kid-Attacker");
    let token_x = sign_token(&attacker_key, "attacker");
    let err1 = v.verify_token(&token_x).await.unwrap_err();
    let err2 = v.verify_token(&token_x).await.unwrap_err();

    for err in [&err1, &err2] {
        assert!(
            matches!(err, JwtAuthError::KeyResolutionFailed { .. }),
            "expected KeyResolutionFailed; got: {err:?}"
        );
    }
}

#[tokio::test]
async fn jwks_500_response_yields_key_resolution_failed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_secs(30));
    let key = synth_key("kid-A");
    let token = sign_token(&key, "user-1");

    let err = v.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::KeyResolutionFailed { .. }),
        "expected KeyResolutionFailed for 500 response; got: {err:?}"
    );
}

#[tokio::test]
async fn jwks_404_response_yields_key_resolution_failed() {
    // A 404 from the JWKS endpoint (misconfigured URL, retired
    // provider) must surface as `KeyResolutionFailed`, not as a
    // panic or a silent allow.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_secs(30));
    let key = synth_key("kid-A");
    let token = sign_token(&key, "user-1");

    let err = v.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::KeyResolutionFailed { .. }),
        "expected KeyResolutionFailed for 404 response; got: {err:?}"
    );
}

#[tokio::test]
async fn jwks_empty_keyset_yields_key_resolution_failed() {
    // 200 OK with a well-formed JWKS document carrying zero keys
    // must NOT silently pass — every `kid` lookup will miss and
    // the resolver must report `KeyResolutionFailed`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"keys":[]}"#))
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_secs(3600), Duration::from_secs(30));
    let key = synth_key("kid-A");
    let token = sign_token(&key, "user-1");

    let err = v.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::KeyResolutionFailed { .. }),
        "expected KeyResolutionFailed for empty keyset; got: {err:?}"
    );
}

#[tokio::test]
async fn jwks_kid_disappearing_after_refresh_yields_key_resolution_failed() {
    // Accidental key rotation that drops an in-use kid: first JWKS
    // serves kid-A; refresh returns a different kid (kid-Z), with
    // kid-A no longer present. A token still signed by kid-A must
    // surface `KeyResolutionFailed`, never falsely verify against a
    // stale cache.
    let server = MockServer::start().await;
    let key_a = synth_key("kid-A");
    let key_z = synth_key("kid-Z");

    // First fetch: kid-A only.
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key_a])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // All later fetches: kid-Z only — kid-A has been rotated out.
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key_z])))
        .mount(&server)
        .await;

    // Short TTL so the second verify forces a refresh.
    let url = format!("{}{}", server.uri(), JWKS_PATH);
    let v = verifier(&url, Duration::from_millis(50), Duration::from_millis(1));

    // First verify succeeds: kid-A is in the cached JWKS.
    let token = sign_token(&key_a, "user-1");
    v.verify_token(&token).await.unwrap();

    // Wait past TTL so the next lookup re-fetches.
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Second verify with the same kid-A token: refresh runs,
    // returns a JWKS without kid-A. The resolver must surface
    // `KeyResolutionFailed`, never accept the token.
    let err = v.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(err, JwtAuthError::KeyResolutionFailed { .. }),
        "expected KeyResolutionFailed after kid rotation drops in-use key; got: {err:?}"
    );
}

#[tokio::test]
async fn jwks_cache_ttl_triggers_refetch() {
    let server = MockServer::start().await;
    let key = synth_key("kid-A");
    // Configure exactly two stubs by call count: any GET to
    // JWKS_PATH gets the same body, but `expect(2)` asserts the
    // resolver re-fetches once after TTL expiry (and only once,
    // not on every subsequent call).
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(jwks_doc(&[&key])))
        .expect(2)
        .mount(&server)
        .await;

    let url = format!("{}{}", server.uri(), JWKS_PATH);
    // 50ms TTL, longer cooldown so cooldown isn't the gate.
    let v = verifier(&url, Duration::from_millis(50), Duration::from_millis(1));
    let token = sign_token(&key, "user-1");

    // First verify: fetch #1.
    v.verify_token(&token).await.unwrap();

    // Within TTL: cache hit, no refetch. (Same call count.)
    v.verify_token(&token).await.unwrap();

    // Past TTL: refetch #2.
    tokio::time::sleep(Duration::from_millis(80)).await;
    v.verify_token(&token).await.unwrap();

    // Within TTL of fetch #2: cache hit again.
    v.verify_token(&token).await.unwrap();

    // `expect(2)` on the mock asserts at drop.
}
