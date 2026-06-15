//! `pine-layout` showcase — a responsive application shell that adapts
//! its navigation (drawer → rail → sidebar) as the viewport grows.
//!
//! `pine-layout` ships **zero CSS**: the look lives in `styles.css`,
//! keyed off the `data-nav-mode` / `data-state` / `data-breakpoint`
//! hooks the components emit. The drawer slide is animated with
//! **`pine-motion`** — imperatively, only on toggle — so resizing
//! *into* drawer width just snaps (no CSS transition is armed, so
//! there's nothing to flash). Resize the window to watch it reflow.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(display = "contents")]
pub struct LayoutDemo {
    /// Live breakpoint, two-way bound from `<pine-app-shell>` via
    /// `pp-model:breakpoint`. Drives the corner badge.
    pub bp: String,
    /// Drawer open state, two-way bound via `pp-model:nav_open`. The
    /// `#[watch]` below animates the slide whenever it flips.
    pub nav_open: bool,
}

#[handlers]
impl LayoutDemo {
    /// Animate the drawer with pine-motion on every open/close. We do
    /// this imperatively (rather than a CSS `transition`) precisely so
    /// a breakpoint-driven reflow into drawer mode doesn't animate —
    /// only a real user toggle does.
    #[watch(nav_open)]
    fn on_nav_open(&mut self, open: bool, old: Option<bool>) {
        // Skip the initial seed — only animate genuine toggles.
        if old.is_some() {
            slide_drawer(open);
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn slide_drawer(open: bool) {
    use pine_motion::{AnimationHandle, Tween, animate};
    use std::cell::RefCell;

    // Retain the latest handle past this call (dropping it would cancel
    // the WAAPI animation). One drawer in this demo → one slot.
    thread_local! {
        static SLIDE: RefCell<Option<AnimationHandle>> = const { RefCell::new(None) };
    }

    let Some(sidebar) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(".pine-app-shell-sidebar").ok().flatten())
    else {
        return;
    };
    // Only the off-canvas drawer slides; the rail / sidebar are static.
    if sidebar.get_attribute("data-nav-mode").as_deref() != Some("drawer") {
        return;
    }

    let (from, to) = if open {
        ("translateX(-100%)", "translateX(0)")
    } else {
        ("translateX(0)", "translateX(-100%)")
    };
    // A plain ease-out — smooth in, no spring overshoot.
    let handle = animate(
        &sidebar,
        &[("transform", from, to)],
        Tween::new().duration(240.0).ease_out(),
    );
    // pine-motion runs with `fill: forwards`, which would keep overriding
    // the element's transform forever — even after a breakpoint reflow
    // turns the sidebar back into an in-grid rail/sidebar (leaving it
    // stuck off-canvas). Cancel the WAAPI animation once it finishes so
    // the element falls back to its CSS (data-state-driven) transform in
    // every mode. CSS already holds the matching end value, so there's no
    // visible jump.
    let anim = handle.raw().clone();
    handle.on_finish(move || anim.cancel());
    SLIDE.with(|s| *s.borrow_mut() = Some(handle));
}

#[cfg(not(target_arch = "wasm32"))]
fn slide_drawer(_open: bool) {}

#[wasm_bindgen(start)]
pub fn main() {
    pine_layout::register_all();
    App::new().register::<LayoutDemo>().run();
}
