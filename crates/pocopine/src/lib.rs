//! pocopine — user-facing umbrella crate.
//!
//! Re-exports the runtime from `pocopine-core` and the `#[component]` /
//! `#[handlers]` attribute macros from `pocopine-macros`. App code should
//! depend on `pocopine` and pull everything from `pocopine::prelude::*`.

#[cfg(not(target_arch = "wasm32"))]
pub use pocopine_auth::RequestContext;
pub use pocopine_auth::{AuthUser, Permission, Principal, Role, Session};
#[allow(deprecated)]
pub use pocopine_core::InjectKey;
pub use pocopine_core::{
    animate, dom, events, focus, id, profiler, refs, scroll_lock, text, tick, timers,
};
pub use pocopine_core::{
    append_list_inline, assert_registry_clean, batch, computed, current_effect, current_scope_id,
    dispatch_event, effect, effect_scoped, effect_with, emit, emit_cancelable,
    emit_cancelable_from, emit_event, emit_event_from, emit_from, emit_from_host, emit_model,
    emit_model_field, emit_raw, emit_raw_from, fetch, flush_sync, inject, invalidate_field,
    invalidate_field_cache, mount_profile_enabled, on_cleanup, on_scope_unmount,
    on_scope_unmount_for, patch_list_at_inline, patch_list_indices_inline, prepend_list_inline,
    provide, registered_component_names, registry_errors, release, remove_list_at_inline,
    replace_field_inline, report_mount_profile, reset_mount_profile, run_now, rw_signal,
    set_auto_flush, signal, spawn, spawn_for_scope, spawn_latest, spawn_latest_for_scope,
    spawn_scoped, store, swap_list_indices_inline, this, trigger_scope, verify_registry, watch,
    watch_field, watch_field_scoped, watch_scope_field, watch_scope_field_now,
    watch_scope_field_scoped, watch_scoped, App, AppPlugin, Body, Component, ComponentMountFn,
    ComponentState, ComponentVTable, Computed, ContextKey, ContextMarker, Doc, DomEventName,
    EffectId, EffectOptions, El, Elapsed, Emit, Handle, HostEl, Inject, IsTeleported,
    LifecycleContext, ListenerHandle, MountEpoch, NearestParent, Parent, ParentId, Refs,
    RegisteredComponent, RegistryError, RegistryErrorKind, RwSignal, Scope, ScopeId, ScopePath,
    ServerError, ServerResult, Setter, Signal, SignalId, Store, StoreHandle, SubtreeHandle,
    TagName, TaskHandle, TeleportHost, TypedEl, Win,
};
#[doc(inline)]
pub use pocopine_core::{create_context, inject_key};
#[cfg(not(target_arch = "wasm32"))]
pub use pocopine_jobs::{
    DeadLetter, JobBackend, JobClient, JobDescriptor, JobFuture, JobHandler, JobId,
    PeriodicSchedule, Worker, WorkerConfig,
};
pub use pocopine_jobs::{JobError, JobResult};
// Note: `store` exists in both the value namespace (the accessor `fn store<T>()`)
// and the macro namespace (the attribute `#[store]`). They don't collide.
pub use pocopine_macros::{app, component, handlers, job, protected, server, store, Emit};

pub mod auth {
    pub use pocopine_auth::*;
}

#[cfg(feature = "analytics")]
pub mod analytics {
    pub use pocopine_analytics::*;
}

#[cfg(feature = "logging")]
pub mod logging {
    pub use pocopine_logging::*;
}

#[cfg(feature = "live")]
pub mod live {
    pub use pocopine_live::*;
}

#[cfg(feature = "observe")]
pub mod observe {
    pub use pocopine_observe::*;
}

#[cfg(not(target_arch = "wasm32"))]
pub mod jobs {
    pub use pocopine_jobs::*;
}

pub mod prelude {
    #[allow(deprecated)]
    pub use crate::InjectKey;
    pub use crate::{
        batch, component, computed, create_context, cx, dispatch, dispatch_event, effect, emit,
        emit_cancelable, emit_cancelable_from, emit_event, emit_event_from, emit_from,
        emit_from_host, emit_model, emit_model_field, emit_raw, emit_raw_from, handlers,
        inject_key, job, on, on_cleanup, on_emit, prepend_list_inline, protected,
        remove_list_at_inline, rw_signal, signal, spawn, spawn_for_scope, spawn_latest,
        spawn_latest_for_scope, spawn_scoped, store, this, watch, App, AppPlugin, AuthUser,
        Component, ComponentState, Computed, ContextKey, ContextMarker, Emit, Handle, Inject,
        JobError, JobResult, NearestParent, Parent, Permission, Principal, Role, RwSignal, Scope,
        ScopeId, ServerError, ServerResult, Setter, Signal, Store,
    };
    pub use wasm_bindgen::prelude::*;
}

#[doc(hidden)]
pub mod __private {
    //! Internals used by macro-generated code. Not a stable API.
    pub use js_sys;
    pub use pocopine_core::registry::{
        clear_active_phf_registry_for_test, set_active_phf_registry,
    };
    pub use pocopine_core::{
        compile_template, component_computed, computed as runtime_computed, inject_pp_data,
        inject_style, mark_registered, register_component_with_mount, register_row_plans,
        register_store_scope, register_template, spawn as runtime_spawn,
        spawn_for_scope as runtime_spawn_for_scope, spawn_latest as runtime_spawn_latest,
        spawn_latest_for_scope as runtime_spawn_latest_for_scope,
        spawn_scoped as runtime_spawn_scoped, store_scope, task::TaskHandle, track,
        with_write_origin, BindingKind, Component, ComponentMountFn, ComponentState,
        ComponentVTable, FromHandlerArg, Handle, HandlerDispatch, LifecycleContext, Scope,
        StaticBinOp, StaticBinding, StaticExpr, StaticListener, StaticLiteral, StaticRowPlan,
        Store, WriteOrigin,
    };
    #[cfg(not(target_arch = "wasm32"))]
    pub use pocopine_jobs as jobs;
    // RFC 060 Tier 4 — `phf` re-exported for `app!{}` macro use.
    pub use phf;
    // RFC-058 Phase 2 — template-plan shape + registry. The
    // macro emits a `&'static StaticTemplatePlan` per component
    // and registers it via `register_template_plan` alongside the
    // existing `register_template` call.
    pub use pocopine_core::directives::for_plan::{
        IfBodyFn, StaticChildHostBinding, StaticChildHostListener, StaticChildHostModel,
        StaticChildMount, StaticForPlan, StaticIfPlan, StaticInterp, StaticNativeModel,
        StaticOpaqueDirective, StaticRef, StaticSlotFragment, StaticSlotOutlet, StaticTeleportPlan,
    };
    pub use pocopine_core::directives::interp::PlannedSegment;
    pub use pocopine_core::templates_plan::{
        apply_static_pp_as_plan, capture_static_interp_target, capture_static_slot_outlet,
        install_static_binding, install_static_child_mount, install_static_for_plan,
        install_static_if_plan, install_static_interp_target, install_static_listener,
        install_static_native_model, install_static_opaque_directive, install_static_ref,
        install_static_teleport_plan, materialize_static_slot_outlet, plan_failure_count,
        record_plan_failure, register_template_plan, reset_plan_failure_count, stamp_if_body_with,
        template_plan_for, StaticInterpTarget, StaticTemplatePlan,
    };
    // RFC-058 Phase 1: mount lifecycle / cleanup helpers exposed
    // for the future generated mount/hydrate code (Phase 2+) to
    // call through the umbrella's stable `::pocopine::__private`
    // namespace. Same shape as the existing `register_template` /
    // `register_row_plans` re-exports above.
    pub use pocopine_core::mount::{
        finalize_compiled_subtree, fire_mount_post_order, fire_ready_next_tick,
        mount_child_component, mount_child_component_with_slots, release_compiled_subtree,
    };
    // RFC-058 §5.5 — parent-owned slot fragment ABI. Phase 1
    // ships the type surface; Phase 3 wires consumers.
    pub use pocopine_core::slot_fragment::{
        stamp_dynamic_slot_with, stamp_static_html, SlotFragment, SlotMountCtx, SlotSet,
        DEFAULT_SLOT_NAME,
    };
    // RFC-058 Phase 1.1 / 1.7 — cleanup-safe directive install
    // helpers. Generated mount code (Phase 2+) calls these
    // directly instead of going through the runtime `dispatch` /
    // `DirectiveCall` path. Re-exported as a module so the macro
    // can write `::pocopine::__private::directives::text::install`.
    pub use pocopine_core::directives;
    pub use serde;
    pub use serde_wasm_bindgen;
    pub use wasm_bindgen;
    pub use wasm_bindgen::JsValue;
    pub use wasm_bindgen_futures;
    pub use web_sys;
}

/// Dispatch an async action and apply a state update when it resolves.
///
/// The canonical shape for any async handler work: "do `X`, then when
/// the result is back, mutate `self` with it." The macro captures the
/// typed handle to `Self` via [`this`], spawns the body on the
/// microtask executor, and routes the awaited value through
/// [`Handle::update`] so reactivity fires exactly once at the end.
///
/// # Shape
///
/// ```ignore
/// dispatch!(
///     <expr evaluated in async context>,  // any expression, including `.await`s
///     |s, result| { /* update block — `s: &mut Self`, `result: T` */ },
/// );
/// ```
///
/// # Example
///
/// ```ignore
/// #[handlers]
/// impl BlogPost {
///     pub fn init(&mut self) {
///         self.loading = true;
///         let post_id = self.post_id;
///         dispatch!(
///             get_post(post_id).await,
///             |s, result| {
///                 s.loading = false;
///                 match result {
///                     Ok(p)  => { s.title = p.title; s.body = p.body; }
///                     Err(e) => { s.error = e.to_string(); }
///                 }
///             },
///         );
///     }
/// }
/// ```
///
/// Must be called inside a `#[handlers]` method — `this::<Self>()`
/// panics outside an invoke context.
#[macro_export]
macro_rules! dispatch {
    ($body:expr, |$s:ident, $result:ident| $update:block $(,)?) => {{
        let __pocopine_handle = $crate::this::<Self>();
        $crate::__private::wasm_bindgen_futures::spawn_local(async move {
            let $result = $body;
            __pocopine_handle.update(|$s| $update);
        });
    }};
}

/// Sugar for [`events::on_scoped`]. Resolves the bare event ident
/// against the [`events::ev`] catalog and adds an implicit `move`
/// so the closure captures by ownership (the right default inside
/// handler / lifecycle hooks where the listener outlives the
/// caller's frame).
///
/// ```ignore
/// use pocopine::prelude::*;
///
/// on!(el, contextmenu, |e| {
///     e.prevent_default();
///     root.update(|r| r.open_at(e.client_x() as f64, e.client_y() as f64));
/// });
/// ```
///
/// Equivalent to:
///
/// ```ignore
/// pocopine::events::on_scoped(
///     &el,
///     pocopine::events::ev::contextmenu,
///     move |e| { … },
/// );
/// ```
///
/// For events outside the catalog (custom-element events, vendor
/// names) reach for [`events::on_named_scoped`] directly.
#[macro_export]
macro_rules! on {
    ($target:expr, $event:ident, |$arg:ident| $body:expr $(,)?) => {
        $crate::events::on_scoped(&$target, $crate::events::ev::$event, move |$arg| $body)
    };
}

/// Sugar for [`events::on_emit_scoped`]. Subscribes to every variant
/// of an [`Emit`] enum on `target`; the typed enum is reconstructed
/// from the `CustomEvent.detail` before the closure runs.
///
/// ```ignore
/// #[derive(Emit)]
/// enum DialogEvent {
///     Close,
///     Confirm { value: String },
/// }
///
/// on_emit!(host, DialogEvent, |e| match e {
///     DialogEvent::Close            => { /* … */ }
///     DialogEvent::Confirm { value } => { /* … */ }
/// });
/// ```
///
/// Equivalent to:
///
/// ```ignore
/// pocopine::events::on_emit_scoped::<DialogEvent, _, _>(
///     &host,
///     move |e| match e { … },
/// );
/// ```
#[macro_export]
macro_rules! on_emit {
    ($target:expr, $emit_ty:ty, |$arg:ident| $body:expr $(,)?) => {
        $crate::events::on_emit_scoped::<$emit_ty, _, _>(&$target, move |$arg| $body)
    };
}

/// Build a space-separated class string from a mix of constants and
/// conditional classes. See [`rfc-010`](../../../rfcs/rfc-010-attribute-fallthrough.md).
///
/// Each comma-separated argument is one of:
///
/// * a string literal — always appended,
/// * `cond => "class"` — appended when `cond` is truthy,
/// * any `&str` / `String` expression — appended when non-empty.
///
/// # Example
///
/// ```ignore
/// let class = cx!(
///     "pine-btn",
///     self.variant == "primary"     => "pine-btn-primary",
///     self.variant == "destructive" => "pine-btn-destructive",
///     self.size == "sm"             => "pine-btn-sm",
///     self.disabled                 => "is-disabled",
///     &self.icon_class,
/// );
/// ```
#[macro_export]
macro_rules! cx {
    () => { ::std::string::String::new() };
    ( $($rest:tt)+ ) => {{
        let mut __cx = ::std::string::String::new();
        $crate::__cx_push!(__cx; $($rest)+);
        __cx
    }};
}

/// Internal helper for [`cx!`]. Walks the argument list recursively,
/// appending each matching case to `$out`. Three arms handle `cond =>
/// "lit"`, bare literal, and arbitrary `&str`/`String` expression.
#[doc(hidden)]
#[macro_export]
macro_rules! __cx_push {
    ($out:ident;) => {};
    // cond => "class-literal"
    ($out:ident; $cond:expr => $cls:literal $(, $($rest:tt)*)?) => {
        if $cond {
            if !$out.is_empty() { $out.push(' '); }
            $out.push_str($cls);
        }
        $( $crate::__cx_push!($out; $($rest)*); )?
    };
    // bare "class-literal"
    ($out:ident; $cls:literal $(, $($rest:tt)*)?) => {
        if !$out.is_empty() { $out.push(' '); }
        $out.push_str($cls);
        $( $crate::__cx_push!($out; $($rest)*); )?
    };
    // runtime expression (`&str` / `String` / `&String`) — emitted
    // when non-empty. Must come after the literal arms; macro_rules!
    // tries arms top-down.
    ($out:ident; $expr:expr $(, $($rest:tt)*)?) => {
        {
            let __s: &str = &$expr;
            if !__s.is_empty() {
                if !$out.is_empty() { $out.push(' '); }
                $out.push_str(__s);
            }
        }
        $( $crate::__cx_push!($out; $($rest)*); )?
    };
}
