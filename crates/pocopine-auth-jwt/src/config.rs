//! Verifier configuration: `JwtConfig`, `KeySource`, `ClaimMap`,
//! `TokenSource`, `Algorithm`.
//!
//! All construction-time invariants (algorithm pinning, JWKS+HMAC
//! incompatibility, etc.) are enforced by [`JwtConfig::validate`].
//! Provider preset constructors call validate and panic on a violation
//! so the bundled configs are guaranteed correct.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use crate::error::JwtAuthError;

/// Algorithms the verifier knows how to handle. **Notably absent:
/// `none`.** A token signed with `alg: none` is impossible to
/// represent in this enum, so it cannot be in any whitelist —
/// algorithm pinning is enforced by construction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Algorithm {
    /// HMAC SHA-256.
    Hs256,
    /// HMAC SHA-384.
    Hs384,
    /// HMAC SHA-512.
    Hs512,
    /// RSA SHA-256 (most common for OIDC providers).
    Rs256,
    /// RSA SHA-384.
    Rs384,
    /// RSA SHA-512.
    Rs512,
    /// ECDSA P-256 SHA-256.
    Es256,
    /// ECDSA P-384 SHA-384.
    Es384,
}

impl Algorithm {
    /// Stable string label for `tracing` events and error messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            Algorithm::Hs256 => "HS256",
            Algorithm::Hs384 => "HS384",
            Algorithm::Hs512 => "HS512",
            Algorithm::Rs256 => "RS256",
            Algorithm::Rs384 => "RS384",
            Algorithm::Rs512 => "RS512",
            Algorithm::Es256 => "ES256",
            Algorithm::Es384 => "ES384",
        }
    }

    /// Whether this algorithm uses an HMAC shared secret (vs a
    /// public key from a JWKS).
    pub fn is_hmac(&self) -> bool {
        matches!(self, Algorithm::Hs256 | Algorithm::Hs384 | Algorithm::Hs512)
    }

    /// Whether this algorithm uses asymmetric public-key crypto.
    pub fn is_asymmetric(&self) -> bool {
        !self.is_hmac()
    }
}

/// HMAC secret bytes. Wrapped to suppress `Debug` / `Display`, and
/// best-effort zeroed when the last shared reference is dropped.
///
/// Credentials shouldn't accidentally land in logs or core dumps.
/// The zeroing is best-effort — Rust doesn't guarantee no copies
/// of the bytes exist elsewhere (e.g. inside `jsonwebtoken`'s
/// internal HMAC state during verification). Treat this as defense-
/// in-depth, not a hardware-grade secret store.
#[derive(Clone)]
pub struct SecretBytes(Arc<ZeroingBytes>);

struct ZeroingBytes(Vec<u8>);

impl Drop for ZeroingBytes {
    fn drop(&mut self) {
        // Volatile-write zeros over the buffer so optimizers don't
        // elide the wipe on the assumption the memory's about to
        // be freed. `core::ptr::write_volatile` is the standard
        // pattern for this; equivalent to `zeroize` crate's basic
        // policy without the dep.
        for byte in self.0.iter_mut() {
            unsafe {
                core::ptr::write_volatile(byte, 0);
            }
        }
    }
}

impl SecretBytes {
    /// Build from a byte buffer. The buffer is moved into an
    /// `Arc`-shared interior so callers can't retain a separate
    /// view that survives drop-zeroing.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Arc::new(ZeroingBytes(bytes.into())))
    }

    /// View of the secret. Limit the call site to the verification
    /// step; never log the result.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0 .0
    }
}

impl<S: Into<Vec<u8>>> From<S> for SecretBytes {
    fn from(value: S) -> Self {
        Self::new(value)
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBytes(<redacted>)")
    }
}

/// Where the verifier loads its signing keys from.
#[derive(Clone, Debug)]
pub enum KeySource {
    /// Fetch a JWKS document from a URL, cache it, and refresh on
    /// `kid`-miss. The 99% case for asymmetric OIDC issuers
    /// (Firebase, Clerk, Auth0, …).
    Jwks {
        /// HTTPS endpoint. The verifier never lowers HTTPS to HTTP;
        /// custom configs that want HTTP for tests must opt in
        /// explicitly through this string.
        url: String,
        /// Default cache TTL when the response carries no
        /// `Cache-Control: max-age` directive.
        cache_ttl: Duration,
        /// Minimum interval between JWKS refreshes triggered by a
        /// `kid` miss. Defends against attacker-driven thrash where
        /// random `kid` values force the verifier to hammer the
        /// JWKS endpoint.
        refresh_cooldown: Duration,
    },
    /// Pre-supplied JWKS document. Useful for offline tests and
    /// air-gapped deployments where the verifier can't reach the
    /// IdP.
    StaticJwks {
        /// Raw JWKS JSON document.
        document: String,
    },
    /// HMAC shared secret. The Supabase mainstream outlier and the
    /// substrate for `pocopine-auth-credentials`.
    Hmac { secret: SecretBytes },
}

impl KeySource {
    /// Whether this source supplies HMAC secrets.
    fn supplies_hmac(&self) -> bool {
        matches!(self, KeySource::Hmac { .. })
    }

    /// Whether this source supplies asymmetric public keys.
    fn supplies_asymmetric(&self) -> bool {
        matches!(self, KeySource::Jwks { .. } | KeySource::StaticJwks { .. })
    }
}

/// Where the verifier looks for the token in an incoming request.
#[derive(Clone, Debug)]
pub enum TokenSource {
    /// `Authorization: Bearer <token>`. Universal across providers.
    Bearer,
    /// Named cookie. Firebase SSR uses `__session`; pocopine's own
    /// credential provider uses `pocopine_session`.
    Cookie(Cow<'static, str>),
}

/// Dotted path through a JSON claims object.
///
/// `["firebase", "sign_in_provider"]` walks
/// `claims["firebase"]["sign_in_provider"]`. The walker stops at any
/// non-object intermediate node and returns `None`.
#[derive(Clone, Debug, Default)]
pub struct ClaimPath {
    pub segments: Vec<String>,
}

impl ClaimPath {
    /// Build a path from `.`-separated dotted notation.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(path: &str) -> Self {
        Self {
            segments: path.split('.').map(String::from).collect(),
        }
    }

    /// Build from explicit segments.
    pub fn segments(segments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            segments: segments.into_iter().map(Into::into).collect(),
        }
    }
}

/// How to project verified claims onto an `AuthUser`.
///
/// The `id` path is required and must resolve to a string. All other
/// paths are optional. Defaults match standard OIDC; provider
/// presets override paths that providers put elsewhere
/// (e.g. Clerk's `org_role`).
#[derive(Clone, Debug)]
pub struct ClaimMap {
    /// Path to the user id. OIDC default: `sub`.
    pub id: ClaimPath,
    /// Path to email.
    pub email: Option<ClaimPath>,
    /// Path to email-verified flag.
    pub email_verified: Option<ClaimPath>,
    /// Path to display name.
    pub name: Option<ClaimPath>,
    /// Path to roles. Resolved value may be a string, an array of
    /// strings, or absent.
    pub roles: Option<ClaimPath>,
    /// Path to permissions. Resolved value may be an array
    /// (Auth0-style RBAC) or absent.
    pub permissions: Option<ClaimPath>,
    /// Path to the space-separated `scope` claim. Used when
    /// `JwtConfig::required_scopes` is non-empty.
    pub scope: Option<ClaimPath>,
}

impl ClaimMap {
    /// Standard OIDC mapping: `sub` → id, `email` → email,
    /// `email_verified` → email_verified, `name` → name. Roles
    /// and permissions are not extracted by default — apps that
    /// need them override the path.
    pub fn oidc() -> Self {
        Self {
            id: ClaimPath::from_str("sub"),
            email: Some(ClaimPath::from_str("email")),
            email_verified: Some(ClaimPath::from_str("email_verified")),
            name: Some(ClaimPath::from_str("name")),
            roles: None,
            permissions: None,
            scope: Some(ClaimPath::from_str("scope")),
        }
    }
}

impl Default for ClaimMap {
    fn default() -> Self {
        Self::oidc()
    }
}

/// Hook for revocation checks. Default JWT verification is
/// stateless; high-security apps that want a denylist or an
/// IdP-side revocation API plug it in here.
pub trait RevocationCheck: Send + Sync + 'static {
    /// Return `true` to reject the token. Receives both the verified
    /// `claims` (post-validation) and the raw token id (`jti` claim
    /// extracted into a string, if present) to keep the trait simple.
    fn is_revoked(&self, claims: &serde_json::Value) -> bool;
}

/// Verifier configuration. Construct via a preset
/// (`JwtVerifier::firebase`, `clerk`, `auth0`, `supabase`,
/// `pocopine`) or by hand for custom OIDC issuers.
///
/// Doesn't implement `Debug` because the `revocation` field is a
/// trait object and the `keys` field can hold an HMAC secret. Use
/// `JwtConfig::summary` (TODO) if you need to log the shape
/// without the secret.
#[derive(Clone)]
pub struct JwtConfig {
    /// Where signing keys come from.
    pub keys: KeySource,
    /// Required `iss` claim. Validation is strict equality.
    pub issuer: Option<String>,
    /// Required `aud` claim values. The token's `aud` may be a
    /// single string or an array; validation succeeds when the
    /// configured set and the token's set intersect non-emptily.
    pub audience: Option<Vec<String>>,
    /// Algorithms accepted on a verified token. **Pinned** —
    /// algorithm-confusion attacks (RS256 token replayed as HS256)
    /// are rejected here before the key is fetched.
    pub algorithms: Vec<Algorithm>,
    /// Clock skew tolerance for `exp` / `iat` / `nbf`.
    pub leeway: Duration,
    /// Where to look for the token. First-match-wins.
    pub sources: Vec<TokenSource>,
    /// Optional revocation hook. None = stateless verification.
    pub revocation: Option<Arc<dyn RevocationCheck>>,
    /// How to project claims onto `AuthUser`.
    pub claim_map: ClaimMap,
    /// Required scopes. Non-empty enables scope validation against
    /// the resolved `scope` claim (space-separated).
    pub required_scopes: Vec<String>,
}

impl JwtConfig {
    /// Validate construction-time invariants. Called by every
    /// preset constructor and by `JwtVerifier::custom`. Returns
    /// [`JwtAuthError::InvalidConfig`] on violation.
    ///
    /// The invariants:
    ///
    /// 1. **Algorithm whitelist non-empty.** An empty whitelist
    ///    rejects every token, which is unlikely to be intentional.
    /// 2. **No mixing HMAC and asymmetric.** A `KeySource::Jwks`
    ///    paired with `Algorithm::Hs256` would be the classic
    ///    JWT-confusion attack surface (attacker takes the
    ///    verifier's RSA public key, treats it as an HMAC secret,
    ///    forges a token). Mirrored: `KeySource::Hmac` paired with
    ///    `Algorithm::Rs256` would silently fail at verification
    ///    time, but it's caught here so misconfigurations surface
    ///    at deployment, not first request.
    /// 3. **At least one token source.** A config with no sources
    ///    can't extract a token from any request.
    pub fn validate(&self) -> Result<(), JwtAuthError> {
        if self.algorithms.is_empty() {
            return Err(JwtAuthError::InvalidConfig {
                reason: "algorithm whitelist is empty".into(),
            });
        }
        if self.sources.is_empty() {
            return Err(JwtAuthError::InvalidConfig {
                reason: "token source list is empty".into(),
            });
        }
        if self.keys.supplies_asymmetric() && self.algorithms.iter().any(Algorithm::is_hmac) {
            return Err(JwtAuthError::InvalidConfig {
                reason: "asymmetric KeySource (Jwks/StaticJwks) cannot accept HMAC \
                         algorithms — this is the classic JWT-confusion attack \
                         shape; use KeySource::Hmac for HS-family algorithms"
                    .into(),
            });
        }
        if self.keys.supplies_hmac() && self.algorithms.iter().any(Algorithm::is_asymmetric) {
            return Err(JwtAuthError::InvalidConfig {
                reason: "HMAC KeySource cannot accept asymmetric algorithms — \
                         use KeySource::Jwks or StaticJwks for RS/ES-family"
                    .into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_hmac() -> KeySource {
        KeySource::Hmac {
            secret: SecretBytes::new(b"01234567890123456789012345678901".to_vec()),
        }
    }

    fn dummy_jwks() -> KeySource {
        KeySource::Jwks {
            url: "https://example/jwks.json".into(),
            cache_ttl: Duration::from_secs(3600),
            refresh_cooldown: Duration::from_secs(30),
        }
    }

    fn base_config(keys: KeySource, alg: Algorithm) -> JwtConfig {
        JwtConfig {
            keys,
            issuer: Some("https://example".into()),
            audience: Some(vec!["test".into()]),
            algorithms: vec![alg],
            leeway: Duration::from_secs(60),
            sources: vec![TokenSource::Bearer],
            revocation: None,
            claim_map: ClaimMap::oidc(),
            required_scopes: vec![],
        }
    }

    #[test]
    fn validate_accepts_consistent_hmac_config() {
        base_config(dummy_hmac(), Algorithm::Hs256)
            .validate()
            .unwrap();
    }

    #[test]
    fn validate_accepts_consistent_jwks_config() {
        base_config(dummy_jwks(), Algorithm::Rs256)
            .validate()
            .unwrap();
    }

    #[test]
    fn validate_rejects_jwks_with_hmac_alg() {
        // The JWT-confusion attack surface: configured to fetch a
        // public key from JWKS but accept HS256 tokens. The HS256
        // verification would take the public key as an HMAC secret
        // — an attacker who knows the public key can forge tokens.
        let cfg = base_config(dummy_jwks(), Algorithm::Hs256);
        let err = cfg.validate().unwrap_err();
        assert!(
            matches!(&err, JwtAuthError::InvalidConfig { reason } if reason.contains("JWT-confusion")),
            "got: {err:?}"
        );
    }

    #[test]
    fn validate_rejects_hmac_with_asymmetric_alg() {
        let cfg = base_config(dummy_hmac(), Algorithm::Rs256);
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, JwtAuthError::InvalidConfig { .. }));
    }

    #[test]
    fn validate_rejects_empty_algorithm_whitelist() {
        let mut cfg = base_config(dummy_hmac(), Algorithm::Hs256);
        cfg.algorithms.clear();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, JwtAuthError::InvalidConfig { .. }));
    }

    #[test]
    fn validate_rejects_empty_token_source_list() {
        let mut cfg = base_config(dummy_hmac(), Algorithm::Hs256);
        cfg.sources.clear();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, JwtAuthError::InvalidConfig { .. }));
    }

    #[test]
    fn secret_bytes_debug_redacts() {
        let s = SecretBytes::new(b"mysupersecret".to_vec());
        assert_eq!(format!("{s:?}"), "SecretBytes(<redacted>)");
    }
}
