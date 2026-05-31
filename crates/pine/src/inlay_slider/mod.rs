//! `<pine-inlay-slider-*>` — the "Inlay" slider primitive (compound).
//!
//! A combined progress-bar + slider where the value field and any
//! controls are *inlaid* into a draggable handle that **doubles as
//! the progress fill**. Unlike [`crate::slider`] there is no separate
//! thumb: the handle is pinned to the track start and **grows by
//! width** as the value rises, floored at its natural content width
//! and capped at the track's inner width — so it never crushes its
//! contents at `min` nor overflows at `max`.
//!
//! Four parts:
//!
//! - **Root** (`pine-inlay-slider-root`) — owns `value`, `min`,
//!   `max`, `step`, `disabled`. Two-way bindable via
//!   `pp-model:value="my_cap"`. Installs the pointer handlers + the
//!   geometry, and provides its Handle to descendants. `pp-ref="root"`.
//! - **Track** (`pine-inlay-slider-track`) — the rail + positioning
//!   context (author styles it `position: relative; padding: <inset>`).
//!   `pp-ref="track"`; holds the Handle.
//! - **Handle** (`pine-inlay-slider-handle`) — the growing pill.
//!   `pp-ref="handle"`. Its default slot hosts the inlaid content
//!   (a number field, a unit selector, the Grip, dividers — all
//!   author-supplied). Root sets its `left` + `width` imperatively.
//! - **Grip** (`pine-inlay-slider-grip`) — the focusable drag
//!   affordance (`role="slider"` + ARIA + keyboard), placed by the
//!   author at the pill's edge.
//!
//! ```html
//! <pine-inlay-slider-root pp-model:value="cap" min="1" max="1024" step="1">
//!   <pine-inlay-slider-track>
//!     <pine-inlay-slider-handle>
//!       <input pp-model.number="cap" />
//!       <select pp-model="unit">…</select>
//!       <pine-inlay-slider-grip></pine-inlay-slider-grip>
//!     </pine-inlay-slider-handle>
//!   </pine-inlay-slider-track>
//! </pine-inlay-slider-root>
//! ```
//!
//! The primitive is value-agnostic: it owns a plain `f64` and the
//! geometry. Unit conversion (MB/GB), formatting, labels and error
//! state live in the component that composes it.
//!
//! v1 is single-value. A two-handle range variation waits for a
//! follow-up, exactly as [`crate::slider`] deferred multi-thumb.

use pocopine::create_context;
use pocopine::events::{self, ev};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, PointerEvent};

create_context!(ROOT: Handle<PineInlaySliderRoot>);

const TRACK_SEL: &str = ".pine-inlay-slider-track";
const HANDLE_SEL: &str = ".pine-inlay-slider-handle";
const GRIP_SEL: &str = ".pine-inlay-slider-grip";

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineInlaySliderRoot.poco",
    role = "scope",
    display = "contents"
)]
// Root holds the Track; the Track holds the Handle; the Handle holds
// the author's inlaid content (incl. the Grip). The Handle's width
// is the progress fill, so there is no separate Range part.
#[slot(default, only = [PineInlaySliderTrack])]
pub struct PineInlaySliderRoot {
    /// Current value. Two-way bindable via `pp-model:value="…"`.
    #[model]
    pub value: f64,
    #[prop]
    pub min: f64,
    #[prop]
    pub max: f64,
    /// Quantization for both keyboard + pointer moves.
    #[prop]
    pub step: f64,
    #[prop]
    pub disabled: bool,
    /// Computed mirror: `(value - min) / (max - min) * 100`, clamped
    /// to 0..100. Drives the Handle's width through the imperative
    /// reposition; descendants observe it for ARIA if they wish.
    pub percent: f64,
}

impl Default for PineInlaySliderRoot {
    fn default() -> Self {
        Self {
            value: 0.0,
            min: 0.0,
            max: 100.0,
            step: 1.0,
            disabled: false,
            percent: 0.0,
        }
    }
}

#[handlers]
impl PineInlaySliderRoot {
    fn on_setup(&mut self) {
        self.recompute_percent();
        ROOT.provide(this::<Self>());
    }

    #[watch(value)]
    fn on_value(&mut self, _: f64, _: Option<f64>) {
        self.recompute_percent();
    }

    #[watch(min)]
    fn on_min(&mut self, _: f64, _: Option<f64>) {
        self.recompute_percent();
    }

    #[watch(max)]
    fn on_max(&mut self, _: f64, _: Option<f64>) {
        self.recompute_percent();
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let Some(root_el) = refs.get("root") else {
            return;
        };
        install_pointer(root_el.clone(), handle.clone());

        // Reposition the growing pill on *any* value change — drag,
        // keyboard, or an author-bound input writing `value` — via the
        // reactive bridge (auto-releases on unmount). `:style` binding
        // can't be used here: reposition measures the handle's natural
        // width by momentarily setting `width:auto`, which a per-frame
        // binding would clobber.
        let el_for_watch = root_el.clone();
        let root_for_watch = handle.clone();
        handle.watch_field::<f64, _>("percent", move |_, _| {
            reposition(&el_for_watch, &root_for_watch);
        });

        // Re-measure when the container reflows (modal resize, density
        // change). Pine measures on demand rather than holding a
        // ResizeObserver — same convention as slider / splitter.
        if let Some(win) = web_sys::window() {
            let el_for_resize = root_el.clone();
            let root_for_resize = handle.clone();
            events::on_scoped(&win, ev::resize, move |_| {
                reposition(&el_for_resize, &root_for_resize);
            });
        }

        // Initial placement once layout settles, then once more after
        // a short delay to catch font-swap reflow (a webfont can change
        // the digit widths, shifting the handle's natural content size).
        let el_for_raf = root_el.clone();
        let root_for_raf = handle.clone();
        pocopine::tick::next_frame(move || reposition(&el_for_raf, &root_for_raf));
        let el_for_late = root_el;
        let root_for_late = handle;
        pocopine::timers::after(80, move || reposition(&el_for_late, &root_for_late));
    }
}

impl PineInlaySliderRoot {
    fn recompute_percent(&mut self) {
        let span = (self.max - self.min).max(f64::EPSILON);
        let pct = ((self.value - self.min) / span) * 100.0;
        self.percent = pct.clamp(0.0, 100.0);
    }

    /// Clamp + snap to the nearest `step` and commit.
    pub fn set_value(&mut self, v: f64) {
        if self.disabled {
            return;
        }
        let clamped = v.clamp(self.min, self.max);
        let step = self.step.max(f64::EPSILON);
        let snapped = ((clamped - self.min) / step).round() * step + self.min;
        let final_v = snapped.clamp(self.min, self.max);
        if (self.value - final_v).abs() > f64::EPSILON {
            self.value = final_v;
        }
    }

    pub fn step_by(&mut self, multiplier: f64) {
        self.set_value(self.value + self.step * multiplier);
    }
}

// ── geometry ──────────────────────────────────────────────────────

/// Parse a CSS length like `"4px"` to its numeric value.
fn px(v: &str) -> f64 {
    v.trim()
        .trim_end_matches("px")
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
}

/// Resolve the Track + Handle and measure the inlay geometry:
/// `(track, handle, inset, track_inner, handle_min)`. `handle_min` is
/// the handle's natural content width, clamped to the inner track.
fn measure(root_el: &Element) -> Option<(Element, HtmlElement, f64, f64, f64)> {
    let track = root_el.query_selector(TRACK_SEL).ok().flatten()?;
    let handle: HtmlElement = root_el
        .query_selector(HANDLE_SEL)
        .ok()
        .flatten()?
        .dyn_into()
        .ok()?;
    let win = web_sys::window()?;
    let inset = win
        .get_computed_style(&track)
        .ok()
        .flatten()
        .and_then(|s| s.get_property_value("padding-left").ok())
        .map(|v| px(&v))
        .unwrap_or(0.0);
    let track_inner = (track.client_width() as f64 - 2.0 * inset).max(0.0);

    // Natural content width: drop the explicit width, read, restore.
    let style = handle.style();
    let prev = style.get_property_value("width").unwrap_or_default();
    let _ = style.set_property("width", "auto");
    let handle_min = (handle.offset_width() as f64).min(track_inner);
    if prev.is_empty() {
        let _ = style.remove_property("width");
    } else {
        let _ = style.set_property("width", &prev);
    }

    Some((track, handle, inset, track_inner, handle_min))
}

/// Pin the Handle to the track start and size it to the current value:
/// `width = handle_min + f·(track_inner − handle_min)`.
fn reposition(root_el: &Element, root: &Handle<PineInlaySliderRoot>) {
    let Some((_track, handle, inset, track_inner, handle_min)) = measure(root_el) else {
        return;
    };
    let percent = root.with(|r| r.percent);
    let f = (percent / 100.0).clamp(0.0, 1.0);
    let w = (handle_min + f * (track_inner - handle_min).max(0.0)).round();
    let style = handle.style();
    let _ = style.set_property("left", &format!("{inset}px"));
    let _ = style.set_property("width", &format!("{w}px"));
}

/// Map a pointer position to a value via the handle's right edge:
/// the desired width is `clientX − trackLeft − inset`, clamped to
/// `[handle_min, track_inner]`, then remapped to `[min, max]`.
fn value_from_pointer(
    root_el: &Element,
    ev: &PointerEvent,
    root: &Handle<PineInlaySliderRoot>,
) -> f64 {
    let Some((track, _handle, inset, track_inner, handle_min)) = measure(root_el) else {
        return root.with(|r| r.value);
    };
    let rect = track.get_bounding_client_rect();
    let span = (track_inner - handle_min).max(1.0);
    let target_w = (ev.client_x() as f64) - rect.left() - inset;
    let w = target_w.clamp(handle_min, track_inner);
    let f = (w - handle_min) / span;
    let (min, max) = root.with(|r| (r.min, r.max));
    min + f * (max - min)
}

fn install_pointer(root_el: Element, root: Handle<PineInlaySliderRoot>) {
    // Shared pointer-id (mirrors slider's drag bookkeeping). Document
    // -level move/up always fire and are gated on this id.
    let dragging = Rc::new(Cell::new(None::<i32>));

    let root_for_down = root_el.clone();
    let handle_for_down = root.clone();
    let dragging_for_down = dragging.clone();
    events::on_scoped(&root_el, ev::pointerdown, move |ev: PointerEvent| {
        if handle_for_down.with(|r| r.disabled) {
            return;
        }
        let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        let on_grip = target.closest(GRIP_SEL).ok().flatten().is_some();
        let on_handle = target.closest(HANDLE_SEL).ok().flatten().is_some();
        let on_track = target.closest(TRACK_SEL).ok().flatten().is_some();
        // Drag from the grip, or from a bare-track click — but never
        // from the inlaid fields (inside the handle, not the grip), so
        // typing / opening a select never starts a drag.
        if !(on_grip || (on_track && !on_handle)) {
            return;
        }
        dragging_for_down.set(Some(ev.pointer_id()));
        ev.prevent_default();
        set_dragging_attr(&root_for_down, true);
        // A track click jumps the value to the pointer; a grip drag
        // keeps the current value until the pointer actually moves.
        if !on_grip {
            let v = value_from_pointer(&root_for_down, &ev, &handle_for_down);
            handle_for_down.update(|r| r.set_value(v));
        }
    });

    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };

    let dragging_for_move = dragging.clone();
    let el_for_move = root_el.clone();
    let root_for_move = root.clone();
    events::on_scoped(&doc, ev::pointermove, move |ev: PointerEvent| {
        if dragging_for_move.get() != Some(ev.pointer_id()) {
            return;
        }
        ev.prevent_default();
        let v = value_from_pointer(&el_for_move, &ev, &root_for_move);
        root_for_move.update(|r| r.set_value(v));
    });

    let dragging_for_up = dragging.clone();
    let el_for_up = root_el.clone();
    events::on_scoped(&doc, ev::pointerup, move |ev: PointerEvent| {
        if dragging_for_up.get() == Some(ev.pointer_id()) {
            dragging_for_up.set(None);
            set_dragging_attr(&el_for_up, false);
        }
    });
    events::on_scoped(&doc, ev::pointercancel, move |ev: PointerEvent| {
        if dragging.get() == Some(ev.pointer_id()) {
            dragging.set(None);
            set_dragging_attr(&root_el, false);
        }
    });
}

fn set_dragging_attr(root_el: &Element, on: bool) {
    if on {
        let _ = root_el.set_attribute("data-dragging", "true");
    } else {
        let _ = root_el.remove_attribute("data-dragging");
    }
}

// ── Track ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineInlaySliderTrack.poco",
    role = "panel",
    display = "contents"
)]
#[slot(default, only = [PineInlaySliderHandle])]
pub struct PineInlaySliderTrack {
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineInlaySliderTrack {}

// ── Handle ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineInlaySliderHandle.poco",
    role = "panel",
    display = "contents"
)]
// Unrestricted slot: the inlaid form (number field, unit select,
// dividers, Grip) is entirely author-supplied.
#[slot(default)]
pub struct PineInlaySliderHandle {
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub percent: f64,
}

#[handlers]
impl PineInlaySliderHandle {}

// ── Grip ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineInlaySliderGrip.poco",
    role = "interactive",
    display = "contents"
)]
#[slot(default)]
pub struct PineInlaySliderGrip {
    #[observe(ROOT)]
    pub value: f64,
    #[observe(ROOT)]
    pub min: f64,
    #[observe(ROOT)]
    pub max: f64,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineInlaySliderGrip {
    // Keyboard moves, routed from the template's `@keydown.*.prevent`
    // bindings. Up / Right increment per WAI-ARIA "slider" advice.
    pub fn step_inc(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineInlaySliderRoot| r.step_by(1.0));
        }
    }

    pub fn step_dec(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineInlaySliderRoot| r.step_by(-1.0));
        }
    }

    pub fn page_inc(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineInlaySliderRoot| r.step_by(10.0));
        }
    }

    pub fn page_dec(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineInlaySliderRoot| r.step_by(-10.0));
        }
    }

    pub fn to_min(&mut self) {
        if let Some(root) = ROOT.inject() {
            let min = root.with(|r| r.min);
            root.update(|r: &mut PineInlaySliderRoot| r.set_value(min));
        }
    }

    pub fn to_max(&mut self) {
        if let Some(root) = ROOT.inject() {
            let max = root.with(|r| r.max);
            root.update(|r: &mut PineInlaySliderRoot| r.set_value(max));
        }
    }
}
