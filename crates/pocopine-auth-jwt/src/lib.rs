//! JWT verification engine for `pocopine-auth`.
//!
//! Implements the verifier slice of RFC-070: one engine that any
//! OIDC-shaped provider (or pocopine's own credential flow) can
//! plug into via a declarative [`JwtConfig`].
//!
//! Provider-specific presets implement the [`Provider`] trait
//! (RFC-074 §5.2). One in-tree preset ships today —
//! [`providers::Firebase`] — paired with a recorded-token
//! integration test (RFC-074 §5.3.1). Other vendors ship in their
//! own `pocopine-auth-jwt-<vendor>` crates following the same
//! convention; see `docs/auth-jwt-providers.md`.
//!
//! ```ignore
//! use pocopine_auth_jwt::{JwtVerifier, providers::Firebase};
//!
//! let verifier = JwtVerifier::from_provider(
//!     Firebase::new("my-project-id"),
//! )?;
//! ```
//!
//! For custom OIDC issuers without a preset, configure the
//! verifier directly:
//!
//! ```ignore
//! use std::time::Duration;
//! use pocopine_auth_jwt::{
//!     Algorithm, ClaimMap, JwtConfig, JwtVerifier, KeySource, TokenSource,
//! };
//!
//! let config = JwtConfig {
//!     keys: KeySource::Jwks {
//!         url: "https://example.com/.well-known/jwks.json".into(),
//!         cache_ttl: Duration::from_secs(3600),
//!         refresh_cooldown: Duration::from_secs(30),
//!     },
//!     issuer: Some("https://example.com/".into()),
//!     audience: Some(vec!["my-api".into()]),
//!     algorithms: vec![Algorithm::Rs256],
//!     leeway: Duration::from_secs(60),
//!     sources: vec![TokenSource::Bearer],
//!     revocation: None,
//!     claim_map: ClaimMap::oidc(),
//!     required_scopes: vec![],
//! };
//! let verifier = JwtVerifier::custom(config).unwrap();
//! ```
//!
//! Construction-time invariants (algorithm pinning, JWKS+HMAC
//! incompatibility) are enforced by [`JwtConfig::validate`], called
//! by [`JwtVerifier::custom`]. The validator rejects the
//! JWT-confusion family of bugs at config time.
//!
//! Diagnostics flow through `tracing`: failures emit a
//! `target: "pocopine.log"` warn-level event with `kind` and a
//! redacted message; counters land on `target: "pocopine.metric"`.

pub mod config;
pub mod error;

#[cfg(not(target_arch = "wasm32"))]
pub mod issuer;
#[cfg(not(target_arch = "wasm32"))]
pub mod jwks;
#[cfg(not(target_arch = "wasm32"))]
pub mod provider;
#[cfg(not(target_arch = "wasm32"))]
pub mod providers;
#[cfg(not(target_arch = "wasm32"))]
pub mod verifier;

pub use config::{
    Algorithm, ClaimMap, ClaimPath, JwtConfig, KeySource, RevocationCheck, SecretBytes, TokenSource,
};
pub use error::JwtAuthError;

#[cfg(not(target_arch = "wasm32"))]
pub use issuer::{IssuedClaims, JwtIssuer};
#[cfg(not(target_arch = "wasm32"))]
pub use provider::Provider;
#[cfg(not(target_arch = "wasm32"))]
pub use verifier::JwtVerifier;
