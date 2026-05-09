//! Token-refresh contract used by [`crate::BearerMiddleware`] when an
//! outgoing `#[server]` call returns [`ServerError::Unauthorized`] for a
//! request the client marked replay-safe (`#[server(idempotent)]`).
//!
//! The contract is provider-agnostic: pocopine doesn't know how Firebase,
//! Clerk, or your own JWT issuer mints fresh credentials. Apps install a
//! refresh implementation through
//! [`crate::AuthPluginBuilder::with_token_refresh`] and the middleware
//! invokes it on demand, calling [`crate::set_token`] with the fresh value
//! before replaying once. If no refresh is configured the middleware
//! propagates `Unauthorized` unchanged — fail-closed per RFC-078 §5.10.4.
//!
//! ## Single-flight
//!
//! The current implementation runs **at most one replay per failed
//! request**, but does *not* deduplicate concurrent refreshes across
//! parallel in-flight requests: if requests A and B both 401 while a
//! refresh is in flight, B can start a second refresh. Single-flight
//! coordination (one shared refresh future per stale-token window) is a
//! follow-up — the wasm runtime is single-threaded so the practical
//! impact is bounded, but a coordinator avoids hammering the issuer.
//!
//! [`ServerError::Unauthorized`]: pocopine_core::ServerError::Unauthorized

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use pocopine_core::ServerError;

/// Future returned by [`TokenRefresh::refresh`].
pub type TokenRefreshFuture = Pin<Box<dyn Future<Output = Result<String, ServerError>> + 'static>>;

/// App-supplied bearer-token refresh.
///
/// Implementations talk to whatever identity provider the app uses
/// (Firebase, Clerk, a custom `#[server]` function backed by a refresh
/// cookie, etc.) and return a fresh bearer string. The middleware calls
/// [`crate::set_token`] with the result before replaying.
///
/// On `Err`, the original `Unauthorized` propagates upward.
pub trait TokenRefresh: 'static {
    fn refresh(&self) -> TokenRefreshFuture;
}

impl<F, Fut> TokenRefresh for F
where
    F: Fn() -> Fut + 'static,
    Fut: Future<Output = Result<String, ServerError>> + 'static,
{
    fn refresh(&self) -> TokenRefreshFuture {
        Box::pin(self())
    }
}

thread_local! {
    static REFRESH: RefCell<Option<Rc<dyn TokenRefresh>>> = const { RefCell::new(None) };
}

/// Register the active token-refresh implementation. Called from
/// [`crate::AuthPluginBuilder::install`] when a refresh fn is configured.
/// Replaces any previously-set refresh; multiple `auth_plugin` installs
/// in the same process aren't supported anyway.
pub(crate) fn set_refresh(refresh: Rc<dyn TokenRefresh>) {
    REFRESH.with(|cell| *cell.borrow_mut() = Some(refresh));
}

pub(crate) fn current_refresh() -> Option<Rc<dyn TokenRefresh>> {
    REFRESH.with(|cell| cell.borrow().clone())
}

#[doc(hidden)]
pub fn __reset_refresh_for_test() {
    REFRESH.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_blanket_impl_drives_through_trait_object() {
        let refresh: Rc<dyn TokenRefresh> = Rc::new(|| async { Ok("new-token".to_string()) });
        let result = block_on(refresh.refresh());
        assert!(matches!(result.as_deref(), Ok("new-token")));
    }

    #[test]
    fn closure_can_propagate_refresh_error() {
        let refresh: Rc<dyn TokenRefresh> =
            Rc::new(|| async { Err(ServerError::unauthorized("refresh denied")) });
        let result = block_on(refresh.refresh());
        assert!(matches!(result, Err(ServerError::Unauthorized(_))));
    }

    /// Minimal block_on: spin-poll a `Pin<Box<dyn Future>>` until it
    /// resolves. Adequate because the futures here never yield to a
    /// runtime — they resolve on the first poll.
    fn block_on<T>(mut fut: Pin<Box<dyn Future<Output = T>>>) -> T {
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};
        struct NoopWake;
        impl Wake for NoopWake {
            fn wake(self: Arc<Self>) {}
            fn wake_by_ref(self: &Arc<Self>) {}
        }
        let waker: Waker = Arc::new(NoopWake).into();
        let mut cx = Context::from_waker(&waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => continue,
            }
        }
    }
}
