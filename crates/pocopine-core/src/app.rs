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
//!         .plugin(my_observability_plugin())
//!         .before_mount(|| web_sys::console::log_1(&"booting".into()))
//!         .run();
//! }
//! ```
//!
//! `Counter::register()` and `pocopine::run()` still work for
//! ad-hoc use — the trait is what `App` calls under the hood.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::rc::Rc;

use crate::lifecycle::LifecycleContext;
use crate::server::ServerError;

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

    /// RFC 062 — per-component compiled mount ABI. Macro-emitted
    /// components override this with a generated mount body; manual
    /// components have no template plan to apply here.
    #[doc(hidden)]
    fn mount_template(
        _root: &Element,
        _scope_id: crate::reactive::ScopeId,
        _proxy: &wasm_bindgen::JsValue,
    ) {
    }
}

/// Component that can be mounted by the client router.
///
/// Route-local configuration lives here so guards/loaders stay next
/// to the component that consumes them. RFC 078 lands this trait
/// before the guard/loader fields are populated.
pub trait RouteComponent: Component {
    fn config() -> RouteConfig<Self>
    where
        Self: Sized,
    {
        RouteConfig::new()
    }
}

/// Route context visible to sync guards.
pub struct RouteContext<'a> {
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
}

/// Concrete client-side route target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteTarget(String);

impl RouteTarget {
    pub fn new(path: impl Into<String>) -> Result<Self, RouteTargetError> {
        let path = path.into();
        if path.is_empty() {
            return Err(RouteTargetError::Empty);
        }
        if !is_app_local_route_target(&path) {
            return Err(RouteTargetError::NotAppLocalPath);
        }
        Ok(Self(path))
    }

    pub fn path(path: impl Into<String>) -> Self {
        Self::new(path).expect("route targets must be app-local paths")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_path(self) -> String {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteTargetError {
    Empty,
    NotAppLocalPath,
}

fn is_app_local_route_target(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains('\\')
}

/// Reason a route cannot continue through the normal mount path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteRejection {
    Unauthorized,
    Forbidden(&'static str),
    Blocked(&'static str),
    NotFound,
    Server(&'static str),
    Custom { reason: &'static str },
}

impl RouteRejection {
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            RouteRejection::Unauthorized => "guard_unauthorized",
            RouteRejection::Forbidden(_) => "guard_forbidden",
            RouteRejection::Blocked(_) => "guard_blocked",
            RouteRejection::NotFound => "guard_not_found",
            RouteRejection::Server(_) => "guard_server_error",
            RouteRejection::Custom { reason } => reason,
        }
    }
}

/// Decision returned by a sync route guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteGuardDecision {
    Allow,
    Reject(RouteRejection),
    Redirect(RouteTarget),
}

/// Sync guard evaluated before a route component mounts.
pub trait RouteGuard: 'static {
    fn decide(&self, ctx: &RouteContext<'_>) -> RouteGuardDecision;
}

impl<F> RouteGuard for F
where
    F: for<'a> Fn(&RouteContext<'a>) -> RouteGuardDecision + 'static,
{
    fn decide(&self, ctx: &RouteContext<'_>) -> RouteGuardDecision {
        self(ctx)
    }
}

/// Context passed to route rejection handlers.
pub struct RouteRejectionContext<'a> {
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
}

/// Action a route rejection handler can take for a rejected navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteRejectionAction {
    Redirect(RouteTarget),
    Paint(RouteErrorSurface),
    AbortNavigation,
}

/// Generic route error surface painted when route control stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteErrorSurface {
    pub title: &'static str,
    pub message: &'static str,
}

impl RouteErrorSurface {
    pub const fn new(title: &'static str, message: &'static str) -> Self {
        Self { title, message }
    }

    pub(crate) fn for_rejection(rejection: &RouteRejection) -> Self {
        match rejection {
            RouteRejection::Unauthorized => Self::new(
                "Authentication required",
                "Sign in to continue to this route.",
            ),
            RouteRejection::Forbidden(_) => {
                Self::new("Access denied", "You do not have access to this route.")
            }
            RouteRejection::Blocked(_) => Self::new(
                "Route unavailable",
                "This route is not available right now.",
            ),
            RouteRejection::NotFound => Self::new("Route not found", "No route matched this URL."),
            RouteRejection::Server(_) => Self::new(
                "Route unavailable",
                "This route could not be loaded right now.",
            ),
            RouteRejection::Custom { .. } => Self::new(
                "Route unavailable",
                "This route is not available right now.",
            ),
        }
    }
}

/// App/plugin extension point for rejected route navigations.
pub trait RouteRejectionHandler: 'static {
    fn handle(
        &self,
        ctx: &RouteRejectionContext<'_>,
        rejection: &RouteRejection,
    ) -> Option<RouteRejectionAction>;
}

impl<F> RouteRejectionHandler for F
where
    F: for<'a> Fn(&RouteRejectionContext<'a>, &RouteRejection) -> Option<RouteRejectionAction>
        + 'static,
{
    fn handle(
        &self,
        ctx: &RouteRejectionContext<'_>,
        rejection: &RouteRejection,
    ) -> Option<RouteRejectionAction> {
        self(ctx, rejection)
    }
}

/// Owned per-navigation context handed to a route loader.
///
/// All fields are owned (`String` / `HashMap`) so the loader's
/// future can outlive the synchronous call that constructed the
/// context. The router clones the matched route's `params` /
/// `query` map at navigation time and passes the result through.
///
/// `navigation_token` records the [`RouteToken`] that was current
/// when the loader started; long-running loaders can poll
/// [`Self::is_navigation_active`] to short-circuit work once the
/// router has moved on (RFC-078 §5.10.5). The router itself drops
/// any result returned under a stale token, so honoring this is
/// only an optimisation — correctness of the painted view does
/// not depend on the loader checking.
#[derive(Clone, Debug)]
pub struct LoaderContext {
    pub path: String,
    pub params: HashMap<String, String>,
    pub query: HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
    pub navigation_token: crate::router::RouteToken,
}

impl LoaderContext {
    /// `true` when the navigation that started this loader is
    /// still the router's current one. Returning `false` means the
    /// user navigated away while the loader was in flight; the
    /// loader can early-exit (its result will be dropped anyway).
    pub fn is_navigation_active(&self) -> bool {
        crate::router::route_token_is_current(self.navigation_token)
    }
}

/// Failure modes a route loader can return.
///
/// `Unauthorized` and `Forbidden` flow through the existing
/// [`RouteRejection`] chain — the loader doesn't have to know
/// that the auth plugin is the eventual handler. `NotFound` and
/// `Server` likewise dispatch through the rejection chain so apps
/// can wire a single error surface that handles both
/// guard-rejection and loader-failure.
///
/// `From<ServerError>` lets a loader body call `?` on a
/// `#[server]` invocation and surface the right router signal
/// automatically.
#[derive(Debug)]
pub enum LoaderError {
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    Server(ServerError),
}

impl From<ServerError> for LoaderError {
    fn from(err: ServerError) -> Self {
        match err {
            ServerError::Unauthorized(_) => LoaderError::Unauthorized,
            ServerError::Forbidden(reason) => LoaderError::Forbidden(reason),
            // App / BadRequest / Network all collapse into Server
            // for routing purposes — the loader closure is free to
            // pre-translate them to NotFound/Forbidden if it has
            // app-specific knowledge.
            other => LoaderError::Server(other),
        }
    }
}

impl LoaderError {
    /// Convert a loader failure into the matching route rejection.
    /// The dynamic message string is **dropped** at this boundary;
    /// per RFC-078 §5.10.7 the rejection chain operates on stable
    /// closed-set identifiers, never on user-visible error
    /// strings. Apps that need to surface the original message
    /// should consume `LoaderError` directly through a custom
    /// route-error component (see [`RouteErrorSurface`]).
    pub fn to_rejection(&self) -> RouteRejection {
        match self {
            LoaderError::Unauthorized => RouteRejection::Unauthorized,
            LoaderError::Forbidden(_) => RouteRejection::Forbidden("loader_forbidden"),
            LoaderError::NotFound(_) => RouteRejection::NotFound,
            LoaderError::Server(_) => RouteRejection::Server("loader_server_error"),
        }
    }
}

/// Pinned future returned by [`RouteLoader::run`]. Erases the
/// concrete loader output type into `Box<dyn Any>` so the router
/// can drive every route's loader through a uniform call site;
/// the [`Loader<T>`] extractor downcasts the contents inside the
/// component's lifecycle hook.
pub type RouteLoaderFuture = Pin<Box<dyn Future<Output = Result<Box<dyn Any>, LoaderError>>>>;

/// Trait-erased route loader stored in the route registry.
///
/// `RouteConfig::loader(...)` accepts any closure returning a
/// future of `Result<T, LoaderError>` and wraps it into this
/// trait object.
pub trait RouteLoader: 'static {
    fn run(&self, ctx: LoaderContext) -> RouteLoaderFuture;
}

struct LoaderClosure<F, T> {
    f: F,
    _t: PhantomData<fn() -> T>,
}

impl<F, Fut, T> RouteLoader for LoaderClosure<F, T>
where
    F: Fn(LoaderContext) -> Fut + 'static,
    Fut: Future<Output = Result<T, LoaderError>> + 'static,
    T: 'static,
{
    fn run(&self, ctx: LoaderContext) -> RouteLoaderFuture {
        let fut = (self.f)(ctx);
        Box::pin(async move { fut.await.map(|t| Box::new(t) as Box<dyn Any>) })
    }
}

/// Loader-produced data extracted in a component lifecycle hook.
///
/// Mirrors the [`crate::plugin::Plugin<T>`] shape: held as an
/// `Rc<T>`, dereferences to `&T`, populated by the router into a
/// thread-local pending slot just before the component mounts and
/// taken by the [`From<LifecycleContext>`] impl during setup.
///
/// `Option<Loader<T>>` is supported for components that may also
/// be mounted via `App::mount_subtree` (no router, no loader);
/// required `Loader<T>` panics if the slot is empty or the stored
/// value's type doesn't match `T`.
pub struct Loader<T: 'static> {
    data: Rc<T>,
}

impl<T: 'static> Clone for Loader<T> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
        }
    }
}

impl<T: 'static> Loader<T> {
    pub fn get(&self) -> &T {
        &self.data
    }
}

impl<T: 'static> std::ops::Deref for Loader<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T: 'static> From<LifecycleContext<'_>> for Loader<T> {
    fn from(_: LifecycleContext<'_>) -> Self {
        match crate::router::take_pending_loader_data::<T>() {
            Some(loader) => loader,
            None => panic!(
                "Loader<{}>: no loader data is available for the mounting \
                 component. Either the route has no `RouteConfig::loader(...)` \
                 or the component is being mounted via `App::mount_subtree` \
                 (in which case use `Option<Loader<{}>>`).",
                std::any::type_name::<T>(),
                std::any::type_name::<T>(),
            ),
        }
    }
}

impl<T: 'static> From<LifecycleContext<'_>> for Option<Loader<T>> {
    fn from(_: LifecycleContext<'_>) -> Self {
        crate::router::take_pending_loader_data::<T>()
    }
}

/// Internal: wrap a typed loader closure in a [`Loader<T>`] from
/// raw `Box<dyn Any>` data. Called by the router pending-slot
/// machinery; `pub(crate)` because it's an internal seam, not API.
pub(crate) fn loader_from_any<T: 'static>(boxed: Box<dyn Any>) -> Result<Loader<T>, Box<dyn Any>> {
    boxed.downcast::<T>().map(|t| Loader { data: Rc::new(*t) })
}

/// Route-local configuration for component `C`.
#[derive(Clone)]
pub struct RouteConfig<C: Component> {
    pub(crate) guards: Vec<Rc<dyn RouteGuard>>,
    pub(crate) loader: Option<Rc<dyn RouteLoader>>,
    _component: PhantomData<fn() -> C>,
}

impl<C: Component> RouteConfig<C> {
    pub fn new() -> Self {
        Self {
            guards: Vec::new(),
            loader: None,
            _component: PhantomData,
        }
    }

    pub fn guard(mut self, guard: impl RouteGuard) -> Self {
        self.guards.push(Rc::new(guard));
        self
    }

    /// Attach an async loader to this route. The router runs the
    /// loader after guards Allow and before the component mounts;
    /// the produced `T` is read by a [`Loader<T>`] extractor in
    /// the component's lifecycle hook.
    ///
    /// **Only one loader per route.** Calling `loader` a second
    /// time panics at config-build time — multiple parallel
    /// fetches should compose inside a single loader body
    /// (`try_join!` returning a struct).
    pub fn loader<F, Fut, T>(mut self, loader: F) -> Self
    where
        F: Fn(LoaderContext) -> Fut + 'static,
        Fut: Future<Output = Result<T, LoaderError>> + 'static,
        T: 'static,
    {
        if self.loader.is_some() {
            panic!(
                "RouteConfig::loader called twice — only one loader per \
                 route is supported. Compose multiple async fetches inside \
                 a single loader body (`tokio::try_join!` returning a struct \
                 of all the data the route needs)."
            );
        }
        self.loader = Some(Rc::new(LoaderClosure {
            f: loader,
            _t: PhantomData,
        }));
        self
    }

    pub(crate) fn into_runtime(self) -> router::RouteRuntimeConfig {
        router::RouteRuntimeConfig {
            guards: self.guards,
            loader: self.loader,
        }
    }
}

impl<C: Component> Default for RouteConfig<C> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod route_config_tests {
    use super::*;

    struct TestRoute;

    impl Component for TestRoute {
        const NAME: &'static str = "test-route";

        fn register() {}
    }

    impl RouteComponent for TestRoute {}

    #[test]
    fn route_component_default_config_has_no_guards() {
        let config = TestRoute::config();
        assert!(config.guards.is_empty());
    }

    #[test]
    fn route_config_stores_sync_guards() {
        let config = RouteConfig::<TestRoute>::new()
            .guard(|_: &RouteContext<'_>| RouteGuardDecision::Reject(RouteRejection::Blocked("x")));
        assert_eq!(config.guards.len(), 1);

        let params = HashMap::new();
        let query = HashMap::new();
        let ctx = RouteContext {
            path: "/test",
            params: &params,
            query: &query,
            matched_pattern: Some("/test"),
        };
        assert_eq!(
            config.guards[0].decide(&ctx),
            RouteGuardDecision::Reject(RouteRejection::Blocked("x"))
        );
    }

    #[test]
    fn route_target_accepts_only_app_local_paths() {
        assert_eq!(RouteTarget::path("/login").into_path(), "/login");
        assert_eq!(
            RouteTarget::new("https://example.com/login"),
            Err(RouteTargetError::NotAppLocalPath)
        );
        assert_eq!(
            RouteTarget::new("//example.com/login"),
            Err(RouteTargetError::NotAppLocalPath)
        );
        assert_eq!(RouteTarget::new(""), Err(RouteTargetError::Empty));
    }

    #[test]
    fn app_records_route_rejection_handlers() {
        let app = App::new().route_rejection_handler(
            |_: &RouteRejectionContext<'_>, _: &RouteRejection| {
                Some(RouteRejectionAction::AbortNavigation)
            },
        );

        assert_eq!(app.route_rejection_handlers.len(), 1);
    }

    #[test]
    fn route_rejection_handler_closure_can_redirect() {
        let handler = |ctx: &RouteRejectionContext<'_>, rejection: &RouteRejection| {
            assert_eq!(ctx.path, "/admin");
            assert_eq!(ctx.params.get("section"), Some(&"users".to_string()));
            assert_eq!(ctx.query.get("tab"), Some(&"active".to_string()));
            assert_eq!(ctx.matched_pattern, Some("/admin/:section"));
            assert_eq!(rejection, &RouteRejection::Unauthorized);
            Some(RouteRejectionAction::Redirect(RouteTarget::path("/login")))
        };
        let mut params = HashMap::new();
        params.insert("section".to_string(), "users".to_string());
        let mut query = HashMap::new();
        query.insert("tab".to_string(), "active".to_string());
        let ctx = RouteRejectionContext {
            path: "/admin",
            params: &params,
            query: &query,
            matched_pattern: Some("/admin/:section"),
        };

        assert_eq!(
            handler.handle(&ctx, &RouteRejection::Unauthorized),
            Some(RouteRejectionAction::Redirect(RouteTarget::path("/login")))
        );
    }

    #[test]
    fn loader_error_from_server_error_maps_unauthorized() {
        let err: LoaderError = ServerError::Unauthorized("token expired".into()).into();
        assert!(matches!(err, LoaderError::Unauthorized));

        let err: LoaderError = ServerError::Forbidden("missing role".into()).into();
        assert!(matches!(err, LoaderError::Forbidden(reason) if reason == "missing role"));

        let err: LoaderError = ServerError::App("kaboom".into()).into();
        assert!(matches!(err, LoaderError::Server(_)));

        let err: LoaderError = ServerError::BadRequest("nope".into()).into();
        assert!(matches!(err, LoaderError::Server(_)));
    }

    #[test]
    fn loader_error_to_rejection_drops_dynamic_messages() {
        // §5.10.7: rejection-chain reasons are stable closed-set
        // identifiers, never user-visible error strings. The
        // mapping enforces that — dynamic messages stay inside the
        // `LoaderError` value the app gets via a custom error
        // surface but never reach the rejection chain.
        assert_eq!(
            LoaderError::Unauthorized.to_rejection(),
            RouteRejection::Unauthorized
        );
        assert_eq!(
            LoaderError::Forbidden("policy:account_locked".into()).to_rejection(),
            RouteRejection::Forbidden("loader_forbidden")
        );
        assert_eq!(
            LoaderError::NotFound("user 42".into()).to_rejection(),
            RouteRejection::NotFound
        );
        assert_eq!(
            LoaderError::Server(ServerError::App("db down".into())).to_rejection(),
            RouteRejection::Server("loader_server_error")
        );
    }

    #[test]
    fn route_config_loader_records_one_loader() {
        let config = RouteConfig::<TestRoute>::new()
            .loader(|_ctx: LoaderContext| async move { Ok::<_, LoaderError>(42_u32) });
        assert!(config.loader.is_some());
    }

    #[test]
    #[should_panic(expected = "RouteConfig::loader called twice")]
    fn route_config_loader_panics_on_duplicate_registration() {
        let _ = RouteConfig::<TestRoute>::new()
            .loader(|_: LoaderContext| async move { Ok::<_, LoaderError>(1_u32) })
            .loader(|_: LoaderContext| async move { Ok::<_, LoaderError>(2_u32) });
    }

    #[test]
    fn loader_from_any_downcasts_correctly() {
        let boxed: Box<dyn Any> = Box::new("hello".to_string());
        let loader = loader_from_any::<String>(boxed).expect("downcast");
        assert_eq!(*loader.get(), "hello");
        // `Loader<T>` derefs to `&T`.
        assert_eq!(loader.len(), 5);
    }

    #[test]
    fn loader_from_any_returns_box_on_type_mismatch() {
        let boxed: Box<dyn Any> = Box::new(42_u32);
        let result = loader_from_any::<String>(boxed);
        assert!(result.is_err(), "type mismatch must surface as Err");
    }

    #[test]
    fn route_error_surface_uses_generic_messages() {
        assert_eq!(
            RouteErrorSurface::for_rejection(&RouteRejection::Forbidden("secret policy name")),
            RouteErrorSurface::new("Access denied", "You do not have access to this route.")
        );
        assert_eq!(
            RouteErrorSurface::for_rejection(&RouteRejection::Server("database exploded")),
            RouteErrorSurface::new(
                "Route unavailable",
                "This route could not be loaded right now."
            )
        );
    }
}

type Hook = Box<dyn FnOnce()>;

/// App-level extension point.
///
/// Plugins receive the in-progress [`App`] builder and return it after
/// installing lifecycle hooks, stores, devtools, logging, analytics, or other
/// app-level wiring. This keeps optional integrations out of `pocopine-core`:
/// a separate crate can expose a plugin value, and applications opt into it
/// from their entrypoint.
pub trait AppPlugin {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn install(self, app: App) -> App;
}

impl<F> AppPlugin for F
where
    F: FnOnce(App) -> App,
{
    fn install(self, app: App) -> App {
        self(app)
    }
}

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
    route_rejection_handlers: Vec<Rc<dyn RouteRejectionHandler>>,
    plugins: crate::plugin::PluginRegistry,
    installing_plugin: Option<&'static str>,
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

    /// Install an app-level plugin.
    ///
    /// The plugin runs while the builder is still being assembled, before
    /// registry verification and mount work. External crates should prefer
    /// this over asking applications to patch core startup logic.
    pub fn plugin<P: AppPlugin>(mut self, plugin: P) -> Self {
        let name = plugin.name();
        let previous = self.installing_plugin.replace(name);
        let mut app = plugin.install(self);
        app.installing_plugin = previous;
        app
    }

    /// Provide a typed runtime service to component lifecycle hooks and
    /// framework plugin hooks.
    ///
    /// Components extract this service with `Plugin<T>` or
    /// `Option<Plugin<T>>` from `on_setup`, `on_mount`, `on_ready`, and
    /// `on_unmount`. This is the primary extension path for reusable
    /// components: the app installs one capability, and every component that
    /// knows how to use it can opt in without being listed by the plugin.
    pub fn provide_plugin<T: 'static>(mut self, service: T) -> Self {
        self.plugins.provide(service, self.installing_plugin);
        self
    }

    /// Dispatch framework event `E` to the installed plugin service `T`.
    ///
    /// `T` must have been provided with [`Self::provide_plugin`] and must
    /// implement [`crate::Hook<E>`].
    pub fn hook_plugin<T, E>(mut self) -> Self
    where
        T: crate::plugin::Hook<E> + 'static,
        E: Clone + 'static,
    {
        self.plugins.hook_plugin::<T, E>(self.installing_plugin);
        self
    }

    /// Dispatch framework event `E` for component `C` to plugin service `T`.
    ///
    /// The service implements `Hook<ForComponent<C, E>>`, so the component
    /// filter is carried in the type system and the runtime performs the
    /// component-name match before invoking the hook. Use this for
    /// app-specific overrides or special cases where the plugin intentionally
    /// targets a known component type. Reusable component families should
    /// normally opt into a provided capability with `Plugin<T>` or
    /// `Option<Plugin<T>>` instead.
    pub fn hook_component_plugin<T, C, E>(mut self) -> Self
    where
        T: crate::plugin::Hook<crate::plugin::ForComponent<C, E>> + 'static,
        C: Component + 'static,
        E: crate::plugin::ComponentEvent,
    {
        self.plugins
            .hook_component_plugin::<T, C, E>(self.installing_plugin);
        self
    }

    /// Register a route. `pattern` is a path with optional `:name`
    /// segments (`"/blog/:id"`) or the 404 fallback `"*"`. `C` must be
    /// a `#[component]` whose tag name is the kebab-case of its ident.
    /// Matching routes paint their component into the
    /// `<pp-outlet>` with captured params passed through as attributes.
    pub fn route<C: Component>(mut self, pattern: &'static str) -> Self {
        C::register();
        router::register_route(pattern, C::NAME);
        self.routes.push(pattern);
        self
    }

    /// Register a component route using its [`RouteComponent`] config.
    ///
    /// This is the source-compatible staging name for RFC 078. After
    /// the route config fields are populated and call sites migrate,
    /// this path can become the main [`Self::route`] implementation.
    pub fn route_component<C: RouteComponent>(self, pattern: &'static str) -> Self {
        self.route_with::<C>(pattern, C::config())
    }

    /// Register a route with explicit route-local configuration.
    ///
    /// This is the additive RFC 078 entrypoint used while the existing
    /// `route::<C>` API remains source-compatible. Once guards/loaders
    /// are implemented and call sites migrate, `route::<C:
    /// RouteComponent>` can become the primary shorthand for
    /// `route_with(pattern, C::config())`.
    pub fn route_with<C: Component>(
        mut self,
        pattern: &'static str,
        config: RouteConfig<C>,
    ) -> Self {
        C::register();
        router::register_route_with_config(pattern, C::NAME, config.into_runtime());
        self.routes.push(pattern);
        self
    }

    /// Install a generic route rejection handler.
    ///
    /// Plugins should use this instead of extending the base [`App`]
    /// with auth-specific methods such as login-route configuration.
    pub fn route_rejection_handler<H: RouteRejectionHandler>(mut self, handler: H) -> Self {
        self.route_rejection_handlers.push(Rc::new(handler));
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
        router::register_route(pattern, C::NAME);
        self.routes.push(pattern);
        self
    }

    /// **Internal — invoked by the `app!{}` macro; do not call directly.**
    /// Records a component name from the macro's static registry without
    /// calling its register function. This gives plugins a complete component
    /// manifest before [`Self::run_with_registry`] performs the authoritative
    /// registry walk.
    ///
    /// The recorded name is **manifest visibility, not registration** —
    /// plugins reading [`Self::registered_components`] will see entries
    /// from this method even though the underlying component is not yet
    /// in the runtime registry. Only [`Self::run_with_registry`]
    /// (driven by the macro's static `phf::Map`) and the eager
    /// [`Self::register`] / [`Self::route`] paths actually register a
    /// component for mount.
    #[doc(hidden)]
    pub fn component_static(mut self, name: &'static str) -> Self {
        self.components.push(name);
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
        let Self {
            components,
            stores: _,
            routes,
            before_mount,
            after_mount,
            route_rejection_handlers,
            plugins,
            installing_plugin: _,
            devtools,
        } = self;
        let boot_start_ms = js_sys::Date::now();
        clear_existing_boot_errors();
        if let Err(errors) = plugins.validate() {
            // Defensive reset: an earlier successful App::run on the
            // same wasm runtime would have activated its own
            // registry. Returning here without clearing would leave
            // those services and hooks active, so a subsequent
            // App::mount_subtree call (or a stray Plugin<T>
            // extractor) would resolve against stale plugin context
            // even though the framework has refused to mount this
            // app. Drop the previous registry first so failure is
            // observable as "no plugins" rather than "old plugins".
            crate::plugin::activate(crate::plugin::PluginRegistry::default());
            router::set_route_rejection_handlers(Vec::new());
            crate::plugin::render_plugin_boot_error(&errors);
            return;
        }
        crate::plugin::activate(plugins);
        router::set_route_rejection_handlers(route_rejection_handlers);
        crate::plugin::emit(crate::plugin::AppBootStarted {
            component_count: components.len(),
            route_count: routes.len(),
        });
        if let Err(errors) = crate::registry::verify_registry() {
            crate::plugin::emit(crate::plugin::AppBootFailed {
                reason: "component_registry",
            });
            crate::registry::render_boot_error(&errors);
            return;
        }
        // Inject the animate-preset atom stylesheet before any
        // component `register()` injects per-component styles, so
        // the preset atoms live earlier in the cascade and
        // component styles still win on specificity ties.
        crate::animate::install();
        for f in before_mount {
            f();
        }
        // RFC 061 Phase 2 — discover the [pp-app] root and mount
        // there. Whole-body mounting is gone; apps that want
        // multiple roots use `mount_subtree::<C>` instead.
        let Some(window) = web_sys::window() else {
            crate::plugin::emit(crate::plugin::AppBootFailed {
                reason: "missing_window",
            });
            return;
        };
        let Some(document) = window.document() else {
            crate::plugin::emit(crate::plugin::AppBootFailed {
                reason: "missing_document",
            });
            return;
        };
        let pp_app = document.query_selector("[pp-app]").ok().flatten();
        if let Some(host) = pp_app {
            mount_pp_app_subtree(&host);
        } else {
            crate::plugin::emit(crate::plugin::AppBootFailed {
                reason: "missing_pp_app_root",
            });
            render_missing_pp_app_root();
            return;
        }
        if !routes.is_empty() {
            router::init();
        }
        #[cfg(feature = "devtools")]
        if devtools {
            crate::devtools::install();
        }
        #[cfg(not(feature = "devtools"))]
        let _ = devtools;
        let after = after_mount;
        if !after.is_empty() {
            spawn_local(async move {
                let _ = js_sys::Promise::resolve(&JsValue::NULL);
                for f in after {
                    f();
                }
            });
        }
        let elapsed = js_sys::Date::now() - boot_start_ms;
        crate::plugin::emit(crate::plugin::AppBootCompleted {
            duration_ms: if elapsed.is_finite() && elapsed >= 0.0 {
                elapsed
            } else {
                0.0
            },
        });
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
    ///
    /// Subtree mounts inherit plugins from the most recent [`App::run`].
    /// If no app has run, `Plugin<T>` extractors observe an empty registry
    /// and `Option<Plugin<T>>` extractors return `None`.
    pub fn mount_subtree<C: Component>(host: &Element) -> SubtreeHandle {
        C::register();
        mount::mount_child_component(host, C::NAME);
        mount::finalize_compiled_subtree(host);
        SubtreeHandle {
            host: host.clone(),
            active: true,
        }
    }
}

fn clear_existing_boot_errors() {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Ok(nodes) = doc.query_selector_all("[data-pocopine-boot-error]") else {
        return;
    };
    for i in 0..nodes.length() {
        let Some(node) = nodes.item(i) else {
            continue;
        };
        if let Some(el) = node.dyn_ref::<Element>() {
            el.remove();
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

    /// Detach this handle from automatic cleanup. The caller takes
    /// responsibility for removing the host subtree later.
    pub fn leak(mut self) {
        self.active = false;
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
