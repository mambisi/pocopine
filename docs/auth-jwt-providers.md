# JWT providers — adding a vendor preset

The pocopine `JwtVerifier` accepts any OIDC-shaped JWT issuer
through a typed `Provider` config struct. One in-tree provider
(Firebase) ships in `pocopine-auth-jwt`; everything else lives in
its own community-maintained crate following the
`pocopine-auth-jwt-<vendor>` naming convention.

This page is the contract for adding a new provider. RFC-074 §5.2
and §5.3 cover the design rationale; this doc is the cookbook.

## The contract

A provider is a struct + a `Provider` impl. That's the whole API.

```rust
use std::time::Duration;
use pocopine_auth_jwt::{
    Algorithm, ClaimMap, ClaimPath, JwtAuthError, JwtConfig,
    KeySource, Provider, TokenSource,
};

/// `Okta` identity verifier configuration.
#[non_exhaustive]
pub struct Okta {
    pub domain: String,
    pub audience: String,
    pub cache_ttl: Duration,
    pub refresh_cooldown: Duration,
    pub leeway: Duration,
}

impl Okta {
    pub fn new(domain: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            audience: audience.into(),
            cache_ttl: Duration::from_secs(3600),
            refresh_cooldown: Duration::from_secs(30),
            leeway: Duration::from_secs(60),
        }
    }
}

impl Provider for Okta {
    fn jwt_config(self) -> Result<JwtConfig, JwtAuthError> {
        Ok(JwtConfig {
            keys: KeySource::Jwks {
                url: format!("https://{}/oauth2/default/v1/keys", self.domain),
                cache_ttl: self.cache_ttl,
                refresh_cooldown: self.refresh_cooldown,
            },
            issuer: Some(format!("https://{}/oauth2/default", self.domain)),
            audience: Some(vec![self.audience]),
            algorithms: vec![Algorithm::Rs256],
            leeway: self.leeway,
            sources: vec![TokenSource::Bearer],
            revocation: None,
            claim_map: ClaimMap::oidc(),
            required_scopes: vec![],
        })
    }
}
```

User-facing API:

```rust
let verifier = JwtVerifier::from_provider(
    Okta::new("example.okta.com", "my-api"),
)?;
```

That's the whole user-visible surface. No registration step, no
runtime discovery — Rust's `impl Trait` does the work.

## Conventions

1. **Struct is `#[non_exhaustive]`.** Adding an optional field on
   the next minor version doesn't break callers because they're
   forced to use struct-update syntax.
2. **`new(...)` constructor for the required fields only.** Other
   fields default to sane values (`Duration::from_secs(3600)` cache
   TTL, 30-second refresh cooldown, 60-second leeway). Users
   override individual defaults via struct-update:
   ```rust
   Okta {
       leeway: Duration::from_secs(120),
       ..Okta::new("example.okta.com", "my-api")
   }
   ```
3. **Pin the algorithm.** Provider configs hardcode the
   algorithm(s) the IdP actually uses (RS256 for OIDC, HS256 for
   Supabase, etc.). Don't accept a list from the user — the IdP
   uses one or two specific algs and accepting more is the
   classic confusion-attack surface.
4. **Default to `TokenSource::Bearer` only.** Cookie sources are
   provider-specific (Firebase has `__session` SSR cookies, Clerk
   doesn't, etc.). When an IdP supports cookies, expose it as an
   opt-in field with a default of `false` — and if the cookie
   uses a different JWKS endpoint than the bearer token, that's
   a separate provider, not a flag.
5. **Use `ClaimMap::oidc()` as the default claim map.** Override
   only when the IdP's claims diverge from OIDC standard.
   Document overrides in the struct's rustdoc.
6. **Build from `Self::new(args)`, never on construction.** The
   `Provider::jwt_config(self)` impl should be straight-line code
   that consumes `self` and returns `Ok(JwtConfig { ... })`. No
   network calls, no fixture parsing — runtime work belongs in
   `JwksResolver`.

## The mandatory integration test

**Every provider — in-tree or third-party — ships a recorded-token
integration test.** No exceptions; this is RFC-074 §5.3.1, encoded
because the original RFC-070 §5.9 presets shipped without tests
and at least one was subtly wrong.

The test pattern (synthesize a keypair → inject as static JWKS →
sign a token → verify round-trip):

```rust
// In your `pocopine-auth-jwt-okta` crate's tests/okta_provider.rs:
use pocopine_auth_jwt::{JwtVerifier, KeySource, Provider};
use pocopine_auth_jwt_okta::Okta;
// ... (see crates/pocopine-auth-jwt-firebase/tests/firebase_provider.rs
//      for the full pattern)

#[tokio::test]
async fn okta_round_trips_a_valid_token() {
    let (pem, jwks, kid) = synth_keypair();
    let mut cfg = Okta::new("example.okta.com", "my-api")
        .jwt_config()
        .unwrap();
    cfg.keys = KeySource::StaticJwks { document: jwks };
    let verifier = JwtVerifier::custom(cfg).unwrap();

    let token = sign_with(/* iss = https://example.okta.com/oauth2/default,
                              aud = my-api,
                              sub = "user-1", */ &pem, kid);
    let user = verifier.verify_token(&token).await.unwrap();
    assert_eq!(user.id, "user-1");
}
```

Plus negative tests for the threat shapes the provider's config
defends against:

- Wrong audience (token meant for a different tenant).
- Wrong issuer (attacker-controlled IdP host).
- Missing `kid` in the JWKS (key rotation gap).

The full pattern lives in
[`crates/pocopine-auth-jwt-firebase/tests/firebase_provider.rs`](../crates/pocopine-auth-jwt-firebase/tests/firebase_provider.rs);
copy that file and adjust for your provider.

## Bundled providers

In-tree provider crates follow the `pocopine-auth-jwt-<vendor>`
naming convention and version independently from the verifier
engine. Apps add one Cargo dep per provider they use:

| Provider | Crate | Notes |
|---|---|---|
| Firebase ID token | [`pocopine-auth-jwt-firebase`](../crates/pocopine-auth-jwt-firebase) | RS256, `Authorization: Bearer` only. SSR `__session` cookies use a different JWKS URL — out of scope. |

## Community providers

If you maintain a third-party provider crate, open a PR adding a
row here. The bar is one passing integration test against the
provider's actual JWKS (mocked via the static-JWKS pattern above).

| Provider | Crate | Maintainer |
|---|---|---|
| _your provider here_ | _your crate_ | _you_ |

## What about OAuth flow / redirect handling?

Out of scope for `pocopine-auth-jwt`. The verifier handles
**JWT verification**, not the OAuth authorization-code flow.

If you want native social login without Firebase:

1. Use the `oauth2` Rust crate to drive the redirect + code
   exchange.
2. Once you have an ID token, hand it to the corresponding
   provider's `JwtVerifier` for verification.
3. Issue a pocopine session token via `JwtIssuer::hs256` so
   subsequent `#[server]` calls use a single token shape.

This is a future RFC's scope (a `pocopine-oauth` crate would wrap
`oauth2` with pocopine-shaped helpers); for now the integration
is application-level glue.
