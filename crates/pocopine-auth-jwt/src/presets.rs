//! Provider presets — declarative `JwtConfig` constructors for
//! mainstream IdPs.
//!
//! Each preset is a one-screen function. Adding a new provider
//! generally means writing one of these, not a new crate.

#![cfg(all(not(target_arch = "wasm32"), feature = "presets"))]

use std::borrow::Cow;
use std::time::Duration;

use crate::config::{
    Algorithm, ClaimMap, ClaimPath, JwtConfig, KeySource, SecretBytes, TokenSource,
};
use crate::error::JwtAuthError;
use crate::verifier::JwtVerifier;

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const DEFAULT_LEEWAY: Duration = Duration::from_secs(60);

impl JwtVerifier {
    /// Firebase ID token verifier.
    ///
    /// `project_id` is the Firebase project id (from
    /// `firebase-config.json` `projectId`). Token sources include
    /// the `Authorization: Bearer` header and Firebase's
    /// `__session` SSR cookie. Roles/permissions claim paths are
    /// not set by default — Firebase puts custom claims under
    /// app-defined keys, so apps override `claim_map.roles` /
    /// `permissions` to point at their own paths.
    pub fn firebase(project_id: &str) -> Result<Self, JwtAuthError> {
        let config = JwtConfig {
            keys: KeySource::Jwks {
                url:
                    "https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com"
                        .into(),
                cache_ttl: DEFAULT_CACHE_TTL,
                refresh_cooldown: DEFAULT_REFRESH_COOLDOWN,
            },
            issuer: Some(format!("https://securetoken.google.com/{project_id}")),
            audience: Some(vec![project_id.to_string()]),
            algorithms: vec![Algorithm::Rs256],
            leeway: DEFAULT_LEEWAY,
            sources: vec![
                TokenSource::Bearer,
                TokenSource::Cookie(Cow::Borrowed("__session")),
            ],
            revocation: None,
            claim_map: ClaimMap {
                id: ClaimPath::from_str("sub"),
                email: Some(ClaimPath::from_str("email")),
                email_verified: Some(ClaimPath::from_str("email_verified")),
                name: Some(ClaimPath::from_str("name")),
                roles: None,
                permissions: None,
                scope: None,
            },
            required_scopes: vec![],
        };
        Self::custom(config)
    }

    /// Clerk session JWT verifier. `frontend_api` is the Clerk
    /// "Frontend API" URL (e.g. `https://my-app.clerk.accounts.dev`)
    /// — the JWKS path is at `/.well-known/jwks.json` under that
    /// origin.
    ///
    /// Clerk session tokens carry `org_role` for organization
    /// membership; the preset extracts that into `roles` for use by
    /// `require_admin` / `require_staff` shortcut guards.
    pub fn clerk(frontend_api: &str) -> Result<Self, JwtAuthError> {
        let frontend_api = frontend_api.trim_end_matches('/');
        let config = JwtConfig {
            keys: KeySource::Jwks {
                url: format!("{frontend_api}/.well-known/jwks.json"),
                cache_ttl: DEFAULT_CACHE_TTL,
                refresh_cooldown: DEFAULT_REFRESH_COOLDOWN,
            },
            issuer: Some(frontend_api.to_string()),
            audience: None,
            algorithms: vec![Algorithm::Rs256],
            leeway: DEFAULT_LEEWAY,
            sources: vec![TokenSource::Bearer],
            revocation: None,
            claim_map: ClaimMap {
                id: ClaimPath::from_str("sub"),
                email: Some(ClaimPath::from_str("email")),
                email_verified: None,
                name: Some(ClaimPath::from_str("name")),
                roles: Some(ClaimPath::from_str("org_role")),
                permissions: None,
                scope: None,
            },
            required_scopes: vec![],
        };
        Self::custom(config)
    }

    /// Auth0 access token verifier. `domain` is the Auth0 tenant
    /// domain (`my-tenant.auth0.com`); `audience` is the API
    /// identifier configured in Auth0 (matches the `aud` claim).
    ///
    /// Auth0 with RBAC enabled writes permissions to the
    /// `permissions` array claim — extracted by default.
    pub fn auth0(domain: &str, audience: &str) -> Result<Self, JwtAuthError> {
        let domain = domain.trim_end_matches('/');
        let config = JwtConfig {
            keys: KeySource::Jwks {
                url: format!("https://{domain}/.well-known/jwks.json"),
                cache_ttl: DEFAULT_CACHE_TTL,
                refresh_cooldown: DEFAULT_REFRESH_COOLDOWN,
            },
            issuer: Some(format!("https://{domain}/")),
            audience: Some(vec![audience.to_string()]),
            algorithms: vec![Algorithm::Rs256],
            leeway: DEFAULT_LEEWAY,
            sources: vec![TokenSource::Bearer],
            revocation: None,
            claim_map: ClaimMap {
                id: ClaimPath::from_str("sub"),
                email: Some(ClaimPath::from_str("email")),
                email_verified: Some(ClaimPath::from_str("email_verified")),
                name: Some(ClaimPath::from_str("name")),
                roles: None,
                permissions: Some(ClaimPath::from_str("permissions")),
                scope: Some(ClaimPath::from_str("scope")),
            },
            required_scopes: vec![],
        };
        Self::custom(config)
    }

    /// Supabase JWT verifier. The mainstream HS256 outlier — the
    /// secret is a project-wide shared key from the Supabase
    /// dashboard.
    ///
    /// Supabase tokens carry `aud: "authenticated"` for normal
    /// users.
    pub fn supabase(jwt_secret: impl Into<SecretBytes>) -> Result<Self, JwtAuthError> {
        let config = JwtConfig {
            keys: KeySource::Hmac {
                secret: jwt_secret.into(),
            },
            issuer: None,
            audience: Some(vec!["authenticated".into()]),
            algorithms: vec![Algorithm::Hs256],
            leeway: DEFAULT_LEEWAY,
            sources: vec![TokenSource::Bearer],
            revocation: None,
            claim_map: ClaimMap {
                id: ClaimPath::from_str("sub"),
                email: Some(ClaimPath::from_str("email")),
                email_verified: None,
                name: None,
                roles: Some(ClaimPath::from_str("role")),
                permissions: None,
                scope: None,
            },
            required_scopes: vec![],
        };
        Self::custom(config)
    }

    /// Pocopine-issued HS256 token verifier. Pairs with
    /// [`crate::JwtIssuer::pocopine`] for the first-party
    /// credential provider. Cookies named `pocopine_session` are
    /// accepted in addition to bearer tokens.
    pub fn pocopine(secret: impl Into<SecretBytes>) -> Result<Self, JwtAuthError> {
        let config = JwtConfig {
            keys: KeySource::Hmac {
                secret: secret.into(),
            },
            issuer: Some("pocopine".into()),
            audience: Some(vec!["pocopine".into()]),
            algorithms: vec![Algorithm::Hs256],
            leeway: DEFAULT_LEEWAY,
            sources: vec![
                TokenSource::Bearer,
                TokenSource::Cookie(Cow::Borrowed("pocopine_session")),
            ],
            revocation: None,
            claim_map: ClaimMap {
                id: ClaimPath::from_str("sub"),
                email: Some(ClaimPath::from_str("email")),
                email_verified: None,
                name: Some(ClaimPath::from_str("name")),
                roles: Some(ClaimPath::from_str("roles")),
                permissions: Some(ClaimPath::from_str("permissions")),
                scope: None,
            },
            required_scopes: vec![],
        };
        Self::custom(config)
    }
}
