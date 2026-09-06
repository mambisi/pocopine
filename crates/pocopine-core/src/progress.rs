//! Top progress bar — the boot loader's bar, reusable from app code.
//!
//! A thin progress bar pinned to the top of the page (YouTube / nprogress
//! style). It is the *same* bar the `index.html` boot loader shows while the
//! wasm bundle downloads; after boot it stays in the DOM (hidden) so app
//! code can reuse it for route changes, fetches, or any async work.
//!
//! Both layers share one JS controller installed on
//! `window.__pocopineProgress` (idempotent): the boot script installs it
//! pre-wasm, and this module reuses it — or injects an equivalent (bar +
//! styles + controller) when an app ships no boot loader.
//!
//! Route navigations drive the bar automatically (see
//! [`observe_route_event`]); for everything else use the API below.
//!
//! ```ignore
//! // RAII: the bar shows now and finishes when `_task` is dropped — so it
//! // can't leak on an early return or `?`.
//! let _task = pocopine::progress::task();
//! let rows = load_rows().await;
//!
//! // …or drive a determinate fraction (e.g. an upload):
//! pocopine::progress::set(bytes_sent as f32 / total as f32);
//! ```
//!
//! NOTE: the controller JS + CSS below mirror the canonical boot loader in
//! `crates/pocopine-cli/assets/loader.js` (injected into `pkg/index.html` by
//! `pocopine build`). They implement the same `window.__pocopineProgress`
//! contract — keep the two in sync.

#[cfg(target_arch = "wasm32")]
mod imp {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(inline_js = r#"
function install() {
  // No-op outside a DOM (e.g. the node-based wasm test battery).
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return { start: function(){}, set: function(){}, inc: function(){}, done: function(){}, error: function(){}, ready: function(){} };
  }
  var w = window;
  if (w.__pocopineProgress) return w.__pocopineProgress;
  // Inject styles unless the boot loader already inlined them.
  if (!document.getElementById('pp-loader-style')) {
    var st = document.createElement('style');
    st.id = 'pp-loader-style';
    st.textContent =
      '#pp-loader{position:fixed;top:0;left:0;right:0;height:3px;z-index:9999;overflow:hidden;pointer-events:none;transition:opacity .3s ease .15s}'
      + '#pp-loader.pp-loader--done{opacity:0}'
      + '.pp-loader__bar{height:100%;width:100%;background:var(--brand,#f8ae45);box-shadow:0 0 8px var(--brand,#f8ae45);transform:translateX(-100%);transition:transform .2s ease;will-change:transform}'
      + '#pp-loader.pp-loader--error .pp-loader__bar{background:var(--destructive,#c04a2b);box-shadow:0 0 8px var(--destructive,#c04a2b)}'
      + '@media (prefers-reduced-motion:reduce){#pp-loader{transition:opacity .3s ease}.pp-loader__bar{transition:none}}';
    (document.head || document.documentElement).appendChild(st);
  }
  var el = null, bar = null, pending = 0, value = 0, determinate = false, trickle = 0, resume = 0, showTimer = 0, shown = false;
  // Deferred-reveal window: a navigation that finishes within SHOW_DELAY ms
  // never flashes the bar; only genuinely slow work reveals it.
  var SHOW_DELAY = 150;
  function ensure() {
    el = document.getElementById('pp-loader');
    if (!el) {
      el = document.createElement('div');
      el.id = 'pp-loader';
      el.setAttribute('role', 'progressbar');
      el.setAttribute('aria-label', 'Loading');
      el.setAttribute('aria-valuemin', '0');
      el.setAttribute('aria-valuemax', '100');
      bar = document.createElement('div');
      bar.className = 'pp-loader__bar';
      el.appendChild(bar);
      (document.body || document.documentElement).appendChild(el);
    }
    if (!bar) bar = el.querySelector('.pp-loader__bar');
  }
  function render() {
    ensure();
    var v = Math.max(0, Math.min(value, 1));
    if (bar) bar.style.transform = 'translateX(' + (v - 1) * 100 + '%)';
    el.setAttribute('aria-valuenow', String(Math.round(v * 100)));
  }
  function show() { ensure(); el.classList.remove('pp-loader--done', 'pp-loader--error'); }
  function reveal() {
    showTimer = 0; shown = true; show();
    if (value < 0.08) value = 0.08;
    render(); startTrickle();
  }
  function startTrickle() {
    clearInterval(trickle);
    trickle = setInterval(function () {
      if (value < 0.95) { value += (0.95 - value) * 0.1; render(); }
    }, 200);
  }
  function hideSplash() {
    var s = document.getElementById('pp-splash');
    if (!s) return;
    s.classList.add('pp-splash--done');
    s.addEventListener('transitionend', function () { s.remove(); }, { once: true });
    setTimeout(function () { s.remove(); }, 700);
  }
  function complete() {
    if (w.__pocopineLocale && !w.__pocopineLocale.ready) return;
    clearTimeout(showTimer); showTimer = 0;
    clearInterval(trickle); clearTimeout(resume);
    pending = 0; value = 1; render(); ensure(); el.classList.add('pp-loader--done');
    hideSplash();
    setTimeout(function () {
      value = 0; determinate = false; shown = false;
      if (bar) {
        bar.style.transition = 'none'; render();
        requestAnimationFrame(function () { if (bar) bar.style.transition = ''; });
      }
      if (el) el.classList.remove('pp-loader--done');
    }, 450);
  }
  var api = {
    // Deferred reveal — fast navigations never flash the bar; slow work does.
    start: function () {
      pending++;
      if (pending === 1 && !determinate && !shown && !showTimer) {
        showTimer = setTimeout(reveal, SHOW_DELAY);
      }
    },
    set: function (f) {
      // A concrete fraction is an explicit "show progress" — reveal at once.
      clearTimeout(showTimer); showTimer = 0; shown = true;
      determinate = true; clearInterval(trickle); clearTimeout(resume);
      value = Math.max(0, Math.min(f, 1)); show(); render();
      if (value >= 1) { complete(); return; }
      // Updates stopped (e.g. a download finished) → trickle the tail so
      // the bar keeps moving through the compile/processing phase.
      resume = setTimeout(function () { determinate = false; startTrickle(); }, 400);
    },
    inc: function (d) { api.set(value + d); },
    done: function () {
      pending = Math.max(0, pending - 1);
      if (pending > 0) return;
      // Finished before the reveal timer fired → cancel it; never flash.
      if (showTimer) { clearTimeout(showTimer); showTimer = 0; return; }
      if (shown) complete();
    },
    error: function () {
      clearTimeout(showTimer); showTimer = 0; shown = true;
      clearInterval(trickle); clearTimeout(resume); ensure();
      el.classList.add('pp-loader--error'); value = 1; render(); hideSplash();
    },
    // App reported ready (AppBootCompleted): drop the splash + finish.
    ready: function () {
      if (w.__pocopineLocale) w.__pocopineLocale.appReady = true;
      complete();
    },
  };
  w.__pocopineProgress = api;
  return api;
}
export function pp_progress_start() { install().start(); }
export function pp_progress_set(f) { install().set(f); }
export function pp_progress_inc(d) { install().inc(d); }
export function pp_progress_done() { install().done(); }
export function pp_progress_error() { install().error(); }
export function pp_progress_ready() { install().ready(); }
// Has a controller been installed (boot loader, or a prior API call)?
// Used to gate the router auto-hook so apps that never opt into a loader
// don't get a surprise nav bar — and so route events aren't even built.
export function pp_progress_installed() {
  return !!(typeof window !== 'undefined' && window.__pocopineProgress);
}
"#)]
    extern "C" {
        pub fn pp_progress_start();
        pub fn pp_progress_set(f: f64);
        pub fn pp_progress_inc(d: f64);
        pub fn pp_progress_done();
        pub fn pp_progress_error();
        pub fn pp_progress_ready();
        pub fn pp_progress_installed() -> bool;
    }
}

/// Begin one loading task. Ref-counted: the bar stays up until a matching
/// [`finish`] for every [`start`], so concurrent tasks (and the automatic
/// route hook) coexist.
///
/// The reveal is *deferred* ~150 ms: a task that finishes within that window
/// never flashes the bar, so quick work (e.g. an already-loaded route) feels
/// instant. Slower work reveals the bar and — once shown — always plays its
/// finish animation to completion. Call [`set`] instead to show a
/// determinate fraction immediately.
pub fn start() {
    #[cfg(target_arch = "wasm32")]
    imp::pp_progress_start();
}

/// Set a determinate fraction in `0.0..=1.0` (e.g. real download progress).
/// Switches the bar out of trickle mode; reaching `1.0` finishes it.
pub fn set(fraction: f32) {
    #[cfg(target_arch = "wasm32")]
    imp::pp_progress_set(fraction as f64);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = fraction;
}

/// Advance the determinate fraction by `amount`.
pub fn inc(amount: f32) {
    #[cfg(target_arch = "wasm32")]
    imp::pp_progress_inc(amount as f64);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = amount;
}

/// Finish one task started with [`start`]. The bar completes and fades out
/// once every [`start`] has a matching `finish`.
pub fn finish() {
    #[cfg(target_arch = "wasm32")]
    imp::pp_progress_done();
}

/// Put the bar in its error state (turns red and stays visible) — signal a
/// failed load so the user knows to retry.
pub fn fail() {
    #[cfg(target_arch = "wasm32")]
    imp::pp_progress_error();
}

/// RAII guard returned by [`task`]: [`start`] on creation, [`finish`] on
/// drop, so the bar can never leak on an early return, `?`, or panic.
#[must_use = "the progress bar finishes when this guard is dropped; bind it to a variable"]
pub struct Task {
    _private: (),
}

/// Begin a progress task; the bar runs until the returned [`Task`] is
/// dropped. Ideal for wrapping an `async` block.
pub fn task() -> Task {
    start();
    Task { _private: () }
}

impl Task {
    /// Set a determinate fraction for this task (see [`set`]).
    pub fn set(&self, fraction: f32) {
        set(fraction);
    }

    /// Advance the determinate fraction (see [`inc`]).
    pub fn inc(&self, amount: f32) {
        inc(amount);
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        finish();
    }
}

/// Whether a progress controller is installed — i.e. the app opted in via
/// the boot loader or a prior call to the API. The router uses this to
/// decide whether to emit route-navigation events (and thus auto-drive the
/// bar): apps that never use a loader pay nothing and see no nav bar.
pub(crate) fn is_installed() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        imp::pp_progress_installed()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Built-in integration: drive the bar + splash off the app lifecycle.
/// Invoked from [`crate::plugin::emit`] for every event; a no-op for
/// anything that isn't an app-boot or route-navigation event.
///
/// - `AppBootCompleted` → `ready()` (drop the boot splash, finish the bar)
/// - `AppBootFailed`    → `error()` (red bar)
/// - `RouteNavigationStarted` / `Completed` / `Failed` → `start` / `finish`
///
/// App-boot events are emitted for every app unconditionally, so they're
/// gated here on a controller actually being installed (boot loader or a
/// prior API call) — a no-loader app self-injects nothing on boot. Route
/// events are already gated upstream by `has_route_navigation_hooks`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn observe_event(ev: &dyn std::any::Any) {
    use crate::plugin::{
        AppBootCompleted, AppBootFailed, RouteNavigationCompleted, RouteNavigationFailed,
        RouteNavigationStarted,
    };
    if ev.is::<AppBootCompleted>() {
        if is_installed() {
            imp::pp_progress_ready();
        }
    } else if ev.is::<AppBootFailed>() {
        if is_installed() {
            fail();
        }
    } else if ev.is::<RouteNavigationStarted>() {
        start();
    } else if ev.is::<RouteNavigationCompleted>() || ev.is::<RouteNavigationFailed>() {
        finish();
    }
}
