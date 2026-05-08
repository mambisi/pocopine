//! Email + password credentials core for pocopine.
//!
//! Apps own their user record. The crate ships:
//!
//! - The [`PasswordCredentials`] trait the app implements on its
//!   own user/account type so the framework can read identity +
//!   password hash off it.
//! - The [`UserStore`] / [`TokenStore`] storage contracts (apps
//!   implement against their database).
//! - The [`Argon2Params`] wrapper with OWASP defaults +
//!   minimum-validation.
//! - A [`Credentials<S, T>`] builder that mounts
//!   `/_pocopine/auth/{signup,login,logout}` through pocopine's
//!   [`ServerPlugin`](pocopine_server::ServerPlugin) surface.
//!
//! The session token issued at the end of `/signup` and `/login` is
//! produced by [`pocopine_auth_jwt::JwtIssuer`] and verified through
//! [`pocopine_auth_jwt::JwtVerifier`] using a paired
//! [`pocopine_auth_jwt::JwtConfig`] returned by
//! [`Credentials::verifier_config`]. Apps install both halves with
//! the same shared secret.
//!
//! ```ignore
//! use pocopine_auth_credentials::{Credentials, PasswordCredentials, UserStore};
//! use pocopine_auth::AuthUser;
//! use pocopine_auth_jwt::{JwtVerifier, SecretBytes};
//! use pocopine_server::{axum::Router, RouterAuthExt, Server};
//!
//! // 1. App-owned user type.
//! struct MyUser { id: String, email: String, hash: String }
//!
//! impl PasswordCredentials for MyUser {
//!     fn id(&self) -> &str { &self.id }
//!     fn email(&self) -> &str { &self.email }
//!     fn password_hash(&self) -> &str { &self.hash }
//!     fn to_auth_user(&self) -> AuthUser {
//!         AuthUser::new(&self.id).with_email(&self.email)
//!     }
//! }
//!
//! // 2. App-owned store, generic over MyUser.
//! struct MyStore { /* db pool, etc. */ }
//!
//! #[async_trait::async_trait]
//! impl UserStore for MyStore {
//!     type User = MyUser;
//!     async fn find_by_email(&self, _: &str) -> _ { unimplemented!() }
//!     async fn find_by_id(&self, _: &str) -> _ { unimplemented!() }
//!     async fn create(&self, _: &str, _: String) -> _ { unimplemented!() }
//! }
//!
//! // ... see docs/auth-credentials.md for the full Postgres example.
//! ```
//!
//! ## Out of scope (deferred to follow-up PRs)
//!
//! - Email flows (`EmailSender` trait, `/password/reset/*`,
//!   `/email/verify/*`) ship in PR-3 of the RFC-074 chain.
//! - Redis / Postgres backends are app-side or community
//!   adapter crates — the framework deliberately doesn't bundle one.
//! - HIBP / breach-list checks ship as a separate plugin crate.
//!
//! ## Privacy
//!
//! - The argon2id PHC string returned from
//!   [`PasswordCredentials::password_hash`] is a secret; never log
//!   it. The crate never serializes it back to the wire (the
//!   signup/login response uses only the [`AuthUser`] projection).
//! - Server-issued tokens carry only `sub` / `iss` / `aud` / `iat` /
//!   `exp` plus whatever fields the app's [`AuthUser`] surfaces
//!   (`email`, `email_verified`, `roles`, `permissions`, custom claims).
//! - Reset / verification tokens (PR-3) will be stored as their
//!   SHA-256 hash — even a full database leak does not yield
//!   reusable tokens.

#![cfg(not(target_arch = "wasm32"))]

mod builder;
mod error;
mod login_id;
mod password;
mod routes;
mod store;

pub use builder::Credentials;
pub use error::CredentialsError;
pub use login_id::{E164PhoneValidator, EmailValidator, LoginIdValidator, UsernameValidator};
pub use password::{Argon2Params, Argon2ParamsError};
pub use store::{
    token::{TokenKind, TokenRecord},
    PasswordCredentials, StoreError, TokenStore, UserStore,
};
