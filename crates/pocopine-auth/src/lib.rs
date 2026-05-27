//! Native auth contracts for pocopine server functions.
//!
//! The crate stays provider-neutral. Pocopine's generated server routes
//! build a host-only request context before decoding the server-function
//! body; host middleware can validate a session/JWT/provider token and
//! insert an [`AuthUser`] or [`Principal`] into request extensions.
//! Guards then inspect that context through ordinary Rust functions.
//!
//! ## Module layout
//!
//! - [`role`] — [`Role`] and [`Permission`] (stringly-typed grant tokens)
//! - [`user`] — [`AuthUser`] (the canonical user payload + claim bag)
//! - [`principal`] — [`Principal`] (request identity) and [`Session`]
//! - [`context`] — [`RequestContext`] and `ensure_*`/`require_*` guards
//!   (host-only)
//! - [`provider`] — [`AuthProvider`], [`SessionStore`], [`AuthError`]
//! - [`predicate`] — [`Predicate`] trait, [`Decision`] outcome, and
//!   combinators (`any_of`, `all_of`, `require_auth`, `require_role`,
//!   `require_permission`)

mod predicate;
mod principal;
mod provider;
mod role;
mod user;

#[cfg(not(target_arch = "wasm32"))]
mod context;

pub use predicate::{
    all_of, any_of, require_auth, require_permission, require_role, Decision, Predicate,
};
pub use principal::{Principal, Session};
pub use role::{Permission, Role};
pub use user::AuthUser;

#[cfg(not(target_arch = "wasm32"))]
pub use context::{
    ensure_login, ensure_permission, ensure_role, require_admin, require_login, require_staff,
    RequestContext, SESSION_COOKIE,
};
pub use provider::{AuthError, AuthResult};
#[cfg(not(target_arch = "wasm32"))]
pub use provider::{AuthFuture, AuthProvider, SessionStore};
