//! Error taxonomy for JWT verification.
//!
//! Errors are intentionally specific so operators can alert on
//! classes (a spike in `KeyResolutionFailed` signals JWKS rotation
//! issues; a spike in `SignatureInvalid` signals possible attacks).
//!
//! `Display` and `Debug` impls deliberately omit token text so logs
//! and error messages don't leak credentials.

use std::fmt;

use pocopine_core::ServerError;

/// Failure reasons returned by the JWT verifier.
#[derive(Debug)]
pub enum JwtAuthError {
    /// Configuration is internally inconsistent — caught at
    /// `JwtConfig::validate()` time, not at verification time.
    ///
    /// Construction-time invariants live here: HMAC key paired with
    /// asymmetric algorithm, JWKS source paired with HMAC algorithm,
    /// empty algorithm whitelist, `none` algorithm in whitelist.
    InvalidConfig { reason: String },

    /// Token present but malformed — wrong segment count, header
    /// not parseable, or base64 decode failed.
    Malformed { reason: &'static str },

    /// Header `alg` is not in the configured whitelist. Includes
    /// the rejected alg name for ops alerting.
    AlgorithmRejected { got: String, allowed: Vec<String> },

    /// JWKS fetch failed or the requested `kid` could not be
    /// resolved after a refresh attempt.
    KeyResolutionFailed { reason: String },

    /// Cryptographic verification failed — signature does not match
    /// the resolved key, or the resolved key is the wrong shape for
    /// the alg.
    SignatureInvalid,

    /// One of `iss` / `aud` / `exp` / `iat` / `nbf` was rejected.
    ClaimRejected { claim: &'static str, reason: String },

    /// The configured required-scope set is not satisfied.
    ScopeMissing { required: String },

    /// `RevocationCheck::is_revoked` returned `true`.
    Revoked,

    /// The configured `ClaimMap` could not extract the user id from
    /// the verified claims.
    ClaimMapFailed { path: String },

    /// `JwtIssuer::sign` failed at runtime — clock unavailable, or
    /// the underlying signer rejected the claims/key. Distinct from
    /// `InvalidConfig` (which is a construction-time invariant
    /// violation).
    SigningFailed { reason: String },
}

impl JwtAuthError {
    /// Stable string label for the error class — used as the
    /// `error.kind` field in tracing events and observability
    /// metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            JwtAuthError::InvalidConfig { .. } => "invalid_config",
            JwtAuthError::Malformed { .. } => "malformed",
            JwtAuthError::AlgorithmRejected { .. } => "algorithm_rejected",
            JwtAuthError::KeyResolutionFailed { .. } => "key_resolution_failed",
            JwtAuthError::SignatureInvalid => "signature_invalid",
            JwtAuthError::ClaimRejected { .. } => "claim_rejected",
            JwtAuthError::ScopeMissing { .. } => "scope_missing",
            JwtAuthError::Revoked => "revoked",
            JwtAuthError::ClaimMapFailed { .. } => "claim_map_failed",
            JwtAuthError::SigningFailed { .. } => "signing_failed",
        }
    }
}

impl fmt::Display for JwtAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Token text is never included.
        match self {
            JwtAuthError::InvalidConfig { reason } => {
                write!(f, "invalid jwt config: {reason}")
            }
            JwtAuthError::Malformed { reason } => {
                write!(f, "malformed token: {reason}")
            }
            JwtAuthError::AlgorithmRejected { got, allowed } => {
                write!(
                    f,
                    "algorithm `{got}` rejected; allowed: {}",
                    allowed.join(", ")
                )
            }
            JwtAuthError::KeyResolutionFailed { reason } => {
                write!(f, "key resolution failed: {reason}")
            }
            JwtAuthError::SignatureInvalid => f.write_str("signature invalid"),
            JwtAuthError::ClaimRejected { claim, reason } => {
                write!(f, "claim `{claim}` rejected: {reason}")
            }
            JwtAuthError::ScopeMissing { required } => {
                write!(f, "scope `{required}` missing")
            }
            JwtAuthError::Revoked => f.write_str("token revoked"),
            JwtAuthError::ClaimMapFailed { path } => {
                write!(f, "claim map failed: could not extract `{path}`")
            }
            JwtAuthError::SigningFailed { reason } => {
                write!(f, "token signing failed: {reason}")
            }
        }
    }
}

impl std::error::Error for JwtAuthError {}

impl From<JwtAuthError> for ServerError {
    fn from(err: JwtAuthError) -> Self {
        // All verification failures map to Unauthorized at the
        // server-function boundary. Operators differentiate via the
        // tracing event's `error.kind` field, not the response
        // status.
        ServerError::unauthorized(err.to_string())
    }
}
