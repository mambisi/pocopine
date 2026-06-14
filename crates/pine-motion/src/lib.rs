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

pub use animate::{AnimationHandle, Channel, IntoTiming, Tween, animate};
pub use drag::{DragAxis, DragConfig, DragConstraints, drag};
pub use easing::{Easing, sample_to_linear_easing};
pub use effects::{HoverMotion, HoverMotionConfig, hover_motion, raise, scale};
pub use gesture::{GestureHandle, PanConfig, PanEvent, PressEndHandler, focus, hover, pan, press};
pub use projection::{LayoutRect, LayoutSnapshot, play_layout, project_with, snapshot_layout};
pub use scroll::{ScrollHandle, ViewConfig, on_view, scroll_progress};
pub use spring::Spring;
pub use stagger::{Origin, Stagger, stagger};
pub use tilt::{TiltConfig, tilt};

/// Thread-local slot for handles whose lifetime must outlive the
/// setup call that installed them.
pub type HandleSlot<T> = std::cell::RefCell<Option<T>>;

/// Common slot for gesture listener handles.
pub type GestureHandleSlot = HandleSlot<GestureHandle>;

/// Common slot for imperative WAAPI animation handles.
pub type AnimationHandleSlot = HandleSlot<AnimationHandle>;
