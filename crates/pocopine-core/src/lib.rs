//! pocopine-core — client-side reactive runtime.
//!
//! Pure compiled-views runtime (RFC-058 Phase 6.5). Every `.poco`
//! template lifts via `#[component]` into a static template plan;
//! mounting walks that plan instead of scanning the DOM for `pp-*`
//! directives. Reactivity is implemented against a real
//! signals-first reactive engine (RFC-095/096): component fields
//! are interned signals with versioned projections; reads and
//! writes resolve through the scoped access; signals, computed
//! values, and watchers compose on the same u64-keyed graph. The
//! JS `Proxy` survives only as the explicit [`scope::js_bridge`]
//! interop shim.

pub mod animate;
pub mod app;
// RFC-100 — content-addressed asset URLs.
pub mod assets;
pub mod client_module;
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
pub mod fingerprint;
pub mod focus;
pub mod handle;
pub mod handler;
pub mod lifecycle;
pub mod loop_scope;
pub mod magics;
pub mod model_runtime;
pub mod mount;
pub mod mutation_channel;
pub mod path;
pub mod payload_scope;
pub mod plugin;
pub mod profiler;
pub mod props;
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
pub mod storage;
pub mod store;
pub mod styles;
pub mod task;
pub mod templates;
pub mod templates_plan;
pub mod text;
pub mod tick;
pub mod timers;
pub mod watch;
pub mod web;

pub use app::{
    encode_route_fragment, encode_route_path_segment, encode_route_query_part, App, AppPlugin,
    Component, IntoRouteTarget, Loader, LoaderContext, LoaderError, PageLink, PageMeta,
    PageMetaContext, PageMetaTag, Prefetch, PrefetchTrigger, RouteComponent, RouteConfig,
    RouteContext, RouteErrorSurface, RouteGuard, RouteGuardDecision, RouteLoader,
    RouteLoaderFuture, RouteMeta, RouteMetaKey, RouteName, RouteQuery, RouteRejection,
    RouteRejectionAction, RouteRejectionContext, RouteRejectionHandler, RouteTarget,
    RouteTargetBuilder, RouteTargetError, RouteUrl, SubtreeHandle,
};
pub use client_module::{ClientModule, ClientModuleError};
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
pub use events::{on_scope_unmount, on_scope_unmount_for, DomEventName, ListenerHandle};
pub use expr::{StaticBinOp, StaticExpr, StaticLiteral};
pub use extractors::{Inject, NearestParent, Parent};
pub use handle::{this, Handle};
pub use handler::{FromHandlerArg, HandlerDispatch};
pub use lifecycle::{
    Body, Doc, El, Elapsed, HostEl, IsTeleported, LifecycleContext, LifecyclePhase, MountEpoch,
    ParentId, Refs, ScopePath, TagName, TeleportHost, TypedEl, Win,
};
pub use model_runtime::{with_write_origin, WriteOrigin};
pub use plugin::{
    AppBootCompleted, AppBootFailed, AppBootStarted, ComponentEvent, ComponentMounted,
    ComponentPluginExt, ComponentReady, ComponentSetup, ComponentUnmounted, ForComponent, Hook,
    Plugin, PluginValidationError, Plugins, RouteNavigationCompleted, RouteNavigationFailed,
    RouteNavigationStarted, ServerFunctionClientCompleted, ServerFunctionClientFailed,
    ServerFunctionClientStarted,
};
pub use profiler::mount::{
    enabled as mount_profile_enabled, report as report_mount_profile, reset as reset_mount_profile,
};
pub use props::{PropValue, Props};
pub use reactive::{
    batch, current_effect, effect, effect_scoped, effect_with, flush_sync, on_cleanup, release,
    run_now, set_auto_flush, track, trigger_scope, EffectId, EffectOptions, ScopeId, SignalId,
};
pub use registry::{
    assert_registry_clean, canonical_component_name, mark_registered, register_component,
    register_component_as, register_component_prefixed, register_component_with_mount,
    registered_component_names, registry_errors, render_boot_error, verify_registry, ComponentCtor,
    ComponentEntry, ComponentMountFn, ComponentVTable, RegisteredComponent, RegistryError,
    RegistryErrorKind, COMPONENT_ENTRIES,
};
pub use router::{
    go, navigate, prefetch, push, reevaluate_current, register_route, replace, NavigationFailure,
    NavigationResult, PrefetchResult, PrefetchSkip, ReturnTo, RouteLocation, RouteToken,
};
pub use scope::{
    append_list_inline, current_scope_id, invalidate_field, invalidate_field_cache, js_bridge,
    patch_list_at_inline, patch_list_indices_inline, prepend_list_inline, remove_list_at_inline,
    replace_field_inline, swap_list_indices_inline, ComponentState, Scope, StaticPropKind,
};
pub use server::{Result as ServerResult, ServerError};
pub use signal::{rw_signal, signal, RwSignal, Setter, Signal};
pub use storage::{LocalStorage, StorageError};
pub use store::{register_store_scope, store, store_scope, stores_object, Store, StoreHandle};
pub use styles::inject_style;
pub use task::{
    spawn, spawn_for_scope, spawn_latest, spawn_latest_for_scope, spawn_scoped, TaskHandle,
};
pub use templates::{
    compile_template, inject_pp_data, is_registered, register_template, template_for,
};
pub use watch::{
    watch, watch_field, watch_field_scoped, watch_scope_field, watch_scope_field_now,
    watch_scope_field_scoped, watch_scoped,
};
