//! Native auth contracts for pocopine server functions.
//!
//! The crate stays provider-neutral. Pocopine's generated server routes
//! build a host-only request context before decoding the server-function
//! body; host middleware can validate a session/JWT/provider token and
//! insert an [`AuthUser`] or [`Principal`] into request extensions.
//! Guards then inspect that context through ordinary Rust functions.

use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::future::Future;
use std::hash::{Hash, Hasher};
#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;

#[cfg(not(target_arch = "wasm32"))]
use http::{Extensions, HeaderMap, Method, Uri};
use pocopine_core::{ServerError, ServerResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Default session cookie name used by the simple auth helpers.
pub const SESSION_COOKIE: &str = "pocopine_session";

/// Role attached to an authenticated user.
///
/// Built-ins cover the common Django-like roles. Use
/// [`Role::named`] for app-specific roles.
#[derive(Clone, Debug)]
pub enum Role {
    /// Administrative user.
    Admin,
    /// Staff/back-office user.
    Staff,
    /// Regular authenticated user.
    User,
    /// App-specific role name.
    Named(String),
}

impl Role {
    /// Build an app-specific role.
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    /// Stable string representation.
    pub fn as_str(&self) -> &str {
        match self {
            Role::Admin => "admin",
            Role::Staff => "staff",
            Role::User => "user",
            Role::Named(name) => name.as_str(),
        }
    }
}

impl PartialEq for Role {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Role {}

impl Hash for Role {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<&str> for Role {
    fn from(value: &str) -> Self {
        match value {
            "admin" => Role::Admin,
            "staff" => Role::Staff,
            "user" => Role::User,
            other => Role::Named(other.to_string()),
        }
    }
}

impl From<String> for Role {
    fn from(value: String) -> Self {
        if value == "admin" {
            Role::Admin
        } else if value == "staff" {
            Role::Staff
        } else if value == "user" {
            Role::User
        } else {
            Role::Named(value)
        }
    }
}

impl Serialize for Role {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Role::from(value))
    }
}

/// Permission attached to an authenticated user.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Permission(String);

impl Permission {
    /// Build a permission name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Stable string representation.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for Permission {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Permission {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Authenticated application user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthUser {
    /// Stable application user id.
    pub id: String,
    /// Optional display/email fields for common apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Roles granted to this user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<Role>,
    /// Fine-grained permissions granted to this user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
}

impl AuthUser {
    /// Build a user from an application id.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            email: None,
            name: None,
            roles: Vec::new(),
            permissions: Vec::new(),
        }
    }

    /// Add an email address.
    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    /// Add a display name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add a role.
    pub fn with_role(mut self, role: impl Into<Role>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Add a permission.
    pub fn with_permission(mut self, permission: impl Into<Permission>) -> Self {
        self.permissions.push(permission.into());
        self
    }

    /// Check whether the user has a role.
    pub fn has_role(&self, role: impl Into<Role>) -> bool {
        let role = role.into();
        self.roles.iter().any(|candidate| candidate == &role)
    }

    /// Check whether the user has a permission.
    pub fn has_permission(&self, permission: impl Into<Permission>) -> bool {
        let permission = permission.into();
        self.permissions
            .iter()
            .any(|candidate| candidate == &permission)
    }
}

/// Request principal. Anonymous requests have no user, but the type still
/// exposes role/permission probes so guard closures stay ergonomic.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Principal {
    user: Option<AuthUser>,
}

impl Principal {
    /// Anonymous principal.
    pub fn anonymous() -> Self {
        Self { user: None }
    }

    /// Authenticated principal.
    pub fn from_user(user: AuthUser) -> Self {
        Self { user: Some(user) }
    }

    /// Whether this request is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.user.is_some()
    }

    /// Authenticated user, if present.
    pub fn user(&self) -> Option<&AuthUser> {
        self.user.as_ref()
    }

    /// Require an authenticated user.
    pub fn require_user(&self) -> ServerResult<&AuthUser> {
        self.user
            .as_ref()
            .ok_or_else(|| ServerError::unauthorized("login required"))
    }

    /// Check whether the authenticated user has a role.
    pub fn has_role(&self, role: impl Into<Role>) -> bool {
        self.user
            .as_ref()
            .is_some_and(|user| user.has_role(role.into()))
    }

    /// Check whether the authenticated user has a permission.
    pub fn has_permission(&self, permission: impl Into<Permission>) -> bool {
        self.user
            .as_ref()
            .is_some_and(|user| user.has_permission(permission.into()))
    }
}

impl From<AuthUser> for Principal {
    fn from(user: AuthUser) -> Self {
        Self::from_user(user)
    }
}

/// Request metadata available to server-function auth guards.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct RequestContext {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    /// Auth principal extracted from host middleware request extensions.
    pub user: Principal,
}

#[cfg(not(target_arch = "wasm32"))]
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
    /// cookies. It does not implement full RFC 6265 quoted-value parsing.
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

/// Auth session metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Stable session id.
    pub id: String,
    /// Authenticated user attached to this session.
    pub user: AuthUser,
    /// Optional Unix epoch expiry in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

impl Session {
    /// Build a session for a user.
    pub fn new(id: impl Into<String>, user: AuthUser) -> Self {
        Self {
            id: id.into(),
            user,
            expires_at_ms: None,
        }
    }

    /// Attach an expiry timestamp.
    pub fn with_expires_at_ms(mut self, expires_at_ms: u64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }
}

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

/// Ensure the request is authenticated.
#[cfg(not(target_arch = "wasm32"))]
pub fn ensure_login(ctx: &RequestContext) -> ServerResult<()> {
    ctx.require_user().map(|_| ())
}

/// Ensure the request has a role.
#[cfg(not(target_arch = "wasm32"))]
pub fn ensure_role(ctx: &RequestContext, role: impl Into<Role>) -> ServerResult<()> {
    if !ctx.user.is_authenticated() {
        return Err(ServerError::unauthorized("login required"));
    }

    let role = role.into();
    if ctx.user.has_role(role.clone()) {
        Ok(())
    } else {
        Err(ServerError::forbidden(format!(
            "missing role `{}`",
            role.as_str()
        )))
    }
}

/// Ensure the request has a permission.
#[cfg(not(target_arch = "wasm32"))]
pub fn ensure_permission(
    ctx: &RequestContext,
    permission: impl Into<Permission>,
) -> ServerResult<()> {
    if !ctx.user.is_authenticated() {
        return Err(ServerError::unauthorized("login required"));
    }

    let permission = permission.into();
    if ctx.user.has_permission(permission.clone()) {
        Ok(())
    } else {
        Err(ServerError::forbidden(format!(
            "missing permission `{}`",
            permission.as_str()
        )))
    }
}

/// Built-in `#[server(guard = ...)]` guard requiring any logged-in user.
#[cfg(not(target_arch = "wasm32"))]
pub async fn require_login(ctx: RequestContext) -> ServerResult<()> {
    ensure_login(&ctx)
}

/// Built-in `#[server(guard = ...)]` guard requiring [`Role::Admin`].
#[cfg(not(target_arch = "wasm32"))]
pub async fn require_admin(ctx: RequestContext) -> ServerResult<()> {
    ensure_role(&ctx, Role::Admin)
}

/// Built-in `#[server(guard = ...)]` guard requiring [`Role::Staff`].
#[cfg(not(target_arch = "wasm32"))]
pub async fn require_staff(ctx: RequestContext) -> ServerResult<()> {
    ensure_role(&ctx, Role::Staff)
}
