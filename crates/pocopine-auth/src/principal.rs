//! Request [`Principal`] (optional auth user) and [`Session`] envelope.

use std::sync::Arc;

use pocopine_core::{ServerError, ServerResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::role::{Permission, Role};
use crate::user::AuthUser;

/// Request principal. Anonymous requests have no user, but the type still
/// exposes role/permission probes so guard closures stay ergonomic.
///
/// The user is held behind `Arc` so middleware → guard → handler hops
/// don't deep-clone the `AuthUser` (with its `claims` map) per request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Principal {
    user: Option<Arc<AuthUser>>,
}

// Hand-rolled (de)serialization avoids depending on serde's `rc`
// feature just for this one `Arc` field. We feed `Option<&AuthUser>`
// to `serialize_field` so serde's Option impl writes the correct
// discriminant (`null` for self-describing formats, an Option tag
// byte for bincode-style formats) — anything else corrupts
// non-self-describing payloads.
impl Serialize for Principal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Principal", 1)?;
        state.serialize_field("user", &self.user.as_deref())?;
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

    /// Authenticated user as a cloned [`Arc`] handle, if present.
    ///
    /// This is a cheap reference-count bump — the underlying
    /// `AuthUser` (and its `claims` map) is shared, not deep-copied.
    /// Use this when middleware → guard → handler hops need to pass
    /// identity along without paying allocation cost, or when storing
    /// the user in a per-request extension that outlives the
    /// `Principal`.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn principal_serialization_emits_option_discriminant() {
        // Authenticated: `{"user":{...}}`.
        let p = Principal::from_user(AuthUser::new("uid-1"));
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"user\":{"), "got: {json}");
        assert!(json.contains("\"id\":\"uid-1\""), "got: {json}");
        let round_tripped: Principal = serde_json::from_str(&json).unwrap();
        assert_eq!(p, round_tripped);
        assert_eq!(round_tripped.user().unwrap().id, "uid-1");

        // Anonymous: `{"user":null}`. Required so non-self-describing
        // formats (bincode, postcard, …) keep the `Option` tag in
        // front of the payload — without the discriminant they can't
        // round-trip.
        let anon = Principal::anonymous();
        let json = serde_json::to_string(&anon).unwrap();
        assert_eq!(json, r#"{"user":null}"#);
        let round_tripped: Principal = serde_json::from_str(&json).unwrap();
        assert!(!round_tripped.is_authenticated());
    }
}
