//! `<pine-splitter-*>` — resizable panel group (compound).
//!
//! Three parts, matching Radix-Vue's Splitter anatomy:
//!
//! - **Group** (`pine-splitter-group`) — owns `direction` +
//!   per-panel sizes (percentages summing to 100). Provides its
//!   Handle. Panels register themselves at `on_setup`.
//! - **Panel** (`pine-splitter-panel`) — a single resizable cell.
//!   `default_size` / `min_size` / `max_size` are static sizing
//!   constraints (all percentages). Templates bind their
//!   `flex-basis` from `sizes[my_index]`.
//! - **ResizeHandle** (`pine-splitter-resize-handle`) —
//!   `role="separator"` between two adjacent Panels. Drag
//!   redistributes the delta between its two neighbours, clamped
//!   against their respective min/max. Keyboard arrows nudge by
//!   1 % (Shift = 10 %), Home / End collapse to the extreme.
//!
//! The Group emits its current `sizes: Vec<f64>` on
//! `pp:update:model` every time a drag commits or a keyboard step
//! fires, so authors can `pp-model:sizes="my_layout"` to persist
//! and restore layouts.
//!
//! ```html
//! <pine-splitter-group direction="horizontal" pp-model:sizes="layout">
//!   <pine-splitter-panel default-size="25" min-size="10"></pine-splitter-panel>
//!   <pine-splitter-resize-handle></pine-splitter-resize-handle>
//!   <pine-splitter-panel default-size="50"></pine-splitter-panel>
//!   <pine-splitter-resize-handle></pine-splitter-resize-handle>
//!   <pine-splitter-panel default-size="25" min-size="10"></pine-splitter-panel>
//! </pine-splitter-group>
//! ```

use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, inject_key, provide};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{EventTarget, PointerEvent};

inject_key!(GROUP: Handle<PineSplitterGroup>);

// Per-Group runtime: handle-level drag state + the document-level
// pointer listeners installed for the currently-active handle, so
// `on_unmount` can detach them cleanly.
struct GroupRuntime {
    group_el: Option<web_sys::Element>,
    pointer_move: Option<Closure<dyn FnMut(PointerEvent)>>,
    pointer_up: Option<Closure<dyn FnMut(PointerEvent)>>,
    dragging_pointer_id: Option<i32>,
}

thread_local! {
    static GROUP_RUNTIME: RefCell<HashMap<ScopeId, GroupRuntime>> =
        RefCell::new(HashMap::new());
}

// ── Group ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineSplitterGroup.poco", role = "scope", display = "contents")]
pub struct PineSplitterGroup {
    /// `"horizontal"` (panels side-by-side) or `"vertical"`
    /// (panels stacked).
    #[prop]
    pub direction: String,
    /// Per-panel sizes as percentages. Order matches the Panels'
    /// registration order, which matches DOM source order.
    ///
    /// `#[prop]` so `pp-model:sizes="my_layout"` hydrates the
    /// parent's value into Group.sizes on mount (and on every
    /// parent-side change), and so authors who only want one-way
    /// binding via `<pine-splitter-group sizes="[25, 50, 25]">`
    /// hit the standard attribute → prop pipeline.
    #[prop]
    pub sizes: Vec<f64>,
    pub min_sizes: Vec<f64>,
    pub max_sizes: Vec<f64>,
    /// Panel scope ids (as raw `u64`s) indexed the same as
    /// `sizes`. Stored as `u64` instead of `ScopeId` so the
    /// `#[component]` macro's generated get/set arms can serde-
    /// round-trip this field like any other.
    pub panel_scopes: Vec<u64>,
}

impl Default for PineSplitterGroup {
    fn default() -> Self {
        Self {
            direction: "horizontal".into(),
            sizes: Vec::new(),
            min_sizes: Vec::new(),
            max_sizes: Vec::new(),
            panel_scopes: Vec::new(),
        }
    }
}

#[handlers]
impl PineSplitterGroup {
    pub fn on_setup(&mut self) {
        provide(&GROUP, this::<Self>());
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        if let Some(scope) = current_scope_id() {
            if let Some(el) = refs.get("group") {
                GROUP_RUNTIME.with(|r| {
                    r.borrow_mut().insert(
                        scope,
                        GroupRuntime {
                            group_el: Some(el),
                            pointer_move: None,
                            pointer_up: None,
                            dragging_pointer_id: None,
                        },
                    );
                });
            }
        }
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            teardown_runtime(scope);
        }
    }
}

impl PineSplitterGroup {
    /// Called by each Panel's `on_setup`. Returns the index
    /// assigned to that Panel so the Panel can bind its flex-basis
    /// to `sizes[idx]`.
    pub fn register_panel(
        &mut self,
        scope_id: ScopeId,
        default_size: f64,
        min_size: f64,
        max_size: f64,
    ) -> usize {
        let idx = self.panel_scopes.len();
        self.panel_scopes.push(scope_id.0);
        // Grow parallel vectors — but preserve any pre-seeded
        // value in `sizes` (e.g. parent hydrated via
        // `pp-model:sizes="[25, 50, 25]"`). The min/max bounds
        // come from the Panel's own props regardless.
        //
        // Deliberately *not* renormalising here: each Panel's
        // `default-size` is taken as-is so authors whose
        // default-sizes sum to 100 get exactly [25, 50, 25] and
        // not a rescaled version based on how far along the
        // registration chain this Panel is.
        if self.sizes.len() <= idx {
            self.sizes.push(default_size);
        }
        if self.min_sizes.len() <= idx {
            self.min_sizes.push(min_size);
        } else {
            self.min_sizes[idx] = min_size;
        }
        if self.max_sizes.len() <= idx {
            self.max_sizes.push(max_size);
        } else {
            self.max_sizes[idx] = max_size;
        }
        idx
    }

    /// Redistribute `delta_pct` between `before_idx` and
    /// `after_idx`. Clamps against both panels' min / max and
    /// returns the actual applied delta (useful for callers that
    /// want to know whether the handle hit a constraint).
    pub fn resize(&mut self, before_idx: usize, after_idx: usize, delta_pct: f64) -> f64 {
        if before_idx >= self.sizes.len() || after_idx >= self.sizes.len() {
            return 0.0;
        }
        let b_cur = self.sizes[before_idx];
        let a_cur = self.sizes[after_idx];
        let b_min = self.min_sizes[before_idx];
        let b_max = self.max_sizes[before_idx];
        let a_min = self.min_sizes[after_idx];
        let a_max = self.max_sizes[after_idx];
        // Step 1: compute a candidate before-size within its own
        // clamp range.
        let b_proposed = (b_cur + delta_pct).clamp(b_min, b_max);
        let candidate_delta = b_proposed - b_cur;
        // Step 2: apply the symmetric change to `after`, clamp,
        // derive the final delta from whichever side binds first.
        let a_proposed = (a_cur - candidate_delta).clamp(a_min, a_max);
        let final_delta = a_cur - a_proposed;
        self.sizes[before_idx] = b_cur + final_delta;
        self.sizes[after_idx] = a_cur - final_delta;
        final_delta
    }

    /// Emit the current `sizes` on the Group's `pp:update:model`
    /// channel. Called after every committed resize (drag end +
    /// keyboard step).
    pub fn emit_sizes(&self) {
        emit_model(self.sizes.clone());
    }
}

fn teardown_runtime(scope: ScopeId) {
    let rt = match GROUP_RUNTIME.with(|r| r.borrow_mut().remove(&scope)) {
        Some(rt) => rt,
        None => return,
    };
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let t: &EventTarget = doc.as_ref();
        if let Some(c) = rt.pointer_move.as_ref() {
            let _ = t.remove_event_listener_with_callback(
                "pointermove",
                c.as_ref().unchecked_ref(),
            );
        }
        if let Some(c) = rt.pointer_up.as_ref() {
            let _ = t.remove_event_listener_with_callback(
                "pointerup",
                c.as_ref().unchecked_ref(),
            );
            let _ = t.remove_event_listener_with_callback(
                "pointercancel",
                c.as_ref().unchecked_ref(),
            );
        }
    }
}

// ── Panel ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSplitterPanel.poco", role = "panel", display = "contents")]
pub struct PineSplitterPanel {
    #[prop]
    pub default_size: f64,
    #[prop]
    pub min_size: f64,
    #[prop]
    pub max_size: f64,
    /// Mirrored from `GROUP.direction` via the macro-installed
    /// observer (replaces a hand-written `watch_scope_field`).
    #[observe(GROUP)]
    pub direction: String,
    /// This panel's index in Group.sizes / panel_index. Seeded at
    /// registration time (`on_setup`) and stable for the panel's
    /// lifetime.
    pub my_index: usize,
    /// Reactive mirror of `group.sizes[my_index]`. Drives the
    /// `flex-basis` inline style. Can't use `#[observe]` directly
    /// — this is a *derived* projection of a Vec, not a 1:1
    /// field mirror, so the watch in `on_ready` pulls the right
    /// slot each time `sizes` ticks.
    pub size: f64,
}

#[handlers]
impl PineSplitterPanel {
    pub fn on_setup(&mut self) {
        // Defaults are opt-in: authors leave them 0 to let Group
        // split evenly. Set the bounds so clamp doesn't collapse
        // everything to 0 when only `default_size` is set.
        if self.max_size == 0.0 {
            self.max_size = 100.0;
        }
        let Some(group) = inject::<Handle<PineSplitterGroup>>(&GROUP) else {
            return;
        };
        let Some(scope) = current_scope_id() else { return };
        let default = if self.default_size > 0.0 {
            self.default_size
        } else {
            // When zero, let normalize_sizes even it out post-
            // registration. A nonzero placeholder keeps the sum
            // non-zero so the normalize factor is well-defined.
            1.0
        };
        let min = self.min_size;
        let max = self.max_size;
        let (my_index, size) = group.update(|g| {
            let idx = g.register_panel(scope, default, min, max);
            (idx, g.sizes.get(idx).copied().unwrap_or(0.0))
        });
        self.my_index = my_index;
        self.size = size;
        // `direction` is mirrored by `#[observe(GROUP)]`.
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(group) = inject::<Handle<PineSplitterGroup>>(&GROUP) else {
            return;
        };
        let group_scope = group.scope_id();
        let handle_for_watch = handle;
        let group_for_watch = group;
        // Watch the full Vec<f64>. When sizes changes for any
        // reason (a drag, a keyboard step, a later panel
        // registering and renormalizing), pull the slot matching
        // our index.
        pocopine::watch_scope_field::<Vec<f64>, _>(group_scope, "sizes", move |v, _| {
            let idx = handle_for_watch.with(|s| s.my_index);
            if let Some(&new_size) = v.get(idx) {
                handle_for_watch.update(|s| s.size = new_size);
            }
            let _ = &group_for_watch;
        });
    }
}

// ── ResizeHandle ──────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSplitterResizeHandle.poco", role = "interactive", display = "contents")]
pub struct PineSplitterResizeHandle {
    #[prop]
    pub disabled: bool,
    /// Mirrored from `GROUP.direction` via the macro-installed
    /// observer.
    #[observe(GROUP)]
    pub direction: String,
    /// Index of the Panel immediately before this handle in DOM
    /// order. Computed in `on_ready` by counting preceding
    /// `pine-splitter-panel` siblings.
    pub before_idx: usize,
    pub after_idx: usize,
    /// `aria-valuenow` for this handle — the size of the panel
    /// immediately before it. Derived from `group.sizes[before_idx]`,
    /// so not a 1:1 field mirror; the watch in `on_ready` keeps it
    /// in sync.
    pub aria_value: f64,
}

#[handlers]
impl PineSplitterResizeHandle {
    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let Some(group) = inject::<Handle<PineSplitterGroup>>(&GROUP) else {
            return;
        };
        // Determine our neighbour indices by counting preceding
        // `pine-splitter-panel` siblings.
        let Some(el) = refs.get("handle") else { return };
        let (before_idx, after_idx) = neighbour_indices(&el);
        let initial_value = group.with(|g| g.sizes.get(before_idx).copied().unwrap_or(0.0));
        // `on_ready` runs under an immutable borrow on
        // `scope.state` (walker.rs:193). Defer the initial
        // write so the borrow can unwind before handle.update
        // re-borrows mutably.
        let handle_for_init = handle.clone();
        pocopine::tick::next(move || {
            handle_for_init.update(|s| {
                s.before_idx = before_idx;
                s.after_idx = after_idx;
                s.aria_value = initial_value;
            });
        });

        let group_scope = group.scope_id();
        let handle_for_aria = handle;
        pocopine::watch_scope_field::<Vec<f64>, _>(group_scope, "sizes", move |v, _| {
            let idx = handle_for_aria.with(|s| s.before_idx);
            if let Some(&val) = v.get(idx) {
                handle_for_aria.update(|s| s.aria_value = val);
            }
        });
    }

    pub fn on_pointer_down(&mut self, ev: wasm_bindgen::JsValue) {
        if self.disabled {
            return;
        }
        let Some(group) = inject::<Handle<PineSplitterGroup>>(&GROUP) else {
            return;
        };
        let Ok(ev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        start_drag(group, self.before_idx, self.after_idx, self.direction.clone(), ev);
    }

    pub fn step_inc(&mut self) {
        self.step(1.0);
    }
    pub fn step_dec(&mut self) {
        self.step(-1.0);
    }
    pub fn step_inc_big(&mut self) {
        self.step(10.0);
    }
    pub fn step_dec_big(&mut self) {
        self.step(-10.0);
    }
    pub fn to_min(&mut self) {
        self.step(-100.0);
    }
    pub fn to_max(&mut self) {
        self.step(100.0);
    }
}

impl PineSplitterResizeHandle {
    fn step(&self, delta: f64) {
        if self.disabled {
            return;
        }
        let Some(group) = inject::<Handle<PineSplitterGroup>>(&GROUP) else {
            return;
        };
        let before = self.before_idx;
        let after = self.after_idx;
        group.update(|g: &mut PineSplitterGroup| {
            g.resize(before, after, delta);
            g.emit_sizes();
        });
    }
}

fn neighbour_indices(handle_el: &web_sys::Element) -> (usize, usize) {
    // `handle_el` is this ResizeHandle's inner `<button>` (where
    // `pp-ref="handle"` sits). One `parent_element` step up lands
    // us on this component's own `<pine-splitter-resize-handle>`
    // custom tag; a second step reaches the Group's inner `<div>`
    // whose children are the Panel / ResizeHandle custom tags in
    // DOM order.
    let Some(own_tag) = handle_el.parent_element() else {
        return (0, 0);
    };
    let Some(parent) = own_tag.parent_element() else {
        return (0, 0);
    };
    let children = parent.children();
    let mut count = 0usize;
    for i in 0..children.length() {
        let Some(child) = children.item(i) else { continue };
        let child_node: &web_sys::Node = child.as_ref();
        let own_tag_node: &web_sys::Node = own_tag.as_ref();
        if child_node.is_same_node(Some(own_tag_node)) {
            // No preceding Panel → return (0, 0); `resize` is a
            // no-op outside the sizes vec bounds.
            if count == 0 {
                return (0, 0);
            }
            return (count - 1, count);
        }
        if child.tag_name().eq_ignore_ascii_case("pine-splitter-panel") {
            count += 1;
        }
    }
    (0, 0)
}

fn start_drag(
    group: Handle<PineSplitterGroup>,
    before_idx: usize,
    after_idx: usize,
    direction: String,
    ev: PointerEvent,
) {
    let group_scope = group.scope_id();
    let Some(group_el) = GROUP_RUNTIME.with(|r| {
        r.borrow()
            .get(&group_scope)
            .and_then(|rt| rt.group_el.clone())
    }) else {
        return;
    };

    ev.prevent_default();
    let start_pointer = if direction == "vertical" {
        ev.client_y() as f64
    } else {
        ev.client_x() as f64
    };
    let start_before_size = group.with(|g| g.sizes.get(before_idx).copied().unwrap_or(0.0));

    GROUP_RUNTIME.with(|r| {
        if let Some(rt) = r.borrow_mut().get_mut(&group_scope) {
            rt.dragging_pointer_id = Some(ev.pointer_id());
        }
    });

    let group_for_move = group.clone();
    let direction_for_move = direction.clone();
    let group_el_for_move = group_el.clone();
    let move_ = Closure::wrap(Box::new(move |ev: PointerEvent| {
        let active = GROUP_RUNTIME.with(|r| {
            r.borrow()
                .get(&group_scope)
                .and_then(|rt| rt.dragging_pointer_id)
                == Some(ev.pointer_id())
        });
        if !active {
            return;
        }
        ev.prevent_default();
        // Recompute the group rect each move — the user may
        // resize the viewport mid-drag.
        let rect_now = group_el_for_move.get_bounding_client_rect();
        let (pointer_now, span) = if direction_for_move == "vertical" {
            (ev.client_y() as f64, rect_now.height().max(1.0))
        } else {
            (ev.client_x() as f64, rect_now.width().max(1.0))
        };
        let delta_px = pointer_now - start_pointer;
        let delta_pct = (delta_px / span) * 100.0;
        // Resize relative to the starting before-size so the total
        // drag is absolute, not cumulative with jitter.
        group_for_move.update(|g: &mut PineSplitterGroup| {
            let b_cur = g.sizes.get(before_idx).copied().unwrap_or(0.0);
            let desired = start_before_size + delta_pct;
            let step = desired - b_cur;
            g.resize(before_idx, after_idx, step);
        });
    }) as Box<dyn FnMut(PointerEvent)>);

    let group_for_up = group;
    let up = Closure::wrap(Box::new(move |ev: PointerEvent| {
        let was_active = GROUP_RUNTIME.with(|r| {
            let mut map = r.borrow_mut();
            if let Some(rt) = map.get_mut(&group_scope) {
                if rt.dragging_pointer_id == Some(ev.pointer_id()) {
                    rt.dragging_pointer_id = None;
                    return true;
                }
            }
            false
        });
        if was_active {
            group_for_up.update(|g: &mut PineSplitterGroup| g.emit_sizes());
        }
    }) as Box<dyn FnMut(PointerEvent)>);

    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let t: &EventTarget = doc.as_ref();
        let _ = t.add_event_listener_with_callback("pointermove", move_.as_ref().unchecked_ref());
        let _ = t.add_event_listener_with_callback("pointerup", up.as_ref().unchecked_ref());
        let _ = t.add_event_listener_with_callback("pointercancel", up.as_ref().unchecked_ref());
    }

    GROUP_RUNTIME.with(|r| {
        if let Some(rt) = r.borrow_mut().get_mut(&group_scope) {
            rt.pointer_move = Some(move_);
            rt.pointer_up = Some(up);
        }
    });
}
