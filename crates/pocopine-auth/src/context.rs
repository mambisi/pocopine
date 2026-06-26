//! Server-side auth helpers over the framework request context.
//!
//! Auth does not own the request carrier. The general
//! [`RequestContext`](pocopine_core::server::RequestContext) lives in
//! `pocopine-core::server`; this module only knows how to read/write
//! auth-specific values from that context's typed extension map.

#![cfg(not(target_arch = "wasm32"))]

use pocopine_core::server::{FromRequestContext, RequestContext};
use pocopine_core::{ServerError, ServerResult};

use crate::principal::Principal;
use crate::role::{Permission, Role};
use crate::user::AuthUser;

/// Default session cookie name used by the simple auth helpers.
pub const SESSION_COOKIE: &str = "pocopine_session";

/// Auth-specific helpers for [`RequestContext`].
pub trait RequestAuthExt {
    /// Auth principal extracted from request extensions.
    fn principal(&self) -> Principal;

    /// Attach an authenticated user.
    fn with_user(self, user: AuthUser) -> Self;

    /// Attach a principal.
    fn with_principal(self, principal: Principal) -> Self;

    /// Session id from the default pocopine auth cookie.
    fn session_id(&self) -> Option<&str>;

    /// Require an authenticated user.
    fn require_user(&self) -> ServerResult<&AuthUser>;
}

impl RequestAuthExt for RequestContext {
    fn principal(&self) -> Principal {
        self.extension::<Principal>()
            .cloned()
            .or_else(|| {
                self.extension::<AuthUser>()
                    .cloned()
                    .map(Principal::from_user)
            })
            .unwrap_or_else(Principal::anonymous)
    }

    fn with_user(self, user: AuthUser) -> Self {
        self.with_principal(Principal::from_user(user))
    }

    fn with_principal(self, principal: Principal) -> Self {
        self.with_extension(principal)
    }

    fn session_id(&self) -> Option<&str> {
        self.cookie(SESSION_COOKIE)
    }

    fn require_user(&self) -> ServerResult<&AuthUser> {
        self.extension::<Principal>()
            .and_then(Principal::user)
            .or_else(|| self.extension::<AuthUser>())
            .ok_or_else(|| ServerError::unauthorized("login required"))
    }
}

/// Ensure the request is authenticated.
pub fn ensure_login(ctx: &RequestContext) -> ServerResult<()> {
    ctx.require_user().map(|_| ())
}

/// Ensure the request has a role.
pub fn ensure_role(ctx: &RequestContext, role: &Role) -> ServerResult<()> {
    if !ctx.principal().is_authenticated() {
        return Err(ServerError::unauthorized("login required"));
    }

    if ctx.principal().has_role(role) {
        Ok(())
    } else {
        Err(ServerError::forbidden(format!(
            "missing role `{}`",
            role.as_str()
        )))
    }
}

/// Ensure the request has a permission.
pub fn ensure_permission(ctx: &RequestContext, permission: &Permission) -> ServerResult<()> {
    if !ctx.principal().is_authenticated() {
        return Err(ServerError::unauthorized("login required"));
    }

    if ctx.principal().has_permission(permission) {
        Ok(())
    } else {
        Err(ServerError::forbidden(format!(
            "missing permission `{}`",
            permission.as_str()
        )))
    }
}

/// Built-in `#[server(guard = ...)]` guard requiring any logged-in user.
pub async fn require_login(ctx: RequestContext) -> ServerResult<()> {
    ensure_login(&ctx)
}

/// Built-in `#[server(guard = ...)]` guard requiring the conventional
/// `admin` role (matched by string).
pub async fn require_admin(ctx: RequestContext) -> ServerResult<()> {
    ensure_role(&ctx, &Role::admin())
}

/// Built-in `#[server(guard = ...)]` guard requiring the conventional
/// `staff` role (matched by string).
pub async fn require_staff(ctx: RequestContext) -> ServerResult<()> {
    ensure_role(&ctx, &Role::staff())
}

impl FromRequestContext for Principal {
    fn from_request_context(ctx: &RequestContext) -> ServerResult<Self> {
        Ok(ctx.principal())
    }
}

impl FromRequestContext for AuthUser {
    fn from_request_context(ctx: &RequestContext) -> ServerResult<Self> {
        ctx.require_user().cloned()
    }
}
