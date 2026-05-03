//! Native auth contracts for pocopine server functions.
//!
//! The crate stays provider-neutral. Pocopine's generated server routes
//! build a host-only request context before decoding the server-function
//! body; host middleware can validate a session/JWT/provider token and
//! insert an [`AuthUser`] or [`Principal`] into request extensions.
//! Guards then inspect that context through ordinary Rust functions.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::future::Future;
use std::hash::{Hash, Hasher};
#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use http::{Extensions, HeaderMap, Method, Uri};
use pocopine_core::{ServerError, ServerResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Default session cookie name used by the simple auth helpers.
pub const SESSION_COOKIE: &str = "pocopine_session";

/// Role attached to an authenticated user.
///
/// A role is a single named string; equality and hashing both go
/// through the underlying string. The `admin` / `staff` / `user`
/// constructors are convenience names — they are *not* privileged
/// in the framework. Pocopine's built-in `require_admin` /
/// `require_staff` guards check `Role::admin()` / `Role::staff()`
/// by string match, so apps that adopt those names get the guard
/// shortcut; apps that use a different taxonomy define their own
/// guards via [`ensure_role`].
///
/// String construction is explicit (`Role::named(s)` /
/// `Role::new(cow)`) — there is no `From<&str>` /
/// `From<String>` impl. That removes the historical footgun where
/// a JWT claim of `"admin"` deserialized into the framework's
/// privileged variant before any app code saw it.
#[derive(Clone, Debug)]
pub struct Role(Cow<'static, str>);

impl Role {
    /// Conventional administrative role. Matched by the built-in
    /// `require_admin` guard. App code is not required to use this
    /// name.
    pub const fn admin() -> Self {
        Self(Cow::Borrowed("admin"))
    }

    /// Conventional staff/back-office role. Matched by
    /// `require_staff`.
    pub const fn staff() -> Self {
        Self(Cow::Borrowed("staff"))
    }

    /// Conventional regular-user role.
    pub const fn user() -> Self {
        Self(Cow::Borrowed("user"))
    }

    /// Build a role from a `&'static str` without allocating, or
    /// from any owned string (allocates once).
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self(name.into())
    }

    /// Build an app-specific role from an owned string.
    pub fn named(name: impl Into<String>) -> Self {
        Self(Cow::Owned(name.into()))
    }

    /// Stable string representation.
    pub fn as_str(&self) -> &str {
        &self.0
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
        Ok(Role::named(value))
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
///
/// The fixed fields (`id`, `email`, `name`, `roles`, `permissions`)
/// are the canonical projection a `ClaimMap` populates from a
/// provider-issued token. Provider-specific data that doesn't fit
/// the projection round-trips through [`AuthUser::claims`] so app
/// code can read it without losing information.
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
    /// Provider-specific claims preserved verbatim. JWT verifiers
    /// dump every unrecognized claim here; app code reads them via
    /// `claims.get("...")` or via provider-specific extension traits
    /// (e.g. `pocopine_auth_jwt::FirebaseClaimsExt`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub claims: BTreeMap<String, serde_json::Value>,
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
            claims: BTreeMap::new(),
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
    pub fn with_role(mut self, role: Role) -> Self {
        self.roles.push(role);
        self
    }

    /// Add a permission.
    pub fn with_permission(mut self, permission: Permission) -> Self {
        self.permissions.push(permission);
        self
    }

    /// Add a provider-specific claim. Repeated keys overwrite.
    pub fn with_claim(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(key.into(), value);
        self
    }

    /// Check whether the user has a role.
    pub fn has_role(&self, role: &Role) -> bool {
        self.roles.iter().any(|candidate| candidate == role)
    }

    /// Check whether the user has a permission.
    pub fn has_permission(&self, permission: &Permission) -> bool {
        self.permissions
            .iter()
            .any(|candidate| candidate == permission)
    }

    /// Look up a provider-specific claim by key.
    pub fn claim(&self, key: &str) -> Option<&serde_json::Value> {
        self.claims.get(key)
    }
}

/// Request principal. Anonymous requests have no user, but the type still
/// exposes role/permission probes so guard closures stay ergonomic.
///
/// The user is held behind `Arc` so middleware → guard → handler hops
/// don't deep-clone the `AuthUser` (with its `claims` map) per request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Principal {
    user: Option<Arc<AuthUser>>,
}

// Hand-rolled (de)serialization avoids depending on serde's `rc` feature
// just for this one Arc field; the wire shape stays identical to the
// previous `Option<AuthUser>` derive.
impl Serialize for Principal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Principal", 1)?;
        match &self.user {
            Some(user) => state.serialize_field("user", user.as_ref())?,
            None => state.skip_field("user")?,
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for Principal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            user: Option<AuthUser>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Principal {
            user: wire.user.map(Arc::new),
        })
    }
}

impl Principal {
    /// Anonymous principal.
    pub fn anonymous() -> Self {
        Self { user: None }
    }

    /// Authenticated principal — takes an owned user.
    pub fn from_user(user: AuthUser) -> Self {
        Self {
            user: Some(Arc::new(user)),
        }
    }

    /// Authenticated principal that reuses an existing `Arc`. Use this
    /// when middleware cached an `Arc<AuthUser>` and wants to hand it to
    /// the request without cloning the underlying user.
    pub fn from_arc(user: Arc<AuthUser>) -> Self {
        Self { user: Some(user) }
    }

    /// Whether this request is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.user.is_some()
    }

    /// Authenticated user, if present.
    pub fn user(&self) -> Option<&AuthUser> {
        self.user.as_deref()
    }

    /// Authenticated user as a clonable handle, if present. Cheap clone.
    pub fn user_arc(&self) -> Option<Arc<AuthUser>> {
        self.user.clone()
    }

    /// Require an authenticated user.
    pub fn require_user(&self) -> ServerResult<&AuthUser> {
        self.user
            .as_deref()
            .ok_or_else(|| ServerError::unauthorized("login required"))
    }

    /// Check whether the authenticated user has a role.
    pub fn has_role(&self, role: &Role) -> bool {
        self.user.as_deref().is_some_and(|user| user.has_role(role))
    }

    /// Check whether the authenticated user has a permission.
    pub fn has_permission(&self, permission: &Permission) -> bool {
        self.user
            .as_deref()
            .is_some_and(|user| user.has_permission(permission))
    }
}

impl From<AuthUser> for Principal {
    fn from(user: AuthUser) -> Self {
        Self::from_user(user)
    }
}

impl From<Arc<AuthUser>> for Principal {
    fn from(user: Arc<AuthUser>) -> Self {
        Self::from_arc(user)
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
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
pub async fn require_login(ctx: RequestContext) -> ServerResult<()> {
    ensure_login(&ctx)
}

/// Built-in `#[server(guard = ...)]` guard requiring the conventional
/// `admin` role (matched by string).
#[cfg(not(target_arch = "wasm32"))]
pub async fn require_admin(ctx: RequestContext) -> ServerResult<()> {
    ensure_role(&ctx, &Role::admin())
}

/// Built-in `#[server(guard = ...)]` guard requiring the conventional
/// `staff` role (matched by string).
#[cfg(not(target_arch = "wasm32"))]
pub async fn require_staff(ctx: RequestContext) -> ServerResult<()> {
    ensure_role(&ctx, &Role::staff())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_constructors_compare_by_string() {
        assert_eq!(Role::admin(), Role::named("admin"));
        assert_eq!(Role::admin(), Role::new("admin"));
        assert_ne!(Role::admin(), Role::user());
        assert_eq!(Role::admin().as_str(), "admin");
    }

    #[test]
    fn role_deserialization_does_not_promote_to_privileged_variant() {
        // The pre-refactor footgun: a JWT claim of "admin" would
        // deserialize into the framework's privileged variant. The
        // new shape stores the string verbatim and still compares
        // equal to `Role::admin()` by value, but no privileged
        // variant exists at the type level.
        let role: Role = serde_json::from_str(r#""admin""#).unwrap();
        assert_eq!(role, Role::admin());
        assert_eq!(role.as_str(), "admin");

        let custom: Role = serde_json::from_str(r#""editor""#).unwrap();
        assert_eq!(custom.as_str(), "editor");
        assert_ne!(custom, Role::admin());
    }

    #[test]
    fn auth_user_round_trips_arbitrary_claims() {
        let user = AuthUser::new("uid-1")
            .with_email("a@example.com")
            .with_role(Role::admin())
            .with_claim("firebase_uid", serde_json::json!("xyz"))
            .with_claim("org_id", serde_json::json!(42));

        let json = serde_json::to_string(&user).unwrap();
        let round_tripped: AuthUser = serde_json::from_str(&json).unwrap();
        assert_eq!(user, round_tripped);
        assert_eq!(
            round_tripped.claim("firebase_uid"),
            Some(&serde_json::json!("xyz"))
        );
        assert_eq!(round_tripped.claim("org_id"), Some(&serde_json::json!(42)));
    }

    #[test]
    fn principal_user_clones_are_cheap() {
        let user = AuthUser::new("uid-1");
        let p1 = Principal::from_user(user);
        let p2 = p1.clone();
        // Both Principals share the same Arc<AuthUser>.
        let arc1 = p1.user_arc().expect("p1 has user");
        let arc2 = p2.user_arc().expect("p2 has user");
        assert!(Arc::ptr_eq(&arc1, &arc2));
    }

    #[test]
    fn principal_serializes_compatibly_with_pre_arc_shape() {
        // Wire format is unchanged: `{ "user": { ... } }`. Hand-rolled
        // (de)serialization keeps backwards compat without depending on
        // serde's `rc` feature.
        let p = Principal::from_user(AuthUser::new("uid-1"));
        let json = serde_json::to_string(&p).unwrap();
        let round_tripped: Principal = serde_json::from_str(&json).unwrap();
        assert_eq!(p, round_tripped);
        assert_eq!(round_tripped.user().unwrap().id, "uid-1");

        let anon = Principal::anonymous();
        let json = serde_json::to_string(&anon).unwrap();
        assert!(!json.contains("user"));
        let round_tripped: Principal = serde_json::from_str(&json).unwrap();
        assert!(!round_tripped.is_authenticated());
    }
}
