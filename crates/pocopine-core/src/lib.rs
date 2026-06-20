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
/// RFC-099 Phase 1b — host-side expression evaluator over
/// `serde_json::Value` (the SSR render backend), parity-gated against
/// the wasm `JsValue` evaluator.
pub mod host_eval;
/// RFC-099 Phase 2c — client-side hydration: attach reactivity to a
/// server-rendered subtree (the "claim" walk).
pub mod hydrate;
/// RFC-099 Phase 1 — JS-identical number formatting, shared by the
/// wasm client and the SSR host renderer.
pub mod js_number;
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
pub mod progress;
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
    App, AppPlugin, Component, IntoRouteTarget, Loader, LoaderContext, LoaderError, PageLink,
    PageMeta, PageMetaContext, PageMetaTag, Prefetch, PrefetchTrigger, RouteComponent, RouteConfig,
    RouteContext, RouteErrorSurface, RouteGuard, RouteGuardDecision, RouteLoader,
    RouteLoaderFuture, RouteMeta, RouteMetaKey, RouteName, RouteQuery, RouteRejection,
    RouteRejectionAction, RouteRejectionContext, RouteRejectionHandler, RouteTarget,
    RouteTargetBuilder, RouteTargetError, RouteUrl, SubtreeHandle, encode_route_fragment,
    encode_route_path_segment, encode_route_query_part,
};
pub use client_module::{ClientModule, ClientModuleError};
pub use computed::{Computed, computed};
#[allow(deprecated)]
pub use context::InjectKey;
pub use context::{ContextKey, ContextMarker, inject, provide};
pub use directives::for_plan::{
    BindingKind, StaticBinding, StaticListener, StaticRowPlan, register_row_plans,
};
pub use emit::{
    Emit, emit, emit_cancelable, emit_cancelable_from, emit_event, emit_event_from, emit_from,
    emit_from_host, emit_model, emit_model_field, emit_raw, emit_raw_from,
};
pub use events::{DomEventName, ListenerHandle, on_scope_unmount, on_scope_unmount_for};
pub use expr::{StaticBinOp, StaticExpr, StaticLiteral};
pub use extractors::{Inject, NearestParent, Parent};
pub use handle::{FieldHandle, Handle, this};
pub use handler::{FromHandlerArg, HandlerDispatch};
pub use lifecycle::{
    Body, Doc, El, Elapsed, HostEl, IsTeleported, LifecycleContext, LifecyclePhase, MountEpoch,
    ParentId, Refs, ScopePath, TagName, TeleportHost, TypedEl, Win,
};
pub use model_runtime::{WriteOrigin, with_write_origin};
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
    EffectId, EffectOptions, ScopeId, SignalId, batch, current_effect, effect, effect_hydrating,
    effect_scoped, effect_with, flush_sync, on_cleanup, release, run_now, set_auto_flush, track,
    trigger_scope,
};
pub use registry::{
    COMPONENT_ENTRIES, ComponentCtor, ComponentEntry, ComponentMountFn, ComponentVTable,
    RegisteredComponent, RegistryError, RegistryErrorKind, assert_registry_clean,
    canonical_component_name, mark_registered, register_component, register_component_as,
    register_component_prefixed, register_component_with_mount, registered_component_names,
    registry_errors, render_boot_error, verify_registry,
};
pub use router::{
    NavigationFailure, NavigationResult, PrefetchResult, PrefetchSkip, ReturnTo, RouteLocation,
    RouteToken, go, navigate, prefetch, push, reevaluate_current, register_route, replace,
};
pub use scope::{
    ComponentState, Scope, StaticPropKind, append_list_inline, current_scope_id, invalidate_field,
    invalidate_field_cache, js_bridge, patch_list_at_inline, patch_list_indices_inline,
    prepend_list_inline, remove_list_at_inline, replace_field_inline, swap_list_indices_inline,
};
pub use server::{Result as ServerResult, ServerError};
pub use signal::{RwSignal, Setter, Signal, rw_signal, signal};
pub use storage::{LocalStorage, StorageError};
pub use store::{Store, StoreHandle, register_store_scope, store, store_scope, stores_object};
pub use styles::inject_style;
pub use task::{
    TaskHandle, spawn, spawn_for_scope, spawn_latest, spawn_latest_for_scope, spawn_scoped,
};
pub use templates::{
    compile_template, inject_pp_data, is_registered, register_template, template_for,
};
pub use watch::{
    watch, watch_field, watch_field_scoped, watch_scope_field, watch_scope_field_now,
    watch_scope_field_scoped, watch_scoped,
};
