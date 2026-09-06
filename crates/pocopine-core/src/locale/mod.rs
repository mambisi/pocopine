//! Locale values are explicit on every target. Only the browser has a
//! committed UI language; host requests and recipient jobs pass their locale.

pub use pocopine_locale::*;
#[doc(hidden)]
pub mod template;

#[cfg(target_arch = "wasm32")]
pub mod client;
