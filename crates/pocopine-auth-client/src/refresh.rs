//! Token-refresh contract used by [`crate::BearerMiddleware`] when an
//! outgoing `#[server]` call returns [`ServerError::Unauthorized`] for a
//! request the client marked replay-safe (`#[server(idempotent)]`).
//!
//! The contract is provider-agnostic: pocopine doesn't know how
//! Firebase, Clerk, or your own JWT issuer mints fresh credentials.
//! Apps install a refresh implementation through
//! [`crate::AuthPluginBuilder::with_token_refresh`] and the middleware
//! invokes it on demand, calling [`crate::set_token`] with the fresh
//! value before replaying once. If no refresh is configured the
//! middleware propagates `Unauthorized` unchanged — fail-closed per
//! RFC-078 §5.10.4.
//!
//! ## Single-flight
//!
//! Concurrent in-flight requests that all 401 used to each fire an
//! independent refresh, hammering the issuer. The middleware now
//! routes every refresh request through [`refresh_single_flight`]:
//! the first caller drives the refresh future to completion; peers
//! that arrive while it's in flight wait on the same outcome.
//! Resolution wakes all waiters and clears the slot for the next
//! refresh window.
//!
//! Wasm is single-threaded, so "concurrent" means "interleaved via
//! `.await`". The coordinator is `Rc<RefCell<…>>`-backed (no atomics
//! needed) and is correct for that execution model.
//!
//! [`ServerError::Unauthorized`]: pocopine_core::ServerError::Unauthorized

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

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
    static IN_FLIGHT: RefCell<Option<Rc<RefreshSlot>>> = const { RefCell::new(None) };
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
    IN_FLIGHT.with(|cell| *cell.borrow_mut() = None);
}

// ─── Single-flight coordinator ──────────────────────────────────────

/// Shared outcome cell for the in-flight refresh. Result is wrapped in
/// `Rc` so all waiters get a cheap clone instead of duplicating either
/// the token string or the `ServerError`.
struct RefreshSlot {
    result: RefCell<Option<Result<Rc<String>, Rc<ServerError>>>>,
    wakers: RefCell<Vec<Waker>>,
}

/// Run the configured `TokenRefresh` if one is installed; if a refresh
/// is already in flight on this thread, await its outcome instead of
/// starting a new one. All concurrent callers receive the same shared
/// `Result<Rc<String>, Rc<ServerError>>`.
pub(crate) async fn refresh_single_flight() -> Result<Rc<String>, Rc<ServerError>> {
    if let Some(slot) = IN_FLIGHT.with(|cell| cell.borrow().clone()) {
        return SlotWait { slot }.await;
    }

    let slot = Rc::new(RefreshSlot {
        result: RefCell::new(None),
        wakers: RefCell::new(Vec::new()),
    });
    IN_FLIGHT.with(|cell| *cell.borrow_mut() = Some(slot.clone()));

    // RAII guard: clears the in-flight slot on every exit path,
    // including the case where the driver future is dropped before
    // `refresh.refresh().await` resolves. Without this, peer waiters
    // would hang on a never-published slot.
    let _guard = ClearOnDrop { slot: slot.clone() };

    let result = match current_refresh() {
        Some(r) => r.refresh().await.map(Rc::new).map_err(Rc::new),
        None => Err(Rc::new(ServerError::unauthorized(
            "no token refresh configured",
        ))),
    };

    publish(&slot, result.clone());
    result
}

fn publish(slot: &Rc<RefreshSlot>, result: Result<Rc<String>, Rc<ServerError>>) {
    *slot.result.borrow_mut() = Some(result);
    let wakers: Vec<_> = slot.wakers.borrow_mut().drain(..).collect();
    for w in wakers {
        w.wake();
    }
}

struct SlotWait {
    slot: Rc<RefreshSlot>,
}

impl Future for SlotWait {
    type Output = Result<Rc<String>, Rc<ServerError>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(result) = self.slot.result.borrow().clone() {
            return Poll::Ready(result);
        }
        self.slot.wakers.borrow_mut().push(cx.waker().clone());
        Poll::Pending
    }
}

/// Clear the in-flight slot on Drop and, if no result was ever
/// published, surface a synthetic error to waiters so they don't
/// hang. Drops happen on:
/// 1. Normal completion (after `publish` set the result).
/// 2. The driving future being cancelled mid-await.
/// 3. A panic in `refresh.refresh()`.
struct ClearOnDrop {
    slot: Rc<RefreshSlot>,
}

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        let unpublished = self.slot.result.borrow().is_none();
        if unpublished {
            publish(
                &self.slot,
                Err(Rc::new(ServerError::unauthorized("refresh aborted"))),
            );
        }
        IN_FLIGHT.with(|cell| {
            // Only clear if the cell still points at our slot — a
            // re-entrant test reset may have already cleared it.
            let mut cell = cell.borrow_mut();
            let same = cell
                .as_ref()
                .map(|in_flight| Rc::ptr_eq(in_flight, &self.slot))
                .unwrap_or(false);
            if same {
                *cell = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Wake, Waker};

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

    #[test]
    fn single_flight_dedupes_concurrent_callers() {
        __reset_refresh_for_test();

        // Refresh yields once before resolving so the second caller
        // can enter `refresh_single_flight` while the first is still
        // in-flight. Without the yield, refresh would resolve
        // synchronously and the second caller would arrive after the
        // slot was already cleared — a meaningless test.
        let count: Rc<std::cell::Cell<usize>> = Rc::new(std::cell::Cell::new(0));
        let count_inner = count.clone();
        set_refresh(Rc::new(move || {
            let count = count_inner.clone();
            async move {
                yield_once().await;
                count.set(count.get() + 1);
                Ok("shared-fresh".to_string())
            }
        }));

        let f1 = Box::pin(refresh_single_flight());
        let f2 = Box::pin(refresh_single_flight());

        let pair = Box::pin(join(f1, f2));
        let (r1, r2) = block_on_unpin(pair);

        assert_eq!(count.get(), 1, "single-flight must coalesce");
        let r1 = r1.expect("first caller resolved Err");
        let r2 = r2.expect("second caller resolved Err");
        assert_eq!(&*r1, "shared-fresh");
        assert_eq!(&*r2, "shared-fresh");
        assert!(
            Rc::ptr_eq(&r1, &r2),
            "both waiters must share the same Rc<String>"
        );

        __reset_refresh_for_test();
    }

    /// Future that returns Pending once, schedules its own waker,
    /// then resolves on the next poll.
    async fn yield_once() {
        struct YieldOnce(bool);
        impl Future for YieldOnce {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.0 {
                    Poll::Ready(())
                } else {
                    self.0 = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }
        YieldOnce(false).await
    }

    #[test]
    fn single_flight_releases_after_completion() {
        __reset_refresh_for_test();

        let count: Rc<std::cell::Cell<usize>> = Rc::new(std::cell::Cell::new(0));
        let count_inner = count.clone();
        set_refresh(Rc::new(move || {
            let count = count_inner.clone();
            async move {
                count.set(count.get() + 1);
                Ok("ok".to_string())
            }
        }));

        let _ = block_on(refresh_single_flight());
        let _ = block_on(refresh_single_flight());
        assert_eq!(count.get(), 2, "second call must start a new refresh");

        __reset_refresh_for_test();
    }

    #[test]
    fn no_refresh_configured_yields_unauthorized() {
        __reset_refresh_for_test();
        let result = block_on(refresh_single_flight());
        assert!(matches!(result, Err(e) if matches!(*e, ServerError::Unauthorized(_))));
    }

    fn block_on<F: Future>(fut: F) -> F::Output {
        struct NoopWake;
        impl Wake for NoopWake {
            fn wake(self: Arc<Self>) {}
            fn wake_by_ref(self: &Arc<Self>) {}
        }
        let waker: Waker = Arc::new(NoopWake).into();
        let mut cx = std::task::Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => continue,
            }
        }
    }

    fn block_on_unpin<F: Future + Unpin>(mut fut: F) -> F::Output {
        struct NoopWake;
        impl Wake for NoopWake {
            fn wake(self: Arc<Self>) {}
            fn wake_by_ref(self: &Arc<Self>) {}
        }
        let waker: Waker = Arc::new(NoopWake).into();
        let mut cx = std::task::Context::from_waker(&waker);
        loop {
            match Pin::new(&mut fut).poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => continue,
            }
        }
    }

    /// Hand-rolled join, avoids pulling `futures-util` just for tests.
    struct Join<A: Future, B: Future> {
        a: Option<Pin<Box<A>>>,
        b: Option<Pin<Box<B>>>,
        a_out: Option<A::Output>,
        b_out: Option<B::Output>,
    }

    fn join<A: Future, B: Future>(a: Pin<Box<A>>, b: Pin<Box<B>>) -> Join<A, B> {
        Join {
            a: Some(a),
            b: Some(b),
            a_out: None,
            b_out: None,
        }
    }

    impl<A: Future, B: Future> Future for Join<A, B> {
        type Output = (A::Output, B::Output);
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // Safety: we never move `self` out, and the inner Box<F>
            // futures are individually pinned (Pin<Box<F>>). Treating
            // `Self` as Unpin is sound for this aggregator.
            let this = unsafe { self.get_unchecked_mut() };
            if this.a_out.is_none() {
                if let Some(fut) = this.a.as_mut() {
                    if let Poll::Ready(v) = fut.as_mut().poll(cx) {
                        this.a_out = Some(v);
                        this.a = None;
                    }
                }
            }
            if this.b_out.is_none() {
                if let Some(fut) = this.b.as_mut() {
                    if let Poll::Ready(v) = fut.as_mut().poll(cx) {
                        this.b_out = Some(v);
                        this.b = None;
                    }
                }
            }
            match (this.a_out.take(), this.b_out.take()) {
                (Some(a), Some(b)) => Poll::Ready((a, b)),
                (a, b) => {
                    this.a_out = a;
                    this.b_out = b;
                    Poll::Pending
                }
            }
        }
    }

    impl<A: Future, B: Future> Unpin for Join<A, B> {}
}
