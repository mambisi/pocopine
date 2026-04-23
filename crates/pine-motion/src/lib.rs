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
pub mod easing;
pub mod gesture;
pub mod spring;
pub mod stagger;

pub use animate::{animate, Channel, IntoTiming, Tween};
pub use easing::{sample_to_linear_easing, Easing};
pub use gesture::{focus, hover, press, GestureHandle, PressEndHandler};
pub use spring::Spring;
pub use stagger::{stagger, Origin, Stagger};
