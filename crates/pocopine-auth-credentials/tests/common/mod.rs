//! Test-only `UserStore` / `TokenStore` stubs.
//!
//! The credentials crate intentionally ships no default backend.
//!
//! Apps implement [`UserStore`] / [`TokenStore`] against their database of choice (see `docs/auth-credentials.md` for a Postgres + `sqlx` walkthrough). The integration tests still need a concrete pair to exercise the routes end-to-end, so they keep their own minimal in-memory implementation here.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use pocopine_auth::AuthUser;
use pocopine_auth_credentials::{
    PasswordCredentials, StoreError, TokenRecord, TokenStore, UserStore,
};
use serde_json::json;

/// The app-side user record. Apps implement [`PasswordCredentials`]
/// on whatever shape their database holds; here it's a tiny struct
/// the tests can construct directly. `login_id` is whatever the
/// configured [`pocopine_auth_credentials::LoginIdValidator`]
/// produced — in the default setup that's a normalized email.
///
/// `password_hash` is `Option<String>` to match the multi-credential
/// trait shape; the integration tests set it to `Some(...)` for
/// password users and could leave it `None` for an OAuth-only /
/// passkey-only account in the same table.
#[derive(Clone)]
pub(crate) struct TestUser {
    pub(crate) id: String,
    pub(crate) login_id: String,
    pub(crate) password_hash: Option<String>,
    pub(crate) email_verified: bool,
}

impl PasswordCredentials for TestUser {
    fn id(&self) -> &str {
        &self.id
    }
    fn login_id(&self) -> &str {
        &self.login_id
    }
    fn password_hash(&self) -> Option<&str> {
        self.password_hash.as_deref()
    }
    fn to_auth_user(&self) -> AuthUser {
        // For the email-shaped default config the login_id IS the
        // email; surface it as such so tokens carry an `email`
        // claim. Phone/username configurations would project
        // differently (claim("phone") = ..., name = ..., etc.).
        AuthUser::new(&self.id)
            .with_email(&self.login_id)
            .with_claim("email_verified", json!(self.email_verified))
    }
}

#[derive(Default)]
pub(crate) struct TestUserStore {
    inner: RwLock<TestUserStoreInner>,
}

impl TestUserStore {
    /// Test helper: seed an OAuth-only / passkey-only user (no
    /// password set) so the login flow can confirm the
    /// password-less branch falls through to InvalidCredentials.
    pub(crate) fn seed_passwordless(&self, login_id: &str) {
        let mut inner = self.inner.write().expect("test lock");
        let id = format!("u{}", inner.by_id.len() + 1);
        let user = TestUser {
            id: id.clone(),
            login_id: login_id.to_string(),
            password_hash: None,
            email_verified: true,
        };
        inner.by_login_id.insert(login_id.to_string(), id.clone());
        inner.by_id.insert(id, user);
    }
}

#[derive(Default)]
struct TestUserStoreInner {
    by_id: HashMap<String, TestUser>,
    /// Maps `login_id (already-normalized) → id`.
    by_login_id: HashMap<String, String>,
}

#[derive(Debug)]
struct DuplicateUser;

impl std::fmt::Display for DuplicateUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("user already exists")
    }
}

impl std::error::Error for DuplicateUser {}

#[async_trait]
impl UserStore for TestUserStore {
    type User = TestUser;

    async fn find_by_login_id(&self, login_id: &str) -> Result<Option<TestUser>, StoreError> {
        // The framework already normalized the value before
        // calling us; defense-in-depth case-insensitive lookup
        // belongs in production stores via column collations,
        // not here.
        let inner = self.inner.read().map_err(poisoned)?;
        Ok(inner
            .by_login_id
            .get(login_id)
            .and_then(|id| inner.by_id.get(id))
            .cloned())
    }

    async fn find_by_id(&self, id: &str) -> Result<Option<TestUser>, StoreError> {
        let inner = self.inner.read().map_err(poisoned)?;
        Ok(inner.by_id.get(id).cloned())
    }

    async fn create(&self, login_id: &str, password_hash: String) -> Result<TestUser, StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        if inner.by_login_id.contains_key(login_id) {
            return Err(Box::new(DuplicateUser));
        }
        // Tests want predictable ids: counter-based.
        let id = format!("u{}", inner.by_id.len() + 1);
        let user = TestUser {
            id: id.clone(),
            login_id: login_id.to_string(),
            password_hash: Some(password_hash),
            email_verified: false,
        };
        inner.by_login_id.insert(login_id.to_string(), id.clone());
        inner.by_id.insert(id, user.clone());
        Ok(user)
    }
}

#[derive(Default)]
pub(crate) struct TestTokenStore {
    inner: RwLock<HashMap<[u8; 32], TokenRecord>>,
}

#[async_trait]
impl TokenStore for TestTokenStore {
    async fn put(&self, hash: [u8; 32], record: TokenRecord) -> Result<(), StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        inner.insert(hash, record);
        Ok(())
    }

    async fn take(&self, hash: [u8; 32], now_ms: u64) -> Result<Option<TokenRecord>, StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        let Some(record) = inner.remove(&hash) else {
            return Ok(None);
        };
        if record.expires_at_ms <= now_ms {
            return Ok(None);
        }
        Ok(Some(record))
    }

    async fn purge_expired(&self, now_ms: u64) -> Result<usize, StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        let before = inner.len();
        inner.retain(|_, r| r.expires_at_ms > now_ms);
        Ok(before - inner.len())
    }
}

fn poisoned<T>(_err: std::sync::PoisonError<T>) -> StoreError {
    Box::<dyn std::error::Error + Send + Sync>::from("test store lock poisoned")
}
