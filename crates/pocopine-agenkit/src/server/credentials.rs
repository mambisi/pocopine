//! Provider credentials (roadmap W6) — **host-only**.
//!
//! A [`ProviderCredential`] is the secret a provider uses to authenticate one
//! request (an API key, or an OAuth access token). W6 moves credentials from
//! "one static key baked into the provider at startup" to a
//! [`ProviderCredentials`] store that resolves the credential **per request**,
//! optionally per [`Principal`] (per-user "bring your own key").
//!
//! Pocopine ships the *trait*, an env-backed *default*, and (W6.3) the OAuth
//! *mechanism* — but never stores the secret itself. The app owns the store
//! (backed by env, a database, or a secrets manager). This is the §D10 boundary
//! and the [no-app-secret-storage] rule: the deploy-time `credentials.toml` is a
//! separate concern (it authenticates the *deploy* process to a host), and these
//! types are host-only — they never appear in a client payload or `agenkit-core`.

use std::future::Future;
use std::pin::Pin;

use pocopine_agenkit_core::AgenkitResult;
use pocopine_auth::Principal;

// The in-memory secret primitive (redacted Debug/Display, zeroized on drop)
// lives in `pocopine-crypto` so OAuth tokens and provider credentials share it.
pub use pocopine_crypto::SecretString;

/// A resolved credential ready to authenticate one provider request.
///
/// Host-only and never serialized: it carries a [`SecretString`], so even a
/// stray `Debug` stays redacted. `#[non_exhaustive]` so new schemes (e.g. a
/// cloud signature) can be added without breaking provider matches.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ProviderCredential {
    /// A static provider API key, sent via the provider's *native* scheme
    /// (Anthropic `x-api-key`, OpenAI `Authorization: Bearer`).
    ApiKey(SecretString),
    /// An OAuth access token, sent as `Authorization: Bearer` regardless of
    /// provider (W6.3).
    Bearer(SecretString),
}

impl ProviderCredential {
    /// A static API-key credential from a plain string.
    pub fn api_key(value: impl Into<String>) -> Self {
        Self::ApiKey(SecretString::new(value))
    }

    /// A bearer (OAuth access) token credential from a plain string.
    pub fn bearer(value: impl Into<String>) -> Self {
        Self::Bearer(SecretString::new(value))
    }
}

/// The object-safe return shape for the async [`ProviderCredentials::resolve`].
type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = AgenkitResult<Option<ProviderCredential>>> + Send + 'a>>;

/// Resolves the credential for a provider at request time, optionally per
/// [`Principal`] (per-user BYOK).
///
/// **The app implements this**, backed by env, a database, or a secrets manager.
/// Pocopine never stores the secret itself. Returning `Ok(None)` means "no
/// credential for this provider/principal" and falls back to the provider's
/// built-in credential — so a store can override only the principals it knows.
pub trait ProviderCredentials: Send + Sync + 'static {
    /// Resolve the credential for `provider` (its alias, e.g. `"anthropic"`)
    /// acting as `principal`. `Ok(None)` ⇒ use the provider's built-in default.
    fn resolve<'a>(&'a self, provider: &'a str, principal: &'a Principal) -> ResolveFuture<'a>;
}

/// The environment variable an [`EnvCredentials`] store reads for a provider
/// alias: the uppercased alias plus `_API_KEY` (e.g. `"anthropic"` →
/// `ANTHROPIC_API_KEY`).
fn env_var_name(provider: &str) -> String {
    format!("{}_API_KEY", provider.to_ascii_uppercase())
}

/// The default store: read [`env_var_name`] from the environment,
/// principal-independent — i.e. today's behaviour. Returns `None` when unset, so
/// the provider's built-in credential still applies.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvCredentials;

impl ProviderCredentials for EnvCredentials {
    fn resolve<'a>(&'a self, provider: &'a str, _principal: &'a Principal) -> ResolveFuture<'a> {
        Box::pin(async move {
            let value = std::env::var(env_var_name(provider)).ok();
            Ok(value.map(ProviderCredential::api_key))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_credential_debug_stays_redacted() {
        // Deriving Debug is safe — the secret is a SecretString, redacted within.
        let cred = ProviderCredential::api_key("sk-PLANTED");
        assert!(!format!("{cred:?}").contains("PLANTED"));
        match cred {
            ProviderCredential::ApiKey(s) => assert_eq!(s.expose(), "sk-PLANTED"),
            _ => panic!("expected ApiKey"),
        }
    }

    #[test]
    fn env_var_name_uppercases_the_alias() {
        assert_eq!(env_var_name("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(env_var_name("openai"), "OPENAI_API_KEY");
        assert_eq!(env_var_name("openrouter"), "OPENROUTER_API_KEY");
    }

    #[tokio::test]
    async fn env_credentials_resolve_none_for_an_unset_provider() {
        // An unset provider resolves to None → the runtime falls back to the
        // provider's built-in credential. (The crate forbids `unsafe`, so we
        // don't mutate the environment; the var-name mapping is covered above and
        // the set-path is a trivial `env::var(..).ok().map(..)`.)
        let none = EnvCredentials
            .resolve(
                "provider-that-is-definitely-unset-xyz",
                &Principal::anonymous(),
            )
            .await
            .unwrap();
        assert!(none.is_none());
    }
}
