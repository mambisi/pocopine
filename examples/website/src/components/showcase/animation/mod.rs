//! `pine-motion` showcase — three sub-demos exercising the library's
//! headline features:
//!
//! * **Spring presets** — side-by-side boxes that bounce with
//!   `gentle` / `stiff` / `wobbly` feel on click. Visualises the
//!   difference between flip-toolkit's canonical presets without
//!   authors having to tweak stiffness/damping by hand.
//! * **Stagger grid** — 12-cell grid that fades in from a
//!   selectable origin (First / Center / Last). Hit "Play" to see
//!   the cascade direction change.
//! * **Drag** — a card with momentum on release + soft rectangular
//!   bounds. Release with velocity and watch the spring finish the
//!   motion.

use pine_motion::{animate, drag, DragAxis, DragConfig, DragConstraints, Origin, Spring, Stagger};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AnimationDemo.poco", style = "animation.css", role = "panel")]
pub struct AnimationDemo {
    /// Label of the last spring preset fired — drives the readout
    /// text beside the three spring boxes.
    pub last_spring: String,
    /// Currently-selected stagger origin. One of `"first"`,
    /// `"center"`, `"last"`. Backs the three origin toggle buttons
    /// and is read by `stagger_play` to build the actual `Stagger`
    /// config.
    pub stagger_origin: String,
}

#[handlers]
impl AnimationDemo {
    /// Attach drag listeners once the demo's DOM is walked. Stashes
    /// the returned `GestureHandle` in a thread-local so the
    /// closures stay alive for the page's lifetime. Any previous
    /// handle is replaced (its `Drop` removes the old listeners).
    pub fn on_mount(&mut self) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        let Ok(Some(el)) = doc.query_selector(".pm-drag-card") else { return };
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
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        let Ok(nodes) = doc.query_selector_all(".pm-stagger-cell") else { return };
        let total = nodes.length() as usize;
        for i in 0..total {
            let Some(node) = nodes.item(i as u32) else { continue };
            let Ok(el) = node.dyn_into::<web_sys::Element>() else { continue };
            let delay = stagger.delay_for(i, total);
            let timing = pine_motion::Tween::new()
                .duration(420.0)
                .easing(pine_motion::Easing::APPLE)
                .delay(delay);
            animate(
                &el,
                &[
                    ("opacity", "0", "1"),
                    ("transform", "translateY(16px) scale(0.9)", "translateY(0) scale(1)"),
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

}

thread_local! {
    /// Held for the lifetime of the page so the drag listeners stay
    /// alive. Mount replaces the prior handle (if any) which drops
    /// the prior closures via `GestureHandle::drop`.
    static DRAG_HANDLE: std::cell::RefCell<Option<pine_motion::GestureHandle>> =
        std::cell::RefCell::new(None);
}

/// Scale-pop one element with the given spring. `translate + scale`
/// so the pop visually bounces without layout-shift; `linear(...)`
/// easing from the spring runs the whole thing on the compositor.
fn pop_box(selector: &str, spring: Spring) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(Some(el)) = doc.query_selector(selector) else { return };
    animate(
        &el,
        &[
            ("transform", "scale(0.6)", "scale(1)"),
        ],
        spring,
    );
}
