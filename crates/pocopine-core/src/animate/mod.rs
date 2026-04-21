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
pub mod presets;
pub mod waapi;

// Flat re-exports — authors import from `pocopine::animate::*`.
pub use collapse::{collapse_to, CollapseOptions};
pub use flip::{flip, flip_from_snapshot, FlipOptions};
pub use presets::{apply_preset, lookup, register_preset, Phase, Preset};
pub use waapi::{animate, AnimateOptions, AnimationHandle, Keyframe};

/// Boot-time installation — inject the preset atom stylesheet. The
/// runtime calls this from `App::new`/`start` so authors don't have
/// to. Idempotent — safe to call repeatedly.
pub fn install() {
    presets::inject_atoms_stylesheet();
}
