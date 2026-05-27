//! Stringly-typed grant tokens: [`Role`] and [`Permission`].

use std::borrow::Cow;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Role attached to an authenticated user.
///
/// A role is a single named string; equality and hashing both go
/// through the underlying string. The `admin` / `staff` / `user`
/// constructors are convenience names — they are *not* privileged
/// in the framework. Pocopine's built-in `require_admin` /
/// `require_staff` guards check `Role::admin()` / `Role::staff()`
/// by string match, so apps that adopt those names get the guard
/// shortcut; apps that use a different taxonomy define their own
/// guards via [`ensure_role`](crate::ensure_role).
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
}
