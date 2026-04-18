//! Component registry.
//!
//! `wasm32-unknown-unknown` has no portable "collect static entries at
//! startup" mechanism (no `linkme`/`ctor`/`inventory` story that survives
//! `wasm-bindgen(start)`), so registration is explicit. The `#[component]`
//! macro emits a `pub fn register()` on the struct; users call it from their
//! startup code before `pocopine::run()`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::scope::Scope;

/// Constructor returned by the `#[component]` macro. Builds a fresh typed
/// `Rc<RefCell<Self>>`, wraps it in a [`Scope`] (which stashes both the
/// erased and typed forms), and returns the scope.
pub type ComponentCtor = fn() -> Scope;

/// Kept as a public type so users with their own registration path have
/// something to hand back to the runtime.
pub struct ComponentEntry {
    pub name: &'static str,
    pub ctor: ComponentCtor,
}

thread_local! {
    static REGISTRY: RefCell<HashMap<&'static str, ComponentCtor>> =
        RefCell::new(HashMap::new());
}

/// Exposed for symmetry with the `ComponentEntry` type.
pub static COMPONENT_ENTRIES: &[ComponentEntry] = &[];

/// Register a component under a name. Called by macro-generated
/// `MyStruct::register()` functions.
pub fn register_component(name: &'static str, ctor: ComponentCtor) {
    REGISTRY.with(|r| {
        r.borrow_mut().insert(name, ctor);
    });
}

/// Instantiate a component by name. `None` if the name wasn't registered.
pub fn instantiate(name: &str) -> Option<Scope> {
    let ctor = REGISTRY.with(|r| r.borrow().get(name).copied());
    ctor.map(|c| c())
}
