//! Authenticated application user payload.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::role::{Permission, Role};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
