//! Provider/session-store traits and the [`AuthError`] type.
//!
//! The traits and futures here are host-only; the wasm client uses
//! its own session bridge (`pocopine-auth-client`).

use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::future::Future;
#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;

use pocopine_core::ServerError;

#[cfg(not(target_arch = "wasm32"))]
use crate::context::RequestContext;
#[cfg(not(target_arch = "wasm32"))]
use crate::principal::Session;
#[cfg(not(target_arch = "wasm32"))]
use crate::user::AuthUser;

/// Auth provider failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthError {
    message: String,
}

impl AuthError {
    /// Build an auth failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AuthError {}

impl From<AuthError> for ServerError {
    fn from(err: AuthError) -> Self {
        ServerError::unauthorized(err.to_string())
    }
}

/// Provider/session result type.
pub type AuthResult<T> = Result<T, AuthError>;

/// Boxed async result used by provider traits without choosing an
/// async-trait dependency.
#[cfg(not(target_arch = "wasm32"))]
pub type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = AuthResult<T>> + Send + 'a>>;

/// Auth provider contract. Clerk/Auth0/Supabase adapters can implement
/// this without changing the server-function guard ABI.
#[cfg(not(target_arch = "wasm32"))]
pub trait AuthProvider: Send + Sync {
    /// Authenticate a request and return an optional user.
    fn authenticate<'a>(&'a self, ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>>;
}

/// Session persistence contract for first-party/simple auth.
#[cfg(not(target_arch = "wasm32"))]
pub trait SessionStore: Send + Sync {
    /// Load a session by id.
    fn load<'a>(&'a self, session_id: &'a str) -> AuthFuture<'a, Option<Session>>;

    /// Save a session.
    fn save<'a>(&'a self, session: Session) -> AuthFuture<'a, ()>;

    /// Delete a session by id.
    fn delete<'a>(&'a self, session_id: &'a str) -> AuthFuture<'a, ()>;
}
