//! Pan gesture — pointer-move driven drag tracking.
//!
//! Fires a stream of [`PanEvent`]s as the user drags across `el`.
//! Simplified port of Motion's `PanSession`: pointerdown on target
//! captures the start, pointermove on window updates the delta, and
//! pointerup/pointercancel ends the session. A configurable
//! distance threshold delays `Start` until the pointer has moved a
//! minimum amount — which prevents a plain click from registering
//! as a micro-pan.
//!
//! Velocity is computed from the last two pointermove samples,
//! converted to units-per-second so the value can hand off cleanly
//! to a [`crate::Spring::with_velocity`] on release (momentum).

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::{Element, PointerEvent};

use super::{is_primary_pointer, GestureHandle};

/// One event in a pan session.
#[derive(Clone, Copy, Debug)]
pub enum PanEvent {
    /// Pointer crossed the distance threshold — pan is now active.
    /// `point` is in client coordinates (matches
    /// `PointerEvent.client{X,Y}`).
    Start { point: (f64, f64) },
    /// Pointer moved. `delta` is total movement since start (cumulative,
    /// not per-frame). `velocity` is the *instantaneous* velocity in
    /// units-per-second from the last two samples.
    Move {
        delta: (f64, f64),
        velocity: (f64, f64),
    },
    /// Pointer released. `velocity` is the fling velocity at release
    /// — hand this to `Spring::with_velocity` for momentum.
    End { velocity: (f64, f64) },
    /// Pan was cancelled (pointercancel, window lost pointer, etc).
    Cancel,
}

/// Pan configuration. `distance_threshold_px` is the minimum pointer
/// movement before `PanEvent::Start` fires (3px matches Motion's
/// default and suppresses spurious pans from a plain click).
#[derive(Clone, Copy, Debug)]
pub struct PanConfig {
    pub distance_threshold_px: f64,
}

impl Default for PanConfig {
    fn default() -> Self {
        Self {
            distance_threshold_px: 3.0,
        }
    }
}

/// Attach a pan observer. The callback receives [`PanEvent`]s as
/// the user drags; hold the returned [`GestureHandle`] for as long
/// as you want the observer active.
pub fn pan<F>(el: &Element, cfg: PanConfig, cb: F) -> GestureHandle
where
    F: FnMut(PanEvent) + 'static,
{
    let mut handle = GestureHandle::new();

    let state: Rc<RefCell<Option<PanState>>> = Rc::new(RefCell::new(None));
    let cb = Rc::new(RefCell::new(cb));

    // pointerdown starts tracking; pan doesn't fire Start until the
    // threshold is crossed.
    {
        let state = state.clone();
        handle.listen::<PointerEvent, _>(el.as_ref(), "pointerdown", move |ev| {
            if !is_primary_pointer(&ev) {
                return;
            }
            let point = (ev.client_x() as f64, ev.client_y() as f64);
            *state.borrow_mut() = Some(PanState {
                start: point,
                last: point,
                prev_sample: (point, now_ms()),
                last_sample: (point, now_ms()),
                started: false,
            });
        });
    }

    // pointermove on window — catches drags that move outside the
    // target element, which is what any reasonable pan wants.
    {
        let state = state.clone();
        let cb = cb.clone();
        let threshold = cfg.distance_threshold_px;
        handle.listen_on_window::<PointerEvent, _>("pointermove", move |ev| {
            let Some(mut st) = state.borrow_mut().take() else { return };
            let now = (ev.client_x() as f64, ev.client_y() as f64);
            let t = now_ms();

            // Cross-threshold check.
            if !st.started {
                let dx = now.0 - st.start.0;
                let dy = now.1 - st.start.1;
                if (dx * dx + dy * dy).sqrt() >= threshold {
                    st.started = true;
                    if let Ok(mut f) = cb.try_borrow_mut() {
                        f(PanEvent::Start { point: st.start });
                    }
                }
            }

            st.prev_sample = st.last_sample;
            st.last_sample = (now, t);
            st.last = now;

            if st.started {
                let velocity = instantaneous_velocity(&st);
                let delta = (now.0 - st.start.0, now.1 - st.start.1);
                if let Ok(mut f) = cb.try_borrow_mut() {
                    f(PanEvent::Move { delta, velocity });
                }
            }
            *state.borrow_mut() = Some(st);
        });
    }

    // pointerup / pointercancel resolve the session.
    {
        let state = state.clone();
        let cb = cb.clone();
        handle.listen_on_window::<PointerEvent, _>("pointerup", move |_ev| {
            let Some(st) = state.borrow_mut().take() else { return };
            if st.started {
                let velocity = instantaneous_velocity(&st);
                if let Ok(mut f) = cb.try_borrow_mut() {
                    f(PanEvent::End { velocity });
                }
            }
        });
    }
    {
        let state = state.clone();
        let cb = cb.clone();
        handle.listen_on_window::<PointerEvent, _>("pointercancel", move |_ev| {
            let Some(st) = state.borrow_mut().take() else { return };
            if st.started {
                if let Ok(mut f) = cb.try_borrow_mut() {
                    f(PanEvent::Cancel);
                }
            }
        });
    }

    handle
}

struct PanState {
    /// Initial client coordinates at pointerdown.
    start: (f64, f64),
    /// Most recent client coordinates.
    last: (f64, f64),
    /// Previous sample — `(point, timestamp_ms)`. Paired with
    /// `last_sample` to compute instantaneous velocity.
    prev_sample: ((f64, f64), f64),
    last_sample: ((f64, f64), f64),
    /// True once the distance threshold was crossed and `Start`
    /// fired.
    started: bool,
}

fn instantaneous_velocity(st: &PanState) -> (f64, f64) {
    let (p0, t0) = st.prev_sample;
    let (p1, t1) = st.last_sample;
    let dt = (t1 - t0).max(1.0); // avoid div-by-zero
    let _ = st.last;
    (((p1.0 - p0.0) / dt) * 1000.0, ((p1.1 - p0.1) / dt) * 1000.0)
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}
