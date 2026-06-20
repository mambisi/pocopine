//! Scope-bound `setTimeout` / `setInterval` helpers — hides the
//! `set_timeout_with_callback…` + `clear_timeout` + `Closure::once`
//! dance behind a small RAII surface and ties timer lifetimes to
//! scope unmount.
//!
//! The pre-abstraction shape every primitive used to write —
//!
//! ```ignore
//! let id = host_window()
//!     .and_then(|w| {
//!         let cb = Closure::once_into_js(move || { /* … */ });
//!         w.set_timeout_with_callback_and_timeout_and_arguments_0(
//!             cb.as_ref().unchecked_ref(), delay as i32,
//!         ).ok()
//!     });
//! // … cancel via clear_timeout_with_handle on unmount, manually …
//! ```
//!
//! becomes:
//!
//! ```ignore
//! timers::after_scoped(delay, move || { /* … */ });
//! ```
//!
//! Three layers of API:
//!
//! 1. **`after`** / **`every`** — single-fire / repeating timer with
//!    a [`TimeoutHandle`] / [`IntervalHandle`] returned. Drop the
//!    handle (or call `.cancel()`) to abort.
//! 2. **`*_scoped`** variants — same thing, but the handle's
//!    cancellation is registered against the current scope's
//!    unmount list so callers don't have to store anything.
//! 3. **[`Debounced`]** — reusable cancel-and-replace slot. The
//!    workhorse for hover / scroll-fade / autosave patterns where
//!    every call cancels the prior pending fire.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::events::on_scope_unmount;

/// Host-safe `window()`. On the host (SSR) `web_sys::window()` aborts
/// (the wasm-bindgen import stub is `extern "C"`/nounwind), so return
/// `None` — every timer becomes a no-op server-side and the real timers
/// run on the client after hydration. RFC-099.
#[cfg(target_arch = "wasm32")]
fn host_window() -> Option<web_sys::Window> {
    web_sys::window()
}
#[cfg(not(target_arch = "wasm32"))]
fn host_window() -> Option<web_sys::Window> {
    None
}

/// RAII handle for a single `setTimeout`. Drop the value or call
/// [`TimeoutHandle::cancel`] to abort; otherwise the timer fires
/// after its delay.
pub struct TimeoutHandle {
    id: Rc<Cell<Option<i32>>>,
    // Keeps the JS-side `Closure` alive across the timer wait. The
    // closure clears `id` on fire so [`is_pending`] reports
    // truthfully, but the closure itself lives at least until the
    // handle drops. Absent on the host (SSR): there is no timer, and a
    // `wasm_bindgen::Closure` can't be constructed off-wasm. RFC-099.
    #[cfg(target_arch = "wasm32")]
    _closure: Closure<dyn FnMut()>,
}

impl TimeoutHandle {
    /// Cancel the pending timer. No-op if it already fired or was
    /// previously cancelled.
    pub fn cancel(&self) {
        if let Some(id) = self.id.take()
            && let Some(w) = host_window()
        {
            w.clear_timeout_with_handle(id);
        }
    }

    /// `true` while the timer is still scheduled to fire.
    pub fn is_pending(&self) -> bool {
        self.id.get().is_some()
    }
}

impl Drop for TimeoutHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Schedule `f` to run after `delay_ms`. The returned
/// [`TimeoutHandle`] cancels the timer when dropped — keep it
/// alive (or use [`after_scoped`] for implicit storage) for the
/// timer to actually fire.
pub fn after<F>(delay_ms: u32, f: F) -> TimeoutHandle
where
    F: FnOnce() + 'static,
{
    let id_cell: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
    // RFC-099 — on the host (SSR) there is no timer; skip building the
    // `Closure` (which aborts off-wasm) and return an inert handle. The
    // real timer runs on the client after hydration re-runs setup/mount.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (delay_ms, f);
        TimeoutHandle { id: id_cell }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let id_for_cb = id_cell.clone();
        let mut user_fn = Some(f);
        // setTimeout's callback fires once but `Closure::wrap` requires
        // FnMut — pull the user fn out of the Option on first fire.
        let closure = Closure::wrap(Box::new(move || {
            id_for_cb.set(None);
            if let Some(f) = user_fn.take() {
                f();
            }
        }) as Box<dyn FnMut()>);
        if let Some(w) = host_window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                delay_ms as i32,
            )
        {
            id_cell.set(Some(id));
        }
        TimeoutHandle {
            id: id_cell,
            _closure: closure,
        }
    }
}

/// Like [`after`], but binds the timer's cancellation to the
/// current scope's unmount — no handle to manage. Panics outside a
/// handler / lifecycle context (same reason as
/// [`crate::events::on_scope_unmount`]).
pub fn after_scoped<F>(delay_ms: u32, f: F)
where
    F: FnOnce() + 'static,
{
    let handle = after(delay_ms, f);
    on_scope_unmount(move || drop(handle));
}

/// RAII handle for a `setInterval`. Drop or [`cancel`] to stop the
/// repeating fire.
///
/// [`cancel`]: IntervalHandle::cancel
pub struct IntervalHandle {
    id: Cell<Option<i32>>,
    // Absent on the host (SSR): no interval, and a `Closure` can't be
    // constructed off-wasm. RFC-099.
    #[cfg(target_arch = "wasm32")]
    _closure: Closure<dyn FnMut()>,
}

impl IntervalHandle {
    pub fn cancel(&self) {
        if let Some(id) = self.id.take()
            && let Some(w) = host_window()
        {
            w.clear_interval_with_handle(id);
        }
    }

    pub fn is_active(&self) -> bool {
        self.id.get().is_some()
    }
}

impl Drop for IntervalHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Schedule `f` to run every `period_ms` milliseconds until the
/// returned handle is dropped. Use [`every_scoped`] for
/// scope-bound storage.
pub fn every<F>(period_ms: u32, f: F) -> IntervalHandle
where
    F: FnMut() + 'static,
{
    // RFC-099 — host (SSR): no interval; skip the `Closure` (aborts
    // off-wasm) and return an inert handle. Runs on the client post-hydrate.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (period_ms, f);
        IntervalHandle {
            id: Cell::new(None),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let closure = Closure::wrap(Box::new(f) as Box<dyn FnMut()>);
        let id = host_window().and_then(|w| {
            w.set_interval_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                period_ms as i32,
            )
            .ok()
        });
        IntervalHandle {
            id: Cell::new(id),
            _closure: closure,
        }
    }
}

/// Scope-bound counterpart to [`every`]. Cancels at unmount.
pub fn every_scoped<F>(period_ms: u32, f: F)
where
    F: FnMut() + 'static,
{
    let handle = every(period_ms, f);
    on_scope_unmount(move || drop(handle));
}

/// Reusable cancel-and-replace timer slot — the workhorse for
/// hover / scroll-fade / autosave debouncing. [`Debounced::schedule`]
/// cancels any earlier pending fire before scheduling the new one,
/// so callers don't have to track the timer id themselves.
///
/// Always wrapped in `Rc` so the same slot can be cloned into
/// multiple event handlers.
///
/// ```ignore
/// let pending = timers::Debounced::new_scoped();  // auto-cancels at unmount
///
/// on!(trigger, mouseenter, |_e| {
///     let root = root.clone();
///     pending.schedule(700, move || root.update(|s| s.open = true));
/// });
/// on!(trigger, mouseleave, |_e| {
///     pending.cancel();
///     root.update(|s| s.open = false);
/// });
/// ```
pub struct Debounced {
    id: Cell<Option<i32>>,
    // Holds the current Closure across its setTimeout wait so the
    // JS callback isn't dropped before it fires. Replaced on each
    // `schedule`.
    closure: Cell<Option<Closure<dyn FnMut()>>>,
}

impl Debounced {
    /// Mint a fresh slot. Drop the `Rc` to cancel any pending fire.
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            id: Cell::new(None),
            closure: Cell::new(None),
        })
    }

    /// Like [`Debounced::new`], but auto-cancels any pending fire
    /// when the current scope unmounts. Panics outside a handler /
    /// lifecycle context.
    pub fn new_scoped() -> Rc<Self> {
        let slot = Self::new();
        let slot_for_cleanup = slot.clone();
        on_scope_unmount(move || slot_for_cleanup.cancel());
        slot
    }

    /// Schedule `f` after `delay_ms`, cancelling any previous
    /// schedule on the same slot.
    pub fn schedule<F>(self: &Rc<Self>, delay_ms: u32, f: F)
    where
        F: FnOnce() + 'static,
    {
        self.cancel();
        let Some(w) = host_window() else { return };
        let me = self.clone();
        let mut user_fn = Some(f);
        let closure = Closure::wrap(Box::new(move || {
            me.id.set(None);
            // Drop the old closure now that it's done firing — keeps
            // the slot light if `schedule` isn't called again.
            me.closure.take();
            if let Some(f) = user_fn.take() {
                f();
            }
        }) as Box<dyn FnMut()>);
        if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            delay_ms as i32,
        ) {
            self.id.set(Some(id));
            self.closure.set(Some(closure));
        }
    }

    /// Cancel any pending fire. No-op if nothing is scheduled.
    pub fn cancel(&self) {
        if let Some(id) = self.id.take()
            && let Some(w) = host_window()
        {
            w.clear_timeout_with_handle(id);
        }
        self.closure.take();
    }

    pub fn is_pending(&self) -> bool {
        self.id.get().is_some()
    }
}

// ── awaitable helpers ────────────────────────────────────────────
//
// Use these inside `pocopine::spawn_scoped(async move { … })` so the
// in-flight `await` is dropped (and the underlying setTimeout /
// requestAnimationFrame fire becomes a no-op) when the scope
// unmounts. For event-driven cancellation (e.g. mouseleave aborts
// a pending mouseenter delay), reach for [`Debounced`] instead —
// async closures don't compose with mid-flight cancellation.

/// Resolve after `delay_ms`. The future stays pending forever if
/// the host has no `window` (e.g. SSR shim) — fine because nothing
/// is driving it in that environment.
pub async fn sleep(delay_ms: u32) {
    let p = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(w) = host_window() {
            let _ =
                w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, delay_ms as i32);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// Resolve on the next `requestAnimationFrame` with the high-res
/// timestamp the browser passes to the RAF callback. Useful for
/// "measure after layout" patterns inside async flows.
pub async fn next_frame() -> f64 {
    let p = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(w) = host_window() {
            let _ = w.request_animation_frame(&resolve);
        }
    });
    JsFuture::from(p)
        .await
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// Resolve on the next microtask. Awaitable counterpart to
/// [`crate::tick::next`] — same execution slot, different shape.
pub async fn next_tick() {
    let _ = JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
}
