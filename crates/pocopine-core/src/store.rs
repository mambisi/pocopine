//! Store registry — singleton components.
//!
//! A store is a component scope that outlives any particular DOM mount. One
//! instance per type per runtime, accessible two ways:
//!
//! * **Templates**: `$store.<name>` resolves to the store's proxy via the
//!   existing magics layer. `$store.preferences.theme` flows through
//!   normal proxy `get` traps, so dep tracking works unchanged.
//! * **Rust**: `pocopine::store::<Preferences>()` returns a [`Handle<T>`]
//!   (same type `pocopine::this::<T>()` returns for a component), which
//!   exposes `update`/`with` closures over the concrete state.
//!
//! The `#[store]` macro emits the body of [`Store::__register_store`] —
//! it builds a typed `Rc<RefCell<Self>>`, wraps it in a `Scope`, and
//! registers that scope in the name-keyed registry below.

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::Reflect;
use wasm_bindgen::JsValue;

use crate::handle::Handle;
use crate::scope::{ComponentState, Scope};

/// Trait implemented by structs annotated with `#[store]`.
pub trait Store: ComponentState + 'static {
    /// Name used in the `$store.<NAME>` magic path. Kebab-case of the
    /// struct ident unless overridden via `#[store(name = "...")]`.
    const STORE_NAME: &'static str;

    /// Idempotent registration — the macro emits the body.
    fn __register_store();

    /// Typed handle to the singleton. The macro emits the body using
    /// [`store_scope`] + [`Scope::typed`].
    fn __handle() -> Handle<Self>
    where
        Self: Sized;
}

/// Back-compat alias so older code calling `StoreHandle<T>` keeps
/// compiling while it migrates to [`Handle<T>`].
pub type StoreHandle<T> = Handle<T>;

/// Short-hand for `T::__handle()`.
pub fn store<T: Store>() -> Handle<T> {
    T::__handle()
}

thread_local! {
    /// Name-keyed registry so the `$store` magic can resolve dotted paths
    /// (`$store.preferences` → this scope's proxy).
    static STORE_SCOPES: RefCell<HashMap<&'static str, Scope>> =
        RefCell::new(HashMap::new());

    /// Pre-built `$store` container — an `Object` whose keys are store
    /// names mapped to their proxies. Lazily (re)built on first read after
    /// the last registration.
    static STORE_OBJECT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Insert a store's scope under `name`. Called from the macro-generated
/// `__register_store` body.
pub fn register_store_scope(name: &'static str, scope: Scope) {
    STORE_SCOPES.with(|s| s.borrow_mut().insert(name, scope));
    STORE_OBJECT.with(|o| *o.borrow_mut() = None);
}

/// Look up a store's scope by name. Used by the walker for tests and by
/// `$store` magic for direct proxy access.
pub fn store_scope(name: &str) -> Option<Scope> {
    STORE_SCOPES.with(|s| s.borrow().get(name).cloned())
}

/// Build (or reuse) the `$store` container.
pub fn stores_object() -> JsValue {
    let cached = STORE_OBJECT.with(|o| o.borrow().clone());
    if let Some(v) = cached {
        return v;
    }
    let obj = js_sys::Object::new();
    STORE_SCOPES.with(|s| {
        for (name, scope) in s.borrow().iter() {
            let proxy = scope.into_proxy();
            let _ = Reflect::set(&obj, &JsValue::from_str(name), &proxy);
        }
    });
    let js: JsValue = obj.into();
    STORE_OBJECT.with(|o| *o.borrow_mut() = Some(js.clone()));
    js
}
