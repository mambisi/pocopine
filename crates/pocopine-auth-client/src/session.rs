//! [`AuthSession`] — reactive client-side identity holder.
//!
//! Installed by [`crate::auth_plugin`] as an [`AppPlugin`] service
//! and looked up by [`crate::predicate_guard`] (and any app code) via
//! `Plugins::get::<AuthSession>()`. Holds the active [`Principal`]
//! plus a monotonic `u64` epoch that bumps on every identity change
//! so middleware can fence stale in-flight responses against the
//! sign-in/sign-out the user just performed.
//!
//! Internal mutability via `Rc<RefCell<…>>` is the wasm-side
//! convention — apps clone the [`AuthSession`] handle freely; the
//! shared interior state is the source of truth.

use std::cell::RefCell;
use std::rc::Rc;

use pocopine_auth::Principal;
use pocopine_core::Plugins;

/// Wasm-side reactive identity service. Cheap to clone — wraps an
/// `Rc<RefCell<…>>` interior. Installed by [`crate::auth_plugin`]
/// and read through [`active_principal`] / [`active_session`].
#[derive(Clone, Default)]
pub struct AuthSession {
    inner: Rc<RefCell<AuthSessionInner>>,
}

#[derive(Default)]
struct AuthSessionInner {
    principal: Principal,
    epoch: u64,
}

impl AuthSession {
    /// Build an anonymous session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Active principal (cheap clone — `Principal` holds an
    /// `Arc<AuthUser>` internally).
    pub fn principal(&self) -> Principal {
        self.inner.borrow().principal.clone()
    }

    /// `true` when [`principal`](Self::principal) carries a user.
    pub fn is_authenticated(&self) -> bool {
        self.inner.borrow().principal.is_authenticated()
    }

    /// Monotonic epoch. Bumps every time the principal changes.
    /// Middleware that captures the epoch on outgoing requests can
    /// use a mismatch on response to detect stale dispatches under
    /// the previous identity (RFC-078 §5.10.5).
    pub fn epoch(&self) -> u64 {
        self.inner.borrow().epoch
    }

    /// Replace the active principal. Bumps the epoch.
    pub fn set_principal(&self, principal: Principal) {
        let mut inner = self.inner.borrow_mut();
        inner.principal = principal;
        inner.epoch = inner.epoch.saturating_add(1);
    }

    /// Sign in: register `token` with the bearer middleware **and**
    /// publish `principal` on this session.
    pub fn sign_in(&self, token: impl Into<String>, principal: Principal) {
        crate::set_token(token);
        self.set_principal(principal);
    }

    /// Sign out: clear the bearer token slot and reset the principal
    /// to anonymous. Call [`pocopine_core::reevaluate_current`]
    /// afterwards if a guarded route is currently mounted — the
    /// router will rerun its guards against the new (anonymous)
    /// `Principal` and unmount the gated component before the next
    /// paint.
    pub fn sign_out(&self) {
        crate::clear_token();
        self.set_principal(Principal::anonymous());
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only override so host unit tests can drive the
    /// middleware's session-aware paths without spinning up an `App`
    /// plus runtime. Real wasm runs go through the plugin registry.
    static TEST_SESSION_OVERRIDE: std::cell::RefCell<Option<AuthSession>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn __set_test_session(session: Option<AuthSession>) {
    TEST_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = session);
}

/// Convenience: read the active [`AuthSession`] from the runtime
/// plugin registry. Returns [`None`] if no session has been
/// installed (the app didn't add `auth_plugin()`).
pub fn active_session() -> Option<AuthSession> {
    #[cfg(test)]
    {
        if let Some(s) = TEST_SESSION_OVERRIDE.with(|c| c.borrow().clone()) {
            return Some(s);
        }
    }
    Plugins.get::<AuthSession>().map(|handle| (*handle).clone())
}

/// Convenience: read the active [`Principal`] from the installed
/// [`AuthSession`]. Returns the anonymous principal when no
/// `AuthSession` has been installed — apps that don't run the auth
/// plugin still get a valid principal value.
pub fn active_principal() -> Principal {
    active_session().map(|s| s.principal()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_auth::AuthUser;

    #[test]
    fn anonymous_session_has_no_user_and_epoch_zero() {
        let session = AuthSession::new();
        assert!(!session.is_authenticated());
        assert_eq!(session.epoch(), 0);
        assert!(session.principal().user().is_none());
    }

    #[test]
    fn set_principal_bumps_epoch_on_each_change() {
        let session = AuthSession::new();
        let user = AuthUser::new("u1");
        session.set_principal(Principal::from_user(user.clone()));
        assert_eq!(session.epoch(), 1);
        assert!(session.is_authenticated());
        assert_eq!(session.principal().user().unwrap().id, "u1");

        // Re-setting the same principal still bumps — the epoch is
        // a write counter, not a value-change counter, so a
        // refresh-without-change still tells observers "things
        // were touched".
        session.set_principal(Principal::from_user(user));
        assert_eq!(session.epoch(), 2);
    }

    #[test]
    fn sign_out_resets_principal_and_clears_bearer_slot() {
        let session = AuthSession::new();
        session.sign_in("token-abc", Principal::from_user(AuthUser::new("u1")));
        assert!(session.is_authenticated());
        assert_eq!(crate::active_token().as_deref(), Some("token-abc"));

        session.sign_out();
        assert!(!session.is_authenticated());
        assert_eq!(crate::active_token(), None);
        // sign_in bumped to 1; sign_out bumped to 2.
        assert_eq!(session.epoch(), 2);
    }

    #[test]
    fn handle_is_cheap_to_clone_and_shares_state() {
        let a = AuthSession::new();
        let b = a.clone();
        a.set_principal(Principal::from_user(AuthUser::new("u1")));
        assert_eq!(a.epoch(), b.epoch());
        assert_eq!(a.principal(), b.principal());
    }
}
