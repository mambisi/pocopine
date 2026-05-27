//! Server-side request context plus the sync/async guard helpers
//! that read it. All host-only — wasm clients use
//! `pocopine-auth-client`.

#![cfg(not(target_arch = "wasm32"))]

use http::{Extensions, HeaderMap, Method, Uri};
use pocopine_core::{ServerError, ServerResult};

use crate::principal::Principal;
use crate::role::{Permission, Role};
use crate::user::AuthUser;

/// Default session cookie name used by the simple auth helpers.
pub const SESSION_COOKIE: &str = "pocopine_session";

/// Request metadata available to server-function auth guards.
///
/// Host-only — wasm clients don't construct a `RequestContext`
/// because they're not on the receiving end of HTTP requests.
/// The client-side equivalent for "what identity is active right
/// now" is `pocopine_auth_client::AuthSession`, which exposes the
/// reactive [`Principal`] without the request-shaped fields
/// (`method`, `uri`, `headers`, cookies, bearer token) that only
/// make sense on the server. A single [`Predicate`](crate::Predicate)
/// value plugs into both surfaces.
#[derive(Clone, Debug)]
pub struct RequestContext {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    /// Auth principal extracted from host middleware request extensions.
    pub user: Principal,
}

impl RequestContext {
    /// Build an anonymous request context from HTTP request parts.
    pub fn new(method: Method, uri: Uri, headers: HeaderMap) -> Self {
        Self {
            method,
            uri,
            headers,
            user: Principal::anonymous(),
        }
    }

    /// Build a request context and pull auth identity from extensions.
    ///
    /// Middleware may insert either [`Principal`] or [`AuthUser`]. If both
    /// are present, [`Principal`] wins.
    pub fn from_parts(
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        extensions: Extensions,
    ) -> Self {
        let user = extensions
            .get::<Principal>()
            .cloned()
            .or_else(|| {
                extensions
                    .get::<AuthUser>()
                    .cloned()
                    .map(Principal::from_user)
            })
            .unwrap_or_else(Principal::anonymous);

        Self {
            method,
            uri,
            headers,
            user,
        }
    }

    /// Attach an authenticated user.
    pub fn with_user(mut self, user: AuthUser) -> Self {
        self.user = Principal::from_user(user);
        self
    }

    /// Attach a principal.
    pub fn with_principal(mut self, principal: Principal) -> Self {
        self.user = principal;
        self
    }

    /// HTTP method used for the server-function call.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// Request URI used for the server-function call.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// All request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Header value as UTF-8 text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// Bearer token from the `Authorization` header, if present.
    pub fn bearer_token(&self) -> Option<&str> {
        let value = self.header("authorization")?;
        let (scheme, token) = value.split_once(' ')?;
        if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
            Some(token.trim())
        } else {
            None
        }
    }

    /// Cookie value by name from the `Cookie` header.
    ///
    /// This deliberately small parser is intended for simple session
    /// cookies. It does not implement full RFC 6265 quoted-value parsing —
    /// apps that need that should reach for the `cookie` crate.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        let cookies = self.header("cookie")?;
        for part in cookies.split(';') {
            if let Some((key, value)) = part.trim().split_once('=') {
                if key.trim() == name {
                    return Some(value.trim());
                }
            }
        }
        None
    }

    /// Session id from the default pocopine auth cookie.
    pub fn session_id(&self) -> Option<&str> {
        self.cookie(SESSION_COOKIE)
    }

    /// Require an authenticated user.
    pub fn require_user(&self) -> ServerResult<&AuthUser> {
        self.user.require_user()
    }
}

/// Ensure the request is authenticated.
pub fn ensure_login(ctx: &RequestContext) -> ServerResult<()> {
    ctx.require_user().map(|_| ())
}

/// Ensure the request has a role.
pub fn ensure_role(ctx: &RequestContext, role: &Role) -> ServerResult<()> {
    if !ctx.user.is_authenticated() {
        return Err(ServerError::unauthorized("login required"));
    }

    if ctx.user.has_role(role) {
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
    if !ctx.user.is_authenticated() {
        return Err(ServerError::unauthorized("login required"));
    }

    if ctx.user.has_permission(permission) {
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
