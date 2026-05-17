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
use serde::{Deserialize, Serialize};

/// Persisted, optimistic identity snapshot used to make reloads feel
/// native while the real browser/provider session is still being
/// confirmed.
///
/// This is a continuity hint, not an authorization boundary. Server
/// functions and provider checks still decide whether the session is
/// actually valid.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthSessionSnapshot {
    pub principal: Principal,
}

impl AuthSessionSnapshot {
    pub fn new(principal: Principal) -> Option<Self> {
        principal.is_authenticated().then_some(Self { principal })
    }
}

/// Wasm-side reactive identity service. Cheap to clone — wraps an
/// `Rc<RefCell<…>>` interior. Installed by [`crate::auth_plugin`]
/// and read through [`active_principal`] / [`active_session`].
#[derive(Clone)]
pub struct AuthSession {
    inner: Rc<RefCell<AuthSessionInner>>,
}

struct AuthSessionInner {
    principal: Principal,
    epoch: u64,
    ready: bool,
    restoring: bool,
}

impl Default for AuthSession {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthSession {
    /// Build an anonymous session whose initial auth check has
    /// already completed.
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(AuthSessionInner {
                principal: Principal::anonymous(),
                epoch: 0,
                ready: true,
                restoring: false,
            })),
        }
    }

    /// Build an anonymous session that still needs an async provider
    /// check before route guards may decide.
    pub fn pending() -> Self {
        Self {
            inner: Rc::new(RefCell::new(AuthSessionInner {
                principal: Principal::anonymous(),
                epoch: 0,
                ready: false,
                restoring: false,
            })),
        }
    }

    /// Build a session from an optimistic persisted snapshot. Route
    /// guards may render from this state, but provider/server
    /// confirmation is still pending.
    pub fn restoring(snapshot: AuthSessionSnapshot) -> Self {
        Self {
            inner: Rc::new(RefCell::new(AuthSessionInner {
                principal: snapshot.principal,
                epoch: 0,
                ready: false,
                restoring: true,
            })),
        }
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

    /// `true` once the app's auth provider has checked persisted
    /// browser state at least once. Guard adapters return
    /// `RouteGuardDecision::Pending` while this is false.
    pub fn is_ready(&self) -> bool {
        self.inner.borrow().ready
    }

    /// `true` when this session is rendering from a persisted
    /// optimistic identity snapshot while the app checks the real
    /// provider/server session in the background.
    pub fn is_restoring(&self) -> bool {
        self.inner.borrow().restoring
    }

    /// Snapshot the current authenticated principal for optimistic
    /// restore on the next page load.
    pub fn snapshot(&self) -> Option<AuthSessionSnapshot> {
        AuthSessionSnapshot::new(self.principal())
    }

    /// Mark the initial auth check as pending. Use this before
    /// starting an async provider hydration task if a plugin could not
    /// know at construction time that a check was needed.
    pub fn mark_pending(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.ready = false;
        inner.restoring = false;
        inner.epoch = inner.epoch.saturating_add(1);
    }

    /// Mark the initial auth check as complete and ask the router to
    /// re-run any guard that paused on this session.
    pub fn mark_ready(&self) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.ready = true;
            inner.restoring = false;
            inner.epoch = inner.epoch.saturating_add(1);
        }
        reevaluate_router();
    }

    /// Monotonic epoch. Bumps every time the principal changes.
    /// Middleware that captures the epoch on outgoing requests can
    /// use a mismatch on response to detect stale dispatches under
    /// the previous identity (RFC-078 §5.10.5).
    pub fn epoch(&self) -> u64 {
        self.inner.borrow().epoch
    }

    /// Replace the active principal. Bumps the epoch and (when
    /// cross-tab sync is installed via
    /// [`crate::AuthPluginBuilder::with_cross_tab_sync`]) broadcasts
    /// the change so peer tabs can re-hydrate.
    pub fn set_principal(&self, principal: Principal) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.principal = principal;
            inner.ready = true;
            inner.restoring = false;
            inner.epoch = inner.epoch.saturating_add(1);
            persist_snapshot(&inner.principal);
        }
        crate::cross_tab::broadcast_session_changed();
        reevaluate_router();
    }

    /// Bump the epoch without changing the principal. Use this when
    /// some external signal (websocket message, cross-tab broadcast,
    /// iframe communication) tells you the session state has shifted
    /// but you don't yet have a fresh `Principal` to publish. The
    /// bearer middleware's identity-change fence reads the epoch, so
    /// a bump alone is enough to drop in-flight responses captured
    /// under the previous identity. Does NOT re-broadcast — call this
    /// from inbound handlers without triggering a feedback loop.
    pub fn bump_epoch(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.epoch = inner.epoch.saturating_add(1);
    }

    /// Apply the token-state signal received from a peer tab.
    ///
    /// A missing token is definitive sign-out, so clear the local
    /// principal and wake guards immediately. A present token means
    /// the peer signed in or refreshed; the local tab still needs its
    /// own provider/server identity check, so only bump the epoch to
    /// fence stale in-flight responses.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn apply_cross_tab_token_state(&self, token_present: bool) {
        if token_present {
            self.bump_epoch();
        } else {
            self.set_principal(Principal::anonymous());
        }
    }

    /// Sign in: register `token` with the bearer middleware **and**
    /// publish `principal` on this session.
    pub fn sign_in(&self, token: impl Into<String>, principal: Principal) {
        crate::set_token(token);
        self.set_principal(principal);
    }

    /// Sign out: clear the bearer token slot and reset the principal
    /// to anonymous. This also marks the initial auth check complete
    /// and asks the router to re-run the current guard so gated
    /// content is removed promptly.
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

fn persist_snapshot(principal: &Principal) {
    let Some(storage) = crate::storage::current_session_snapshot_storage() else {
        return;
    };
    if let Some(snapshot) = AuthSessionSnapshot::new(principal.clone()) {
        storage.save_snapshot(&snapshot);
    } else {
        storage.clear_snapshot();
    }
}

fn reevaluate_router() {
    #[cfg(target_arch = "wasm32")]
    pocopine_core::reevaluate_current();
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_auth::AuthUser;

    #[test]
    fn anonymous_session_has_no_user_and_epoch_zero() {
        let session = AuthSession::new();
        assert!(session.is_ready());
        assert!(!session.is_restoring());
        assert!(!session.is_authenticated());
        assert_eq!(session.epoch(), 0);
        assert!(session.principal().user().is_none());
    }

    #[test]
    fn pending_session_blocks_until_ready() {
        let session = AuthSession::pending();
        assert!(!session.is_ready());
        assert!(!session.is_restoring());
        assert_eq!(session.epoch(), 0);

        session.mark_ready();
        assert!(session.is_ready());
        assert!(!session.is_restoring());
        assert_eq!(session.epoch(), 1);
    }

    #[test]
    fn restoring_session_uses_snapshot_until_confirmed() {
        let snapshot = AuthSessionSnapshot::new(Principal::from_user(AuthUser::new("u1"))).unwrap();
        let session = AuthSession::restoring(snapshot);
        assert!(!session.is_ready());
        assert!(session.is_restoring());
        assert!(session.is_authenticated());
        assert_eq!(session.principal().user().unwrap().id, "u1");

        session.mark_ready();
        assert!(session.is_ready());
        assert!(!session.is_restoring());
    }

    #[test]
    fn set_principal_bumps_epoch_on_each_change() {
        let session = AuthSession::new();
        let user = AuthUser::new("u1");
        session.set_principal(Principal::from_user(user.clone()));
        assert!(session.is_ready());
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
    fn principal_changes_write_through_to_snapshot_storage() {
        crate::storage::__reset_storage_for_test();
        let storage = Rc::new(crate::storage::TestStorage::default());
        crate::storage::install_session_snapshot_storage(storage.clone());

        let session = AuthSession::new();
        session.set_principal(Principal::from_user(AuthUser::new("u1")));
        assert_eq!(
            storage
                .snapshot
                .borrow()
                .as_ref()
                .and_then(|snapshot| snapshot.principal.user())
                .map(|user| user.id.clone()),
            Some("u1".to_string())
        );

        session.sign_out();
        assert!(storage.snapshot.borrow().is_none());
        crate::storage::__reset_storage_for_test();
    }

    #[test]
    fn handle_is_cheap_to_clone_and_shares_state() {
        let a = AuthSession::new();
        let b = a.clone();
        a.set_principal(Principal::from_user(AuthUser::new("u1")));
        assert_eq!(a.epoch(), b.epoch());
        assert_eq!(a.principal(), b.principal());
    }

    #[test]
    fn bump_epoch_advances_without_changing_principal() {
        let session = AuthSession::new();
        let user = AuthUser::new("u1");
        session.set_principal(Principal::from_user(user.clone()));
        let before = session.epoch();
        let principal_before = session.principal();
        session.bump_epoch();
        assert_eq!(session.epoch(), before + 1);
        assert_eq!(session.principal(), principal_before);
    }

    #[test]
    fn cross_tab_missing_token_clears_principal() {
        crate::storage::__reset_storage_for_test();
        let storage = Rc::new(crate::storage::TestStorage::default());
        crate::storage::install_session_snapshot_storage(storage.clone());

        let session = AuthSession::new();
        session.set_principal(Principal::from_user(AuthUser::new("u1")));
        assert!(session.is_authenticated());
        assert!(storage.snapshot.borrow().is_some());

        session.apply_cross_tab_token_state(false);

        assert!(!session.is_authenticated());
        assert!(session.is_ready());
        assert!(!session.is_restoring());
        assert_eq!(session.epoch(), 2);
        assert!(storage.snapshot.borrow().is_none());
        crate::storage::__reset_storage_for_test();
    }

    #[test]
    fn cross_tab_present_token_only_bumps_epoch() {
        let session = AuthSession::new();
        session.set_principal(Principal::from_user(AuthUser::new("u1")));
        let before = session.epoch();
        let principal_before = session.principal();

        session.apply_cross_tab_token_state(true);

        assert_eq!(session.epoch(), before + 1);
        assert_eq!(session.principal(), principal_before);
    }
}
