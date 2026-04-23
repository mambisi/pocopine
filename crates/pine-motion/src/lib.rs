//! `pine-motion` — a Motion.dev-style animation library for pocopine.
//!
//! Built on top of [`pocopine_core::animate`]'s WAAPI primitives.
//! Adds spring physics (via linear-easing sampling, runs on the GPU
//! compositor), named easing presets, and ergonomic entry points.
//!
//! The v0.1 surface is intentionally small:
//!
//! ```ignore
//! use pine_motion::{animate, Spring, Tween, Easing};
//!
//! // Tween
//! animate(&el, &[("opacity", "0", "1")], Tween::new().duration(300));
//!
//! // Spring — sampled into `linear(...)`, GPU-accelerated
//! animate(&el, &[("transform", "scale(0.8)", "scale(1)")], Spring::gentle());
//!
//! // Named easing
//! animate(&el, &[("opacity", "0", "1")], Easing::APPLE);
//! ```
//!
//! Upcoming modules (later PRs): `stagger`, `gesture`, `drag`,
//! `scroll`, `projection`.

pub mod animate;
pub mod drag;
pub mod easing;
pub mod effects;
pub mod gesture;
pub mod projection;
pub mod scroll;
pub mod spring;
pub mod stagger;
pub mod tilt;

pub use animate::{animate, AnimationHandle, Channel, IntoTiming, Tween};
pub use drag::{drag, DragAxis, DragConfig, DragConstraints};
pub use easing::{sample_to_linear_easing, Easing};
pub use effects::{hover_motion, raise, scale, HoverMotion, HoverMotionConfig};
pub use gesture::{focus, hover, pan, press, GestureHandle, PanConfig, PanEvent, PressEndHandler};
pub use projection::{play_layout, project_with, snapshot_layout, LayoutRect, LayoutSnapshot};
pub use scroll::{on_view, scroll_progress, ScrollHandle, ViewConfig};
pub use spring::Spring;
pub use stagger::{stagger, Origin, Stagger};
pub use tilt::{tilt, TiltConfig};

/// Thread-local slot for handles whose lifetime must outlive the
/// setup call that installed them.
pub type HandleSlot<T> = std::cell::RefCell<Option<T>>;

/// Common slot for gesture listener handles.
pub type GestureHandleSlot = HandleSlot<GestureHandle>;

/// Common slot for imperative WAAPI animation handles.
pub type AnimationHandleSlot = HandleSlot<AnimationHandle>;
