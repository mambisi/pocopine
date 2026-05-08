//! [`Credentials<S, T>`] builder + [`ServerPlugin`] install.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use pocopine_auth_jwt::{
    Algorithm, ClaimMap, JwtConfig, JwtIssuer, KeySource, SecretBytes, TokenSource,
};
use pocopine_server::{Server, ServerPlugin};

use crate::error::CredentialsError;
use crate::id::default_id_generator;
use crate::password::{hash_password_sync, Argon2Params};
use crate::routes;
use crate::store::{TokenStore, UserStore};

/// Default issuer / audience strings for tryout / unconfigured apps.
const DEFAULT_ISSUER: &str = "pocopine";
const DEFAULT_AUDIENCE: &str = "pocopine";
/// Default mounted path for the credentials routes.
pub(crate) const ROUTE_PREFIX: &str = "/_pocopine/auth";
/// Default mount-base for the wasm app's typed login client (informational
/// only; the bridge ships in `pocopine-auth-client`).
pub(crate) const SESSION_TTL_DEFAULT_SECS: u64 = 60 * 60;
/// Default minimum password length.
pub(crate) const DEFAULT_MIN_PASSWORD_LEN: usize = 8;

type IdGen = Arc<dyn Fn() -> String + Send + Sync + 'static>;
type PasswordValidator =
    Arc<dyn Fn(&str) -> Result<(), &'static str> + Send + Sync + 'static>;

/// First-party credentials builder.
///
/// Generic over the [`UserStore`] and [`TokenStore`] backends; ships
/// as a [`ServerPlugin`] that mounts `/_pocopine/auth/{signup,login,logout}`
/// and provides the runtime service handlers extract through axum
/// state. See the crate-level rustdoc for the wiring example.
pub struct Credentials<S: UserStore, T: TokenStore> {
    secret: SecretBytes,
    store: S,
    tokens: T,
    issuer_name: String,
    audience: String,
    session_ttl: Duration,
    argon: Argon2Params,
    id_generator: IdGen,
    password_validator: PasswordValidator,
    cookie_name: Cow<'static, str>,
}

impl<S: UserStore, T: TokenStore> Credentials<S, T> {
    /// Build a credentials configuration with explicit dependencies.
    /// `secret` is the HS256 shared key for session JWTs; pair the
    /// same value through [`Self::verifier_config`] when constructing
    /// `JwtVerifier::custom(...)` for the auth middleware.
    pub fn new(secret: SecretBytes, store: S, tokens: T) -> Self {
        Self {
            secret,
            store,
            tokens,
            issuer_name: DEFAULT_ISSUER.to_string(),
            audience: DEFAULT_AUDIENCE.to_string(),
            session_ttl: Duration::from_secs(SESSION_TTL_DEFAULT_SECS),
            argon: Argon2Params::owasp_default(),
            id_generator: Arc::new(default_id_generator),
            password_validator: Arc::new(default_password_validator),
            cookie_name: Cow::Borrowed(pocopine_auth::SESSION_COOKIE),
        }
    }

    /// Override the session JWT lifetime.
    pub fn with_session_ttl(mut self, ttl: Duration) -> Self {
        self.session_ttl = ttl;
        self
    }

    /// Override argon2 parameters. `params` must pass
    /// [`Argon2Params::validate`] — weaker parameters are rejected
    /// at builder time.
    pub fn with_argon_params(
        mut self,
        params: Argon2Params,
    ) -> Result<Self, CredentialsError> {
        params
            .validate()
            .map_err(|_| CredentialsError::Configuration("argon2 params below OWASP minimum"))?;
        self.argon = params;
        Ok(self)
    }

    /// Override the issuer string (`iss` claim).
    pub fn with_issuer(mut self, name: impl Into<String>) -> Self {
        self.issuer_name = name.into();
        self
    }

    /// Override the audience string (`aud` claim).
    pub fn with_audience(mut self, name: impl Into<String>) -> Self {
        self.audience = name.into();
        self
    }

    /// Override the user-id generator. Default: millis-prefix +
    /// UUIDv7 (see [`default_id_generator`](crate::default_id_generator)).
    pub fn with_id_generator(
        mut self,
        f: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        self.id_generator = Arc::new(f);
        self
    }

    /// Override the password validator. Default: minimum 8 characters,
    /// no other checks. Apps that want NIST SP 800-63B-style checks
    /// or HIBP-k-anon hooks plug them in here.
    pub fn with_password_validator(
        mut self,
        f: impl Fn(&str) -> Result<(), &'static str> + Send + Sync + 'static,
    ) -> Self {
        self.password_validator = Arc::new(f);
        self
    }

    /// Override the session cookie name. Default: `pocopine_session`.
    pub fn with_cookie_name(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.cookie_name = name.into();
        self
    }

    /// Verifier config that pairs with this credential issuer. Pass
    /// it to `JwtVerifier::custom(...)` and install the resulting
    /// verifier on the router so request middleware can validate
    /// session tokens issued by this builder.
    ///
    /// `KeySource::Hmac` is built from the same secret used for
    /// signing — the issuer / audience / algorithm / Bearer source
    /// stay aligned by construction.
    pub fn verifier_config(&self) -> JwtConfig {
        let mut claim_map = ClaimMap::oidc();
        claim_map.roles = Some(pocopine_auth_jwt::ClaimPath::from_str("roles"));
        claim_map.permissions = Some(pocopine_auth_jwt::ClaimPath::from_str("permissions"));
        JwtConfig {
            keys: KeySource::Hmac {
                secret: self.secret.clone(),
            },
            issuer: Some(self.issuer_name.clone()),
            audience: Some(vec![self.audience.clone()]),
            algorithms: vec![Algorithm::Hs256],
            leeway: Duration::from_secs(60),
            sources: vec![
                TokenSource::Bearer,
                TokenSource::Cookie(self.cookie_name.clone()),
            ],
            revocation: None,
            claim_map,
            required_scopes: vec![],
        }
    }
}

/// Internal handle wrapping a configured [`Credentials`] for
/// sharing with axum handlers via `Arc`. Cheap to clone.
pub(crate) struct CredentialsHandle<S: UserStore, T: TokenStore> {
    pub(crate) issuer: JwtIssuer,
    pub(crate) store: S,
    /// Token store for password-reset / email-verification records.
    /// PR-3 wires the `/password/reset/*` and `/email/verify/*`
    /// routes against this; PR-2 stores the trait object so
    /// builder-time configuration is stable across the chain.
    #[allow(dead_code)]
    pub(crate) tokens: T,
    /// Default lifetime applied by the issuer for tokens signed
    /// without an explicit TTL. Fed to [`JwtIssuer::with_default_ttl`]
    /// during [`Credentials::install`] and read back here for
    /// future builder methods (e.g. cookie `Max-Age` defaults).
    #[allow(dead_code)]
    pub(crate) session_ttl: Duration,
    pub(crate) argon: Argon2Params,
    /// Pre-baked argon2id PHC string used by the constant-time login
    /// path: when the user lookup misses, we still call
    /// `verify_password(submitted, dummy_hash)` so the negative
    /// branch's CPU cost matches a wrong-password hit on a real
    /// user.
    pub(crate) dummy_hash: String,
    pub(crate) id_generator: IdGen,
    pub(crate) password_validator: PasswordValidator,
    /// Session cookie name. Read by future cookie-issuance paths
    /// (PR-3 `Set-Cookie` on session creation).
    #[allow(dead_code)]
    pub(crate) cookie_name: Cow<'static, str>,
    /// Issuer / audience strings — kept for symmetry with
    /// `verifier_config()` and for tracing fields on observability
    /// emit; the issuer is the source of truth for signing.
    #[allow(dead_code)]
    pub(crate) issuer_name: String,
    #[allow(dead_code)]
    pub(crate) audience: String,
}

impl<S: UserStore, T: TokenStore> ServerPlugin for Credentials<S, T> {
    fn name(&self) -> &'static str {
        "pocopine-auth-credentials"
    }

    fn install(self, server: Server) -> Server {
        // Pre-bake the dummy argon2id hash synchronously. `install`
        // typically runs inside the tokio runtime via
        // `Server::serve.await`, so spawning a nested runtime would
        // panic; one inline argon2 hash at startup is the
        // straightforward path. The dummy uses the configured params
        // so the timing of the constant-time-login negative branch
        // matches a wrong-password hit on a real user.
        let dummy_hash = match hash_password_sync("__pocopine_dummy_password__", &self.argon) {
            Ok(h) => h,
            Err(_) => {
                tracing::error!(
                    target: "pocopine.log",
                    "pocopine-auth-credentials: failed to pre-bake dummy argon2 hash; \
                     constant-time login path will be unavailable"
                );
                String::new()
            }
        };

        let issuer = JwtIssuer::hs256(
            self.secret.clone(),
            self.issuer_name.clone(),
            self.audience.clone(),
        )
        .with_default_ttl(self.session_ttl);

        let handle = Arc::new(CredentialsHandle {
            issuer,
            store: self.store,
            tokens: self.tokens,
            session_ttl: self.session_ttl,
            argon: self.argon,
            dummy_hash,
            id_generator: self.id_generator,
            password_validator: self.password_validator,
            cookie_name: self.cookie_name,
            issuer_name: self.issuer_name,
            audience: self.audience,
        });

        let router = routes::router(handle);
        server.router_mut(|r| r.nest(ROUTE_PREFIX, router))
    }
}

/// Default password validator: minimum length only. Apps override
/// via [`Credentials::with_password_validator`].
fn default_password_validator(password: &str) -> Result<(), &'static str> {
    if password.chars().count() < DEFAULT_MIN_PASSWORD_LEN {
        return Err("password must be at least 8 characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{StoreError, TokenStore, UserStore};
    use crate::user::User;
    use async_trait::async_trait;

    /// Bare-bones test stub — the crate doesn't ship a default
    /// in-memory backend, so the few tests that need a `Credentials`
    /// instance use this stub. It supports only the operations the
    /// builder-level tests touch (none of them, currently — the
    /// builder tests focus on validation and `verifier_config`).
    struct StubUserStore;
    struct StubTokenStore;

    #[async_trait]
    impl UserStore for StubUserStore {
        async fn find_by_email(&self, _: &str) -> Result<Option<User>, StoreError> {
            Ok(None)
        }
        async fn find_by_id(&self, _: &str) -> Result<Option<User>, StoreError> {
            Ok(None)
        }
        async fn create(&self, _: User) -> Result<(), StoreError> {
            Ok(())
        }
        async fn update(&self, _: User) -> Result<(), StoreError> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
    }

    #[async_trait]
    impl TokenStore for StubTokenStore {
        async fn put(
            &self,
            _: [u8; 32],
            _: crate::store::TokenRecord,
        ) -> Result<(), StoreError> {
            Ok(())
        }
        async fn take(
            &self,
            _: [u8; 32],
            _: u64,
        ) -> Result<Option<crate::store::TokenRecord>, StoreError> {
            Ok(None)
        }
        async fn purge_expired(&self, _: u64) -> Result<usize, StoreError> {
            Ok(0)
        }
    }

    fn fixed_secret() -> SecretBytes {
        SecretBytes::new(b"unit-test-shared-secret-32-bytes".to_vec())
    }

    #[test]
    fn default_password_validator_rejects_short_passwords() {
        assert!(default_password_validator("hunter2").is_err());
        assert!(default_password_validator("hunter22").is_ok());
        assert!(default_password_validator("really long password").is_ok());
    }

    #[test]
    fn verifier_config_has_hmac_key_and_pinned_alg() {
        let creds = Credentials::new(fixed_secret(), StubUserStore, StubTokenStore);
        let cfg = creds.verifier_config();
        assert_eq!(cfg.algorithms, vec![Algorithm::Hs256]);
        assert!(matches!(cfg.keys, KeySource::Hmac { .. }));
        assert_eq!(cfg.issuer.as_deref(), Some(DEFAULT_ISSUER));
        assert_eq!(
            cfg.audience.as_deref(),
            Some(&[DEFAULT_AUDIENCE.to_string()][..])
        );
        cfg.validate().unwrap();
    }

    #[test]
    fn now_ms_helper_sanity() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        assert!(ms > 1_700_000_000_000);
    }
}
