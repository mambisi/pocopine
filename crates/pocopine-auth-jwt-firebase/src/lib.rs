//! Firebase Authentication ID-token verifier preset for
//! `pocopine-auth-jwt`.
//!
//! ```ignore
//! use pocopine_auth_jwt::JwtVerifier;
//! use pocopine_auth_jwt_firebase::Firebase;
//!
//! let verifier = JwtVerifier::from_provider(
//!     Firebase::new("my-project-id"),
//! )?;
//! ```
//!
//! Tokens are accepted from the `Authorization: Bearer <id-token>`
//! header by default. The `__session` SSR cookie source is gated
//! behind a deliberate opt-in because Firebase verifies session
//! cookies against a **different** JWKS endpoint than ID tokens —
//! the bundled URL only handles ID tokens. Apps that want SSR
//! session-cookie verification should configure a separate
//! verifier (or wait for the dedicated Firebase session-cookie
//! preset).
//!
//! Roles and permissions are not extracted by default. Firebase
//! puts custom roles under app-defined custom-claim paths, so
//! apps that want them override `claim_map.roles` /
//! `claim_map.permissions` to point at the right path on top of
//! `Firebase::new(...).jwt_config()`.
//!
//! ## Crate split rationale
//!
//! Firebase-specific code lives here, separate from
//! `pocopine-auth-jwt`, so:
//!
//! - The verifier engine has zero vendor coupling.
//! - This crate versions independently — a Firebase-specific fix
//!   doesn't bump the engine.
//! - The `pocopine-auth-jwt-<vendor>` naming convention from
//!   RFC-074 is symmetric: in-tree and community-maintained
//!   provider crates follow the same shape.
//!
//! Apps that use multiple providers (e.g. Firebase + Auth0) add
//! one dep per provider to their server `Cargo.toml` and call
//! `JwtVerifier::from_provider(...)` for each.

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use pocopine_auth_jwt::{
    Algorithm, ClaimMap, JwtAuthError, JwtConfig, KeySource, Provider, TokenSource,
};

/// Firebase ID-token JWKS endpoint. Only used for `id_token`
/// verification; session-cookie verification uses a different URL
/// (not handled by this preset — see module docs).
const FIREBASE_ID_TOKEN_JWKS: &str =
    "https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com";

/// Default JWKS cache TTL for Firebase. Google publishes the
/// signing keys with `Cache-Control: max-age` headers around an
/// hour; the JWKS resolver respects those when present.
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// Default cooldown between adversary-driven `kid`-miss refreshes.
/// 30 seconds matches the default in `JwksResolver`.
const DEFAULT_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

/// Default clock-skew leeway for `exp` / `iat` / `nbf`. Generous
/// because Firebase issues tokens from Google's clock, which can
/// skew vs the verifier host by tens of seconds.
const DEFAULT_LEEWAY: Duration = Duration::from_secs(60);

/// Firebase identity verifier configuration.
///
/// Construct with [`Firebase::new`] and pass to
/// [`pocopine_auth_jwt::JwtVerifier::from_provider`]. Override
/// individual fields via struct-update syntax:
///
/// ```ignore
/// use std::time::Duration;
/// use pocopine_auth_jwt_firebase::Firebase;
///
/// let firebase = Firebase {
///     leeway: Duration::from_secs(120),
///     ..Firebase::new("my-project-id")
/// };
/// ```
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Firebase {
    /// Firebase Console project id (from
    /// `firebase-config.json` `projectId`).
    pub project_id: String,
    /// Default JWKS cache TTL. The resolver also honors the
    /// upstream `Cache-Control: max-age` header when present.
    pub cache_ttl: Duration,
    /// Minimum interval between JWKS refreshes triggered by a
    /// `kid` cache miss; defends against adversary-driven thrash.
    pub refresh_cooldown: Duration,
    /// Clock-skew tolerance for `exp` / `iat` / `nbf`.
    pub leeway: Duration,
}

impl Firebase {
    /// Build a Firebase config with sane defaults. Most apps only
    /// override `project_id` via this constructor and use the
    /// resulting config directly.
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            cache_ttl: DEFAULT_CACHE_TTL,
            refresh_cooldown: DEFAULT_REFRESH_COOLDOWN,
            leeway: DEFAULT_LEEWAY,
        }
    }
}

impl Provider for Firebase {
    fn jwt_config(self) -> Result<JwtConfig, JwtAuthError> {
        Ok(JwtConfig {
            keys: KeySource::Jwks {
                url: FIREBASE_ID_TOKEN_JWKS.to_string(),
                cache_ttl: self.cache_ttl,
                refresh_cooldown: self.refresh_cooldown,
            },
            issuer: Some(format!(
                "https://securetoken.google.com/{}",
                self.project_id
            )),
            audience: Some(vec![self.project_id]),
            algorithms: vec![Algorithm::Rs256],
            leeway: self.leeway,
            sources: vec![TokenSource::Bearer],
            revocation: None,
            claim_map: ClaimMap::oidc(),
            required_scopes: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firebase_config_uses_correct_iss_and_aud_for_project() {
        let cfg = Firebase::new("my-cool-project").jwt_config().unwrap();
        assert_eq!(
            cfg.issuer.as_deref(),
            Some("https://securetoken.google.com/my-cool-project")
        );
        assert_eq!(
            cfg.audience.as_deref(),
            Some(&["my-cool-project".to_string()][..])
        );
    }

    #[test]
    fn firebase_pins_rs256_only() {
        let cfg = Firebase::new("p").jwt_config().unwrap();
        assert_eq!(cfg.algorithms, vec![Algorithm::Rs256]);
    }

    #[test]
    fn firebase_uses_id_token_jwks_endpoint() {
        let cfg = Firebase::new("p").jwt_config().unwrap();
        match cfg.keys {
            KeySource::Jwks { url, .. } => {
                assert_eq!(url, FIREBASE_ID_TOKEN_JWKS);
            }
            _ => panic!("expected JWKS key source for Firebase"),
        }
    }

    #[test]
    fn firebase_only_accepts_bearer_by_default() {
        let cfg = Firebase::new("p").jwt_config().unwrap();
        assert_eq!(cfg.sources.len(), 1);
        assert!(matches!(cfg.sources[0], TokenSource::Bearer));
    }

    #[test]
    fn firebase_validate_passes_construction_invariants() {
        // The materialized config must pass the same validate()
        // checks the verifier runs — RS256 + JWKS, RS256 in
        // whitelist, sources non-empty.
        Firebase::new("p")
            .jwt_config()
            .unwrap()
            .validate()
            .expect("Firebase preset must be construction-time valid");
    }
}
