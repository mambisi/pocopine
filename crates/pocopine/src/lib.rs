//! pocopine — user-facing umbrella crate.
//!
//! Re-exports the runtime from `pocopine-core` and the `#[component]` /
//! `#[handlers]` attribute macros from `pocopine-macros`. App code should
//! depend on `pocopine` and pull everything from `pocopine::prelude::*`.

pub use pocopine_core::{focus, id, refs, scroll_lock, tick};
pub use pocopine_core::{
    batch, computed, current_effect, current_scope_id, dispatch_event, effect, effect_with,
    fetch, flush_sync, on_cleanup, release, run, run_now, rw_signal, set_auto_flush, signal,
    store, this, trigger_scope, watch, watch_field, App, Component, ComponentState, Computed,
    EffectId, EffectOptions, Handle, RwSignal, Scope, ScopeId, ServerError, ServerResult,
    Setter, Signal, SignalId, Store, StoreHandle,
};
// Note: `store` exists in both the value namespace (the accessor `fn store<T>()`)
// and the macro namespace (the attribute `#[store]`). They don't collide.
pub use pocopine_macros::{component, handlers, server, store};

pub mod prelude {
    pub use crate::{
        batch, component, computed, cx, dispatch, dispatch_event, effect, handlers,
        on_cleanup, run, rw_signal, signal, store, this, watch, App, Component,
        ComponentState, Computed, Handle, RwSignal, Scope, ScopeId, ServerError, ServerResult,
        Setter, Signal, Store,
    };
    pub use wasm_bindgen::prelude::*;
}

#[doc(hidden)]
pub mod __private {
    //! Internals used by macro-generated code. Not a stable API.
    pub use js_sys;
    pub use pocopine_core::{
        inject_pp_data, inject_style, register_component, register_store_scope,
        register_template, store_scope, Component, ComponentState, FromHandlerArg, Handle,
        HandlerDispatch, Scope, Store,
    };
    pub use serde_wasm_bindgen;
    pub use wasm_bindgen;
    pub use wasm_bindgen::JsValue;
    pub use wasm_bindgen_futures;
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
