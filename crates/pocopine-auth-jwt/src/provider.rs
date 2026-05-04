//! `Provider` trait — declarative shape for vendor JWT presets.
//!
//! Per RFC-074 §5.2: a provider is a typed config struct that
//! materializes a [`JwtConfig`]. Third-party crates implement
//! [`Provider`] for their identity provider; users construct the
//! typed config and pass it to
//! [`JwtVerifier::from_provider`](crate::JwtVerifier::from_provider).
//!
//! Why a trait instead of plain functions:
//!
//! - **IDE discoverability.** Hover over `Firebase` and see every
//!   config field; `..Default::default()` works for partial
//!   overrides.
//! - **`#[non_exhaustive]` for safe field additions.** Adding an
//!   optional field on `Firebase` (e.g. `tenant: Option<String>`
//!   for Multi-Tenancy) doesn't break callers because they're
//!   forced to use struct-update syntax.
//! - **Builder methods belong on the config**, not on the verifier.
//!   Provider-specific knobs stay in the provider's namespace.
//!
//! The trait is sync — verifier construction shouldn't block on
//! the network. Runtime JWKS fetches are handled by
//! [`crate::jwks::JwksResolver`].

#![cfg(not(target_arch = "wasm32"))]

use crate::config::JwtConfig;
use crate::error::JwtAuthError;

/// Provider-specific config that knows how to build a
/// verifier-ready [`JwtConfig`].
///
/// Consumes `self` because providers are one-shot descriptions —
/// after `jwt_config()`, the owned strings (project_id, domain, …)
/// move into the resulting config.
pub trait Provider {
    /// Materialize the verifier config. May fail when the provider
    /// performs construction-time checks (e.g. parsing a static
    /// JWKS document); most return `Ok(...)` directly.
    fn jwt_config(self) -> Result<JwtConfig, JwtAuthError>;
}
