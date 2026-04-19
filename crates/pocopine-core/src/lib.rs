//! pocopine-core — client-side reactive runtime.
//!
//! Port of Alpine.js's reactivity + directive model to Rust/WASM. Reactivity
//! is implemented against a real `js_sys::Proxy` so `get`/`set` traps match
//! upstream semantics for dependency tracking. Directives walk the DOM,
//! register effects, and are torn down through a `MutationObserver`.
//!
//! Signals, computed values, and watchers compose with the same engine via
//! a synthetic [`reactive::SIGNAL_SCOPE`].

pub mod app;
pub mod computed;
pub mod devtools;
pub mod directives;
pub mod fetch;
pub mod handle;
pub mod handler;
pub mod loop_scope;
pub mod magics;
pub mod path;
pub mod reactive;
pub mod refs;
pub mod registry;
pub mod router;
pub mod scope;
pub mod server;
pub mod signal;
pub mod store;
pub mod styles;
pub mod templates;
pub mod walker;
pub mod watch;

pub use app::{App, Component};
pub use computed::{computed, Computed};
pub use handle::{this, Handle};
pub use handler::HandlerDispatch;
pub use server::{Result as ServerResult, ServerError};
pub use store::{
    register_store_scope, store, store_scope, stores_object, Store, StoreHandle,
};
pub use reactive::{
    batch, current_effect, effect, effect_with, flush_sync, on_cleanup, release, run_now,
    set_auto_flush, trigger_scope, EffectId, EffectOptions, ScopeId, SignalId, SIGNAL_SCOPE,
};
pub use registry::{register_component, ComponentCtor, ComponentEntry, COMPONENT_ENTRIES};
pub use router::{navigate, register_route};
pub use scope::{current_scope_id, ComponentState, Scope};
pub use signal::{rw_signal, signal, RwSignal, Setter, Signal};
pub use styles::inject_style;
pub use templates::{inject_pp_data, is_registered, register_template, template_for};
pub use walker::{start, start_on_body};
pub use watch::watch;

/// Convenience re-export alias so `pocopine_core::run()` reads well.
pub fn run() {
    walker::start_on_body();
}
