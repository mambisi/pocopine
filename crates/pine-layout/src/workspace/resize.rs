//! Self-contained pointer + keyboard resize for workspace regions.
//!
//! pine-layout ships its **own** resize engine rather than depending on the
//! `pine` crate's `splitter`. This replicates splitter's document-listener
//! drag pattern (`events::on` pointermove/up/cancel held in a
//! `ListenerHandle` vec, cleared on release) for a single edge handle, plus
//! a pure [`resolve_size`] for clamp + snap-to-collapse that is unit-tested
//! on the host.

/// The axis a resize handle drags along.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    /// Horizontal drag resizes width (sidebar / aside / outer panels).
    Horizontal,
    /// Vertical drag resizes height (bottom panel).
    Vertical,
}

/// Pure resize math (host-testable): apply a pointer `delta` to a starting
/// size, flip it for end-anchored handles (`invert`), clamp to `[min, max]`,
/// and snap to `min` when the result falls below `collapse_at`.
pub(crate) fn resolve_size(
    start: f64,
    delta: f64,
    invert: bool,
    min: f64,
    max: f64,
    collapse_at: Option<f64>,
) -> f64 {
    let signed = if invert { -delta } else { delta };
    let size = (start + signed).clamp(min, max);
    match collapse_at {
        Some(threshold) if size < threshold => min,
        _ => size,
    }
}

/// Begin a pointer drag from a region's resize handle.
///
/// `start_pointer` is the pointer's client coordinate (x for `Horizontal`,
/// y for `Vertical`) at pointerdown; `invert` flips the delta for
/// end-anchored regions (a right-side aside grows as the pointer moves
/// left). `on_size` fires live with each clamped size during the drag;
/// `on_commit` fires once on release with the final, snap-resolved size.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_drag(
    axis: Axis,
    invert: bool,
    start_pointer: f64,
    start_size: f64,
    min: f64,
    max: f64,
    collapse_at: Option<f64>,
    on_size: std::rc::Rc<dyn Fn(f64)>,
    on_commit: std::rc::Rc<dyn Fn(f64)>,
) {
    use pocopine::events::{self, ListenerHandle, ev};
    use std::cell::RefCell;
    use std::rc::Rc;
    use web_sys::PointerEvent;

    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let handles: Rc<RefCell<Vec<ListenerHandle>>> = Rc::new(RefCell::new(Vec::with_capacity(3)));

    let pointer_of = move |ev: &PointerEvent| {
        if axis == Axis::Vertical {
            ev.client_y() as f64
        } else {
            ev.client_x() as f64
        }
    };

    // During the drag the size only clamps (no snap) so the handle tracks
    // the pointer; the collapse snap applies on release.
    let move_handle = events::on(&doc, ev::pointermove, move |ev: PointerEvent| {
        ev.prevent_default();
        let size = resolve_size(
            start_size,
            pointer_of(&ev) - start_pointer,
            invert,
            min,
            max,
            None,
        );
        on_size(size);
    });

    let release = {
        let handles = handles.clone();
        move |ev: PointerEvent| {
            let delta = pointer_of(&ev) - start_pointer;
            on_commit(resolve_size(
                start_size,
                delta,
                invert,
                min,
                max,
                collapse_at,
            ));
            handles.borrow_mut().clear();
        }
    };
    let up = events::on(&doc, ev::pointerup, release.clone());
    let cancel = events::on(&doc, ev::pointercancel, release);
    handles.borrow_mut().extend([move_handle, up, cancel]);
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_drag(
    _axis: Axis,
    _invert: bool,
    _start_pointer: f64,
    _start_size: f64,
    _min: f64,
    _max: f64,
    _collapse_at: Option<f64>,
    _on_size: std::rc::Rc<dyn Fn(f64)>,
    _on_commit: std::rc::Rc<dyn Fn(f64)>,
) {
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_to_min_max() {
        assert_eq!(
            resolve_size(240.0, 1000.0, false, 180.0, 480.0, None),
            480.0
        );
        assert_eq!(
            resolve_size(240.0, -1000.0, false, 180.0, 480.0, None),
            180.0
        );
        assert_eq!(resolve_size(240.0, 40.0, false, 180.0, 480.0, None), 280.0);
    }

    #[test]
    fn invert_flips_delta_for_end_anchored() {
        // Right-side aside: pointer moving left (negative delta) grows it.
        assert_eq!(resolve_size(280.0, -50.0, true, 200.0, 500.0, None), 330.0);
        assert_eq!(resolve_size(280.0, 50.0, true, 200.0, 500.0, None), 230.0);
    }

    #[test]
    fn snaps_to_collapsed_below_threshold() {
        // Atlassian-style: drag a 240 sidebar below the 200 threshold -> 20px rail.
        assert_eq!(
            resolve_size(240.0, -60.0, false, 20.0, 480.0, Some(200.0)),
            20.0
        );
        // Still above the threshold -> keep the dragged width.
        assert_eq!(
            resolve_size(240.0, -20.0, false, 20.0, 480.0, Some(200.0)),
            220.0
        );
    }
}
