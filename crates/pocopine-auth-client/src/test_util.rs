//! Crate-private test helpers shared across module tests. Compiled
//! only under `#[cfg(test)]`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}

/// Public for tests that need to manually poll a future (e.g.,
/// drop-mid-await scenarios) rather than using the `block_on`
/// convenience.
pub(crate) fn noop_waker() -> Waker {
    Arc::new(NoopWake).into()
}

/// Spin-poll any `Future` to completion. Adequate because the
/// futures driven by these tests don't bind a real runtime — they
/// either resolve synchronously or schedule their own waker via
/// [`yield_once`].
pub(crate) fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut fut = Box::pin(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => continue,
        }
    }
}

/// Like [`block_on`] but for already-`Unpin` futures (e.g., a
/// `Pin<Box<F>>` aggregator) that you want to poll without a
/// double-Box.
pub(crate) fn block_on_unpin<F: Future + Unpin>(mut fut: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    loop {
        match Pin::new(&mut fut).poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => continue,
        }
    }
}

/// Future that returns `Pending` once, schedules its own waker, and
/// resolves on the next poll. Use to introduce a real `.await` point
/// in synthetic test futures so concurrent callers can interleave.
pub(crate) async fn yield_once() {
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
