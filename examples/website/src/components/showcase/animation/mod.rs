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
    animate, drag, focus, on_view, pan, play_layout, press, raise, scroll_progress,
    snapshot_layout, tilt, AnimationHandleSlot, DragAxis, DragConfig, DragConstraints, Easing,
    GestureHandleSlot, Origin, PanConfig, PanEvent, ScrollHandle, Spring, Stagger, TiltConfig,
    Tween, ViewConfig,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
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
    /// Label of the last easing preset fired.
    pub last_easing: String,
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
    /// When set (e.g. `"springs"`), render only that subsection — the
    /// per-technique motion pages embed `<animation-demo only="…">`.
    /// Empty shows the full combined demo (the default elsewhere).
    #[prop]
    pub only: String,
    /// Live readout for the pan demo.
    pub pan_delta: String,
    pub pan_velocity: String,
    pub show_intro: bool,
    pub show_springs: bool,
    pub show_easing: bool,
    pub show_stagger: bool,
    pub show_state: bool,
    pub show_hover: bool,
    pub show_focus: bool,
    pub show_press: bool,
    pub show_pan: bool,
    pub show_tilt: bool,
    pub show_drag: bool,
    pub show_scroll: bool,
    pub show_inview: bool,
    pub show_flip: bool,
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

        // Each demo subsection is wired independently (`if let`), so a
        // single-technique page that only renders one of them still works.
        if let Ok(Some(el)) = doc.query_selector(".pm-drag-card") {
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
            DRAG_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));

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
            PRESS_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        if let Ok(Some(card)) = doc.query_selector(".pm-tilt-card") {
            let handle = tilt(
                &card,
                TiltConfig {
                    divisor: 18.0,
                    ..Default::default()
                },
            );
            TILT_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        // Hover card — lift + shadow on hover via the `raise` preset.
        if let Ok(Some(el)) = doc.query_selector(".pm-hover-card") {
            let handle = raise(&el);
            HOVER_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        // Focus input — spring a focus ring in on focus, out on blur.
        if let Ok(Some(el)) = doc.query_selector(".pm-focus-input") {
            let target = el.clone();
            let handle = focus(&el, move |focused, _| {
                let (from, to) = if focused {
                    (
                        "0 0 0 0 rgba(59,130,246,0)",
                        "0 0 0 3px rgba(59,130,246,0.45)",
                    )
                } else {
                    (
                        "0 0 0 3px rgba(59,130,246,0.45)",
                        "0 0 0 0 rgba(59,130,246,0)",
                    )
                };
                animate(&target, &[("boxShadow", from, to)], Spring::gentle());
            });
            FOCUS_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        // Press button — spring down on press, bounce back on release.
        if let Ok(Some(el)) = doc.query_selector(".pm-press-btn") {
            let down = el.clone();
            let handle = press(&el, move |_| {
                animate(
                    &down,
                    &[("transform", "scale(1)", "scale(0.92)")],
                    Spring::stiff(),
                );
                let up = down.clone();
                Some(Box::new(move |_, _| {
                    animate(
                        &up,
                        &[("transform", "scale(0.92)", "scale(1)")],
                        Spring::wobbly(),
                    );
                }))
            });
            PRESS_BTN_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        // Pan surface — report live delta + velocity into the readout.
        if let Ok(Some(el)) = doc.query_selector(".pm-pan-zone") {
            let to_self = this::<AnimationDemo>();
            let handle = pan(&el, PanConfig::default(), move |ev| match ev {
                PanEvent::Move { delta, velocity } => {
                    to_self.update(|s: &mut AnimationDemo| {
                        s.pan_delta = format!("{:.0}, {:.0}", delta.0, delta.1);
                        s.pan_velocity = format!("{:.0}, {:.0}", velocity.0, velocity.1);
                    });
                }
                PanEvent::End { .. } | PanEvent::Cancel => {
                    to_self.update(|s: &mut AnimationDemo| s.pan_velocity = "0, 0".into());
                }
                _ => {}
            });
            PAN_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        // Scroll-progress bar — fills as the page scrolls.
        if doc
            .query_selector(".pm-scroll-fill")
            .ok()
            .flatten()
            .is_some()
        {
            let handle = scroll_progress(|progress| {
                if let Some(doc) = web_sys::window().and_then(|w| w.document())
                    && let Ok(Some(fill)) = doc.query_selector(".pm-scroll-fill") {
                        let _ = fill.set_attribute(
                            "style",
                            &format!("width:{}%", (progress * 100.0).round()),
                        );
                    }
            });
            SCROLL_PROGRESS_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }

        // In-view reveal — fade + rise when the box enters the viewport.
        if let Ok(Some(el)) = doc.query_selector(".pm-inview-box") {
            let target = el.clone();
            let handle = on_view(
                &el,
                ViewConfig {
                    threshold: 0.25,
                    once: true,
                },
                move |shown| {
                    if shown {
                        animate(
                            &target,
                            &[
                                ("opacity", "0", "1"),
                                ("transform", "translateY(24px)", "translateY(0px)"),
                            ],
                            Spring::gentle(),
                        );
                    }
                },
            );
            INVIEW_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
        }
    }

    /// FLIP reorder: snapshot positions, move the first item to the end
    /// of the DOM directly (no reactive list, so node identity is kept),
    /// then play the delta — each item springs from old to new position.
    pub fn flip_shuffle(&mut self) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(Some(root)) = doc.query_selector(".pm-flip-list") else {
            return;
        };
        let snapshot = snapshot_layout(&root);
        if let Some(first) = root.first_element_child() {
            let _ = root.append_child(&first);
        }
        play_layout(&root, snapshot, Spring::gentle());
    }

    pub fn easing_quad(&mut self) {
        self.last_easing = "EASE_OUT_QUAD".into();
        animate_easing_box(Easing::EASE_OUT_QUAD);
    }
    pub fn easing_expo(&mut self) {
        self.last_easing = "EASE_OUT_EXPO".into();
        animate_easing_box(Easing::EASE_OUT_EXPO);
    }
    pub fn easing_back(&mut self) {
        self.last_easing = "EASE_OUT_BACK".into();
        animate_easing_box(Easing::EASE_OUT_BACK);
    }
    pub fn easing_circ(&mut self) {
        self.last_easing = "EASE_OUT_CIRC".into();
        animate_easing_box(Easing::EASE_OUT_CIRC);
    }
    pub fn easing_apple(&mut self) {
        self.last_easing = "APPLE".into();
        animate_easing_box(Easing::APPLE);
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
        // `only` empty → the full combined demo; otherwise just one
        // subsection (the per-technique motion pages set this).
        let all = self.only.is_empty();
        self.show_intro = all;
        self.show_springs = all || self.only == "springs";
        self.show_easing = all || self.only == "easing";
        self.show_stagger = all || self.only == "stagger";
        self.show_state = all || self.only == "state";
        self.show_hover = all || self.only == "hover";
        self.show_focus = all || self.only == "focus";
        self.show_press = all || self.only == "press";
        self.show_pan = all || self.only == "pan";
        self.show_tilt = all || self.only == "tilt";
        self.show_drag = all || self.only == "drag";
        self.show_scroll = all || self.only == "scroll";
        self.show_inview = all || self.only == "inview";
        self.show_flip = all || self.only == "flip";
    }
}

thread_local! {
    /// Held for the lifetime of the page so the drag listeners stay
    /// alive. Mount replaces the prior handle (if any) which drops
    /// the prior closures via `GestureHandle::drop`.
    static DRAG_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static PRESS_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static TILT_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static HOVER_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static FOCUS_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static PRESS_BTN_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static PAN_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static SCROLL_PROGRESS_HANDLE: GestureHandleSlot = GestureHandleSlot::default();
    static INVIEW_HANDLE: RefCell<Option<ScrollHandle>> = const { RefCell::new(None) };
    static STATE_ANIMATION_HANDLE: AnimationHandleSlot = AnimationHandleSlot::default();
}

/// Send the easing-demo box across its track with the given easing so
/// the curve is visible. Reverts to the start (no fill) for the next click.
fn animate_easing_box(easing: Easing) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(Some(el)) = doc.query_selector(".pm-easing-box") else {
        return;
    };
    animate(
        &el,
        &[("transform", "translateX(0)", "translateX(240px)")],
        Tween::new().duration(700.0).easing(easing),
    );
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
