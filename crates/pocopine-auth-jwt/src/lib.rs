//! JWT verification engine for `pocopine-auth`.
//!
//! Implements RFC-070's design: one verifier engine that supports
//! Firebase, Clerk, Auth0, Supabase, custom OIDC issuers, and
//! pocopine-issued HS256 tokens through declarative
//! [`JwtConfig`] presets — not per-provider crates.
//!
//! ```ignore
//! use pocopine_auth_jwt::JwtVerifier;
//! use pocopine_server::RouterAuthExt;
//! use pocopine_server::axum::Router;
//!
//! let verifier = JwtVerifier::firebase("my-project-id").unwrap();
//! let router: Router = Router::new();
//! // ... register server-function routes ...
//! let router = router.with_auth(verifier);
//! ```
//!
//! Construction-time invariants (algorithm pinning, JWKS+HMAC
//! incompatibility) are enforced by [`JwtConfig::validate`]; preset
//! constructors call it for you, so the bundled presets are
//! guaranteed correct against the JWT-confusion family of bugs.
//!
//! Diagnostics flow through `tracing`: failures emit a
//! `target: "pocopine.log"` warn-level event with `error.kind` and
//! a redacted message; counters land on `target: "pocopine.metric"`.

pub mod config;
pub mod error;

#[cfg(not(target_arch = "wasm32"))]
pub mod issuer;
#[cfg(not(target_arch = "wasm32"))]
pub mod jwks;
#[cfg(all(not(target_arch = "wasm32"), feature = "presets"))]
pub mod presets;
#[cfg(not(target_arch = "wasm32"))]
pub mod verifier;

pub use config::{
    Algorithm, ClaimMap, ClaimPath, JwtConfig, KeySource, RevocationCheck, SecretBytes, TokenSource,
};
pub use error::JwtAuthError;

#[cfg(not(target_arch = "wasm32"))]
pub use issuer::{IssuedClaims, JwtIssuer};
#[cfg(not(target_arch = "wasm32"))]
pub use verifier::JwtVerifier;
