//! First-class motion for pocopine.
//!
//! Four pieces, each imperative and directly usable:
//!
//! - [`animate`] — Web Animations API wrapper. Keyframes + options in,
//!   [`AnimationHandle`] out. The escape hatch for custom motion.
//! - [`apply_preset`] — stamp a named preset's `pp-transition:*` attrs
//!   on an element so the existing `pp-transition` state machine
//!   animates it on `pp-if` / `pp-show` transitions. Declarative.
//!   Built-in presets: `fade`, `scale`, `fade-scale`, `zoom`,
//!   `slide-up` / `slide-down` / `slide-left` / `slide-right`,
//!   `collapse`, `none`.
//! - [`flip_from_snapshot`] — FLIP layout animation given a
//!   before-rect. Used by the `animate = "flip"` macro integration
//!   in pp-for's keyed reconcile; authors call it directly when they
//!   move an element themselves.
//! - [`collapse_to`] — auto-height expand / collapse (the `collapse`
//!   preset dispatches through here at runtime).
//!
//! ## Author declaration via `#[component]`
//!
//! The macro forwards `transition = "…"` / `transition_in = "…"` /
//! `transition_out = "…"` / `animate = "flip"` to the generated
//! `on_setup` so a component's rendered root gets the right preset
//! stamped without the author writing any glue. See
//! `pocopine-macros` and RFC-038.
//!
//! ## Author usage at the call site
//!
//! Per-instance overrides go through the `transition` / `transition-in`
//! / `transition-out` HTML attributes on the Pine primitive's tag:
//!
//! ```html
//! <pine-dialog-content transition="slide-up">...</pine-dialog-content>
//! <pine-tooltip-content transition-in="scale" transition-out="fade">...</pine-tooltip-content>
//! <pine-popover-content transition="none">...</pine-popover-content>
//! ```

pub mod collapse;
pub mod flip;
pub mod motion;
pub mod presets;
pub mod waapi;

// Flat re-exports — authors import from `pocopine::animate::*`.
pub use collapse::{collapse_to, CollapseOptions};
pub use flip::{flip, flip_from_snapshot, FlipOptions};
pub use motion::{is_reduced as motion_is_reduced, MotionPreference};
pub use presets::{apply_preset, lookup, register_preset, Phase, Preset};
pub use waapi::{animate, AnimateOptions, AnimationHandle, Keyframe};

/// Current motion preference snapshot. Convenience re-export so
/// authors don't have to know the submodule path.
pub fn motion_preference() -> MotionPreference {
    motion::current()
}

/// Boot-time installation — inject the preset atom stylesheet and
/// start the motion-preference media-query watcher. Called from
/// `App::run`. Idempotent.
pub fn install() {
    presets::inject_atoms_stylesheet();
    motion::install();
}

/// Re-export of [`crate::directives::transition::enter_subtree_staggered`]
/// so authors can reach for stagger from the same module they
/// already import for `apply_preset` / `animate`.
pub fn enter_subtree_staggered<F: FnOnce() + 'static>(
    root: &web_sys::Element,
    stagger_ms: u32,
    on_done: F,
) {
    crate::directives::transition::enter_subtree_staggered(root, stagger_ms, on_done);
}

/// Disable RFC-038 transitions globally. Intended for tests that
/// were written before primitives gained default mount/unmount
/// transitions and assert post-toggle DOM state synchronously. With
/// this on, `enter` / `leave` invoke their `on_done` callback right
/// away — no CSS class swap, no waiting for transition-end.
pub fn disable_transitions() {
    crate::directives::transition::set_disabled(true);
}

/// Re-enable transitions after a previous `disable_transitions()`.
pub fn enable_transitions() {
    crate::directives::transition::set_disabled(false);
}
