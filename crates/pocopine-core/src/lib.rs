//! pocopine-core — client-side reactive runtime.
//!
//! Port of Alpine.js's reactivity + directive model to Rust/WASM. Reactivity
//! is implemented against a real `js_sys::Proxy` so `get`/`set` traps match
//! upstream semantics for dependency tracking. Directives walk the DOM,
//! register effects, and are torn down through a `MutationObserver`.
//!
//! Signals, computed values, and watchers compose with the same engine via
//! a synthetic [`reactive::SIGNAL_SCOPE`].

pub mod computed;
pub mod directives;
pub mod handler;
pub mod magics;
pub mod reactive;
pub mod registry;
pub mod scope;
pub mod signal;
pub mod styles;
pub mod templates;
pub mod walker;
pub mod watch;

pub use computed::{computed, Computed};
pub use handler::HandlerDispatch;
pub use reactive::{
    batch, current_effect, effect, effect_with, flush_sync, on_cleanup, release, run_now,
    set_auto_flush, EffectId, EffectOptions, ScopeId, SignalId, SIGNAL_SCOPE,
};
pub use registry::{register_component, ComponentCtor, ComponentEntry, COMPONENT_ENTRIES};
pub use scope::{ComponentState, Scope};
pub use signal::{rw_signal, signal, RwSignal, Setter, Signal};
pub use styles::inject_style;
pub use templates::{inject_pp_data, is_registered, register_template, template_for};
pub use walker::{start, start_on_body};
pub use watch::watch;

/// Convenience re-export alias so `pocopine_core::run()` reads well.
pub fn run() {
    walker::start_on_body();
}
