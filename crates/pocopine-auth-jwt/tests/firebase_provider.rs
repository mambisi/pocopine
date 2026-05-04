// Recorded-token integration test for the Firebase provider —
// the discipline RFC-074 §5.3.1 mandates for every in-tree
// provider.
//
// We generate an RSA keypair at test time, synthesize a JWKS
// document with the public key, build the Firebase preset's
// config but swap `KeySource::Jwks { url }` for
// `KeySource::StaticJwks { document: synthesized_jwks }`, sign a
// Firebase-shaped token with the private key, and verify it
// round-trips end-to-end through `JwtVerifier`. No network calls.
//
// This catches three classes of bug:
//
// 1. Issuer / audience drift — the test signs with `iss =
//    https://securetoken.google.com/{project}` and `aud =
//    {project}` exactly matching the preset's config, so any
//    typo on either side breaks the test.
// 2. Claim-map mismatch — `sub` → `id`, `email` → `email`, etc.
// 3. Algorithm pinning — the test uses RS256; if Firebase is
//    accidentally extended to accept HS256, the JWT-confusion
//    invariant in `JwtConfig::validate()` will reject the
//    construction.

#![cfg(not(target_arch = "wasm32"))]

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::{encode, Algorithm as JwtAlg, EncodingKey, Header};
use pocopine_auth_jwt::providers::Firebase;
use pocopine_auth_jwt::{JwtConfig, JwtVerifier, KeySource, Provider};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{json, Value};

/// Synthesize a JWKS + matching private-key PEM for tests. Returns
/// `(private_key_pem, jwks_json, kid)`.
fn synth_keypair() -> (String, String, &'static str) {
    let kid = "firebase-test-kid";
    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("generate rsa keypair");
    let public_key = RsaPublicKey::from(&private_key);

    let private_pem = private_key
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("encode private key to pem")
        .to_string();

    let n = URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be());
    let e = URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be());

    let jwks = json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": kid,
            "n": n,
            "e": e,
        }]
    })
    .to_string();

    (private_pem, jwks, kid)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Sign a token with the Firebase claim shape.
fn sign_firebase_token(
    private_pem: &str,
    kid: &str,
    project_id: &str,
    sub: &str,
    extra: Value,
) -> String {
    let mut header = Header::new(JwtAlg::RS256);
    header.kid = Some(kid.to_string());

    let mut claims = json!({
        "sub": sub,
        "iss": format!("https://securetoken.google.com/{project_id}"),
        "aud": project_id,
        "iat": now_secs(),
        "exp": now_secs() + 3600,
    });
    if let Value::Object(extra_map) = extra {
        if let Value::Object(claim_map) = &mut claims {
            for (k, v) in extra_map {
                claim_map.insert(k, v);
            }
        }
    }

    let key = EncodingKey::from_rsa_pem(private_pem.as_bytes()).expect("parse rsa pem");
    encode(&header, &claims, &key).expect("sign token")
}

/// Build a verifier from the Firebase preset, swapping the live
/// JWKS URL for our synthesized static document.
fn firebase_verifier_with_static_jwks(project_id: &str, jwks: String) -> JwtVerifier {
    let mut cfg: JwtConfig = Firebase::new(project_id)
        .jwt_config()
        .expect("firebase config valid");
    cfg.keys = KeySource::StaticJwks { document: jwks };
    JwtVerifier::custom(cfg).expect("verifier build")
}

#[tokio::test]
async fn firebase_round_trips_a_valid_id_token() {
    let project = "pocopine-test";
    let (pem, jwks, kid) = synth_keypair();
    let verifier = firebase_verifier_with_static_jwks(project, jwks);

    let token = sign_firebase_token(
        &pem,
        kid,
        project,
        "firebase-uid-1",
        json!({
            "email": "alice@example.com",
            "email_verified": true,
            "name": "Alice",
        }),
    );

    let user = verifier.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "firebase-uid-1");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    assert_eq!(user.name.as_deref(), Some("Alice"));
    // Raw claims preserved for provider-specific access.
    assert_eq!(user.claim("aud"), Some(&json!(project)));
}

#[tokio::test]
async fn firebase_rejects_wrong_audience() {
    let project = "pocopine-test";
    let (pem, jwks, kid) = synth_keypair();
    let verifier = firebase_verifier_with_static_jwks(project, jwks);

    // Sign with the wrong audience: a token meant for a different
    // Firebase project. The Firebase preset's audience-equals-
    // project-id config must reject it.
    let mut header = Header::new(JwtAlg::RS256);
    header.kid = Some(kid.to_string());
    let claims = json!({
        "sub": "firebase-uid-1",
        "iss": format!("https://securetoken.google.com/{project}"),
        "aud": "some-other-project",
        "iat": now_secs(),
        "exp": now_secs() + 3600,
    });
    let key = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
    let token = encode(&header, &claims, &key).unwrap();

    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(
            err,
            pocopine_auth_jwt::JwtAuthError::ClaimRejected { claim: "aud", .. }
        ),
        "wrong-audience token must be rejected; got: {err:?}"
    );
}

#[tokio::test]
async fn firebase_rejects_wrong_issuer() {
    let project = "pocopine-test";
    let (pem, jwks, kid) = synth_keypair();
    let verifier = firebase_verifier_with_static_jwks(project, jwks);

    // Sign with a non-Firebase issuer (e.g. an attacker-controlled
    // domain pretending to issue Firebase-shaped tokens).
    let mut header = Header::new(JwtAlg::RS256);
    header.kid = Some(kid.to_string());
    let claims = json!({
        "sub": "firebase-uid-1",
        "iss": "https://evil.example/firebase",
        "aud": project,
        "iat": now_secs(),
        "exp": now_secs() + 3600,
    });
    let key = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
    let token = encode(&header, &claims, &key).unwrap();

    let err = verifier.verify_token(&token).await.unwrap_err();
    assert!(
        matches!(
            err,
            pocopine_auth_jwt::JwtAuthError::ClaimRejected { claim: "iss", .. }
        ),
        "wrong-issuer token must be rejected; got: {err:?}"
    );
}

#[tokio::test]
async fn firebase_rejects_kid_not_in_jwks() {
    let project = "pocopine-test";
    let (pem, _jwks, _kid) = synth_keypair();
    // Build a JWKS that doesn't contain the kid we'll sign with.
    let empty_jwks = json!({ "keys": [] }).to_string();
    let verifier = firebase_verifier_with_static_jwks(project, empty_jwks);

    let token = sign_firebase_token(&pem, "wrong-kid", project, "firebase-uid-1", json!({}));

    let err = verifier.verify_token(&token).await.unwrap_err();
    // Static JWKS configs don't trigger network refresh, so this
    // surfaces as KeyResolutionFailed rather than rate-limited.
    assert!(
        matches!(
            err,
            pocopine_auth_jwt::JwtAuthError::KeyResolutionFailed { .. }
        ),
        "missing-kid token must fail key resolution; got: {err:?}"
    );
}
