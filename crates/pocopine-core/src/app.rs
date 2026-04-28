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

use crate::router;
use crate::store::Store;
use crate::walker;

/// Every component participates in the app surface via this trait. The
/// `#[component]` macro emits the impl automatically.
pub trait Component {
    /// The runtime name (kebab-case of the struct ident unless overridden).
    /// Identical to the registered tag name.
    const NAME: &'static str;
    /// Register this component (scope constructor, template, stylesheet)
    /// with the runtime. Idempotent for the *same* owner — re-registering
    /// the same `(canonical, owner)` pair is a no-op (RFC 056 §6.1). A
    /// distinct owner colliding on the same canonical tag records a
    /// [`crate::RegistryError`] instead of silently overwriting.
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
    routes: Vec<&'static str>,
    before_mount: Vec<Hook>,
    after_mount: Vec<Hook>,
    devtools: bool,
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

    /// Register a route. `pattern` is a path with optional `:name`
    /// segments (`"/blog/:id"`) or the 404 fallback `"*"`. `C` must be
    /// a `#[component]` whose tag name is the kebab-case of its ident.
    /// Matching routes paint their component into the
    /// `<pp-outlet>` with captured params passed through as attributes.
    pub fn route<C: Component>(mut self, pattern: &'static str) -> Self {
        C::register();
        router::register_route(pattern.to_string(), C::NAME);
        self.routes.push(pattern);
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

    /// Install the devtools overlay on `run()`. The panel lists every
    /// live scope, its current state, and its registered refs. Toggle
    /// visibility with `Ctrl+Shift+D`. Keep this off in release builds
    /// — the poll loop is cheap but not free.
    ///
    /// When the crate is built with `--no-default-features`
    /// (devtools feature disabled), this method still exists for
    /// API stability but the flag is ignored at `run()` time.
    pub fn with_devtools(mut self) -> Self {
        self.devtools = true;
        self
    }

    /// RFC 060 Tier 4 — variant of [`Self::run`] that drives the
    /// registry off an explicit `&'static phf::Map<&'static str,
    /// &'static ComponentVTable>` produced by the `app!{}`
    /// macro. Iterates the map's values and calls each vtable's
    /// `register` fn (idempotent via the Tier 1
    /// `mark_registered` guard) before mounting. The thread-local
    /// runtime registries still back lookups; this method is
    /// the bridge between the static phf surface and the
    /// existing runtime data.
    pub fn run_with_registry(
        self,
        registry: &'static phf::Map<&'static str, &'static crate::registry::ComponentVTable>,
    ) {
        for vtable in registry.values() {
            (vtable.register)();
        }
        self.run();
    }

    /// Fire pre-mount hooks, mount registered components on the body,
    /// initialise the router (if any routes were registered), then
    /// fire post-mount hooks.
    ///
    /// RFC 056 §6.2: before any mount work the registry is verified;
    /// when collisions exist the boot error surface is rendered and
    /// no further mount work runs.
    pub fn run(self) {
        if let Err(errors) = crate::registry::verify_registry() {
            crate::registry::render_boot_error(&errors);
            return;
        }
        // Inject the animate-preset atom stylesheet before any
        // component `register()` injects per-component styles, so
        // the preset atoms live earlier in the cascade and
        // component styles still win on specificity ties.
        crate::animate::install();
        for f in self.before_mount {
            f();
        }
        if let Some(body) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
        {
            walker::start_compiled(&body.into());
        }
        if !self.routes.is_empty() {
            router::init();
        }
        #[cfg(feature = "devtools")]
        if self.devtools {
            crate::devtools::install();
        }
        #[cfg(not(feature = "devtools"))]
        let _ = self.devtools;
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

    /// Snapshot of the registered route patterns. Debug utility only.
    pub fn registered_routes(&self) -> &[&'static str] {
        &self.routes
    }
}
