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
pub mod context;
pub mod devtools;
pub mod directives;
pub mod emit;
pub mod expr;
pub mod fetch;
pub mod focus;
pub mod handle;
pub mod handler;
pub mod id;
pub mod loop_scope;
pub mod magics;
pub mod slot_scope;
pub mod slots;
pub mod path;
pub mod lifecycle;
pub mod reactive;
pub mod refs;
pub mod registry;
pub mod router;
pub mod scope;
pub mod scroll_lock;
pub mod server;
pub mod signal;
pub mod store;
pub mod styles;
pub mod templates;
pub mod tick;
pub mod walker;
pub mod watch;

pub use app::{App, Component};
pub use computed::{computed, Computed};
pub use context::{inject, provide, InjectKey};
pub use emit::{
    emit, emit_cancelable, emit_cancelable_from, emit_from, emit_from_host,
};
pub use handle::{this, Handle};
pub use handler::{FromHandlerArg, HandlerDispatch};
pub use lifecycle::{
    Body, Doc, El, Elapsed, HostEl, IsTeleported, LifecycleContext, MountEpoch, ParentId,
    Refs, ScopePath, Slots, TagName, TeleportHost, TypedEl, Win,
};
pub use magics::dispatch_event;
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
pub use templates::{
    compile_template, inject_pp_data, is_registered, register_template, template_for,
};
pub use walker::{start, start_on_body};
pub use watch::{watch, watch_field, watch_scope_field};

/// Convenience re-export alias so `pocopine_core::run()` reads well.
pub fn run() {
    walker::start_on_body();
}
