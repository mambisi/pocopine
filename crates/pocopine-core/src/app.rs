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

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::Element;

use crate::mount;
use crate::router;
use crate::store::Store;

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

    /// **Internal — invoked by the `app!{}` macro; do not call
    /// directly.** Records a route without eagerly calling
    /// `C::register()`. Safe only when paired with
    /// [`Self::run_with_registry`]: the static `&'static phf::Map`
    /// is the authoritative registry, and skipping the eager
    /// `register()` keeps it that way. Direct user calls would
    /// route to a component the registry never registered.
    #[doc(hidden)]
    pub fn route_static<C: Component>(mut self, pattern: &'static str) -> Self {
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
        crate::registry::set_active_phf_registry(registry);
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
        // RFC 061 Phase 2 — discover the [pp-app] root and mount
        // there. Whole-body mounting is gone; apps that want
        // multiple roots use `mount_subtree::<C>` instead.
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };
        let pp_app = document.query_selector("[pp-app]").ok().flatten();
        if let Some(host) = pp_app {
            mount_pp_app_subtree(&host);
        } else {
            render_missing_pp_app_root();
            return;
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

    /// RFC 061 Phase 2 — typed escape hatch for mounting a
    /// `#[component]` into an arbitrary DOM element. Intended
    /// for tooling: devtools panels, test harnesses,
    /// Storybook-style component galleries, embedded widgets.
    /// Default app shape stays [`App::run`] with a `[pp-app]`
    /// root.
    ///
    /// Registers `C` (idempotent via the Tier 1 guard) and
    /// mounts it onto `host`. Returns a [`SubtreeHandle`] whose
    /// `unmount()` tears down the scope tree + lifecycle hooks
    /// + DOM cleanly.
    pub fn mount_subtree<C: Component>(host: &Element) -> SubtreeHandle {
        C::register();
        mount::mount_child_component(host, C::NAME);
        SubtreeHandle {
            host: host.clone(),
            active: true,
        }
    }
}

/// RFC 061 Phase 2 — handle returned by [`App::mount_subtree`].
/// Drop or call [`Self::unmount`] to release the scope tree's
/// effects, listeners, and DOM children.
#[must_use = "drop or call `.unmount()` to clean up the subtree"]
pub struct SubtreeHandle {
    host: Element,
    active: bool,
}

impl SubtreeHandle {
    fn release(&mut self) {
        if !self.active {
            return;
        }
        mount::release_compiled_subtree(&self.host);
        self.host.set_inner_html("");
        self.active = false;
    }

    /// Tear down the subtree. Releases the scope tree
    /// (effects + listeners + DOM refs) and clears the host's
    /// children. After this the host element remains in the
    /// DOM but contains nothing pocopine owns.
    pub fn unmount(mut self) {
        self.release();
    }
}

impl Drop for SubtreeHandle {
    fn drop(&mut self) {
        self.release();
    }
}

/// RFC 061 Phase 3 — compiled root discovery for `[pp-app]`.
/// This is the app-root sibling of [`App::mount_subtree`]:
/// both paths call `mount_child_component` for known component
/// tags, then finalize the compiled subtree. The app root differs
/// only by discovering route-authored descendants from the static
/// registry instead of receiving a typed `C`.
fn mount_pp_app_subtree(host: &Element) {
    crate::styles::inject_style("__pp_cloak", "[pp-cloak] { display: none !important; }");
    let names = crate::templates::registered_template_names();
    if !names.is_empty() {
        let selector = names.join(",");
        if let Ok(matches) = host.query_selector_all(&selector) {
            for i in 0..matches.length() {
                let Some(node) = matches.item(i) else {
                    continue;
                };
                let Ok(el) = node.dyn_into::<Element>() else {
                    continue;
                };
                let tag = el.local_name();
                mount::mount_child_component(&el, &tag);
                mount::finalize_compiled_subtree(&el);
            }
        }
    }
    if let Ok(outlets) = host.query_selector_all("pp-outlet") {
        for i in 0..outlets.length() {
            let Some(node) = outlets.item(i) else {
                continue;
            };
            if let Ok(el) = node.dyn_into::<Element>() {
                router::set_outlet(el);
            }
        }
    }
}

/// RFC 061 Phase 2 — paint a friendly boot error when
/// [`App::run`] can't find a `[pp-app]` root. Renders a fixed
/// overlay without clearing `<body>`, so test harnesses and
/// host-page diagnostics survive the fatal boot error.
fn render_missing_pp_app_root() {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Some(body) = doc.body() else { return };
    if let Ok(Some(existing)) = body.query_selector("[data-pocopine-boot-error=\"missing-pp-app\"]")
    {
        existing.remove();
    }
    let Ok(banner) = doc.create_element("div") else {
        return;
    };
    let _ = banner.set_attribute("data-pocopine-boot-error", "missing-pp-app");
    let _ = banner.set_attribute(
        "style",
        "position:fixed;inset:0;background:#1b1b1f;color:#f5f5f7;\
         font-family:ui-monospace,monospace;padding:24px;overflow:auto;\
         z-index:2147483647;",
    );
    banner.set_inner_html(
        "<h2 style=\"margin:0 0 12px 0;color:#ff6b6b;\">pocopine: \
         no <code>[pp-app]</code> root found</h2>\
         <p style=\"margin:0 0 16px 0;\">\
         pocopine v2 is compiled-mount-only — `App::run()` looks for \
         a single element with the <code>pp-app</code> attribute and \
         mounts the active route there. Add it to your HTML host:</p>\
         <pre style=\"background:#0d0d10;padding:12px;border-radius:4px;\
         overflow:auto;\">&lt;body&gt;\n  &lt;div pp-app&gt;&lt;/div&gt;\n&lt;/body&gt;</pre>\
         <p style=\"margin:16px 0 0 0;color:#a0a0a0;font-size:0.875rem;\">\
         Apps that need multiple roots use \
         <code>App::mount_subtree::&lt;C&gt;(host)</code> instead. \
         See RFC 061 for the migration guide.</p>",
    );
    let _ = body.append_child(&banner);
    web_sys::console::error_1(
        &"pocopine: App::run() found no [pp-app] root — refusing to mount. See RFC 061.".into(),
    );
}
