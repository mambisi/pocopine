//! pocopine-core — client-side reactive runtime.
//!
//! Pure compiled-views runtime (RFC-058 Phase 6.5). Every `.poco`
//! template lifts via `#[component]` into a static template plan;
//! mounting walks that plan instead of scanning the DOM for `pp-*`
//! directives. Reactivity is implemented against a real
//! `js_sys::Proxy` so `get`/`set` traps match Alpine-style semantics
//! for dependency tracking; signals, computed values, and watchers
//! compose with the same engine via a synthetic
//! [`reactive::SIGNAL_SCOPE`].

pub mod animate;
pub mod app;
pub mod component_computed;
pub mod computed;
pub mod context;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod directives;
pub mod dom;
pub mod emit;
pub mod events;
pub mod expr;
pub mod extractors;
pub mod fetch;
pub mod focus;
pub mod handle;
pub mod handler;
pub mod id;
pub mod lifecycle;
pub mod loop_scope;
pub mod magics;
pub mod model_runtime;
pub mod path;
pub mod profiler;
pub mod reactive;
pub mod refs;
pub mod registry;
pub mod router;
pub mod scope;
pub mod scroll_lock;
pub mod server;
pub mod signal;
pub mod slot_fragment;
pub mod slot_scope;
#[deprecated(note = "compiled-mount-only — see RFC 061. \
                     Slot content flows through `slot_fragment::*` \
                     compiled fragments; this runtime store retires in Phase 3.")]
pub mod slots;
pub mod store;
pub mod styles;
pub mod task;
pub mod templates;
pub mod templates_plan;
pub mod text;
pub mod tick;
pub mod timers;
pub mod walker;
pub mod watch;

pub use app::{App, Component, SubtreeHandle};
pub use computed::{computed, Computed};
#[allow(deprecated)]
pub use context::InjectKey;
pub use context::{inject, provide, ContextKey, ContextMarker};
pub use directives::for_plan::{
    register_row_plans, BindingKind, StaticBinding, StaticListener, StaticRowPlan,
};
pub use emit::{
    emit, emit_cancelable, emit_cancelable_from, emit_event, emit_event_from, emit_from,
    emit_from_host, emit_model, emit_model_field, emit_raw, emit_raw_from, Emit,
};
pub use events::{on_scope_unmount, DomEventName, ListenerHandle};
pub use extractors::{Inject, NearestParent, Parent};
pub use handle::{this, Handle};
pub use handler::{FromHandlerArg, HandlerDispatch};
pub use lifecycle::{
    Body, Doc, El, Elapsed, HostEl, IsTeleported, LifecycleContext, MountEpoch, ParentId, Refs,
    ScopePath, TagName, TeleportHost, TypedEl, Win,
};
pub use magics::dispatch_event;
pub use model_runtime::{with_write_origin, WriteOrigin};
pub use profiler::mount::{
    enabled as mount_profile_enabled, report as report_mount_profile, reset as reset_mount_profile,
};
pub use reactive::{
    batch, current_effect, effect, effect_scoped, effect_with, flush_sync, on_cleanup, release,
    run_now, set_auto_flush, track, trigger_scope, EffectId, EffectOptions, ScopeId, SignalId,
    SIGNAL_SCOPE,
};
pub use registry::{
    assert_registry_clean, mark_registered, register_component, register_component_as,
    register_component_prefixed, registered_component_names, registry_errors, render_boot_error,
    verify_registry, ComponentCtor, ComponentEntry, ComponentVTable, RegisteredComponent,
    RegistryError, RegistryErrorKind, COMPONENT_ENTRIES,
};
pub use router::{navigate, register_route};
pub use scope::{
    append_list_inline, current_scope_id, invalidate_field, invalidate_field_cache,
    patch_list_at_inline, patch_list_indices_inline, replace_field_inline,
    swap_list_indices_inline, ComponentState, Scope,
};
pub use server::{Result as ServerResult, ServerError};
pub use signal::{rw_signal, signal, RwSignal, Setter, Signal};
pub use store::{register_store_scope, store, store_scope, stores_object, Store, StoreHandle};
pub use styles::inject_style;
pub use task::{spawn, spawn_latest, spawn_scoped, TaskHandle};
pub use templates::{
    compile_template, inject_pp_data, is_registered, register_template, template_for,
};
pub use watch::{
    watch, watch_field, watch_field_scoped, watch_scope_field, watch_scope_field_now,
    watch_scope_field_scoped, watch_scoped,
};
