//! Wasm-side auth surface for pocopine.
//!
//! Five surfaces:
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
//! - **Reactive identity.** [`AuthSession`] is a plugin service the
//!   [`auth_plugin`] installs. It holds the active [`pocopine_auth::Principal`]
//!   plus a monotonic epoch; components extract it via
//!   `Plugin<AuthSession>` / `Option<Plugin<AuthSession>>` per RFC-076.
//!   The bearer middleware also captures the epoch on outgoing
//!   authenticated requests and fences the response on completion: if the
//!   user signed in/out while the request was in flight, the middleware
//!   returns `Err(ServerError::Unauthorized("session_changed"))` instead
//!   of letting stale-identity data through (RFC-078 §5.10.5).
//! - **Auth-aware route guards + login redirect.** [`auth_plugin`]
//!   is an [`AppPlugin`](pocopine_core::AppPlugin) that registers a
//!   [`RouteRejectionHandler`](pocopine_core::RouteRejectionHandler)
//!   mapping `RouteRejection::Unauthorized` to the configured login
//!   route. [`predicate_guard`] adapts any
//!   [`pocopine_auth::Predicate`] (`require_auth`, `require_role`,
//!   …) into a [`RouteGuard`](pocopine_core::RouteGuard) that reads
//!   the live [`AuthSession`].
//! - **Token refresh.** Apps install a provider-specific [`TokenRefresh`]
//!   via [`AuthPluginBuilder::with_token_refresh`]; the bearer middleware
//!   uses it for the replay-on-401 flow described above.
//!
//! ## Usage
//!
//! ```ignore
//! use pocopine::App;
//! use pocopine_auth_client::auth_plugin;
//!
//! App::new()
//!     .plugin(
//!         auth_plugin()
//!             .login_route("/login")
//!             .with_bearer_middleware(true)
//!             .with_token_refresh(|| async {
//!                 // Talk to your provider here. For example:
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
//! ## What this is *not*
//!
//! - **Not a token store.** Storage is in-memory and per-process;
//!   persistence across reloads (cookie, localStorage, IndexedDB) is
//!   the app's job.
//! - **Not provider-coupled.** [`set_token`] accepts any string; the
//!   server-side verifier decides validity. [`TokenRefresh`] is a
//!   provider-supplied closure or struct, not a built-in.
//! - **Not a single-flight refresh coordinator yet.** Multiple
//!   concurrent requests that all 401 may each trigger an independent
//!   refresh. The wasm runtime is single-threaded so the practical
//!   blast radius is bounded; coalescing is a follow-up — see
//!   [`refresh`] module docs.
//!
//! ## Privacy
//!
//! The middleware never logs the token. Per RFC-077 §6, no body,
//! headers, cookies, or query strings enter framework events. Bearer
//! attachment is a pure mutation on the in-flight `FetchRequest`.

mod plugin;
mod refresh;
mod session;

pub use plugin::{auth_plugin, predicate_guard, AuthPluginBuilder, DEFAULT_RETURN_TO_PARAM};
pub use refresh::{TokenRefresh, TokenRefreshFuture};
pub use session::{active_principal, active_session, AuthSession};

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
/// Replaces any previously-set token.
pub fn set_token(token: impl Into<String>) {
    *TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned") = Some(token.into());
}

/// Drop the active token. Subsequent calls go out without an
/// `Authorization` header.
pub fn clear_token() {
    *TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned") = None;
}

/// Read the active token, if any.
pub fn active_token() -> Option<String> {
    TOKEN
        .lock()
        .expect("pocopine_auth_client::TOKEN mutex poisoned")
        .clone()
}

/// Fetch middleware that attaches `Authorization: Bearer <token>` to
/// every outgoing request when [`active_token`] is `Some`, refreshes
/// the token once on `Unauthorized` for replay-safe requests, and
/// fences the response against identity changes that happened while
/// the request was in flight.
///
/// All three concerns live in one middleware so the active-token,
/// epoch, and refresh state are observed atomically with respect to
/// one outgoing request — a single rejection handler position in the
/// chain (registered once at `install` time) wins by being simple to
/// reason about.
pub struct BearerMiddleware;

impl FetchMiddleware for BearerMiddleware {
    fn call(&self, mut request: FetchRequest, next: FetchNext) -> FetchMiddlewareFuture {
        // Single read of the token slot: the "did we authenticate this
        // request?" decision and the actual header attachment must
        // observe the same value, otherwise a concurrent `set_token` /
        // `clear_token` between two locks could leave us with
        // had_token=true but no header attached (or vice versa). Wasm
        // is single-threaded so this is impossible in production, but
        // tighten the invariant for host-test robustness and to keep
        // the code honest if pocopine ever grows a multi-threaded
        // wasm runtime.
        let token_snapshot = active_token();
        let had_token = token_snapshot.is_some();
        let captured_epoch = if had_token {
            // Only capture when we're actually authenticating this
            // request. An anonymous request can't go stale on identity
            // change because it didn't depend on identity.
            session::active_session().map(|s| s.epoch())
        } else {
            None
        };

        if let Some(token) = token_snapshot.as_deref() {
            request.set_header("authorization", format!("Bearer {token}"));
        }

        let is_replay_safe = request.is_replay_safe();
        Box::pin(async move {
            let response = next.clone().run(request.clone()).await;

            // Normalize 401-shaped signals. `pocopine_core::fetch::call`
            // converts non-2xx to `ServerError::Network("HTTP 401")` at
            // the top, dropping any body before the auth middleware can
            // see it. We need to recognise the auth-shaped error before
            // that happens, so we treat both `Ok(status == 401)` and
            // `Err(Unauthorized)` as the same logical signal.
            let unauthorized = matches!(&response, Ok(r) if r.status == 401)
                || matches!(&response, Err(ServerError::Unauthorized(_)));

            // Refresh-on-Unauthorized + replay once. Gated on
            // `is_replay_safe` (i.e. `#[server(idempotent)]`) per RFC-078
            // §5.10.4 and on having actually attached a token (refresh
            // can't help an endpoint that 401s without auth context).
            let response = if unauthorized && had_token && is_replay_safe {
                if let Some(refresh) = refresh::current_refresh() {
                    match refresh.refresh().await {
                        Ok(new_token) => {
                            set_token(new_token.clone());
                            let mut retry = request;
                            retry.set_header("authorization", format!("Bearer {new_token}"));
                            next.run(retry).await
                        }
                        Err(err) => Err(err),
                    }
                } else {
                    // No refresh configured — propagate fail-closed with
                    // an auth-shaped error so the caller can match on it.
                    Err(ServerError::unauthorized("token expired"))
                }
            } else if unauthorized && had_token {
                // Replay disabled (or non-idempotent server fn). Still
                // surface the Unauthorized shape for callers.
                Err(ServerError::unauthorized("token expired"))
            } else if unauthorized {
                // Anonymous request that 401'd. Refresh wouldn't help;
                // surface the auth shape for the caller.
                Err(ServerError::unauthorized("auth required"))
            } else {
                response
            };

            // Identity-change fence. Refresh rotates the token but does
            // *not* bump the epoch (epoch tracks principal changes), so
            // a successful refresh-replay still passes the fence. A
            // concurrent sign-in/sign-out that bumped the epoch trips
            // it — the response is from the previous identity.
            if let Some(captured) = captured_epoch {
                let now = session::active_session().map(|s| s.epoch());
                if now != Some(captured) {
                    return Err(ServerError::unauthorized("session_changed"));
                }
            }

            response
        })
    }
}

thread_local! {
    /// Tracks whether [`install`] has already registered
    /// [`BearerMiddleware`] on the current thread's chain. Makes
    /// repeated calls (e.g. `install()` plus
    /// `auth_plugin().with_bearer_middleware(true)`) safe — the
    /// second one is a no-op rather than a silent
    /// double-register.
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
    use pocopine_auth::{AuthUser, Principal};
    use pocopine_core::fetch::{
        __reset_middleware_chain_for_test, freeze_middleware_chain, FetchResponse,
    };
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

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

    /// Synchronous block_on for tests. The chain returns immediately
    /// (no real network), so a single poll-loop is sufficient.
    fn block_on<F: Future>(future: F) -> F::Output {
        struct NoopWake;
        impl Wake for NoopWake {
            fn wake(self: Arc<Self>) {}
            fn wake_by_ref(self: &Arc<Self>) {}
        }
        let waker: Waker = Arc::new(NoopWake).into();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(future);
        loop {
            match Pin::new(&mut fut).as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => continue,
            }
        }
    }

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
}
