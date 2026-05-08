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
/// struct, etc.). The argon2id PHC string in
/// [`password_hash`](Self::password_hash) is the only credentials-
/// specific field the framework looks at; everything else (timestamps,
/// roles, profile fields, OAuth links) lives on the app's record
/// untouched.
///
/// ## Login identifier
///
/// [`login_id`](Self::login_id) returns whatever string the user
/// types to sign in: email, phone number, username, account number,
/// etc. The framework treats it as opaque — the
/// [`crate::LoginIdValidator`] configured on the [`crate::Credentials`]
/// builder validates and normalizes the value before it reaches
/// the store. Apps that want email-based auth use the default
/// [`crate::EmailValidator`]; phone / username apps swap in
/// [`crate::E164PhoneValidator`] or [`crate::UsernameValidator`];
/// truly custom shapes implement [`crate::LoginIdValidator`] directly.
///
/// ## Privacy
///
/// `password_hash` is a secret. Never log it. The app's `Debug` impl
/// for the user type SHOULD redact it; the credentials crate's own
/// types do (`Debug` on the response shape excludes the hash).
pub trait PasswordCredentials: Send + Sync + 'static {
    /// Stable identifier — used as the JWT `sub` claim. Independent
    /// of [`login_id`](Self::login_id) (which can change when the
    /// user updates their email / phone / username; this can't).
    fn id(&self) -> &str;

    /// The opaque, **already-normalized** identifier the user typed
    /// to sign in. Email, phone (E.164), username, account number —
    /// whatever the configured [`crate::LoginIdValidator`] produced.
    /// The framework matches this byte-for-byte against the
    /// normalized form of the login request input; case folding /
    /// trimming has already happened at the validator layer.
    fn login_id(&self) -> &str;

    /// Argon2id PHC string for the user's current password.
    fn password_hash(&self) -> &str;

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

    /// Return the user with the given `login_id`, if any. The value
    /// is the **already-normalized** form (lowercase email, +E.164
    /// phone, …) — implementors do a literal match against the
    /// stored field. Case folding / trimming has already happened
    /// at the validator layer; defending against case-different
    /// inserts is the validator's responsibility, not the store's.
    async fn find_by_login_id(&self, login_id: &str) -> Result<Option<Self::User>, StoreError>;

    /// Return the user with `id`, if any.
    async fn find_by_id(&self, id: &str) -> Result<Option<Self::User>, StoreError>;

    /// Insert a new user with `login_id` (already normalized) and
    /// the freshly-hashed `password_hash`. Implementations generate
    /// the id, set any app-specific defaults (display name, role on
    /// signup, timestamps, …), and return the constructed user
    /// record.
    ///
    /// Implementors MUST reject by error (not silently upsert) when
    /// a user with the same `login_id` already exists; the credentials
    /// crate maps any error from this method to
    /// [`crate::CredentialsError::LoginIdTaken`] for the `409 Conflict`
    /// response on the signup route. (If the failure is something
    /// other than duplicate-login — e.g. a connection drop — the
    /// `tracing` log carries the original error class while the
    /// HTTP response stays the closed-set `login_id_taken`. Callers
    /// that need to distinguish should re-check availability before
    /// retrying.)
    async fn create(&self, login_id: &str, password_hash: String)
        -> Result<Self::User, StoreError>;
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
