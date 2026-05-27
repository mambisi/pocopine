//! Sync, cross-target permission checks via the [`Predicate`] trait
//! and the closed-set [`Decision`] outcome.

use pocopine_core::ServerError;

use crate::principal::Principal;
use crate::role::{Permission, Role};

/// Outcome of a [`Predicate`] check against a [`Principal`].
///
/// `Deny`'s reason is a stable, closed-set `&'static str` identifier
/// (e.g. `"unauthorized"`, `"missing_role:admin"`). It is consumed
/// by the server-side `From<Decision>` adapter (mapped to
/// `ServerError::Unauthorized` / `Forbidden`) and by the client-side
/// `RouteGuard` blanket impl (mapped to `RouteRejection::Unauthorized`
/// / `Forbidden`). Reasons must never carry user input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(&'static str),
}

/// Sync, cross-target permission check against a [`Principal`].
///
/// One predicate value plugs into two install points:
///
/// - **Server-side**: `#[server(guard = require_role("admin"))]` —
///   the macro turns the predicate into a `Result<(), ServerError>`
///   via the [`From<Decision>`] adapter.
/// - **Client-side**: `App::route::<Dashboard>("/dashboard")
///   .guard(require_role("admin"))` — `pocopine-auth-client` ships
///   a blanket `impl<P: Predicate> RouteGuard for P` that reads the
///   reactive client `Principal` and maps `Deny` into
///   `RouteRejection`.
///
/// The trait is intentionally sync. Async work (token refresh, DB
/// lookups) belongs in a route loader where there is already an
/// async context with a structured error surface; predicates just
/// inspect already-resolved identity.
pub trait Predicate: Send + Sync + 'static {
    fn check(&self, principal: &Principal) -> Decision;
}

impl<F> Predicate for F
where
    F: Fn(&Principal) -> Decision + Send + Sync + 'static,
{
    fn check(&self, principal: &Principal) -> Decision {
        self(principal)
    }
}

/// Predicate matching any authenticated user.
pub fn require_auth() -> impl Predicate {
    |principal: &Principal| {
        if principal.is_authenticated() {
            Decision::Allow
        } else {
            Decision::Deny("unauthorized")
        }
    }
}

/// Predicate matching any user holding `role` (string match).
pub fn require_role(role: &'static str) -> impl Predicate {
    move |principal: &Principal| {
        if !principal.is_authenticated() {
            return Decision::Deny("unauthorized");
        }
        if principal.has_role(&Role::new(role)) {
            Decision::Allow
        } else {
            Decision::Deny("forbidden")
        }
    }
}

/// Predicate matching any user holding `permission` (string match).
pub fn require_permission(permission: &'static str) -> impl Predicate {
    move |principal: &Principal| {
        if !principal.is_authenticated() {
            return Decision::Deny("unauthorized");
        }
        if principal.has_permission(&Permission::new(permission)) {
            Decision::Allow
        } else {
            Decision::Deny("forbidden")
        }
    }
}

/// Predicate that allows when **either** child predicate allows.
/// Tries `p` first; if `p` denies, tries `q`. The reason on `Deny`
/// is the second predicate's reason — the assumption is the broader
/// (latter) check carries the more useful user-visible failure.
pub fn any_of<P, Q>(p: P, q: Q) -> impl Predicate
where
    P: Predicate,
    Q: Predicate,
{
    move |principal: &Principal| match p.check(principal) {
        Decision::Allow => Decision::Allow,
        Decision::Deny(_) => q.check(principal),
    }
}

/// Predicate that allows only when **both** child predicates allow.
/// Returns the first `Deny` reason (short-circuits).
pub fn all_of<P, Q>(p: P, q: Q) -> impl Predicate
where
    P: Predicate,
    Q: Predicate,
{
    move |principal: &Principal| match p.check(principal) {
        Decision::Deny(reason) => Decision::Deny(reason),
        Decision::Allow => q.check(principal),
    }
}

/// Server-side adapter: `#[server(guard = require_role("admin"))]`
/// expects a guard returning `Result<(), ServerError>`. A `Decision`
/// maps cleanly onto that.
///
/// `"unauthorized"` (and reasons starting with that prefix) become
/// `ServerError::Unauthorized`; everything else becomes
/// `ServerError::Forbidden`. Apps that want a different mapping
/// should write a wrapper guard that inspects the predicate's
/// `Decision` directly.
impl From<Decision> for Result<(), ServerError> {
    fn from(decision: Decision) -> Self {
        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => {
                if reason == "unauthorized" {
                    Err(ServerError::unauthorized(reason))
                } else {
                    Err(ServerError::forbidden(reason))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user::AuthUser;

    fn build_user_with_role(role: Role) -> AuthUser {
        AuthUser::new("uid-1").with_role(role)
    }

    #[test]
    fn require_auth_allows_authenticated_denies_anonymous() {
        let auth = require_auth();
        assert_eq!(
            auth.check(&Principal::from_user(AuthUser::new("uid-1"))),
            Decision::Allow
        );
        assert_eq!(
            auth.check(&Principal::anonymous()),
            Decision::Deny("unauthorized")
        );
    }

    #[test]
    fn require_role_denies_unauthorized_before_role_check() {
        let admin_only = require_role("admin");
        // Anonymous: missing identity comes first, so the reason
        // is `unauthorized` rather than `forbidden` — keeps the
        // server-side adapter mapping clean (401 vs 403).
        assert_eq!(
            admin_only.check(&Principal::anonymous()),
            Decision::Deny("unauthorized")
        );
        assert_eq!(
            admin_only.check(&Principal::from_user(build_user_with_role(Role::user()))),
            Decision::Deny("forbidden")
        );
        assert_eq!(
            admin_only.check(&Principal::from_user(build_user_with_role(Role::admin()))),
            Decision::Allow
        );
    }

    #[test]
    fn require_permission_short_circuits_on_anonymous() {
        let perm = require_permission("posts.write");
        assert_eq!(
            perm.check(&Principal::anonymous()),
            Decision::Deny("unauthorized")
        );

        let user = AuthUser::new("uid-1").with_permission(Permission::new("posts.write"));
        assert_eq!(perm.check(&Principal::from_user(user)), Decision::Allow);
    }

    #[test]
    fn any_of_returns_second_reason_on_double_deny() {
        let p = any_of(require_role("admin"), require_role("editor"));
        let viewer = Principal::from_user(build_user_with_role(Role::user()));
        // First check denies (`forbidden`), second check denies
        // (`forbidden`); reason comes from the second.
        assert_eq!(p.check(&viewer), Decision::Deny("forbidden"));
        // Anonymous: both branches deny with `unauthorized`; the
        // second branch's reason wins (still `unauthorized`).
        assert_eq!(
            p.check(&Principal::anonymous()),
            Decision::Deny("unauthorized")
        );
    }

    #[test]
    fn any_of_allows_when_either_predicate_allows() {
        let p = any_of(require_role("admin"), require_role("editor"));
        let editor = Principal::from_user(build_user_with_role(Role::new("editor")));
        let admin = Principal::from_user(build_user_with_role(Role::admin()));
        assert_eq!(p.check(&editor), Decision::Allow);
        assert_eq!(p.check(&admin), Decision::Allow);
    }

    #[test]
    fn all_of_short_circuits_on_first_deny() {
        let p = all_of(require_auth(), require_role("admin"));
        // Anonymous: short-circuits on require_auth, returns
        // `unauthorized`.
        assert_eq!(
            p.check(&Principal::anonymous()),
            Decision::Deny("unauthorized")
        );
        // Non-admin authenticated: passes require_auth, fails
        // require_role with `forbidden`.
        let viewer = Principal::from_user(build_user_with_role(Role::user()));
        assert_eq!(p.check(&viewer), Decision::Deny("forbidden"));
        let admin = Principal::from_user(build_user_with_role(Role::admin()));
        assert_eq!(p.check(&admin), Decision::Allow);
    }

    #[test]
    fn decision_to_server_result_maps_unauthorized_vs_forbidden() {
        let allow: Result<(), ServerError> = Decision::Allow.into();
        assert!(allow.is_ok());

        let unauth: Result<(), ServerError> = Decision::Deny("unauthorized").into();
        assert!(matches!(unauth, Err(ServerError::Unauthorized(_))));

        // Anything other than `"unauthorized"` becomes Forbidden so
        // the closed reason set drives 401 vs 403 cleanly.
        let forbidden: Result<(), ServerError> = Decision::Deny("forbidden").into();
        assert!(matches!(forbidden, Err(ServerError::Forbidden(_))));

        let custom: Result<(), ServerError> = Decision::Deny("missing_role:admin").into();
        assert!(matches!(custom, Err(ServerError::Forbidden(_))));
    }

    #[test]
    fn closure_implements_predicate() {
        // Free closures `Fn(&Principal) -> Decision` plug straight
        // into the trait via the blanket impl.
        let only_uid_one = |p: &Principal| match p.user() {
            Some(user) if user.id == "uid-1" => Decision::Allow,
            _ => Decision::Deny("forbidden"),
        };
        assert_eq!(
            only_uid_one.check(&Principal::from_user(AuthUser::new("uid-1"))),
            Decision::Allow
        );
        assert_eq!(
            only_uid_one.check(&Principal::from_user(AuthUser::new("uid-2"))),
            Decision::Deny("forbidden")
        );
    }
}
