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

use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, inject_key, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{EventTarget, PointerEvent};

inject_key!(ROOT: Handle<PineSliderRoot>);

// Per-Root runtime: captures installed listeners + drag state so
// `on_unmount` can detach cleanly.
struct RootRuntime {
    root_el: Option<web_sys::Element>,
    pointer_down: Option<Closure<dyn FnMut(PointerEvent)>>,
    pointer_move: Option<Closure<dyn FnMut(PointerEvent)>>,
    pointer_up: Option<Closure<dyn FnMut(PointerEvent)>>,
    dragging_pointer_id: Option<i32>,
}

thread_local! {
    static ROOT_RUNTIME: RefCell<HashMap<ScopeId, RootRuntime>> =
        RefCell::new(HashMap::new());
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineSliderRoot.poco", role = "scope")]
pub struct PineSliderRoot {
    /// Current value. Two-way bindable via
    /// `pp-model:value="my_volume"` on the tag.
    #[prop]
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
    pub fn on_setup(&mut self) {
        self.recompute_percent();
        provide(&ROOT, this::<Self>());
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

    pub fn on_ready(
        &self,
        handle: pocopine::Handle<Self>,
        refs: pocopine::Refs,
        scope: ScopeId,
    ) {
        let Some(root_el) = refs.get("root") else { return };
        install_pointer(scope, root_el, handle);
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            teardown_pointer(scope);
        }
    }
}

impl PineSliderRoot {
    fn recompute_percent(&mut self) {
        let span = (self.max - self.min).max(f64::EPSILON);
        let pct = ((self.value - self.min) / span) * 100.0;
        self.percent = pct.clamp(0.0, 100.0);
    }

    /// Clamp + snap to the nearest `step` and commit. Emits
    /// `pp:update:model` when the value actually changed.
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
            emit_value_update(final_v);
        }
    }

    pub fn step_by(&mut self, multiplier: f64) {
        self.set_value(self.value + self.step * multiplier);
    }
}

/// Emit `pp:update:model` from Root's own element so a parent's
/// `pp-model:value` catches the change regardless of which Slider
/// sub-part triggered it.
fn emit_value_update(value: f64) {
    let Some(scope) = current_scope_id() else { return };
    let Some(root_el) = refs::get_on(scope, "root") else { return };
    pocopine::emit_from(&root_el, "pp:update:model", value);
}

fn install_pointer(
    scope: ScopeId,
    root_el: web_sys::Element,
    root: Handle<PineSliderRoot>,
) {
    // pointerdown: snap to click position + capture the pointer so
    // we keep getting move / up even when the cursor leaves the
    // slider bounds.
    let root_for_down = root_el.clone();
    let handle_for_down = root.clone();
    let down = Closure::wrap(Box::new(move |ev: PointerEvent| {
        if handle_for_down.with(|r| r.disabled) {
            return;
        }
        let _ = root_for_down.set_pointer_capture(ev.pointer_id());
        ROOT_RUNTIME.with(|r| {
            if let Some(rt) = r.borrow_mut().get_mut(&scope) {
                rt.dragging_pointer_id = Some(ev.pointer_id());
            }
        });
        ev.prevent_default();
        let v = value_from_pointer(&root_for_down, &ev, &handle_for_down);
        handle_for_down.update(|r: &mut PineSliderRoot| r.set_value(v));
    }) as Box<dyn FnMut(PointerEvent)>);

    // pointermove: only reacts while a drag is in progress for this scope.
    let root_for_move = root_el.clone();
    let handle_for_move = root.clone();
    let move_ = Closure::wrap(Box::new(move |ev: PointerEvent| {
        let active = ROOT_RUNTIME.with(|r| {
            r.borrow()
                .get(&scope)
                .and_then(|rt| rt.dragging_pointer_id)
                == Some(ev.pointer_id())
        });
        if !active {
            return;
        }
        let v = value_from_pointer(&root_for_move, &ev, &handle_for_move);
        handle_for_move.update(|r: &mut PineSliderRoot| r.set_value(v));
    }) as Box<dyn FnMut(PointerEvent)>);

    // pointerup / pointercancel: end the drag.
    let root_for_up = root_el.clone();
    let up = Closure::wrap(Box::new(move |ev: PointerEvent| {
        let was_active = ROOT_RUNTIME.with(|r| {
            let mut map = r.borrow_mut();
            let Some(rt) = map.get_mut(&scope) else {
                return false;
            };
            if rt.dragging_pointer_id == Some(ev.pointer_id()) {
                rt.dragging_pointer_id = None;
                true
            } else {
                false
            }
        });
        if was_active {
            let _ = root_for_up.release_pointer_capture(ev.pointer_id());
        }
    }) as Box<dyn FnMut(PointerEvent)>);

    let target: &EventTarget = root_el.as_ref();
    let _ = target.add_event_listener_with_callback("pointerdown", down.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("pointermove", move_.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("pointerup", up.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("pointercancel", up.as_ref().unchecked_ref());

    ROOT_RUNTIME.with(|r| {
        r.borrow_mut().insert(
            scope,
            RootRuntime {
                root_el: Some(root_el),
                pointer_down: Some(down),
                pointer_move: Some(move_),
                pointer_up: Some(up),
                dragging_pointer_id: None,
            },
        );
    });
}

fn teardown_pointer(scope: ScopeId) {
    let Some(rt) = ROOT_RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    let Some(el) = rt.root_el.as_ref() else { return };
    let target: &EventTarget = el.as_ref();
    if let Some(c) = rt.pointer_down.as_ref() {
        let _ =
            target.remove_event_listener_with_callback("pointerdown", c.as_ref().unchecked_ref());
    }
    if let Some(c) = rt.pointer_move.as_ref() {
        let _ =
            target.remove_event_listener_with_callback("pointermove", c.as_ref().unchecked_ref());
    }
    if let Some(c) = rt.pointer_up.as_ref() {
        let _ = target.remove_event_listener_with_callback("pointerup", c.as_ref().unchecked_ref());
        let _ = target
            .remove_event_listener_with_callback("pointercancel", c.as_ref().unchecked_ref());
    }
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
#[component(template = "PineSliderTrack.poco", role = "panel")]
pub struct PineSliderTrack {
    /// Mirrored for the `data-orientation` / `data-disabled` attrs.
    pub orientation: String,
    pub disabled: bool,
}

#[handlers]
impl PineSliderTrack {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.with(|r| {
                self.orientation = r.orientation.clone();
                self.disabled = r.disabled;
            });
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject::<Handle<PineSliderRoot>>(&ROOT) else {
            return;
        };
        let h1 = handle.clone();
        watch_scope_field::<String, _>(root.scope_id(), "orientation", move |v, _| {
            let v = v.clone();
            h1.update(|s| s.orientation = v);
        });
        let h2 = handle;
        watch_scope_field::<bool, _>(root.scope_id(), "disabled", move |&v, _| {
            h2.update(|s| s.disabled = v);
        });
    }
}

// ── Range ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSliderRange.poco", role = "visual")]
pub struct PineSliderRange {
    pub orientation: String,
    pub disabled: bool,
    pub percent: f64,
}

#[handlers]
impl PineSliderRange {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.with(|r| {
                self.orientation = r.orientation.clone();
                self.disabled = r.disabled;
                self.percent = r.percent;
            });
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject::<Handle<PineSliderRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();

        let h1 = handle.clone();
        watch_scope_field::<f64, _>(root_scope, "percent", move |&p, _| {
            h1.update(|s| s.percent = p);
        });
        let h2 = handle.clone();
        watch_scope_field::<String, _>(root_scope, "orientation", move |v, _| {
            let v = v.clone();
            h2.update(|s| s.orientation = v);
        });
        let h3 = handle;
        watch_scope_field::<bool, _>(root_scope, "disabled", move |&v, _| {
            h3.update(|s| s.disabled = v);
        });
    }
}

// ── Thumb ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSliderThumb.poco", role = "interactive")]
pub struct PineSliderThumb {
    /// Mirrored from Root for ARIA + positioning.
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub percent: f64,
    pub orientation: String,
    pub disabled: bool,
}

#[handlers]
impl PineSliderThumb {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.with(|r| {
                self.value = r.value;
                self.min = r.min;
                self.max = r.max;
                self.percent = r.percent;
                self.orientation = r.orientation.clone();
                self.disabled = r.disabled;
            });
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject::<Handle<PineSliderRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();
        let h = handle.clone();
        watch_scope_field::<f64, _>(root_scope, "value", move |&v, _| {
            h.update(|s| s.value = v);
        });
        let h = handle.clone();
        watch_scope_field::<f64, _>(root_scope, "percent", move |&p, _| {
            h.update(|s| s.percent = p);
        });
        let h = handle.clone();
        watch_scope_field::<f64, _>(root_scope, "min", move |&v, _| {
            h.update(|s| s.min = v);
        });
        let h = handle.clone();
        watch_scope_field::<f64, _>(root_scope, "max", move |&v, _| {
            h.update(|s| s.max = v);
        });
        let h = handle.clone();
        watch_scope_field::<String, _>(root_scope, "orientation", move |v, _| {
            let v = v.clone();
            h.update(|s| s.orientation = v);
        });
        let h = handle;
        watch_scope_field::<bool, _>(root_scope, "disabled", move |&v, _| {
            h.update(|s| s.disabled = v);
        });
    }

    // Keyboard moves: the `@keydown.*.prevent` bindings in the
    // template route each recognised key to one of these handlers.
    // Direction for Up / Right matches WAI-ARIA "slider" advice:
    // increment value, regardless of orientation's visual axis.
    pub fn step_inc(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSliderRoot| r.step_by(1.0));
        }
    }

    pub fn step_dec(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSliderRoot| r.step_by(-1.0));
        }
    }

    pub fn page_inc(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSliderRoot| r.step_by(10.0));
        }
    }

    pub fn page_dec(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSliderRoot| r.step_by(-10.0));
        }
    }

    pub fn to_min(&mut self) {
        if let Some(root) = inject(&ROOT) {
            let min = root.with(|r| r.min);
            root.update(|r: &mut PineSliderRoot| r.set_value(min));
        }
    }

    pub fn to_max(&mut self) {
        if let Some(root) = inject(&ROOT) {
            let max = root.with(|r| r.max);
            root.update(|r: &mut PineSliderRoot| r.set_value(max));
        }
    }
}
