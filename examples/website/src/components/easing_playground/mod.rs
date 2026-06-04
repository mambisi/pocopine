//! An interactive cubic-bezier easing editor + animation preview. Drag
//! the two control points to shape the curve; pick an animation and hit
//! Play to run it with that easing (pine-motion `Easing::CubicBezier`).
//! Used as the live demo on `/motion/easing`.

use pine_motion::{animate, Easing, Tween};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Editor SVG side length (viewBox `0 0 SIZE SIZE`).
const SIZE: f64 = 220.0;
/// Inset margin: the 0..1 curve region is drawn inside `[PAD, SIZE-PAD]`
/// so springy overshoot (y up to ~1.2) still lands inside the box.
const PAD: f64 = 34.0;

#[derive(Serialize, Deserialize)]
#[component(
    template = "EasingPlayground.poco",
    style = "easing_playground.css",
    role = "panel"
)]
pub struct EasingPlayground {
    /// Cubic-bezier control points. `x` ∈ 0..1 (CSS requirement); `y` may
    /// overshoot for springy curves.
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    /// Point currently being dragged: `"p1"` | `"p2"` | `""`.
    pub dragging: String,
    /// Which properties to animate (multi-select). move/scale/rotate are
    /// merged into one `transform` channel; fade is a separate `opacity`.
    pub move_on: bool,
    pub scale_on: bool,
    pub rotate_on: bool,
    pub fade_on: bool,
    // Derived display values (recomputed on change so the template binds
    // strings, not f64 — avoids attribute coercion surprises).
    pub path_d: String,
    pub p1_cx: String,
    pub p1_cy: String,
    pub p2_cx: String,
    pub p2_cy: String,
    pub bezier_label: String,
}

impl Default for EasingPlayground {
    fn default() -> Self {
        let mut me = Self {
            x1: 0.25,
            y1: 0.1,
            x2: 0.25,
            y2: 1.0,
            dragging: String::new(),
            move_on: true,
            scale_on: false,
            rotate_on: false,
            fade_on: false,
            path_d: String::new(),
            p1_cx: String::new(),
            p1_cy: String::new(),
            p2_cx: String::new(),
            p2_cy: String::new(),
            bezier_label: String::new(),
        };
        me.recompute();
        me
    }
}

#[handlers]
impl EasingPlayground {
    pub fn start_p1(&mut self) {
        self.dragging = "p1".into();
    }
    pub fn start_p2(&mut self) {
        self.dragging = "p2".into();
    }
    pub fn end_drag(&mut self) {
        self.dragging = String::new();
    }

    /// Pointer move over the editor — if a handle is held, map the pointer
    /// into curve space (x ∈ 0..1, y inverted + overshoot-allowed).
    pub fn on_move(&mut self, ev: web_sys::MouseEvent) {
        if self.dragging.is_empty() {
            return;
        }
        let Some(svg) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector(".ce-editor-svg").ok().flatten())
        else {
            return;
        };
        let rect = svg.get_bounding_client_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        // Map within the inset region (account for PAD) so the handle
        // tracks the pointer 1:1 with the drawn curve.
        let pad_x = PAD / SIZE * rect.width();
        let pad_y = PAD / SIZE * rect.height();
        let nx = ((ev.client_x() as f64 - rect.left() - pad_x) / (rect.width() - 2.0 * pad_x))
            .clamp(0.0, 1.0);
        let ny = (1.0
            - (ev.client_y() as f64 - rect.top() - pad_y) / (rect.height() - 2.0 * pad_y))
            .clamp(-0.2, 1.2);
        if self.dragging == "p1" {
            self.x1 = nx;
            self.y1 = ny;
        } else {
            self.x2 = nx;
            self.y2 = ny;
        }
        self.recompute();
    }

    pub fn toggle_move(&mut self) {
        self.move_on = !self.move_on;
    }
    pub fn toggle_scale(&mut self) {
        self.scale_on = !self.scale_on;
    }
    pub fn toggle_rotate(&mut self) {
        self.rotate_on = !self.rotate_on;
    }
    pub fn toggle_fade(&mut self) {
        self.fade_on = !self.fade_on;
    }

    /// Run every selected property on the preview box at once with the
    /// current curve. move / scale / rotate merge into one `transform`
    /// channel; fade is a separate `opacity` channel.
    pub fn play(&mut self) {
        let easing = Easing::CubicBezier([self.x1, self.y1, self.x2, self.y2]);
        let mut from_t = String::new();
        let mut to_t = String::new();
        if self.move_on {
            from_t.push_str("translateX(0) ");
            to_t.push_str("translateX(220px) ");
        }
        if self.scale_on {
            from_t.push_str("scale(0.4) ");
            to_t.push_str("scale(1) ");
        }
        if self.rotate_on {
            from_t.push_str("rotate(0deg) ");
            to_t.push_str("rotate(180deg) ");
        }
        let mut channels: Vec<(&'static str, &str, &str)> = Vec::new();
        if !from_t.is_empty() {
            channels.push(("transform", from_t.as_str(), to_t.as_str()));
        }
        if self.fade_on {
            channels.push(("opacity", "0", "1"));
        }
        if channels.is_empty() {
            return;
        }
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(Some(el)) = doc.query_selector(".ce-preview-box") {
                animate(&el, &channels, Tween::new().duration(900.0).easing(easing));
            }
        }
    }

    pub fn preset_ease(&mut self) {
        self.set_points(0.25, 0.1, 0.25, 1.0);
    }
    pub fn preset_in(&mut self) {
        self.set_points(0.42, 0.0, 1.0, 1.0);
    }
    pub fn preset_out(&mut self) {
        self.set_points(0.0, 0.0, 0.58, 1.0);
    }
    pub fn preset_in_out(&mut self) {
        self.set_points(0.42, 0.0, 0.58, 1.0);
    }
    pub fn preset_back(&mut self) {
        self.set_points(0.34, 1.56, 0.64, 1.0);
    }
}

impl EasingPlayground {
    fn set_points(&mut self, x1: f64, y1: f64, x2: f64, y2: f64) {
        self.x1 = x1;
        self.y1 = y1;
        self.x2 = x2;
        self.y2 = y2;
        self.recompute();
    }

    /// Recompute the SVG geometry + label. The curve runs inside the inset
    /// box `[PAD, SIZE-PAD]`; SVG y is inverted (curve y = 0 → bottom).
    fn recompute(&mut self) {
        let inner = SIZE - 2.0 * PAD;
        let start = SIZE - PAD; // P0 y (curve y = 0)
        let end = PAD; // P3 y (curve y = 1)
        let p1x = PAD + self.x1 * inner;
        let p1y = start - self.y1 * inner;
        let p2x = PAD + self.x2 * inner;
        let p2y = start - self.y2 * inner;
        self.path_d = format!("M {PAD} {start} C {p1x} {p1y} {p2x} {p2y} {start} {end}");
        self.p1_cx = format!("{p1x:.1}");
        self.p1_cy = format!("{p1y:.1}");
        self.p2_cx = format!("{p2x:.1}");
        self.p2_cy = format!("{p2y:.1}");
        self.bezier_label = format!(
            "cubic-bezier({:.2}, {:.2}, {:.2}, {:.2})",
            self.x1, self.y1, self.x2, self.y2
        );
    }
}
