//! Storage contracts for users and ephemeral tokens.
//!
//! The credentials crate doesn't own the "user" concept — apps do.
//! [`UserStore`] is generic over an app-supplied [`PasswordCredentials`]
//! impl, so the same record an app already has in its database
//! (Postgres row, Mongo document, custom struct, …) plays the role
//! of the credentialed account. The framework only needs the four
//! identity hooks the trait exposes; everything else (display name,
//! avatar, tenant id, profile fields, OAuth-linked accounts) is the
//! app's concern.
//!
//! See `docs/auth-credentials.md` for an end-to-end Postgres + `sqlx`
//! example.

pub mod token;

use async_trait::async_trait;
use std::error::Error;

#[allow(unused_imports)]
pub use token::{TokenKind, TokenRecord};

use pocopine_auth::AuthUser;

/// Boxed storage error. Backends produce their own concrete error
/// type; the trait wraps it as a trait object so the generic surface
/// in [`crate::Credentials<S, T>`] doesn't grow another type
/// parameter.
pub type StoreError = Box<dyn Error + Send + Sync + 'static>;

/// What the credentials crate needs to read off the app's user
/// record to verify a password and issue a session token.
///
/// Apps implement this on their own user type (Postgres row, custom
/// struct, etc.). Everything outside the four trait methods —
/// profile fields, OAuth-linked-provider columns, passkey
/// public-key list, audit timestamps — lives on the app's record
/// untouched.
///
/// ## Scope
///
/// This crate is **email + password specifically**. The Django-shaped
/// path: users sign up with an email address, hash a password,
/// authenticate against that pair. Phone-OTP, username-based social
/// auth, OAuth, passkeys are **different credential types** and
/// belong in sibling crates implementing their own traits
/// (`PhoneOtpCredentials`, `GoogleOAuthCredentials`,
/// `PasskeyCredentials`, …) on the **same** `AppUser` record.
///
/// ## Multiple credential types per user
///
/// Real apps often link more than one auth method to a single user
/// (email + password and Google OAuth and phone OTP, all → the
/// same `user_id`). [`PasswordCredentials`] is **one credential
/// type** in that mix; future sibling crates ship traits for the
/// others. Apps implement multiple on the same `AppUser` struct as
/// their database picks up the corresponding columns / link tables.
///
/// To make this work, [`password_hash`](Self::password_hash) returns
/// **`Option<&str>`** — `None` means "this user exists but doesn't
/// have a password set" (signed up via OAuth / passkey only). The
/// login flow treats `None` like "user not found": it runs the
/// constant-time dummy hash and returns
/// [`crate::CredentialsError::InvalidCredentials`], so a probe
/// against the login route can't distinguish "no password" from
/// "wrong password" or "no account".
///
/// ## Privacy
///
/// `password_hash` is a secret. Never log it. The app's `Debug` impl
/// for the user type SHOULD redact it; the credentials crate's own
/// types do (the signup/login response builds an `AuthUser`
/// projection that never serializes the hash).
pub trait PasswordCredentials: Send + Sync + 'static {
    /// Stable identifier — used as the JWT `sub` claim. Independent
    /// of the email (which can change when the user updates their
    /// address; this can't).
    fn id(&self) -> &str;

    /// The user's already-normalized email address (lowercase,
    /// trimmed). The framework matches this byte-for-byte against
    /// the normalized form of the login request input.
    fn email(&self) -> &str;

    /// Argon2id PHC string for the user's current password.
    /// **`None` for users without a password set** (OAuth-only,
    /// passkey-only, OTP-only). The framework treats `None` like
    /// "no such user" — the login flow falls through to the
    /// constant-time dummy hash and returns
    /// [`crate::CredentialsError::InvalidCredentials`], so an
    /// attacker probing for "is this email an OAuth-only account?"
    /// gets the same response shape as "is this email registered?".
    fn password_hash(&self) -> Option<&str>;

    /// Project to an [`AuthUser`] for token issuance and
    /// `Principal` construction. The credentials crate calls this
    /// at sign-up and login time to (a) populate the JWT claims
    /// (`email`, `roles`, `permissions`, custom claims, …) and
    /// (b) shape the `user` field of the `{ token, user }` response.
    /// Apps decide which roles/permissions/claims appear by what
    /// they put on the returned [`AuthUser`].
    fn to_auth_user(&self) -> AuthUser;
}

/// User-record storage contract.
///
/// Implement this against your database of choice. The
/// `docs/auth-credentials.md` walkthrough shows a Postgres + `sqlx`
/// implementation; SQLite, Redis, or any custom backend works the
/// same way.
///
/// The associated `User` type is the app's record — pocopine never
/// constructs it. The store is the only place that knows how to
/// hydrate it (`find_by_*`) or build a new one (`create`).
#[async_trait]
pub trait UserStore: Send + Sync + 'static {
    /// The app's user / account type.
    type User: PasswordCredentials;

    /// Return the user with `email`, if any. The framework folds
    /// the request input to lowercase before calling this, so a
    /// literal byte-for-byte match against the stored email
    /// suffices; implementors do **not** need to add their own
    /// `LOWER(...)` collation.
    async fn find_by_email(&self, email: &str) -> Result<Option<Self::User>, StoreError>;

    /// Return the user with `id`, if any.
    async fn find_by_id(&self, id: &str) -> Result<Option<Self::User>, StoreError>;

    /// Insert a new user with `email` (already lowercased) and the
    /// freshly-hashed `password_hash`. Implementations generate
    /// the id, set any app-specific defaults (display name, role
    /// on signup, timestamps, …), and return the constructed user
    /// record.
    ///
    /// Implementors MUST reject by error (not silently upsert) when
    /// a user with the same email already exists; the credentials
    /// crate maps any error from this method to
    /// [`crate::CredentialsError::EmailTaken`] for the `409 Conflict`
    /// response on the signup route. (If the failure is something
    /// other than duplicate-email — e.g. a connection drop — the
    /// `tracing` log carries the original error class while the
    /// HTTP response stays the closed-set `email_taken`. Callers
    /// that need to distinguish should re-check email availability
    /// before retrying.)
    async fn create(&self, email: &str, password_hash: String) -> Result<Self::User, StoreError>;
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
///
/// PR-2 ships the trait shape but no routes that consume it yet;
/// PR-3 wires `/password/reset/*` and `/email/verify/*` against
/// these methods.
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
