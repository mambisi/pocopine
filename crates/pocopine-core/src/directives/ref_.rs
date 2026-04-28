//! `pp-ref="<name>"` — pin the element under its enclosing scope so
//! Rust handlers can reach it via [`crate::refs`].
//!
//! `pp-ref` is registered exclusively through the macro-emitted
//! template plan (`StaticRef` entries on `apply_static_plan`); this
//! module no longer carries a runtime install entry. The file is
//! retained so the directive namespace stays consistent with
//! related references (`#[deprecated]` removal lives in a follow-up).
