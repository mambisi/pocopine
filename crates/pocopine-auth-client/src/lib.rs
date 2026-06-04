//! Wasm-side auth surface for pocopine.
//!
//! Eight surfaces:
//!
//! - **Token slot.** [`set_token`], [`clear_token`], and [`active_token`]
//!   manage a process-global `Option<String>`.
//! - **Fetch middleware.** [`BearerMiddleware`] reads the active token
//!   and attaches `Authorization: Bearer <token>` to every outgoing
//!   `#[server]` request via the RFC-078 fetch chain. On
//!   [`ServerError::Unauthorized`] for a [replay-safe](pocopine_core::fetch::FetchRequest::is_replay_safe)
//!   request it invokes the app-configured [`TokenRefresh`] (if any),
//!   calls [`set_token`] with the fresh value, and replays once. Apps
//!   register the middleware via [`install`] or via the plugin builder
//!   ([`auth_plugin().with_bearer_middleware(true)`](auth_plugin)) — both
//!   are idempotent, so mixing them is safe.
//! - **Token refresh + single-flight coordinator.** Apps install a
//!   provider-specific [`TokenRefresh`] via
//!   [`AuthPluginBuilder::with_token_refresh`]; concurrent in-flight
//!   401s coalesce into one issuer call via [`refresh`]'s shared-slot
//!   coordinator (see module docs).
//! - **Token persistence.** Pluggable [`TokenStorage`] so the bearer
//!   token survives reload. Provided impls in [`storage`]:
//!   `InMemory` (default no-op), `LocalStorage`, `SessionStorage`.
//!   Wired via [`AuthPluginBuilder::with_token_storage`].
//! - **Session snapshot persistence.** Pluggable
//!   [`SessionSnapshotStorage`] for optimistic identity restore.
//!   Snapshots let route shells render immediately after a reload
//!   while Firebase, `/me`, or another provider confirms the real
//!   session. They are UI continuity hints, not authorization proof.
//! - **Cross-tab session coordination.** Sign-in / sign-out in tab A
//!   propagates to peer tabs of the same origin via a
//!   `BroadcastChannel`. Opt-in via
//!   [`AuthPluginBuilder::with_cross_tab_sync`]; requires
//!   [`with_token_storage`](AuthPluginBuilder::with_token_storage)
//!   so peer tabs can read the new credential. See [`cross_tab`].
//! - **Reactive identity.** [`AuthSession`] is a plugin service the
//!   [`auth_plugin`] installs. It holds the active [`pocopine_auth::Principal`]
//!   plus a monotonic epoch; components extract it via
//!   `Plugin<AuthSession>` / `Option<Plugin<AuthSession>>` per RFC-076.
//!   The bearer middleware also captures the epoch on outgoing
//!   authenticated requests and fences the response on completion: if
//!   the user signed in/out while the request was in flight, the
//!   middleware returns `Err(ServerError::Unauthorized("session_changed"))`
//!   instead of letting stale-identity data through (RFC-078 §5.10.5).
//!   The fence runs on both the response side AND the refresh-replay
//!   write side, so a refresh resolving after a sign-out doesn't
//!   re-pollute the cleared token slot.
//! - **Auth-aware route guards + login redirect.** [`auth_plugin`]
//!   is an [`AppPlugin`](pocopine_core::AppPlugin) that registers a
//!   [`RouteRejectionHandler`](pocopine_core::RouteRejectionHandler)
//!   mapping `RouteRejection::Unauthorized` to the configured login
//!   route. [`predicate_guard`] adapts any
//!   [`pocopine_auth::Predicate`] (`require_auth`, `require_role`,
//!   …) into a [`RouteGuard`](pocopine_core::RouteGuard) that reads
//!   the live [`AuthSession`].
//!
//! ## Usage
//!
//! ```ignore
//! use pocopine::App;
//! use pocopine_auth_client::{auth_plugin, storage::LocalStorage};
//!
//! App::new()
//!     .plugin(
//!         auth_plugin()
//!             .login_route("/login")
//!             .with_bearer_middleware(true)
//!             // Persist token across reload:
//!             .with_token_storage(LocalStorage::new("auth_token"))
//!             // Sync identity changes across tabs:
//!             .with_cross_tab_sync(true)
//!             // Refresh + replay on Unauthorized for idempotent calls:
//!             .with_token_refresh(|| async {
//!                 my_app::refresh_session_token().await
//!             }),
//!     )
//!     .run();
//!
//! // After successful sign-in (Firebase SDK, Clerk SDK, your own
//! // login server function, etc.):
//! pocopine_auth_client::set_token(token);
//!
//! // Subsequent #[server] calls authenticate automatically.
//! // Calls marked #[server(idempotent)] also retry once after a
//! // successful refresh on Unauthorized.
//! let user = my_app::get_current_user().await?;
//!
//! // On sign-out:
//! pocopine_auth_client::clear_token();
//! ```
//!
//! See `docs/guides/auth/client.md` for a step-by-step walkthrough of each
//! surface, the security tradeoffs, and the cross-tab/refresh wire
//! traces.
//!
//! ## What this is *not*
//!
//! - **Not provider-coupled.** [`set_token`] accepts any string; the
//!   server-side verifier decides validity. [`TokenRefresh`] is a
//!   provider-supplied closure or struct, not a built-in.
//! - **Not an `httpOnly` cookie issuer.** The [`storage`] impls
//!   persist the bearer in JavaScript-readable browser storage; for
//!   high-value applications, prefer an `httpOnly` cookie set by the
//!   server (then skip the bearer middleware entirely — the browser
//!   sends the cookie automatically). See `storage.rs` module docs
//!   for the full XSS-vs-durability tradeoff.
//! - **Not a route-aware predicate adapter.** [`predicate_guard`] is
//!   `Principal`-only; guards that need to inspect `params` /
//!   `query` (e.g., `/users/:id` requiring `principal.id == params["id"]`)
//!   must write the closure directly. See [`predicate_guard`]'s
//!   docstring for the pattern.
//!
//! ## Privacy
//!
//! The middleware never logs the token. Per RFC-077 §6, no body,
//! headers, cookies, or query strings enter framework events. Bearer
//! attachment is a pure mutation on the in-flight `FetchRequest`.

pub mod cross_tab;
mod plugin;
mod refresh;
mod session;
pub mod storage;

#[cfg(test)]
mod test_util;

pub use plugin::{auth_plugin, predicate_guard, AuthPluginBuilder, DEFAULT_RETURN_TO_PARAM};
pub use refresh::{TokenRefresh, TokenRefreshFuture};
pub use session::{active_principal, active_session, AuthSession, AuthSessionSnapshot};
pub use storage::{SessionSnapshotStorage, TokenStorage};

use std::cell::Cell;
use std::sync::Mutex;

use pocopine_core::fetch::{
    install_middleware, FetchMiddleware, FetchMiddlewareFuture, FetchNext, FetchRequest,
};
use pocopine_core::ServerError;

/// Process-global bearer token. `Mutex` is portable across host (where
/// unit tests live) and wasm (where the real runtime is single-threaded
/// and the lock is uncontended). `Mutex::new` is const-fn since Rust
/// 1.63 so no `OnceLock` is needed.
static TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// Register the bearer token attached to subsequent `#[server]` calls.
/// Replaces any previously-set token. Writes through to the configured
/// [`TokenStorage`] when one is installed via
/// [`AuthPluginBuilder::with_token_storage`].
pub fn set_token(token: impl Into<String>) {
    let token = token.into();
    *TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned") = Some(token.clone());
    if let Some(storage) = storage::current_storage() {
        storage.save(&token);
    }
}

/// Drop the active token. Subsequent calls go out without an
/// `Authorization` header. Also clears the configured [`TokenStorage`].
pub fn clear_token() {
    *TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned") = None;
    if let Some(storage) = storage::current_storage() {
        storage.clear();
    }
}

/// Read the active token, if any.
pub fn active_token() -> Option<String> {
    TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned")
        .clone()
}

/// Replace the in-memory token slot from the configured
/// [`TokenStorage`]. Called at boot (so a refreshing user's token
/// survives reload) and from the cross-tab listener (so a sign-in in
/// tab A propagates to tab B without an extra round-trip). No-op if
/// no storage is installed or storage has no value.
///
/// Does **not** broadcast across tabs — this is the inbound side of
/// the broadcast handshake; calling it inside the broadcast listener
/// without recursion guard is the correct shape.
pub fn hydrate_from_storage() {
    let Some(storage) = storage::current_storage() else {
        return;
    };
    *TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned") = storage.load();
}

/// Fetch middleware that attaches `Authorization: Bearer <token>`,
/// refreshes once on `Unauthorized` for replay-safe requests, and
/// fences responses against identity changes that happened in
/// flight. All three concerns live here so token, epoch, and
/// refresh state are observed atomically per outgoing request.
pub struct BearerMiddleware;

impl FetchMiddleware for BearerMiddleware {
    fn call(&self, mut request: FetchRequest, next: FetchNext) -> FetchMiddlewareFuture {
        // Snapshot once: the auth-decision and the header-attach must
        // observe the same token, else a `set_token`/`clear_token`
        // between two locks could split the invariant.
        let token_snapshot = active_token();
        let had_token = token_snapshot.is_some();
        // Capture the session handle once. Both fences below
        // (write-side after refresh, response-side post-completion)
        // read its epoch — sharing the handle avoids two extra
        // RwLock reads + Arc clones on the plugin registry per
        // authenticated request.
        let session_handle = if had_token {
            session::active_session()
        } else {
            None
        };
        let captured_epoch = session_handle.as_ref().map(|s| s.epoch());

        if let Some(token) = token_snapshot.as_deref() {
            request.set_header("authorization", format!("Bearer {token}"));
        }

        let is_replay_safe = request.is_replay_safe();
        Box::pin(async move {
            let response = next.clone().run(request.clone()).await;

            // `pocopine_core::fetch::call` collapses non-2xx into
            // `Network("HTTP 401")` before deserializing the body, so
            // we recognise both shapes as the same auth signal.
            let unauthorized = matches!(&response, Ok(r) if r.status == 401)
                || matches!(&response, Err(ServerError::Unauthorized(_)));

            // RFC-078 §5.10.4: refresh + replay only for replay-safe
            // requests that actually carried a token. Concurrent 401s
            // coalesce into one issuer call via `refresh_single_flight`.
            let response = if unauthorized && had_token && is_replay_safe {
                if refresh::current_refresh().is_some() {
                    match refresh::refresh_single_flight().await {
                        Ok(new_token) => {
                            // Write-side identity fence: a sign-out
                            // during refresh advances the epoch; writing
                            // the new token would re-pollute a cleared
                            // slot. The response-side fence below can't
                            // undo a slot write — only this gate can.
                            let still_valid =
                                session_handle.as_ref().map(|s| s.epoch()) == captured_epoch;
                            if still_valid {
                                set_token(new_token.as_str());
                                let mut retry = request;
                                retry.set_header(
                                    "authorization",
                                    format!("Bearer {}", new_token.as_str()),
                                );
                                next.run(retry).await
                            } else {
                                Err(ServerError::unauthorized("session_changed"))
                            }
                        }
                        Err(err) => Err((*err).clone()),
                    }
                } else {
                    Err(ServerError::unauthorized("token expired"))
                }
            } else if unauthorized && had_token {
                Err(ServerError::unauthorized("token expired"))
            } else if unauthorized {
                Err(ServerError::unauthorized("auth required"))
            } else {
                response
            };

            // Refresh rotates the token but does NOT bump the epoch;
            // sign-in/out does. So a refresh-replay passes; a
            // concurrent identity change trips this fence.
            if let Some(captured) = captured_epoch {
                let now = session_handle.as_ref().map(|s| s.epoch());
                if now != Some(captured) {
                    return Err(ServerError::unauthorized("session_changed"));
                }
            }

            response
        })
    }
}

thread_local! {
    /// Idempotency guard for [`install`]; second-and-later calls
    /// no-op rather than double-registering on the fetch chain.
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Install [`BearerMiddleware`] on the global fetch chain. MUST be
/// called before the first `App::run` / `pocopine_core::fetch::call`,
/// per RFC-078 §5.10.3 (fetch middleware is trusted-plugin code; the
/// chain freezes at boot).
///
/// Idempotent: a second call is a no-op so apps can mix this with
/// `auth_plugin().with_bearer_middleware(true)` without ending up with
/// two `BearerMiddleware` entries on the chain. Calling [`install`]
/// after the chain has frozen still panics with the diagnostic from
/// `pocopine_core::fetch::install_middleware`.
pub fn install() {
    INSTALLED.with(|installed| {
        if installed.replace(true) {
            return;
        }
        install_middleware(BearerMiddleware);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{block_on, noop_waker, yield_once};
    use pocopine_auth::{AuthUser, Principal};
    use pocopine_core::fetch::{
        __reset_middleware_chain_for_test, freeze_middleware_chain, FetchResponse,
    };
    use std::cell::RefCell;
    use std::future::Future;
    use std::rc::Rc;
    use std::task::{Context, Poll};

    // The token store, INSTALLED flag, and `pocopine_core::fetch`
    // chain are all process- or thread-local globals. Serialize all
    // tests in this binary so they don't observe each other's writes.
    // We rely on `pocopine_core::fetch::call` happening to compile on
    // the host target — if that ever moves behind
    // `#[cfg(target_arch = "wasm32")]`, port these to
    // `wasm_bindgen_test`.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Acquire SERIAL while tolerating prior-test poison: the
    /// `should_panic` cases here panic by design, and a stray panic in
    /// any other test would otherwise cascade into "every other test
    /// panics on Mutex::lock". The test isolation we need from SERIAL
    /// is "only one test holds it at a time" — poison state is noise.
    fn lock_serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn full_reset() {
        clear_token();
        __reset_middleware_chain_for_test();
        refresh::__reset_refresh_for_test();
        storage::__reset_storage_for_test();
        INSTALLED.with(|c| c.set(false));
    }

    // ─── token slot ─────────────────────────────────────────────────

    #[test]
    fn set_then_active_returns_the_set_token() {
        let _guard = lock_serial();
        full_reset();
        set_token("alpha");
        assert_eq!(active_token().as_deref(), Some("alpha"));
        clear_token();
    }

    #[test]
    fn clear_token_removes_the_value() {
        let _guard = lock_serial();
        full_reset();
        set_token("beta");
        assert_eq!(active_token().as_deref(), Some("beta"));
        clear_token();
        assert_eq!(active_token(), None);
    }

    #[test]
    fn set_token_replaces_previous() {
        let _guard = lock_serial();
        full_reset();
        set_token("first");
        set_token("second");
        assert_eq!(active_token().as_deref(), Some("second"));
    }

    #[test]
    fn active_token_with_nothing_set_returns_none() {
        let _guard = lock_serial();
        full_reset();
        assert_eq!(active_token(), None);
    }

    // ─── install() idempotency ──────────────────────────────────────

    #[test]
    fn install_is_idempotent_on_repeated_calls() {
        let _guard = lock_serial();
        full_reset();

        install();
        install();
        install();

        // Drive a request; only one Authorization header should be
        // present even if multiple registrations would have produced
        // a chain of BearerMiddlewares all trying to set it.
        set_token("only-one");
        let captured = dispatch_and_capture();
        let auth_count = captured
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .count();
        assert_eq!(auth_count, 1);

        full_reset();
    }

    // ─── BearerMiddleware ───────────────────────────────────────────

    /// Drive a `pocopine_core::fetch::call::<(), ()>("/probe", &())`
    /// through the chain and return the request the trailing capture
    /// middleware saw.
    fn dispatch_and_capture() -> FetchRequest {
        let seen: Rc<RefCell<Option<FetchRequest>>> = Rc::new(RefCell::new(None));
        let seen_inner = seen.clone();
        install_middleware(move |request: FetchRequest, _next: FetchNext| {
            *seen_inner.borrow_mut() = Some(request);
            async move {
                Ok(FetchResponse {
                    status: 200,
                    body: r#"{"Ok":null}"#.to_string(),
                })
            }
        });

        let _ = block_on(pocopine_core::fetch::call::<(), ()>("/probe", &()));
        let captured = seen.borrow_mut().take();
        captured.expect("capture middleware never observed a request")
    }

    #[test]
    fn bearer_middleware_attaches_authorization_when_token_set() {
        let _guard = lock_serial();
        full_reset();
        set_token("token-abc");
        install();

        let captured = dispatch_and_capture();
        let auth = captured
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth, Some("Bearer token-abc"));

        full_reset();
    }

    #[test]
    fn bearer_middleware_omits_authorization_when_token_unset() {
        let _guard = lock_serial();
        full_reset();
        install();

        let captured = dispatch_and_capture();
        let auth = captured
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        assert!(
            auth.is_none(),
            "expected no authorization header, got {auth:?}"
        );

        full_reset();
    }

    #[test]
    fn bearer_middleware_replaces_authorization_on_token_change() {
        let _guard = lock_serial();

        full_reset();
        set_token("first");
        install();
        let first = dispatch_and_capture();
        let count_first = first
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .count();
        assert_eq!(count_first, 1);

        full_reset();
        set_token("second");
        install();
        let second = dispatch_and_capture();
        let auth_second = second
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth_second, Some("Bearer second"));

        full_reset();
    }

    #[test]
    #[should_panic(expected = "fetch::install_middleware called after the chain was frozen")]
    fn install_after_freeze_panics() {
        let guard = lock_serial();
        full_reset();
        install();
        freeze_middleware_chain();
        drop(guard);
        // Force a fresh INSTALLED flag so the second install actually
        // hits the (now-frozen) fetch module.
        INSTALLED.with(|c| c.set(false));
        install();
    }

    // ─── refresh-on-401 + replay ────────────────────────────────────

    fn dispatch_replay_safe() -> Result<(), ServerError> {
        block_on(pocopine_core::fetch::call_replay_safe::<(), ()>(
            "/probe",
            &(),
        ))
    }

    fn dispatch_default() -> Result<(), ServerError> {
        block_on(pocopine_core::fetch::call::<(), ()>("/probe", &()))
    }

    /// Install a counter middleware that returns Unauthorized on the
    /// first call and Ok on subsequent calls; records each call's auth
    /// header.
    fn install_401_then_ok_counter() -> Rc<RefCell<Vec<Option<String>>>> {
        let log: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let log_inner = log.clone();
        install_middleware(move |request: FetchRequest, _next: FetchNext| {
            let log = log_inner.clone();
            async move {
                let auth = request
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                    .map(|(_, v)| v.clone());
                let attempt = {
                    let mut l = log.borrow_mut();
                    l.push(auth);
                    l.len()
                };
                if attempt == 1 {
                    Ok(FetchResponse {
                        status: 401,
                        body: r#"{"Err":{"Unauthorized":"token expired"}}"#.to_string(),
                    })
                } else {
                    Ok(FetchResponse {
                        status: 200,
                        body: r#"{"Ok":null}"#.to_string(),
                    })
                }
            }
        });
        log
    }

    #[test]
    fn refresh_replays_replay_safe_request_with_new_token() {
        let _guard = lock_serial();
        full_reset();
        set_token("stale");
        install();

        refresh::set_refresh(Rc::new(|| async { Ok("fresh".to_string()) }));

        let log = install_401_then_ok_counter();

        let result = dispatch_replay_safe();
        assert!(result.is_ok(), "expected replayed Ok, got {result:?}");
        let log = log.borrow();
        assert_eq!(log.len(), 2, "expected one 401 and one replay, got {log:?}");
        assert_eq!(log[0].as_deref(), Some("Bearer stale"));
        assert_eq!(log[1].as_deref(), Some("Bearer fresh"));
        assert_eq!(active_token().as_deref(), Some("fresh"));

        full_reset();
    }

    #[test]
    fn non_replay_safe_request_does_not_refresh() {
        let _guard = lock_serial();
        full_reset();
        set_token("stale");
        install();

        refresh::set_refresh(Rc::new(|| async {
            panic!("refresh must not run for non-idempotent requests")
        }));
        let log = install_401_then_ok_counter();

        let result = dispatch_default();
        assert!(matches!(result, Err(ServerError::Unauthorized(_))));
        assert_eq!(log.borrow().len(), 1);
        assert_eq!(active_token().as_deref(), Some("stale"));

        full_reset();
    }

    #[test]
    fn no_refresh_configured_propagates_unauthorized() {
        let _guard = lock_serial();
        full_reset();
        set_token("stale");
        install();
        let log = install_401_then_ok_counter();

        let result = dispatch_replay_safe();
        assert!(matches!(result, Err(ServerError::Unauthorized(_))));
        assert_eq!(log.borrow().len(), 1);

        full_reset();
    }

    #[test]
    fn refresh_failure_propagates_to_caller() {
        let _guard = lock_serial();
        full_reset();
        set_token("stale");
        install();

        refresh::set_refresh(Rc::new(|| async {
            Err(ServerError::unauthorized("refresh denied"))
        }));
        let log = install_401_then_ok_counter();

        let result = dispatch_replay_safe();
        match result {
            Err(ServerError::Unauthorized(msg)) => {
                assert!(
                    msg.contains("refresh denied"),
                    "expected refresh's own error message, got {msg:?}"
                );
            }
            other => panic!("expected Unauthorized from refresh, got {other:?}"),
        }
        assert_eq!(log.borrow().len(), 1);

        full_reset();
    }

    #[test]
    fn unauthorized_without_token_does_not_refresh() {
        let _guard = lock_serial();
        full_reset();
        install();

        refresh::set_refresh(Rc::new(|| async {
            panic!("refresh must not run when no token was attached")
        }));
        let _log = install_401_then_ok_counter();

        let result = dispatch_replay_safe();
        assert!(matches!(result, Err(ServerError::Unauthorized(_))));

        full_reset();
    }

    // ─── epoch fence ────────────────────────────────────────────────

    /// Server middleware that captures the auth header (proving
    /// BearerMiddleware ran), bumps the session epoch (simulating an
    /// in-flight sign-out), then returns Ok. The outer fence in
    /// BearerMiddleware should turn the Ok into Unauthorized.
    fn install_session_bumping_ok() -> Rc<RefCell<Option<String>>> {
        let captured: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let captured_inner = captured.clone();
        install_middleware(move |request: FetchRequest, _next: FetchNext| {
            let captured = captured_inner.clone();
            async move {
                *captured.borrow_mut() = request
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                    .map(|(_, v)| v.clone());
                if let Some(s) = active_session() {
                    s.set_principal(Principal::anonymous());
                }
                Ok(FetchResponse {
                    status: 200,
                    body: r#"{"Ok":null}"#.to_string(),
                })
            }
        });
        captured
    }

    fn install_session_for_test(initial: AuthSession) {
        session::__set_test_session(Some(initial));
    }

    fn full_reset_with_session() {
        full_reset();
        session::__set_test_session(None);
    }

    #[test]
    fn fence_passes_when_epoch_unchanged() {
        let _guard = lock_serial();
        full_reset_with_session();

        let session = AuthSession::new();
        session.set_principal(Principal::from_user(AuthUser::new("u1")));
        install_session_for_test(session);
        set_token("t1");
        install();

        install_middleware(|_req: FetchRequest, _next: FetchNext| async move {
            Ok(FetchResponse {
                status: 200,
                body: r#"{"Ok":null}"#.to_string(),
            })
        });

        let result = block_on(pocopine_core::fetch::call::<(), ()>("/probe", &()));
        assert!(
            result.is_ok(),
            "expected Ok with stable epoch, got {result:?}"
        );

        full_reset_with_session();
    }

    #[test]
    fn fence_drops_response_when_session_changed_mid_flight() {
        let _guard = lock_serial();
        full_reset_with_session();

        let session = AuthSession::new();
        session.set_principal(Principal::from_user(AuthUser::new("u1")));
        install_session_for_test(session);
        set_token("t1");
        install();

        let _captured = install_session_bumping_ok();

        let result = block_on(pocopine_core::fetch::call::<(), ()>("/probe", &()));
        match result {
            Err(ServerError::Unauthorized(msg)) => {
                assert_eq!(msg, "session_changed");
            }
            other => panic!("expected Unauthorized session_changed, got {other:?}"),
        }

        full_reset_with_session();
    }

    #[test]
    fn fence_does_not_apply_to_anonymous_requests() {
        let _guard = lock_serial();
        full_reset_with_session();

        let session = AuthSession::new();
        install_session_for_test(session);
        // No set_token — request will not carry auth.
        install();

        let _captured = install_session_bumping_ok();

        let result = block_on(pocopine_core::fetch::call::<(), ()>("/probe", &()));
        assert!(
            result.is_ok(),
            "anonymous request must pass even when epoch bumps, got {result:?}"
        );

        full_reset_with_session();
    }

    // ─── TokenStorage write-through + hydration ─────────────────────

    #[test]
    fn set_token_writes_through_to_configured_storage() {
        let _guard = lock_serial();
        full_reset();

        let test_storage = Rc::new(storage::TestStorage::default());
        storage::install_storage(test_storage.clone());

        set_token("persisted");
        assert_eq!(test_storage.saved.borrow().as_deref(), Some("persisted"));

        clear_token();
        assert_eq!(test_storage.saved.borrow().as_deref(), None);

        full_reset();
    }

    #[test]
    fn hydrate_from_storage_repopulates_in_memory_token() {
        let _guard = lock_serial();
        full_reset();

        let test_storage = Rc::new(storage::TestStorage::default());
        *test_storage.saved.borrow_mut() = Some("from-storage".to_string());
        storage::install_storage(test_storage.clone());

        // Slot starts empty (full_reset cleared it).
        assert_eq!(active_token(), None);

        hydrate_from_storage();
        assert_eq!(active_token().as_deref(), Some("from-storage"));

        full_reset();
    }

    #[test]
    fn hydrate_with_no_storage_is_a_noop() {
        let _guard = lock_serial();
        full_reset();
        // No storage installed.
        hydrate_from_storage();
        assert_eq!(active_token(), None);
        full_reset();
    }

    #[test]
    fn hydrate_clears_in_memory_token_when_storage_is_empty() {
        let _guard = lock_serial();
        full_reset();

        // Pre-populate the in-memory slot, then hydrate from an empty
        // storage. Hydration should clear the slot to match storage.
        set_token("in-memory");
        let test_storage = Rc::new(storage::TestStorage::default());
        storage::install_storage(test_storage);
        // saved is None.

        hydrate_from_storage();
        assert_eq!(active_token(), None);

        full_reset();
    }

    // ─── Single-flight refresh integrated through middleware ────────

    /// Install a counter middleware that returns Unauthorized on the
    /// first call, then 200 on subsequent calls. Counts how many
    /// distinct requests reached the chain bottom.
    fn install_401_then_ok_n_times(reach: Rc<std::cell::Cell<usize>>) {
        let reach_inner = reach;
        install_middleware(move |request: FetchRequest, _next: FetchNext| {
            let reach = reach_inner.clone();
            async move {
                let _ = request;
                let attempt = {
                    let n = reach.get() + 1;
                    reach.set(n);
                    n
                };
                if attempt == 1 {
                    Ok(FetchResponse {
                        status: 401,
                        body: r#"{"Err":{"Unauthorized":"token expired"}}"#.to_string(),
                    })
                } else {
                    Ok(FetchResponse {
                        status: 200,
                        body: r#"{"Ok":null}"#.to_string(),
                    })
                }
            }
        });
    }

    #[test]
    fn single_flight_refresh_through_middleware() {
        // Verifies the middleware path uses refresh_single_flight,
        // not refresh.refresh() directly. We can't easily run two
        // concurrent fetch::call invocations through one block_on,
        // so this is the integration-level smoke: one call with
        // refresh configured drives one refresh, replays once, gets
        // Ok.
        let _guard = lock_serial();
        full_reset();

        set_token("stale");
        install();

        let refresh_count = Rc::new(std::cell::Cell::new(0));
        let refresh_count_inner = refresh_count.clone();
        refresh::set_refresh(Rc::new(move || {
            let c = refresh_count_inner.clone();
            async move {
                c.set(c.get() + 1);
                Ok("fresh".to_string())
            }
        }));

        let chain_calls = Rc::new(std::cell::Cell::new(0));
        install_401_then_ok_n_times(chain_calls.clone());

        let result = block_on(pocopine_core::fetch::call_replay_safe::<(), ()>(
            "/probe",
            &(),
        ));
        assert!(result.is_ok());
        assert_eq!(refresh_count.get(), 1);
        assert_eq!(chain_calls.get(), 2, "one 401 + one replay");
        assert_eq!(active_token().as_deref(), Some("fresh"));

        full_reset();
    }

    #[test]
    fn refresh_does_not_pollute_token_slot_after_signout_during_flight() {
        // P1 scenario: user calls sign_out() while a refresh is in
        // flight. Refresh resolves with a fresh token; the middleware
        // must NOT write that token to the slot — otherwise the
        // signed-out user has a valid bearer for the next request.
        let _guard = lock_serial();
        full_reset_with_session();

        let session = AuthSession::new();
        session.set_principal(Principal::from_user(AuthUser::new("u1")));
        install_session_for_test(session.clone());
        set_token("stale");
        install();

        // Refresh that mutates session epoch (simulating sign-out
        // arriving while refresh is in flight) before returning the
        // new token.
        let session_for_refresh = session.clone();
        refresh::set_refresh(Rc::new(move || {
            let s = session_for_refresh.clone();
            async move {
                // Sign-out happens while we wait for the issuer.
                s.set_principal(Principal::anonymous());
                Ok("would-be-leaked".to_string())
            }
        }));

        // 401 then 200 (so a successful retry would happen if the
        // middleware didn't gate on epoch).
        let chain_calls = Rc::new(std::cell::Cell::new(0));
        install_401_then_ok_n_times(chain_calls.clone());

        let result = block_on(pocopine_core::fetch::call_replay_safe::<(), ()>(
            "/probe",
            &(),
        ));

        match result {
            Err(ServerError::Unauthorized(msg)) => {
                assert!(
                    msg.contains("session_changed") || msg.contains("session"),
                    "unexpected reason: {msg}"
                );
            }
            other => panic!("expected Unauthorized session_changed, got {other:?}"),
        }
        // Token slot must NOT contain the leaked token. The pre-refresh
        // value ("stale") is fine; the leaked value ("would-be-leaked")
        // is not. Either is acceptable as long as we didn't write the
        // refreshed bearer post-signout.
        let token = active_token();
        assert_ne!(
            token.as_deref(),
            Some("would-be-leaked"),
            "refresh post-signout must not pollute the token slot"
        );
        // The replay must NOT have happened either (chain saw the 401
        // only).
        assert_eq!(chain_calls.get(), 1, "replay must not run after signout");

        full_reset_with_session();
    }

    #[test]
    fn aborted_refresh_unblocks_waiters_with_error() {
        // Drive refresh_single_flight directly. First caller drops
        // its future before the refresh resolves — second caller
        // should not hang; it should receive the synthetic
        // "refresh aborted" Unauthorized.
        let _guard = lock_serial();
        full_reset();

        // Refresh future yields once before resolving so we can drop
        // the driver mid-flight.
        let yielded = Rc::new(std::cell::Cell::new(false));
        let yielded_inner = yielded.clone();
        refresh::set_refresh(Rc::new(move || {
            let yielded = yielded_inner.clone();
            async move {
                if !yielded.get() {
                    yielded.set(true);
                    yield_once().await;
                }
                Ok("never-reached".to_string())
            }
        }));

        // Driver
        let mut driver = Box::pin(refresh::refresh_single_flight());
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(driver.as_mut().poll(&mut cx), Poll::Pending));

        // Spawn a waiter while the driver is still pending.
        let mut waiter = Box::pin(refresh::refresh_single_flight());
        assert!(matches!(waiter.as_mut().poll(&mut cx), Poll::Pending));

        // Drop the driver — its ClearOnDrop guard publishes a
        // synthetic error and clears IN_FLIGHT. The waiter must
        // resolve to that error on next poll.
        drop(driver);

        match waiter.as_mut().poll(&mut cx) {
            Poll::Ready(Err(e)) => match &*e {
                ServerError::Unauthorized(msg) => {
                    assert!(msg.contains("aborted"), "unexpected reason: {msg}");
                }
                other => panic!("expected Unauthorized, got {other:?}"),
            },
            other => panic!("waiter must resolve after driver drops, got {other:?}"),
        }

        full_reset();
    }
}
