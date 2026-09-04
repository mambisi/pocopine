//! Crate-private test helpers shared across module tests. Compiled
//! only under `#[cfg(test)]`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};

/// The token slot, the `INSTALLED` flag, the refresh single-flight, the
/// snapshot storage, and the `pocopine_core::fetch` middleware chain are
/// all process- or thread-local globals. `cargo test` runs this crate's
/// tests on several threads in one process, so every test that reads or
/// writes any of them must hold this lock for its whole body — otherwise
/// one test's reset lands between another's setup and assertion.
///
/// It lives here rather than in a single module's `mod tests` because the
/// globals are shared crate-wide: `session`, `plugin`, `refresh`, and
/// `storage` all reach them.
static SERIAL: Mutex<()> = Mutex::new(());

/// Acquire [`SERIAL`] while tolerating prior-test poison: the
/// `should_panic` cases panic by design, and a stray panic elsewhere would
/// otherwise cascade into "every other test panics on `Mutex::lock`". What
/// we need is "only one test holds it at a time"; poison state is noise.
pub(crate) fn lock_serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

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
