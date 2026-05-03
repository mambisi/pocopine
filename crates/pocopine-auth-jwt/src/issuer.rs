//! `JwtIssuer` — sign tokens with a configured key.
//!
//! Symmetric counterpart to `JwtVerifier` for the credential-
//! provider path: pocopine-issued tokens use the same key and
//! claim machinery the verifier uses. Only HS256 in v1; RS256
//! issuance lands when the credentials provider is finalized.

#![cfg(not(target_arch = "wasm32"))]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;

use crate::config::{Algorithm, SecretBytes};
use crate::error::JwtAuthError;

/// Standard JWT claim set issued by `JwtIssuer`. Apps add custom
/// claims by serializing into `extra` (any JSON-serializable type).
#[derive(Debug, Serialize)]
pub struct IssuedClaims<'a, T: Serialize> {
    /// Subject (user id).
    pub sub: &'a str,
    /// Issuer.
    pub iss: &'a str,
    /// Audience.
    pub aud: &'a str,
    /// Issued-at, Unix seconds.
    pub iat: u64,
    /// Expiry, Unix seconds.
    pub exp: u64,
    /// Optional JWT id (for revocation lookups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<&'a str>,
    /// Custom claims merged at the same level. Use `serde(flatten)`
    /// in your own type to lift fields up.
    #[serde(flatten)]
    pub extra: T,
}

/// HS256 token issuer.
#[derive(Clone)]
pub struct JwtIssuer {
    secret: SecretBytes,
    issuer: String,
    audience: String,
    default_ttl: Duration,
}

impl JwtIssuer {
    /// Build an HS256 issuer for pocopine-issued tokens. The issuer
    /// and audience are application-defined strings; for the
    /// credential provider they default to `"pocopine"`.
    pub fn hs256(
        secret: impl Into<SecretBytes>,
        issuer: impl Into<String>,
        audience: impl Into<String>,
    ) -> Self {
        Self {
            secret: secret.into(),
            issuer: issuer.into(),
            audience: audience.into(),
            default_ttl: Duration::from_secs(60 * 60),
        }
    }

    /// Override the default token lifetime.
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// Sign a token for `subject` with the default TTL.
    pub fn sign<T: Serialize>(&self, subject: &str, extra: T) -> Result<String, JwtAuthError> {
        self.sign_with_ttl(subject, extra, self.default_ttl)
    }

    /// Sign a token for `subject` with an explicit TTL.
    pub fn sign_with_ttl<T: Serialize>(
        &self,
        subject: &str,
        extra: T,
        ttl: Duration,
    ) -> Result<String, JwtAuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_err(|e| JwtAuthError::SigningFailed {
                reason: format!("system clock before unix epoch: {e}"),
            })?;
        let claims = IssuedClaims {
            sub: subject,
            iss: &self.issuer,
            aud: &self.audience,
            iat: now,
            exp: now.saturating_add(ttl.as_secs()),
            jti: None,
            extra,
        };
        let header = Header::new(Algorithm::Hs256.into_jwt());
        encode(
            &header,
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| JwtAuthError::SigningFailed {
            reason: e.to_string(),
        })
    }
}
