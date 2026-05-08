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
/// the tests can construct directly.
#[derive(Clone)]
pub(crate) struct TestUser {
    pub(crate) id: String,
    pub(crate) email: String,
    pub(crate) password_hash: String,
    pub(crate) email_verified: bool,
}

impl PasswordCredentials for TestUser {
    fn id(&self) -> &str {
        &self.id
    }
    fn email(&self) -> &str {
        &self.email
    }
    fn password_hash(&self) -> &str {
        &self.password_hash
    }
    fn to_auth_user(&self) -> AuthUser {
        AuthUser::new(&self.id)
            .with_email(&self.email)
            .with_claim("email_verified", json!(self.email_verified))
    }
}

#[derive(Default)]
pub(crate) struct TestUserStore {
    inner: RwLock<TestUserStoreInner>,
}

#[derive(Default)]
struct TestUserStoreInner {
    by_id: HashMap<String, TestUser>,
    /// Maps `email_lower → id`.
    by_email: HashMap<String, String>,
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

    async fn find_by_email(&self, email: &str) -> Result<Option<TestUser>, StoreError> {
        let inner = self.inner.read().map_err(poisoned)?;
        let lower = email.to_ascii_lowercase();
        Ok(inner
            .by_email
            .get(&lower)
            .and_then(|id| inner.by_id.get(id))
            .cloned())
    }

    async fn find_by_id(&self, id: &str) -> Result<Option<TestUser>, StoreError> {
        let inner = self.inner.read().map_err(poisoned)?;
        Ok(inner.by_id.get(id).cloned())
    }

    async fn create(&self, email: &str, password_hash: String) -> Result<TestUser, StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        let lower = email.to_ascii_lowercase();
        if inner.by_email.contains_key(&lower) {
            return Err(Box::new(DuplicateUser));
        }
        // Tests want predictable ids: counter-based.
        let id = format!("u{}", inner.by_id.len() + 1);
        let user = TestUser {
            id: id.clone(),
            email: lower.clone(),
            password_hash,
            email_verified: false,
        };
        inner.by_email.insert(lower, id.clone());
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
