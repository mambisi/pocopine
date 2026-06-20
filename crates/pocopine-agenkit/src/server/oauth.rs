//! OAuth-backed provider credentials (roadmap W6.3) — the agenkit **adapter**
//! over the provider-neutral [`pocopine_auth_oauth`] mechanism.
//!
//! The OAuth flow itself (PKCE, code exchange, refresh, the token-store
//! contract) lives in `pocopine-auth-oauth`, so it is shared with login. This
//! module is the thin bridge that turns a principal's stored token into a
//! [`ProviderCredential::Bearer`] for an outbound model call: it maps the caller
//! [`Principal`] to the store's opaque `subject` (the user id), refreshes the
//! token near expiry (saving the result), and collapses the neutral
//! [`OAuthError`] into an [`AgenkitError`] at the §D10 boundary.
//!
//! The flow types are re-exported, so `pocopine_agenkit::server::{OAuthConfig,
//! OAuthToken, begin_authorization, …}` resolve as before.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use pocopine_auth::Principal;

use super::credentials::{ProviderCredential, ProviderCredentials};

// Re-export the neutral OAuth surface so the agenkit credential API is one stop.
pub use pocopine_auth_oauth::{
    Authorization, OAuthConfig, OAuthError, OAuthToken, OAuthTokenStore, StoreFuture,
    begin_authorization, complete_authorization, refresh,
};

/// Collapse a neutral [`OAuthError`] into an [`AgenkitError`]. The neutral error
/// already carries only stable, redacted detail (never a token-endpoint body),
/// so this is safe; the agenkit boundary further reduces it to a kind (§D10).
fn to_agenkit(err: OAuthError) -> AgenkitError {
    AgenkitError::provider(format!("oauth ({err})"))
}

/// A [`ProviderCredentials`] that resolves a principal's stored OAuth token to a
/// `Bearer` credential, refreshing it — and saving the result through the app's
/// [`OAuthTokenStore`] — when it is at/near expiry.
///
/// An [`OAuthConfig`] authenticates **one** identity provider, so in a
/// multi-provider app you **must** scope each resolver with [`for_provider`] —
/// otherwise its token would be handed to every provider (e.g. an Anthropic
/// OAuth token sent to the OpenAI endpoint). Unscoped is convenient only when
/// there is a single provider.
///
/// [`for_provider`]: OAuthCredentials::for_provider
pub struct OAuthCredentials<S> {
    config: OAuthConfig,
    store: S,
    refresh_margin: Duration,
    /// The provider id this resolver serves; `None` → every provider.
    provider: Option<String>,
}

impl<S: OAuthTokenStore> OAuthCredentials<S> {
    /// Build an OAuth-backed credential resolver. Tokens within 60s of expiry are
    /// refreshed proactively.
    pub fn new(config: OAuthConfig, store: S) -> Self {
        Self {
            config,
            store,
            refresh_margin: Duration::from_secs(60),
            provider: None,
        }
    }

    /// Scope this resolver to a single provider id — required when more than one
    /// provider is registered, so the token is never handed to the wrong API. A
    /// request to any other provider resolves to `None` (the provider's own
    /// default credential applies).
    pub fn for_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Override the proactive-refresh margin.
    pub fn refresh_margin(mut self, margin: Duration) -> Self {
        self.refresh_margin = margin;
        self
    }
}

impl<S: OAuthTokenStore> ProviderCredentials for OAuthCredentials<S> {
    fn resolve<'a>(
        &'a self,
        provider: &'a str,
        principal: &'a Principal,
    ) -> Pin<Box<dyn Future<Output = AgenkitResult<Option<ProviderCredential>>> + Send + 'a>> {
        Box::pin(async move {
            // Scoped resolver: a request for any other provider gets nothing from
            // here (its default credential applies), so one provider's OAuth token
            // is never sent to another's API.
            if let Some(scope) = &self.provider
                && scope != provider
            {
                return Ok(None);
            }
            // The store keys on an opaque subject — the authenticated user id.
            // An anonymous caller has none, so there's nothing to resolve.
            let Some(subject) = principal.user().map(|u| u.id.clone()) else {
                return Ok(None);
            };
            let Some(mut token) = self.store.load(&subject).await.map_err(to_agenkit)? else {
                // No token for this subject → fall back to the provider default.
                return Ok(None);
            };
            // Refresh proactively when near expiry. With no refresh token we fall
            // through and return the (possibly expired) access token, letting the
            // provider surface the auth failure.
            if token.is_expired(self.refresh_margin)
                && let Some(refresh_token) = &token.refresh_token
            {
                token = refresh(&self.config, refresh_token)
                    .await
                    .map_err(to_agenkit)?;
                self.store
                    .save(&subject, token.clone())
                    .await
                    .map_err(to_agenkit)?;
            }
            Ok(Some(ProviderCredential::Bearer(token.access_token)))
        })
    }
}
