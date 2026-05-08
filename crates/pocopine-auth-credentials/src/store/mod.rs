//! Storage contracts for users and ephemeral tokens.
//!
//! This crate ships **only the trait shapes**. Concrete backends
//! (Postgres, SQLite, Redis, …) live with the app or in
//! community-maintained adapter crates so the framework doesn't
//! quietly couple to one storage flavor. See
//! `docs/auth-credentials.md` for an end-to-end example
//! (Postgres + `sqlx`).

pub mod token;

use async_trait::async_trait;
use std::error::Error;

#[allow(unused_imports)]
pub use token::{TokenKind, TokenRecord};

use crate::user::User;

/// Boxed storage error. Backends produce their own concrete error
/// type; the trait wraps it as a trait object so the generic surface
/// in [`crate::Credentials<S, T>`] doesn't grow another type
/// parameter.
pub type StoreError = Box<dyn Error + Send + Sync + 'static>;

/// User-record storage contract.
///
/// Implement this against your database of choice. The
/// `docs/auth-credentials.md` walkthrough shows a Postgres + `sqlx`
/// implementation; SQLite, Redis, or any custom backend works the
/// same way.
#[async_trait]
pub trait UserStore: Send + Sync + 'static {
    /// Return the user with `email`, if any. Implementations should
    /// match case-insensitively — the credentials crate folds to
    /// lower at signup, but defenders shouldn't rely on call-site
    /// casing.
    async fn find_by_email(&self, email: &str) -> Result<Option<User>, StoreError>;

    /// Return the user with `id`, if any.
    async fn find_by_id(&self, id: &str) -> Result<Option<User>, StoreError>;

    /// Insert `user`. Implementors MUST reject by error (not silently
    /// upsert) when a user with the same `id` or `email` already
    /// exists; the credentials crate maps the error to
    /// [`crate::CredentialsError::EmailTaken`] only for the `email`
    /// branch, but `id` collisions on a generated UUIDv7 are pathological.
    async fn create(&self, user: User) -> Result<(), StoreError>;

    /// Replace the user with the same `id`. Implementors update
    /// `updated_at_ms` from the supplied record (the caller has
    /// already bumped it).
    async fn update(&self, user: User) -> Result<(), StoreError>;

    /// Remove the user with `id`. Idempotent — deleting a missing
    /// id returns `Ok(())`.
    async fn delete(&self, id: &str) -> Result<(), StoreError>;
}

/// Ephemeral, hashed-token store for password-reset and
/// email-verification flows.
///
/// Tokens are stored under their SHA-256 hash. Even a full database
/// leak does not yield reusable tokens — the raw value only ever
/// lives in the email body and the redirect URL.
///
/// `take` is single-use: implementors MUST remove-and-return
/// atomically so a replay against the token can't re-confirm.
#[async_trait]
pub trait TokenStore: Send + Sync + 'static {
    /// Persist `record` under `token_hash`. Replaces any record
    /// previously stored under the same hash.
    async fn put(&self, token_hash: [u8; 32], record: TokenRecord) -> Result<(), StoreError>;

    /// Atomically remove and return the record at `token_hash`,
    /// if one exists and has not expired (`expires_at_ms > now_ms`).
    /// Expired records are silently dropped.
    async fn take(
        &self,
        token_hash: [u8; 32],
        now_ms: u64,
    ) -> Result<Option<TokenRecord>, StoreError>;

    /// Drop every record where `expires_at_ms <= now_ms`. Returns
    /// the number of records dropped.
    async fn purge_expired(&self, now_ms: u64) -> Result<usize, StoreError>;
}
