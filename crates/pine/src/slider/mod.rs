//! `<pine-slider-*>` — range input primitive (compound).
//!
//! Four parts, matching Radix / Reka's Slider surface:
//!
//! - **Root** (`pine-slider-root`) — owns `value`, `min`, `max`,
//!   `step`, `orientation`, `disabled`. Two-way bindable via
//!   `pp-model:value="my_volume"`. Installs the pointer handlers
//!   on its own element so clicks anywhere on the slider surface
//!   (Track, Range, or Thumb) start a drag. Provides its Handle
//!   to descendants.
//! - **Track** (`pine-slider-track`) — the rail the Range fills.
//!   Tagged with `pp-ref="track"` so Root knows where to measure
//!   the pointer position from.
//! - **Range** (`pine-slider-range`) — the filled portion, sized
//!   as `width: <percent>%` (horizontal) or
//!   `height: <percent>%` (vertical).
//! - **Thumb** (`pine-slider-thumb`) — `role="slider"` +
//!   `aria-valuemin` / `-max` / `-now` / `-orientation`. Keyboard
//!   moves: arrows (step), Page ↑↓ (10×), Home / End
//!   (min / max). Positioned at `left: <percent>%`.
//!
//! ```html
//! <pine-slider-root pp-model:value="volume" min="0" max="100" step="5">
//!   <pine-slider-track>
//!     <pine-slider-range></pine-slider-range>
//!   </pine-slider-track>
//!   <pine-slider-thumb></pine-slider-thumb>
//! </pine-slider-root>
//! ```
//!
//! v1 supports a single thumb and both orientations. Multi-thumb
//! range sliders wait for a follow-up.

use pocopine::create_context;
use pocopine::events::{self, ev};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::rc::Rc;
use web_sys::PointerEvent;

create_context!(ROOT: Handle<PineSliderRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineSliderRoot.poco", role = "scope", display = "contents")]
// RFC 049 — Slider's Root holds the Track (visual bar) + one
// or more Thumbs (drag handles). The Range lives inside the
// Track; Thumbs live at Root level so the author positions
// them directly.
#[slot(default, only = [PineSliderTrack, PineSliderThumb])]
pub struct PineSliderRoot {
    /// Current value. Two-way bindable via
    /// `pp-model:value="my_volume"` on the tag.
    #[model]
    pub value: f64,
    #[prop]
    pub min: f64,
    #[prop]
    pub max: f64,
    /// Quantization for both keyboard + pointer moves.
    #[prop]
    pub step: f64,
    /// `"horizontal"` (default) or `"vertical"`. Drives the
    /// `data-orientation` attr + the drag geometry.
    #[prop]
    pub orientation: String,
    #[prop]
    pub disabled: bool,
    /// Computed mirror: `(value - min) / (max - min) * 100`,
    /// clamped to 0..100. Drives Range's width and Thumb's
    /// offset via template `style=` bindings so authors don't
    /// have to derive it themselves.
    pub percent: f64,
}

impl Default for PineSliderRoot {
    fn default() -> Self {
        Self {
            value: 0.0,
            min: 0.0,
            max: 100.0,
            step: 1.0,
            orientation: "horizontal".into(),
            disabled: false,
            percent: 0.0,
        }
    }
}

#[handlers]
impl PineSliderRoot {
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
        install_pointer(root_el, handle);
    }
}

impl PineSliderRoot {
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

fn install_pointer(root_el: web_sys::Element, root: Handle<PineSliderRoot>) {
    // The drag's pointer id lives in an Rc<Cell> shared by all
    // four listener closures. No thread-local side table needed —
    // `events::on_scoped` ties each listener to the current scope's
    // unmount, and the Rc dies with the closures.
    let dragging = Rc::new(Cell::new(None::<i32>));

    // pointerdown on the slider root — snaps to click position +
    // marks the drag as active.
    let root_for_down = root_el.clone();
    let handle_for_down = root.clone();
    let dragging_for_down = dragging.clone();
    events::on_scoped(&root_el, ev::pointerdown, move |ev| {
        if handle_for_down.with(|r| r.disabled) {
            return;
        }
        dragging_for_down.set(Some(ev.pointer_id()));
        ev.prevent_default();
        let v = value_from_pointer(&root_for_down, &ev, &handle_for_down);
        handle_for_down.update(|r| r.set_value(v));
    });

    // pointermove + pointerup attach to `document`. Pointer capture
    // on the slider itself is flaky across real browsers (the
    // event can be retargeted to the original hit element instead
    // of the capturing one). Document-level listeners always fire,
    // and we gate them on `dragging` so they're no-ops when the
    // user isn't dragging this slider.
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };

    let dragging_for_move = dragging.clone();
    events::on_scoped(&doc, ev::pointermove, move |ev| {
        if dragging_for_move.get() != Some(ev.pointer_id()) {
            return;
        }
        ev.prevent_default();
        let v = value_from_pointer(&root_el, &ev, &root);
        root.update(|r| r.set_value(v));
    });

    let dragging_for_up = dragging.clone();
    events::on_scoped(&doc, ev::pointerup, move |ev: PointerEvent| {
        if dragging_for_up.get() == Some(ev.pointer_id()) {
            dragging_for_up.set(None);
        }
    });
    events::on_scoped(&doc, ev::pointercancel, move |ev: PointerEvent| {
        if dragging.get() == Some(ev.pointer_id()) {
            dragging.set(None);
        }
    });
}

/// Compute the Slider value corresponding to a pointer position
/// relative to the Track's bounding rect. Falls back to Root's
/// rect if Track isn't findable. Honours orientation.
fn value_from_pointer(
    root_el: &web_sys::Element,
    ev: &PointerEvent,
    root: &Handle<PineSliderRoot>,
) -> f64 {
    let rect = root_el
        .query_selector(".pine-slider-track")
        .ok()
        .flatten()
        .map(|t| t.get_bounding_client_rect())
        .unwrap_or_else(|| root_el.get_bounding_client_rect());
    let (min, max, orientation) = root.with(|r| (r.min, r.max, r.orientation.clone()));
    let pct = if orientation == "vertical" {
        let h = rect.height().max(1.0);
        let y = (ev.client_y() as f64) - rect.top();
        (y / h).clamp(0.0, 1.0)
    } else {
        let w = rect.width().max(1.0);
        let x = (ev.client_x() as f64) - rect.left();
        (x / w).clamp(0.0, 1.0)
    };
    min + pct * (max - min)
}

// ── Track ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineSliderTrack.poco",
    role = "panel",
    display = "contents"
)]
// RFC 049 — Track holds the Range (filled portion). Nothing
// else: the Range's positioning is the only thing that draws
// inside the Track.
#[slot(default, only = [PineSliderRange])]
pub struct PineSliderTrack {
    /// Mirrored for the `data-orientation` / `data-disabled` attrs.
    #[observe(ROOT)]
    pub orientation: String,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineSliderTrack {}

// ── Range ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineSliderRange.poco",
    role = "visual",
    display = "contents"
)]
pub struct PineSliderRange {
    #[observe(ROOT)]
    pub orientation: String,
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub percent: f64,
}

#[handlers]
impl PineSliderRange {}

// ── Thumb ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineSliderThumb.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineSliderThumb {
    /// Mirrored from Root for ARIA + positioning.
    #[observe(ROOT)]
    pub value: f64,
    #[observe(ROOT)]
    pub min: f64,
    #[observe(ROOT)]
    pub max: f64,
    #[observe(ROOT)]
    pub percent: f64,
    #[observe(ROOT)]
    pub orientation: String,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineSliderThumb {
    // Keyboard moves: the `@keydown.*.prevent` bindings in the
    // template route each recognised key to one of these handlers.
    // Direction for Up / Right matches WAI-ARIA "slider" advice:
    // increment value, regardless of orientation's visual axis.
    pub fn step_inc(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineSliderRoot| r.step_by(1.0));
        }
    }

    pub fn step_dec(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineSliderRoot| r.step_by(-1.0));
        }
    }

    pub fn page_inc(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineSliderRoot| r.step_by(10.0));
        }
    }

    pub fn page_dec(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineSliderRoot| r.step_by(-10.0));
        }
    }

    pub fn to_min(&mut self) {
        if let Some(root) = ROOT.inject() {
            let min = root.with(|r| r.min);
            root.update(|r: &mut PineSliderRoot| r.set_value(min));
        }
    }

    pub fn to_max(&mut self) {
        if let Some(root) = ROOT.inject() {
            let max = root.with(|r| r.max);
            root.update(|r: &mut PineSliderRoot| r.set_value(max));
        }
    }
}
