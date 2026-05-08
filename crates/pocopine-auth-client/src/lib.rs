//! Wasm-side auth surface for pocopine.
//!
//! Four surfaces:
//!
//! - **Token slot.** [`set_token`], [`clear_token`], and [`active_token`]
//!   manage a process-global `Option<String>`.
//! - **Fetch middleware.** [`BearerMiddleware`] reads the active token
//!   and attaches `Authorization: Bearer <token>` to every outgoing
//!   `#[server]` request via the RFC-078 fetch chain. Apps register it
//!   once at startup with [`install`] (or via
//!   [`auth_plugin().with_bearer_middleware(true)`](auth_plugin)).
//! - **Reactive identity.** [`AuthSession`] is a plugin service the
//!   [`auth_plugin`] installs. It holds the active [`pocopine_auth::Principal`]
//!   plus a monotonic epoch; components extract it via
//!   `Plugin<AuthSession>` / `Option<Plugin<AuthSession>>` per
//!   RFC-076.
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
//! // At app boot, before App::run:
//! pocopine_auth_client::install();
//!
//! // After successful sign-in (Firebase SDK, Clerk SDK, your own
//! // login server function, etc.):
//! pocopine_auth_client::set_token(token);
//!
//! // Subsequent #[server] calls authenticate automatically:
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
//! - **Not a refresh helper.** Apps decide refresh strategy. When the
//!   current token expires the next `#[server]` call returns
//!   `ServerError::Unauthorized` and the app calls [`set_token`] again.
//!   Single-flight refresh + replay-on-401 is deferred — depends on
//!   `#[server(idempotent)]` (RFC-078 §5.10.4).
//! - **Not provider-coupled.** [`set_token`] accepts any string; the
//!   server-side verifier decides validity.
//! - **Not a session-epoch guard.** Aborting in-flight requests when
//!   identity changes is deferred — depends on a future `AuthSession`
//!   plugin service (RFC-078 §5.10.5).
//!
//! ## Privacy
//!
//! The middleware never logs the token. Per RFC-077 §6, no body,
//! headers, cookies, or query strings enter framework events. Bearer
//! attachment is a pure mutation on the in-flight `FetchRequest`.

mod plugin;
mod session;

pub use plugin::{auth_plugin, predicate_guard, AuthPluginBuilder, DEFAULT_RETURN_TO_PARAM};
pub use session::{active_principal, active_session, AuthSession};

use std::sync::Mutex;
use std::sync::OnceLock;

use pocopine_core::fetch::{
    install_middleware, FetchMiddleware, FetchMiddlewareFuture, FetchNext, FetchRequest,
};

static TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn store() -> &'static Mutex<Option<String>> {
    TOKEN.get_or_init(|| Mutex::new(None))
}

/// Register the bearer token attached to subsequent `#[server]` calls.
/// Replaces any previously-set token.
pub fn set_token(token: impl Into<String>) {
    if let Ok(mut guard) = store().lock() {
        *guard = Some(token.into());
    }
}

/// Drop the active token. Subsequent calls go out without an
/// `Authorization` header.
pub fn clear_token() {
    if let Ok(mut guard) = store().lock() {
        *guard = None;
    }
}

/// Read the active token, if any.
pub fn active_token() -> Option<String> {
    store().lock().ok().and_then(|guard| guard.clone())
}

/// Fetch middleware that attaches `Authorization: Bearer <token>` to
/// every outgoing request when [`active_token`] is `Some`. Uses
/// `FetchRequest::set_header` so a re-set replaces rather than
/// duplicates the header.
pub struct BearerMiddleware;

impl FetchMiddleware for BearerMiddleware {
    fn call(&self, mut request: FetchRequest, next: FetchNext) -> FetchMiddlewareFuture {
        if let Some(token) = active_token() {
            request.set_header("authorization", format!("Bearer {token}"));
        }
        Box::pin(async move { next.run(request).await })
    }
}

/// Install [`BearerMiddleware`] on the global fetch chain. MUST be
/// called before the first `App::run` / `pocopine_core::fetch::call`,
/// per RFC-078 §5.10.3 (fetch middleware is trusted-plugin code; the
/// chain freezes at boot). Calling [`install`] after the chain is
/// frozen panics with the diagnostic from
/// `pocopine_core::fetch::install_middleware`.
pub fn install() {
    install_middleware(BearerMiddleware);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_core::fetch::{
        __reset_middleware_chain_for_test, freeze_middleware_chain, FetchResponse,
    };
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    // The token store is process-global. Serialize all tests so they
    // don't observe each other's writes. The middleware chain is
    // thread-local so does not need this serialization, but tests in
    // this module assert on combined token/middleware state, so SERIAL
    // covers both.
    static SERIAL: Mutex<()> = Mutex::new(());

    // ─── token slot ─────────────────────────────────────────────────

    #[test]
    fn set_then_active_returns_the_set_token() {
        let _guard = SERIAL.lock().unwrap();
        clear_token();
        set_token("alpha");
        assert_eq!(active_token().as_deref(), Some("alpha"));
        clear_token();
    }

    #[test]
    fn clear_token_removes_the_value() {
        let _guard = SERIAL.lock().unwrap();
        set_token("beta");
        assert_eq!(active_token().as_deref(), Some("beta"));
        clear_token();
        assert_eq!(active_token(), None);
    }

    #[test]
    fn set_token_replaces_previous() {
        let _guard = SERIAL.lock().unwrap();
        set_token("first");
        set_token("second");
        assert_eq!(active_token().as_deref(), Some("second"));
        clear_token();
    }

    #[test]
    fn active_token_with_nothing_set_returns_none() {
        let _guard = SERIAL.lock().unwrap();
        clear_token();
        assert_eq!(active_token(), None);
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
    /// through the chain. Installs a capture middleware AFTER the
    /// caller has installed `BearerMiddleware`, so the captured
    /// `FetchRequest` reflects the post-bearer-attach state.
    /// Returns the captured request.
    fn dispatch_and_capture() -> FetchRequest {
        let seen: Rc<RefCell<Option<FetchRequest>>> = Rc::new(RefCell::new(None));
        let seen_inner = seen.clone();
        install_middleware(move |request: FetchRequest, _next: FetchNext| {
            *seen_inner.borrow_mut() = Some(request);
            async move {
                Ok(FetchResponse {
                    status: 200,
                    // Result<(), ServerError>::Ok(()) — serde's
                    // externally-tagged Result encoding.
                    body: r#"{"Ok":null}"#.to_string(),
                })
            }
        });

        // Drive the chain. The result is irrelevant to our assertions
        // (we only care about what the capture middleware saw); ignore
        // any deserialization outcome.
        let _ = block_on(pocopine_core::fetch::call::<(), ()>("/probe", &()));

        let captured = seen
            .borrow_mut()
            .take()
            .expect("capture middleware never observed a request");
        captured
    }

    #[test]
    fn bearer_middleware_attaches_authorization_when_token_set() {
        let _guard = SERIAL.lock().unwrap();
        __reset_middleware_chain_for_test();
        set_token("token-abc");
        install();

        let captured = dispatch_and_capture();
        let auth = captured
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth, Some("Bearer token-abc"));

        clear_token();
        __reset_middleware_chain_for_test();
    }

    #[test]
    fn bearer_middleware_omits_authorization_when_token_unset() {
        let _guard = SERIAL.lock().unwrap();
        __reset_middleware_chain_for_test();
        clear_token();
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

        __reset_middleware_chain_for_test();
    }

    #[test]
    fn bearer_middleware_replaces_authorization_on_token_change() {
        let _guard = SERIAL.lock().unwrap();

        // First dispatch with token "first".
        __reset_middleware_chain_for_test();
        set_token("first");
        install();
        let first = dispatch_and_capture();
        let auth_first = first
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth_first, Some("Bearer first"));
        let count_first = first
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .count();
        assert_eq!(count_first, 1);

        // Second dispatch with token "second" — must replace, not
        // duplicate.
        __reset_middleware_chain_for_test();
        set_token("second");
        install();
        let second = dispatch_and_capture();
        let auth_second = second
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth_second, Some("Bearer second"));
        let count_second = second
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .count();
        assert_eq!(
            count_second, 1,
            "expected exactly one authorization header after token replace"
        );

        clear_token();
        __reset_middleware_chain_for_test();
    }

    #[test]
    #[should_panic(expected = "fetch::install_middleware called after the chain was frozen")]
    fn install_after_freeze_panics() {
        let guard = SERIAL.lock().unwrap();
        __reset_middleware_chain_for_test();
        install();
        freeze_middleware_chain();
        // Drop the SERIAL guard before the expected panic so it
        // isn't poisoned for subsequent tests; the chain itself is
        // thread-local and other tests reset it before their own
        // assertions.
        drop(guard);
        // The second install must panic.
        install();
    }
}
