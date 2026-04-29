//! Directive install entries.
//!
//! RFC-058 Phase 6.5 retired the runtime mount, the
//! `DirectiveCall`-based dispatch loop, and the directive registry.
//! Every directive is now reached through a typed `install` entry
//! invoked by the macro-emitted `apply_static_plan` path; this
//! module is just the namespace that groups those entries together.

pub mod anchor;
pub mod bind;
pub mod flip;
pub mod for_;
pub mod for_plan;
pub mod html;
pub mod if_;
pub mod interp;
pub mod intersect;
pub mod model;
pub mod on;
pub mod ref_;
pub mod resize;
pub mod roving;
pub mod show;
pub mod teleport;
pub mod text;
pub mod transition;
