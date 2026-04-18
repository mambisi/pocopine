//! `App` — the builder that wires registered components / stores and
//! starts the runtime.
//!
//! Every component emitted by `#[component]` implements the [`Component`]
//! trait; every store emitted by `#[store]` implements the [`Store`]
//! trait. `App` accepts those bounds so a project's startup reads as
//! a single declarative chain:
//!
//! ```ignore
//! use pocopine::prelude::*;
//!
//! #[wasm_bindgen(start)]
//! pub fn main() {
//!     App::new()
//!         .register::<Counter>()
//!         .register::<TodoList>()
//!         .store::<Preferences>()
//!         .before_mount(|| web_sys::console::log_1(&"booting".into()))
//!         .run();
//! }
//! ```
//!
//! `Counter::register()` and `pocopine::run()` still work for
//! ad-hoc use — the trait is what `App` calls under the hood.

use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;

use crate::store::Store;
use crate::walker;

/// Every component participates in the app surface via this trait. The
/// `#[component]` macro emits the impl automatically.
pub trait Component {
    /// The runtime name (kebab-case of the struct ident unless overridden).
    /// Identical to the registered tag name.
    const NAME: &'static str;
    /// Register this component (scope constructor, template, stylesheet)
    /// with the runtime. Idempotent — safe to call more than once, later
    /// calls just overwrite the registry entries.
    fn register();
}

type Hook = Box<dyn FnOnce()>;

/// Application-level wiring and lifecycle.
///
/// Construct with [`App::new`], chain `.register::<T>()` / `.store::<S>()`
/// / `.before_mount(...)` / `.after_mount(...)`, and end with `.run()`.
#[derive(Default)]
pub struct App {
    components: Vec<&'static str>,
    stores: Vec<&'static str>,
    before_mount: Vec<Hook>,
    after_mount: Vec<Hook>,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a component. Delegates to the trait method and records the
    /// runtime name for introspection.
    pub fn register<C: Component>(mut self) -> Self {
        C::register();
        self.components.push(C::NAME);
        self
    }

    /// Register a store singleton. Delegates to [`Store::__register_store`].
    pub fn store<S: Store>(mut self) -> Self {
        S::__register_store();
        self.stores.push(S::STORE_NAME);
        self
    }

    /// Run `f` before the initial DOM walk.
    pub fn before_mount(mut self, f: impl FnOnce() + 'static) -> Self {
        self.before_mount.push(Box::new(f));
        self
    }

    /// Run `f` after the initial DOM walk completes. Scheduled on the next
    /// microtask so scopes bound during the walk are visible.
    pub fn after_mount(mut self, f: impl FnOnce() + 'static) -> Self {
        self.after_mount.push(Box::new(f));
        self
    }

    /// Fire pre-mount hooks, start the walker, then fire post-mount hooks.
    pub fn run(self) {
        for f in self.before_mount {
            f();
        }
        walker::start_on_body();
        let after = self.after_mount;
        if !after.is_empty() {
            spawn_local(async move {
                let _ = js_sys::Promise::resolve(&JsValue::NULL);
                for f in after {
                    f();
                }
            });
        }
    }

    /// Snapshot of the registered component names. Debug utility only —
    /// the runtime registry is the source of truth.
    pub fn registered_components(&self) -> &[&'static str] {
        &self.components
    }

    /// Snapshot of the registered store names. Debug utility only.
    pub fn registered_stores(&self) -> &[&'static str] {
        &self.stores
    }
}
