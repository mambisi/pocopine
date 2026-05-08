//! [`User`] — the credentials-managed user record.

use std::collections::BTreeMap;
use std::fmt;

use pocopine_auth::{Permission, Role};
use serde::{Deserialize, Serialize};

/// Credentials-managed user record.
///
/// `password_hash` is an argon2id PHC string. Treat it as a secret —
/// never log it, never serialize it to a wire format that escapes the
/// trusted boundary. The `Debug` impl below redacts it.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// Stable application user id. Generated at signup time by the
    /// configured id-generator (default: millis-prefix + UUIDv7).
    pub id: String,
    /// Email address — case-folded to lower at signup.
    pub email: String,
    /// Whether email verification has succeeded. Always `false`
    /// after signup; the email-verification flow (PR-3) flips it.
    pub email_verified: bool,
    /// Argon2id PHC string. **Never logged.**
    pub password_hash: String,
    /// Roles granted to the user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<Role>,
    /// Fine-grained permissions granted to the user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
    /// Created at, Unix milliseconds.
    pub created_at_ms: u64,
    /// Updated at, Unix milliseconds.
    pub updated_at_ms: u64,
    /// App-defined metadata (display name, avatar URL, tenant id, …).
    /// Pocopine never reads these fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl fmt::Debug for User {
    /// Redacts `password_hash`. Tests assert that `format!("{user:?}")`
    /// never contains the hash.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("email_verified", &self.email_verified)
            .field("password_hash", &"<redacted>")
            .field("roles", &self.roles)
            .field("permissions", &self.permissions)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl User {
    /// Build a user. `created_at_ms` and `updated_at_ms` are set to
    /// the same value; bump `updated_at_ms` on subsequent edits.
    pub fn new(
        id: impl Into<String>,
        email: impl Into<String>,
        password_hash: impl Into<String>,
        now_ms: u64,
    ) -> Self {
        Self {
            id: id.into(),
            email: email.into(),
            email_verified: false,
            password_hash: password_hash.into(),
            roles: Vec::new(),
            permissions: Vec::new(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            metadata: BTreeMap::new(),
        }
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

    /// Set a metadata field.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_password_hash() {
        let user = User::new(
            "u1",
            "alice@example.com",
            "$argon2id$v=19$m=65536,t=3,p=4$...real-hash...",
            1700000000_000,
        );
        let debug = format!("{user:?}");
        assert!(
            !debug.contains("$argon2id"),
            "Debug must not leak the password hash; got: {debug}"
        );
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn round_trips_through_json() {
        let user = User::new("u1", "alice@example.com", "$argon2id$v=19$..", 1700000000_000)
            .with_role(Role::admin());
        let json = serde_json::to_string(&user).unwrap();
        let back: User = serde_json::from_str(&json).unwrap();
        assert_eq!(user, back);
    }
}
