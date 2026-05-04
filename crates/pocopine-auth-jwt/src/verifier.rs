//! `JwtVerifier` — implements `AuthProvider` over a `JwtConfig`.
//!
//! Responsibilities, in order:
//! 1. Extract a token from the configured sources.
//! 2. Parse the JWT header, check `alg` against the whitelist.
//! 3. Resolve the signing key (HMAC secret, static JWKS, or
//!    `JwksResolver` cache).
//! 4. Verify the signature + claim set (`iss` / `aud` / `exp` /
//!    `iat` / `nbf`) with the configured leeway.
//! 5. Run optional `RevocationCheck` and required-scope check.
//! 6. Project the claims onto an `AuthUser` via the `ClaimMap`.
//!
//! Diagnostics flow through `tracing` with `target: "pocopine.log"`
//! (ops alerts) and `pocopine.metric` (counters). Token text is
//! never included in any log line.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use pocopine_auth::{
    AuthError, AuthFuture, AuthProvider, AuthUser, Permission, RequestContext, Role,
};
use pocopine_observe::{LOG_TARGET, METRIC_TARGET};
use serde_json::Value;

use crate::config::{Algorithm, ClaimMap, ClaimPath, JwtConfig, KeySource, TokenSource};
use crate::error::JwtAuthError;
use crate::jwks::JwksResolver;

/// Counter name emitted on every successful verification.
const COUNTER_VERIFICATIONS: &str = "auth.jwt.verifications";
/// Counter name emitted on every rejection. The `kind` field
/// carries the [`JwtAuthError::kind`] label so operators can
/// alert on classes.
const COUNTER_REJECTIONS: &str = "auth.jwt.rejections";

/// JWT verifier implementing [`AuthProvider`]. Construct via a
/// preset (`JwtVerifier::firebase`, etc.) or
/// [`JwtVerifier::custom`].
#[derive(Clone)]
pub struct JwtVerifier {
    inner: Arc<VerifierInner>,
}

struct VerifierInner {
    config: JwtConfig,
    /// Resolver for `KeySource::Jwks` and `KeySource::StaticJwks`.
    /// `None` for `KeySource::Hmac`.
    jwks: Option<JwksResolver>,
}

impl JwtVerifier {
    /// Build a verifier from any [`Provider`](crate::Provider)
    /// — a typed provider config (e.g.
    /// [`crate::providers::Firebase`]) that materializes a
    /// `JwtConfig`.
    ///
    /// ```ignore
    /// use pocopine_auth_jwt::{JwtVerifier, providers::Firebase};
    /// let verifier = JwtVerifier::from_provider(Firebase::new("my-project"))?;
    /// ```
    pub fn from_provider<P: crate::provider::Provider>(provider: P) -> Result<Self, JwtAuthError> {
        Self::custom(provider.jwt_config()?)
    }

    /// Build a verifier from a custom config. Use the preset
    /// constructors (`firebase`, `clerk`, …) when possible.
    ///
    /// Validates the config (algorithm pinning, key/alg consistency,
    /// non-empty whitelists) and returns an error on violation.
    pub fn custom(config: JwtConfig) -> Result<Self, JwtAuthError> {
        config.validate()?;
        let jwks = match &config.keys {
            KeySource::Jwks {
                url,
                cache_ttl,
                refresh_cooldown,
            } => Some(JwksResolver::new(
                url.clone(),
                *cache_ttl,
                *refresh_cooldown,
            )),
            KeySource::StaticJwks { document } => {
                let parsed: jsonwebtoken::jwk::JwkSet =
                    serde_json::from_str(document).map_err(|e| JwtAuthError::InvalidConfig {
                        reason: format!("static jwks document is not valid JWKS json: {e}"),
                    })?;
                // Pre-seed the resolver. Effectively-infinite TTL +
                // cooldown so the resolver never fires a network
                // refresh for a static source.
                Some(JwksResolver::with_seed(
                    String::new(),
                    std::time::Duration::from_secs(u32::MAX as u64),
                    std::time::Duration::from_secs(u32::MAX as u64),
                    parsed,
                ))
            }
            KeySource::Hmac { .. } => None,
        };
        Ok(Self {
            inner: Arc::new(VerifierInner { config, jwks }),
        })
    }

    /// Verify a single token string and return the projected user.
    /// This is the entry point that `AuthProvider::authenticate`
    /// drives once it has extracted the token from a request.
    pub async fn verify_token(&self, token: &str) -> Result<AuthUser, JwtAuthError> {
        let header = decode_header(token).map_err(|e| JwtAuthError::Malformed {
            reason: malformed_reason(e),
        })?;

        // Algorithm pinning: reject before key resolution. Any alg
        // not in our enum (including `none` and EdDSA) is rejected;
        // any alg in the enum but not in the configured whitelist
        // is rejected.
        let our_alg = match Algorithm::from_jwt(header.alg) {
            Some(a) => a,
            None => {
                return Err(JwtAuthError::AlgorithmRejected {
                    got: format!("{:?}", header.alg),
                    allowed: self.allowed_alg_strs(),
                });
            }
        };
        if !self.inner.config.algorithms.contains(&our_alg) {
            return Err(JwtAuthError::AlgorithmRejected {
                got: our_alg.as_str().to_string(),
                allowed: self.allowed_alg_strs(),
            });
        }

        // Resolve the decoding key.
        let decoding_key = match &self.inner.config.keys {
            KeySource::Hmac { secret } => DecodingKey::from_secret(secret.as_bytes()),
            KeySource::Jwks { .. } | KeySource::StaticJwks { .. } => {
                let kid = header.kid.ok_or(JwtAuthError::Malformed {
                    reason: "JWT header missing `kid` (required for JWKS sources)",
                })?;
                let resolver = self.inner.jwks.as_ref().expect(
                    "config validation guarantees jwks resolver exists for asymmetric sources",
                );
                resolver.key_for(&kid).await?
            }
        };

        // Build jsonwebtoken Validation matching our config.
        //
        // jsonwebtoken's `Validation::new` defaults `iss = None` and
        // skips issuer validation in that case automatically — we
        // only need to call `set_issuer` when an issuer is
        // configured. Same for `set_audience`.
        //
        // The `validate_aud = false` in the audience-None branch is
        // load-bearing: jsonwebtoken's default `validate_aud = true`
        // would otherwise enforce the audience check shape even
        // without an expected list. **Crucially this toggle MUST
        // NOT appear in the issuer branch** — the prior version
        // disabled `validate_aud` whenever issuer was unset,
        // silently bypassing audience validation for a legitimate
        // `issuer=None, audience=Some(...)` configuration.
        let mut validation = Validation::new(our_alg.into_jwt());
        validation.leeway = self.inner.config.leeway.as_secs();
        if let Some(iss) = &self.inner.config.issuer {
            validation.set_issuer(&[iss]);
        }
        if let Some(audiences) = &self.inner.config.audience {
            validation.set_audience(audiences);
        } else {
            validation.validate_aud = false;
        }
        validation.validate_exp = true;
        validation.validate_nbf = true;

        let token_data =
            decode::<Value>(token, &decoding_key, &validation).map_err(map_jwt_error)?;

        let claims = token_data.claims;

        // Required-scope check.
        if !self.inner.config.required_scopes.is_empty() {
            let scope_str = self
                .inner
                .config
                .claim_map
                .scope
                .as_ref()
                .and_then(|p| walk_path(&claims, p))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let token_scopes: std::collections::HashSet<&str> =
                scope_str.split_whitespace().collect();
            for required in &self.inner.config.required_scopes {
                if !token_scopes.contains(required.as_str()) {
                    return Err(JwtAuthError::ScopeMissing {
                        required: required.clone(),
                    });
                }
            }
        }

        // Revocation check.
        if let Some(check) = &self.inner.config.revocation {
            if check.is_revoked(&claims) {
                return Err(JwtAuthError::Revoked);
            }
        }

        // Project to AuthUser via ClaimMap. Consume by value so the
        // claim map is moved into AuthUser.claims rather than
        // deep-cloned per request.
        project_claims(claims, &self.inner.config.claim_map)
    }

    fn allowed_alg_strs(&self) -> Vec<String> {
        self.inner
            .config
            .algorithms
            .iter()
            .map(|a| a.as_str().to_string())
            .collect()
    }
}

impl AuthProvider for JwtVerifier {
    fn authenticate<'a>(&'a self, ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>> {
        Box::pin(async move {
            // Extract token from the configured sources.
            let token = match extract_token(ctx, &self.inner.config.sources) {
                Some(t) => t,
                None => {
                    // No credential present: anonymous. Not an
                    // error; downstream guards decide.
                    return Ok(None);
                }
            };

            match self.verify_token(&token).await {
                Ok(user) => {
                    tracing::debug!(
                        target: METRIC_TARGET,
                        counter = COUNTER_VERIFICATIONS,
                        result = "ok",
                    );
                    Ok(Some(user))
                }
                Err(err) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        kind = err.kind(),
                        error = %err,
                        "jwt verification rejected"
                    );
                    tracing::debug!(
                        target: METRIC_TARGET,
                        counter = COUNTER_REJECTIONS,
                        kind = err.kind(),
                    );
                    // The trait surface returns AuthError, not
                    // JwtAuthError. Stringify so the upstream layer
                    // can log + treat as anonymous; guards reject
                    // the anonymous request.
                    Err(AuthError::new(err.to_string()))
                }
            }
        })
    }
}

fn extract_token(ctx: &RequestContext, sources: &[TokenSource]) -> Option<String> {
    for source in sources {
        match source {
            TokenSource::Bearer => {
                if let Some(t) = ctx.bearer_token() {
                    return Some(t.to_string());
                }
            }
            TokenSource::Cookie(name) => {
                if let Some(t) = ctx.cookie(name.as_ref()) {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

fn malformed_reason(err: jsonwebtoken::errors::Error) -> &'static str {
    match err.kind() {
        jsonwebtoken::errors::ErrorKind::InvalidToken => "invalid token shape",
        jsonwebtoken::errors::ErrorKind::Base64(_) => "base64 decode failed",
        jsonwebtoken::errors::ErrorKind::Json(_) => "header or claims not json",
        _ => "header parse failed",
    }
}

fn map_jwt_error(err: jsonwebtoken::errors::Error) -> JwtAuthError {
    use jsonwebtoken::errors::ErrorKind;
    match err.kind() {
        ErrorKind::InvalidSignature | ErrorKind::InvalidEcdsaKey | ErrorKind::InvalidRsaKey(_) => {
            JwtAuthError::SignatureInvalid
        }
        ErrorKind::ExpiredSignature => JwtAuthError::ClaimRejected {
            claim: "exp",
            reason: "token has expired".into(),
        },
        ErrorKind::InvalidIssuer => JwtAuthError::ClaimRejected {
            claim: "iss",
            reason: "issuer does not match".into(),
        },
        ErrorKind::InvalidAudience => JwtAuthError::ClaimRejected {
            claim: "aud",
            reason: "audience does not match".into(),
        },
        ErrorKind::ImmatureSignature => JwtAuthError::ClaimRejected {
            claim: "nbf",
            reason: "token not yet valid".into(),
        },
        // `InvalidAlgorithm` from the upstream decode is unreachable
        // here — we pin the algorithm before calling `decode` (see
        // `from_jwt` above). Anything else is a generic claim-side
        // failure that we surface verbatim.
        _ => JwtAuthError::ClaimRejected {
            claim: "unknown",
            reason: err.to_string(),
        },
    }
}

/// Walk a `ClaimPath` through a JSON value. Returns `None` at any
/// non-object intermediate node or missing segment.
fn walk_path<'a>(claims: &'a Value, path: &ClaimPath) -> Option<&'a Value> {
    let mut node = claims;
    for segment in &path.segments {
        node = node.get(segment)?;
    }
    Some(node)
}

fn project_claims(claims: Value, map: &ClaimMap) -> Result<AuthUser, JwtAuthError> {
    // First read the named projections by reference. This walk
    // doesn't allocate apart from the small `id`/email/name strings
    // we have to own (they live on `AuthUser`).
    let id = walk_path(&claims, &map.id)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| JwtAuthError::ClaimMapFailed {
            path: map.id.segments.join("."),
        })?;

    let mut user = AuthUser::new(id);

    if let Some(p) = &map.email {
        if let Some(s) = walk_path(&claims, p).and_then(Value::as_str) {
            user.email = Some(s.to_string());
        }
    }
    if let Some(p) = &map.name {
        if let Some(s) = walk_path(&claims, p).and_then(Value::as_str) {
            user.name = Some(s.to_string());
        }
    }
    if let Some(p) = &map.roles {
        if let Some(roles) = walk_path(&claims, p) {
            for s in extract_string_or_array(roles) {
                user.roles.push(Role::named(s));
            }
        }
    }
    if let Some(p) = &map.permissions {
        if let Some(perms) = walk_path(&claims, p) {
            for s in extract_string_or_array(perms) {
                user.permissions.push(Permission::new(s));
            }
        }
    }

    // Stash the entire raw claims object so app code can reach
    // provider-specific fields (Firebase identities, Clerk org_id,
    // …) via `user.claim("key")`. Move the map by value — claims
    // are typed `serde_json::Map<String, Value>` which is itself a
    // BTreeMap, so the conversion is essentially a rename.
    if let Value::Object(obj) = claims {
        user.claims = obj.into_iter().collect();
    }

    Ok(user)
}

fn extract_string_or_array(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}
