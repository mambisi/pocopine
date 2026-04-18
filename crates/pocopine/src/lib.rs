//! pocopine — user-facing umbrella crate.
//!
//! Re-exports the runtime from `pocopine-core` and the `#[component]` /
//! `#[handlers]` attribute macros from `pocopine-macros`. App code should
//! depend on `pocopine` and pull everything from `pocopine::prelude::*`.

pub use pocopine_core::{
    batch, computed, current_effect, effect, effect_with, flush_sync, on_cleanup, release, run,
    run_now, rw_signal, set_auto_flush, signal, watch, ComponentState, Computed, EffectId,
    EffectOptions, RwSignal, Scope, ScopeId, Setter, Signal, SignalId,
};
pub use pocopine_macros::{component, handlers};

pub mod prelude {
    pub use crate::{
        batch, component, computed, effect, handlers, on_cleanup, run, rw_signal, signal,
        watch, ComponentState, Computed, RwSignal, Setter, Signal,
    };
    pub use wasm_bindgen::prelude::*;
}

#[doc(hidden)]
pub mod __private {
    //! Internals used by macro-generated code. Not a stable API.
    pub use js_sys;
    pub use pocopine_core::{
        inject_pp_data, inject_style, register_component, register_template, ComponentState,
        HandlerDispatch,
    };
    pub use serde_wasm_bindgen;
    pub use wasm_bindgen;
    pub use wasm_bindgen::JsValue;
}
