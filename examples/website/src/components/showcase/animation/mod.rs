//! `pine-motion` showcase — sub-demos exercising the library's
//! headline features:
//!
//! * **Spring presets** — side-by-side boxes that bounce with
//!   `gentle` / `stiff` / `wobbly` feel on click. Visualises the
//!   difference between flip-toolkit's canonical presets without
//!   authors having to tweak stiffness/damping by hand.
//! * **Stagger grid** — 12-cell grid that fades in from a
//!   selectable origin (First / Center / Last). Hit "Play" to see
//!   the cascade direction change.
//! * **State animation** — sliders drive transform and depth state;
//!   the target box springs to each new value.
//! * **3D tilt** — pointer-driven card rotation with child depth
//!   layers via `data-pm-tilt`.
//! * **Drag** — a card with momentum on release + soft rectangular
//!   bounds. Release with velocity and watch the spring finish the
//!   motion.

use pine_motion::{
    animate, drag, press, tilt, AnimationHandleSlot, DragAxis, DragConfig, DragConstraints,
    GestureHandleSlot, Origin, Spring, Stagger, TiltConfig,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "AnimationDemo.poco",
    style = "animation.css",
    role = "panel"
)]
pub struct AnimationDemo {
    /// Label of the last spring preset fired — drives the readout
    /// text beside the three spring boxes.
    pub last_spring: String,
    /// Currently-selected stagger origin. One of `"first"`,
    /// `"center"`, `"last"`. Backs the three origin toggle buttons
    /// and is read by `stagger_play` to build the actual `Stagger`
    /// config.
    pub stagger_origin: String,
    /// Motion.dev-style state animation target values.
    pub motion_x: f64,
    pub motion_y: f64,
    pub motion_rotate: f64,
    pub motion_scale: f64,
    pub motion_raise: f64,
    pub motion_shadow: f64,
}

#[handlers]
impl AnimationDemo {
    /// Attach drag listeners once the demo's DOM is walked. Stashes
    /// the returned `GestureHandle` in a thread-local so the
    /// closures stay alive for the page's lifetime. Any previous
    /// handle is replaced (its `Drop` removes the old listeners).
    pub fn on_mount(&mut self) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(Some(el)) = doc.query_selector(".pm-drag-card") else {
            return;
        };
        let handle = drag(
            &el,
            DragConfig {
                axis: DragAxis::Both,
                constraints: Some(DragConstraints::rect(-180.0, 180.0, -80.0, 80.0)),
                momentum: true,
                snap_to_origin: false,
                release_spring: Spring::gentle(),
            },
        );
        DRAG_HANDLE.with(|slot| {
            *slot.borrow_mut() = Some(handle);
        });

        let press_el = el.clone();
        let handle = press(&el, move |_| {
            animate_card_depth(
                &press_el,
                CARD_REST_SHADOW,
                CARD_PRESSED_SHADOW,
                CARD_REST_FILTER,
                CARD_PRESSED_FILTER,
                Spring::stiff(),
            );
            let release_el = press_el.clone();
            Some(Box::new(move |_, _| {
                animate_card_depth(
                    &release_el,
                    CARD_PRESSED_SHADOW,
                    CARD_REST_SHADOW,
                    CARD_PRESSED_FILTER,
                    CARD_REST_FILTER,
                    Spring::gentle(),
                );
            }))
        });
        PRESS_HANDLE.with(|slot| {
            *slot.borrow_mut() = Some(handle);
        });

        if let Ok(Some(card)) = doc.query_selector(".pm-tilt-card") {
            let handle = tilt(
                &card,
                TiltConfig {
                    divisor: 18.0,
                    ..Default::default()
                },
            );
            TILT_HANDLE.with(|slot| {
                *slot.borrow_mut() = Some(handle);
            });
        }
    }

    pub fn pop_gentle(&mut self) {
        self.last_spring = "gentle".into();
        pop_box(".pm-spring-box.gentle", Spring::gentle());
    }

    pub fn pop_stiff(&mut self) {
        self.last_spring = "stiff".into();
        pop_box(".pm-spring-box.stiff", Spring::stiff());
    }

    pub fn pop_wobbly(&mut self) {
        self.last_spring = "wobbly".into();
        pop_box(".pm-spring-box.wobbly", Spring::wobbly());
    }

    pub fn stagger_play(&mut self) {
        let origin = match self.stagger_origin.as_str() {
            "center" => Origin::Center,
            "last" => Origin::Last,
            _ => Origin::First,
        };
        let stagger = Stagger::new(60.0).from(origin);
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(nodes) = doc.query_selector_all(".pm-stagger-cell") else {
            return;
        };
        let total = nodes.length() as usize;
        for i in 0..total {
            let Some(node) = nodes.item(i as u32) else {
                continue;
            };
            let Ok(el) = node.dyn_into::<web_sys::Element>() else {
                continue;
            };
            let delay = stagger.delay_for(i, total);
            let timing = pine_motion::Tween::new()
                .duration(420.0)
                .easing(pine_motion::Easing::APPLE)
                .delay(delay);
            animate(
                &el,
                &[
                    ("opacity", "0", "1"),
                    (
                        "transform",
                        "translateY(16px) scale(0.9)",
                        "translateY(0) scale(1)",
                    ),
                ],
                timing,
            );
        }
    }

    pub fn set_origin_first(&mut self) {
        self.stagger_origin = "first".into();
    }
    pub fn set_origin_center(&mut self) {
        self.stagger_origin = "center".into();
    }
    pub fn set_origin_last(&mut self) {
        self.stagger_origin = "last".into();
    }

    #[watch(motion_x)]
    fn on_motion_x(&self, _: f64, prev: Option<f64>) {
        let from = self.state_transform_with(
            prev.unwrap_or(self.motion_x),
            self.motion_y,
            self.motion_rotate,
            self.motion_scale,
            self.motion_raise,
        );
        self.animate_state_box(from, None);
    }

    #[watch(motion_y)]
    fn on_motion_y(&self, _: f64, prev: Option<f64>) {
        let from = self.state_transform_with(
            self.motion_x,
            prev.unwrap_or(self.motion_y),
            self.motion_rotate,
            self.motion_scale,
            self.motion_raise,
        );
        self.animate_state_box(from, None);
    }

    #[watch(motion_rotate)]
    fn on_motion_rotate(&self, _: f64, prev: Option<f64>) {
        let from = self.state_transform_with(
            self.motion_x,
            self.motion_y,
            prev.unwrap_or(self.motion_rotate),
            self.motion_scale,
            self.motion_raise,
        );
        self.animate_state_box(from, None);
    }

    #[watch(motion_scale)]
    fn on_motion_scale(&self, _: f64, prev: Option<f64>) {
        let from = self.state_transform_with(
            self.motion_x,
            self.motion_y,
            self.motion_rotate,
            prev.unwrap_or(self.motion_scale),
            self.motion_raise,
        );
        self.animate_state_box(from, None);
    }

    #[watch(motion_raise)]
    fn on_motion_raise(&self, _: f64, prev: Option<f64>) {
        let from = self.state_transform_with(
            self.motion_x,
            self.motion_y,
            self.motion_rotate,
            self.motion_scale,
            prev.unwrap_or(self.motion_raise),
        );
        self.animate_state_box(from, None);
    }

    #[watch(motion_shadow)]
    fn on_motion_shadow(&self, _: f64, prev: Option<f64>) {
        let from_transform = self.state_transform();
        let from_shadow = state_shadow(prev.unwrap_or(self.motion_shadow));
        self.animate_state_box(from_transform, Some(from_shadow));
    }

    pub fn reset_state_motion(&mut self) {
        let from_transform = self.state_transform();
        let from_shadow = state_shadow(self.motion_shadow);
        self.motion_x = 0.0;
        self.motion_y = 0.0;
        self.motion_rotate = 0.0;
        self.motion_scale = 1.0;
        self.motion_raise = 0.0;
        self.motion_shadow = 16.0;
        self.animate_state_box(from_transform, Some(from_shadow));
    }

    pub fn state_pop(&mut self) {
        let from_transform = self.state_transform();
        let from_shadow = state_shadow(self.motion_shadow);
        self.motion_scale = 1.12;
        self.motion_raise = 18.0;
        self.motion_shadow = 32.0;
        self.animate_state_box(from_transform, Some(from_shadow));
    }

    fn state_transform(&self) -> String {
        self.state_transform_with(
            self.motion_x,
            self.motion_y,
            self.motion_rotate,
            self.motion_scale,
            self.motion_raise,
        )
    }

    fn state_transform_with(&self, x: f64, y: f64, rotate: f64, scale: f64, raise: f64) -> String {
        state_transform(x, y, rotate, effective_scale(scale), raise)
    }

    fn animate_state_box(&self, from_transform: String, from_shadow: Option<String>) {
        let to_transform = self.state_transform();
        let from_shadow = from_shadow.unwrap_or_else(|| state_shadow(self.motion_shadow));
        let to_shadow = state_shadow(self.motion_shadow);
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(Some(el)) = doc.query_selector(".pm-state-box") else {
            return;
        };
        STATE_ANIMATION_HANDLE.with(|slot| {
            if let Some(handle) = slot.borrow_mut().take() {
                handle.cancel();
            }
            let handle = animate(
                &el,
                &[
                    ("transform", &from_transform, &to_transform),
                    ("boxShadow", &from_shadow, &to_shadow),
                ],
                Spring::wobbly(),
            );
            *slot.borrow_mut() = Some(handle);
        });
    }

    pub fn on_setup(&mut self) {
        if self.motion_scale == 0.0 {
            self.motion_scale = 1.0;
        }
        if self.motion_shadow == 0.0 {
            self.motion_shadow = 16.0;
        }
    }
}

thread_local! {
    /// Held for the lifetime of the page so the drag listeners stay
    /// alive. Mount replaces the prior handle (if any) which drops
    /// the prior closures via `GestureHandle::drop`.
    static DRAG_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static PRESS_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static TILT_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static STATE_ANIMATION_HANDLE: AnimationHandleSlot = AnimationHandleSlot::default();
}

const CARD_REST_SHADOW: &str = "0 14px 26px rgba(0, 0, 0, 0.18)";
const CARD_PRESSED_SHADOW: &str =
    "0 7px 12px rgba(0, 0, 0, 0.20), inset 0 2px 6px rgba(255, 255, 255, 0.35)";
const CARD_REST_FILTER: &str = "brightness(1)";
const CARD_PRESSED_FILTER: &str = "brightness(0.96)";

fn animate_card_depth(
    el: &web_sys::Element,
    from_shadow: &str,
    to_shadow: &str,
    from_filter: &str,
    to_filter: &str,
    spring: Spring,
) {
    animate(
        el,
        &[
            ("boxShadow", from_shadow, to_shadow),
            ("filter", from_filter, to_filter),
        ],
        spring,
    );
}

fn state_transform(x: f64, y: f64, rotate: f64, scale: f64, raise: f64) -> String {
    format!(
        "translate({x}px, {}px) rotate({rotate}deg) scale({scale})",
        y - raise
    )
}

fn effective_scale(scale: f64) -> f64 {
    if scale <= 0.0 {
        1.0
    } else {
        scale
    }
}

fn state_shadow(depth: f64) -> String {
    let depth = depth.clamp(0.0, 48.0);
    format!("0 {}px {}px rgba(0, 0, 0, 0.16)", depth * 0.75, depth * 1.8)
}

/// Scale-pop one element with the given spring. `translate + scale`
/// so the pop visually bounces without layout-shift; `linear(...)`
/// easing from the spring runs the whole thing on the compositor.
fn pop_box(selector: &str, spring: Spring) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(Some(el)) = doc.query_selector(selector) else {
        return;
    };
    animate(&el, &[("transform", "scale(0.6)", "scale(1)")], spring);
}
