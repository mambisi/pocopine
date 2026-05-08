//! Email + password credentials core for pocopine.
//!
//! Ships the server-side primitives an app needs to register the
//! "Django-shaped" path: a [`User`] record with an argon2id password
//! hash, a [`UserStore`] / [`TokenStore`] split for users and
//! ephemeral tokens, in-memory backends for the tryout case, and a
//! [`Credentials`] builder that installs `/_pocopine/auth/signup`,
//! `/login`, and `/logout` routes through pocopine's
//! [`ServerPlugin`] surface.
//!
//! The session token issued at the end of `/signup` and `/login` is
//! produced by [`pocopine_auth_jwt::JwtIssuer`] and verified through
//! [`pocopine_auth_jwt::JwtVerifier`] using a paired
//! [`pocopine_auth_jwt::JwtConfig`] returned by
//! [`Credentials::verifier_config`]. Apps install both halves with
//! the same shared secret.
//!
//! ```ignore
//! use pocopine_auth_credentials::Credentials;
//! use pocopine_auth_jwt::{JwtVerifier, SecretBytes};
//! use pocopine_server::{axum::Router, RouterAuthExt, Server};
//!
//! // Implement `UserStore` + `TokenStore` against your database;
//! // the `docs/auth-credentials.md` tutorial walks a Postgres +
//! // `sqlx` example end to end.
//! use my_app::{PgUserStore, PgTokenStore};
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     let secret = SecretBytes::new(std::env::var("POCOPINE_AUTH_SECRET")
//!         .expect("set POCOPINE_AUTH_SECRET to >= 32 random bytes"));
//!     let store = PgUserStore::connect().await;
//!     let tokens = PgTokenStore::connect().await;
//!     let creds = Credentials::new(secret, store, tokens);
//!
//!     let verifier = JwtVerifier::custom(creds.verifier_config()).unwrap();
//!     let router = Router::new();
//!     Server::new(router.with_auth(verifier))
//!         .plugin(creds)
//!         .serve("0.0.0.0:3000")
//!         .await
//! }
//! ```
//!
//! ## Out of scope (deferred to follow-up PRs)
//!
//! - Email flows (`EmailSender` trait, `/password/reset/*`,
//!   `/email/verify/*`) ship in PR-3 of the RFC-074 chain.
//! - Redis / Postgres backends live in their own crates
//!   (`pocopine-auth-credentials-redis`, …).
//! - HIBP / breach-list checks ship as a separate plugin crate.
//!
//! ## Privacy
//!
//! - The argon2id PHC string in [`User::password_hash`] is **never**
//!   logged. The `Debug` impl on [`User`] redacts it.
//! - Server-issued tokens carry only `sub` / `iss` / `aud` / `iat` /
//!   `exp` / `email` / `email_verified` / `roles` / `permissions`.
//! - Reset / verification tokens are stored as their SHA-256 hash —
//!   even a full database leak does not yield reusable tokens.

#![cfg(not(target_arch = "wasm32"))]

mod builder;
mod error;
mod id;
mod password;
mod routes;
mod store;
mod user;

pub use builder::Credentials;
pub use error::CredentialsError;
pub use id::default_id_generator;
pub use password::{Argon2Params, Argon2ParamsError};
pub use store::{
    token::{TokenKind, TokenRecord},
    StoreError, TokenStore, UserStore,
};
pub use user::User;
