//! Test-only `UserStore` / `TokenStore` stubs.
//!
//! The credentials crate intentionally ships no default backend.
//!
//! Apps implement [`UserStore`] / [`TokenStore`] against their database of choice (see `docs/auth-credentials.md` for a Postgres + `sqlx` walkthrough). The integration tests still need a concrete pair to exercise the routes end-to-end, so they keep their own minimal in-memory implementation here.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use pocopine_auth_credentials::{StoreError, TokenRecord, TokenStore, User, UserStore};

#[derive(Default)]
pub(crate) struct TestUserStore {
    inner: RwLock<TestUserStoreInner>,
}

#[derive(Default)]
struct TestUserStoreInner {
    by_id: HashMap<String, User>,
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
    async fn find_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        let inner = self.inner.read().map_err(poisoned)?;
        let lower = email.to_ascii_lowercase();
        Ok(inner
            .by_email
            .get(&lower)
            .and_then(|id| inner.by_id.get(id))
            .cloned())
    }

    async fn find_by_id(&self, id: &str) -> Result<Option<User>, StoreError> {
        let inner = self.inner.read().map_err(poisoned)?;
        Ok(inner.by_id.get(id).cloned())
    }

    async fn create(&self, user: User) -> Result<(), StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        let lower = user.email.to_ascii_lowercase();
        if inner.by_email.contains_key(&lower) || inner.by_id.contains_key(&user.id) {
            return Err(Box::new(DuplicateUser));
        }
        inner.by_email.insert(lower, user.id.clone());
        inner.by_id.insert(user.id.clone(), user);
        Ok(())
    }

    async fn update(&self, user: User) -> Result<(), StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        if let Some(existing) = inner.by_id.get(&user.id) {
            let old_lower = existing.email.to_ascii_lowercase();
            let new_lower = user.email.to_ascii_lowercase();
            if old_lower != new_lower {
                inner.by_email.remove(&old_lower);
                inner.by_email.insert(new_lower, user.id.clone());
            }
        } else {
            inner
                .by_email
                .insert(user.email.to_ascii_lowercase(), user.id.clone());
        }
        inner.by_id.insert(user.id.clone(), user);
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let mut inner = self.inner.write().map_err(poisoned)?;
        if let Some(user) = inner.by_id.remove(id) {
            inner.by_email.remove(&user.email.to_ascii_lowercase());
        }
        Ok(())
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
