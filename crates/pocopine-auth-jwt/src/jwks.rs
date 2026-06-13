//! JWKS fetching, caching, and `kid`-miss refresh.
//!
//! The resolver exists so the verifier never re-fetches public keys
//! per-request: every IdP serves a JWKS document with multiple `kid`s,
//! caches it for an hour, and rotates keys overnight. We mirror that:
//! one HTTP fetch on first lookup or cache TTL expiry, then `kid`
//! lookups against the cached `JwkSet`.
//!
//! The cooldown defends against attacker-driven thrash: a client
//! sending tokens with random `kid` values would otherwise force
//! unbounded JWKS refetches.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use jsonwebtoken::jwk::JwkSet;
use tokio::sync::Mutex;

use crate::error::JwtAuthError;

/// Shared, thread-safe JWKS cache. Cloning is `Arc::clone`.
#[derive(Clone)]
pub struct JwksResolver {
    inner: Arc<Inner>,
}

struct Inner {
    url: String,
    cache_ttl: Duration,
    refresh_cooldown: Duration,
    /// Cached HTTP client. `reqwest::Client` is internally an
    /// `Arc<ClientRef>`, so cloning is cheap and the connection
    /// pool persists across fetches — building a fresh client per
    /// fetch defeated TLS keep-alive in the previous version.
    http: reqwest::Client,
    state: Mutex<JwksState>,
}

#[derive(Default)]
struct JwksState {
    /// Currently cached JWKS document (parsed).
    jwks: Option<JwkSet>,
    /// Wall-clock time the document was last fetched. Used to
    /// trigger TTL-driven re-fetches.
    last_fetch: Option<Instant>,
    /// Wall-clock time of the most recent attempted refresh
    /// (success or failure). Drives the cooldown.
    last_attempt: Option<Instant>,
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        // The default builder cannot fail on stable Rust targets
        // (only "build" return Result for compile-time-disabled
        // features). Falling back to the default client preserves
        // the resolver shape without panicking.
        .unwrap_or_default()
}

impl JwksResolver {
    /// Build a resolver. The first lookup triggers the first fetch
    /// — there's no eager warm-up.
    pub fn new(url: String, cache_ttl: Duration, refresh_cooldown: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                url,
                cache_ttl,
                refresh_cooldown,
                http: build_http_client(),
                state: Mutex::new(JwksState::default()),
            }),
        }
    }

    /// Build a resolver that's pre-loaded with a parsed JWKS. The
    /// resolver behaves as if a fetch just succeeded; subsequent
    /// `key_for` calls are cache hits until TTL expiry.
    /// `cache_ttl == u32::MAX seconds` paired with the right URL
    /// (or empty URL) makes a static-only resolver.
    pub(crate) fn with_seed(
        url: String,
        cache_ttl: Duration,
        refresh_cooldown: Duration,
        seed: JwkSet,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                url,
                cache_ttl,
                refresh_cooldown,
                http: build_http_client(),
                state: Mutex::new(JwksState {
                    jwks: Some(seed),
                    last_fetch: Some(Instant::now()),
                    last_attempt: None,
                }),
            }),
        }
    }

    /// Resolve a `kid` into a `DecodingKey`. The flow is:
    ///
    /// 1. If a cache exists and isn't expired and contains the kid,
    ///    return the key.
    /// 2. If the cooldown hasn't elapsed since the last refresh
    ///    attempt, return `KeyResolutionFailed("rate-limited")` —
    ///    don't hammer the JWKS endpoint.
    /// 3. Otherwise fetch fresh JWKS, replace the cache, retry the
    ///    `kid` lookup.
    pub async fn key_for(&self, kid: &str) -> Result<DecodingKey, JwtAuthError> {
        let mut state = self.inner.state.lock().await;
        let now = Instant::now();

        // Fast path: cache hit and not stale.
        if let Some(jwks) = state.jwks.as_ref() {
            let stale = state
                .last_fetch
                .is_none_or(|t| now.saturating_duration_since(t) >= self.inner.cache_ttl);
            if !stale && let Some(jwk) = jwks.find(kid) {
                return DecodingKey::from_jwk(jwk).map_err(|e| JwtAuthError::KeyResolutionFailed {
                    reason: format!("decode jwk for kid `{kid}`: {e}"),
                });
            }
        }

        // Slow path: refresh, but respect the cooldown.
        if let Some(last) = state.last_attempt
            && now.saturating_duration_since(last) < self.inner.refresh_cooldown
        {
            return Err(JwtAuthError::KeyResolutionFailed {
                reason: format!(
                    "kid `{kid}` not in cache and refresh rate-limited; \
                         most recent attempt was {:?} ago",
                    now.saturating_duration_since(last)
                ),
            });
        }
        state.last_attempt = Some(now);
        let fetched = fetch_jwks(&self.inner.http, &self.inner.url).await?;
        state.jwks = Some(fetched);
        state.last_fetch = Some(now);

        match state.jwks.as_ref().and_then(|jwks| jwks.find(kid)) {
            Some(jwk) => {
                DecodingKey::from_jwk(jwk).map_err(|e| JwtAuthError::KeyResolutionFailed {
                    reason: format!("decode jwk for kid `{kid}`: {e}"),
                })
            }
            None => Err(JwtAuthError::KeyResolutionFailed {
                reason: format!("kid `{kid}` not found after JWKS refresh"),
            }),
        }
    }
}

async fn fetch_jwks(http: &reqwest::Client, url: &str) -> Result<JwkSet, JwtAuthError> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|e| JwtAuthError::KeyResolutionFailed {
            reason: format!("fetch jwks from `{url}`: {e}"),
        })?;
    if !response.status().is_success() {
        return Err(JwtAuthError::KeyResolutionFailed {
            reason: format!("jwks endpoint `{url}` returned {}", response.status()),
        });
    }
    response
        .json::<JwkSet>()
        .await
        .map_err(|e| JwtAuthError::KeyResolutionFailed {
            reason: format!("parse jwks json from `{url}`: {e}"),
        })
}
