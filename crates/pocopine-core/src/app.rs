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

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
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
///
/// Components without route-local config use
/// `#[derive(RouteComponent)]` (from `pocopine-macros`) instead of
/// writing the empty impl by hand; components overriding `config()`
/// keep the manual impl.
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

/// Route context passed to page metadata factories.
pub struct PageMetaContext<'a> {
    pub path: &'a str,
    pub full_path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub hash: Option<&'a str>,
    pub route_pattern: Option<&'static str>,
    pub component: Option<&'static str>,
}

/// Metadata that Pocopine manages in the document `<head>` for the
/// currently mounted page.
///
/// Route components attach this through [`RouteConfig::page_meta`].
/// Unlike [`RouteMeta`], this is document/head metadata: title,
/// description, canonical URL, Open Graph tags, robots directives,
/// and similar browser-visible page metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMeta {
    title: Option<String>,
    meta_tags: Vec<PageMetaTag>,
    links: Vec<PageLink>,
}

impl PageMeta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(self, content: impl Into<String>) -> Self {
        self.meta_name("description", content)
    }

    pub fn robots(self, content: impl Into<String>) -> Self {
        self.meta_name("robots", content)
    }

    pub fn canonical(self, href: impl Into<String>) -> Self {
        self.link("canonical", href)
    }

    pub fn og_title(self, content: impl Into<String>) -> Self {
        self.meta_property("og:title", content)
    }

    pub fn og_description(self, content: impl Into<String>) -> Self {
        self.meta_property("og:description", content)
    }

    pub fn meta_name(mut self, name: impl Into<String>, content: impl Into<String>) -> Self {
        self.meta_tags.push(PageMetaTag::Name {
            name: name.into(),
            content: content.into(),
        });
        self
    }

    pub fn meta_property(
        mut self,
        property: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.meta_tags.push(PageMetaTag::Property {
            property: property.into(),
            content: content.into(),
        });
        self
    }

    pub fn link(mut self, rel: impl Into<String>, href: impl Into<String>) -> Self {
        self.links.push(PageLink {
            rel: rel.into(),
            href: href.into(),
        });
        self
    }

    pub fn title_text(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn meta_tags(&self) -> &[PageMetaTag] {
        &self.meta_tags
    }

    pub fn links(&self) -> &[PageLink] {
        &self.links
    }

    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.meta_tags.is_empty() && self.links.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageMetaTag {
    Name { name: String, content: String },
    Property { property: String, content: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageLink {
    pub rel: String,
    pub href: String,
}

pub(crate) type PageMetaFactory = Rc<dyn for<'a> Fn(&PageMetaContext<'a>) -> PageMeta>;

/// Stable application-owned name for a route.
///
/// Named routes are optional. They exist for Rust-side navigation
/// where the caller wants parameter validation rather than stringly
/// assembling `/users/{id}` paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RouteName(&'static str);

impl RouteName {
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Query pairs attached to a [`RouteTarget`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteQuery {
    pairs: Vec<(String, String)>,
}

impl RouteQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pair(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.pairs.push((key.into(), value.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub(crate) fn append_to(&self, path: &mut String) {
        if self.pairs.is_empty() {
            return;
        }
        let hash = path.find('#').map(|idx| path.split_off(idx));
        let joiner = if path.contains('?') { '&' } else { '?' };
        path.push(joiner);
        for (idx, (key, value)) in self.pairs.iter().enumerate() {
            if idx > 0 {
                path.push('&');
            }
            push_encoded_route_query_part(key, path);
            path.push('=');
            push_encoded_route_query_part(value, path);
        }
        if let Some(hash) = hash {
            path.push_str(&hash);
        }
    }
}

impl<const N: usize> From<[(&str, &str); N]> for RouteQuery {
    fn from(value: [(&str, &str); N]) -> Self {
        let mut query = RouteQuery::new();
        for (key, val) in value {
            query = query.pair(key, val);
        }
        query
    }
}

/// Percent-encode one path segment for a Pocopine route URL.
///
/// This encodes `/`, `?`, `#`, spaces, and other reserved bytes so a
/// dynamic id can safely become exactly one route segment.
pub fn encode_route_path_segment(input: &str) -> String {
    let mut out = String::new();
    push_encoded_route_path_segment(input, &mut out);
    out
}

/// Percent-encode one query key or value for a Pocopine route URL.
pub fn encode_route_query_part(input: &str) -> String {
    let mut out = String::new();
    push_encoded_route_query_part(input, &mut out);
    out
}

/// Percent-encode one URL fragment value for a Pocopine route URL.
pub fn encode_route_fragment(input: &str) -> String {
    let mut out = String::new();
    push_encoded_route_query_part(input, &mut out);
    out
}

pub(crate) fn push_encoded_route_path_segment(input: &str, out: &mut String) {
    pocopine_codec::percent_encode_into(out, input);
}

pub(crate) fn push_encoded_route_query_part(input: &str, out: &mut String) {
    pocopine_codec::percent_encode_into(out, input);
}

/// Builder for app-local route URLs.
///
/// Use this when any part of the path, query, or fragment comes from
/// data. `segment(...)`, `query(...)`, and `hash(...)` encode their
/// inputs before producing the final [`RouteTarget`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteUrl {
    path: String,
    query: RouteQuery,
    hash: Option<String>,
}

impl RouteUrl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn root() -> Self {
        Self {
            path: "/".into(),
            query: RouteQuery::new(),
            hash: None,
        }
    }

    /// Start from an already-formed app-local path.
    pub fn path(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            query: RouteQuery::new(),
            hash: None,
        }
    }

    /// Append one encoded path segment.
    pub fn segment(mut self, value: impl AsRef<str>) -> Self {
        if self.path.is_empty() || !self.path.ends_with('/') {
            self.path.push('/');
        }
        push_encoded_route_path_segment(value.as_ref(), &mut self.path);
        self
    }

    pub fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query = self.query.pair(key, value);
        self
    }

    pub fn query_pairs(mut self, query: impl Into<RouteQuery>) -> Self {
        self.query = query.into();
        self
    }

    pub fn hash(mut self, hash: impl AsRef<str>) -> Self {
        self.hash = Some(encode_route_fragment(hash.as_ref()));
        self
    }

    pub fn into_string(self) -> String {
        let mut out = if self.path.is_empty() {
            "/".to_string()
        } else {
            self.path
        };
        let existing_hash = out.find('#').map(|idx| out.split_off(idx));
        self.query.append_to(&mut out);
        if let Some(hash) = self.hash {
            out.push('#');
            out.push_str(&hash);
        } else if let Some(hash) = existing_hash {
            out.push_str(&hash);
        }
        out
    }

    pub fn target(self) -> Result<RouteTarget, RouteTargetError> {
        RouteTarget::new(self.into_string())
    }
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
        if route_target_path(&path).starts_with("/_pocopine") {
            return Err(RouteTargetError::ReservedNamespace);
        }
        Ok(Self(path))
    }

    pub fn path(path: impl Into<String>) -> Self {
        match Self::new(path) {
            Ok(target) => target,
            Err(err) => panic!("invalid route target: {err}"),
        }
    }

    pub fn path_with_query(
        path: impl Into<String>,
        query: impl Into<RouteQuery>,
    ) -> Result<Self, RouteTargetError> {
        RouteUrl::path(path).query_pairs(query).target()
    }

    pub fn url(url: RouteUrl) -> Result<Self, RouteTargetError> {
        url.target()
    }

    pub fn named(name: RouteName) -> RouteTargetBuilder {
        RouteTargetBuilder::new(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_path(self) -> String {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteTargetError {
    Empty,
    NotAppLocalPath,
    ReservedNamespace,
    UnknownRouteName(&'static str),
    DuplicateRouteName(&'static str),
    MissingParam(String),
    EmptyParam(String),
    UnbuildablePattern(&'static str),
}

impl fmt::Display for RouteTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("route target is empty"),
            Self::NotAppLocalPath => f.write_str("route target must be an app-local path"),
            Self::ReservedNamespace => {
                f.write_str("route target uses the reserved /_pocopine namespace")
            }
            Self::UnknownRouteName(name) => write!(f, "unknown route name `{name}`"),
            Self::DuplicateRouteName(name) => write!(f, "duplicate route name `{name}`"),
            Self::MissingParam(param) => write!(f, "missing route param `{param}`"),
            Self::EmptyParam(param) => write!(f, "route param `{param}` is empty"),
            Self::UnbuildablePattern(pattern) => {
                write!(f, "route pattern `{pattern}` cannot be built")
            }
        }
    }
}

impl std::error::Error for RouteTargetError {}

fn is_app_local_route_target(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains('\\')
}

fn route_target_path(target: &str) -> &str {
    target
        .split(['?', '#'])
        .next()
        .filter(|path| !path.is_empty())
        .unwrap_or("/")
}

/// Builder returned by [`RouteTarget::named`].
#[derive(Clone, Debug)]
pub struct RouteTargetBuilder {
    name: RouteName,
    params: HashMap<String, String>,
    query: RouteQuery,
}

impl RouteTargetBuilder {
    fn new(name: RouteName) -> Self {
        Self {
            name,
            params: HashMap::new(),
            query: RouteQuery::new(),
        }
    }

    pub fn param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    pub fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query = self.query.pair(key, value);
        self
    }

    pub fn query_pairs(mut self, query: impl Into<RouteQuery>) -> Self {
        self.query = query.into();
        self
    }

    pub fn build(self) -> Result<RouteTarget, RouteTargetError> {
        crate::router::target_for_name(self.name, &self.params, self.query)
    }
}

/// Convert common caller inputs into a validated [`RouteTarget`].
pub trait IntoRouteTarget {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError>;
}

impl IntoRouteTarget for RouteTarget {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError> {
        Ok(self)
    }
}

impl IntoRouteTarget for &RouteTarget {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError> {
        Ok(self.clone())
    }
}

impl IntoRouteTarget for &str {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError> {
        RouteTarget::new(self)
    }
}

impl IntoRouteTarget for String {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError> {
        RouteTarget::new(self)
    }
}

impl IntoRouteTarget for RouteUrl {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError> {
        self.target()
    }
}

impl IntoRouteTarget for &RouteUrl {
    fn into_route_target(self) -> Result<RouteTarget, RouteTargetError> {
        self.clone().target()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RouteMetaId {
    name: &'static str,
    type_id: TypeId,
}

/// Typed key for route metadata.
///
/// Metadata is declared inside `RouteComponent::config()` through
/// [`RouteConfig::meta`]. This keeps route policy local to the route
/// component while preserving Pocopine's central `App::route(...)`
/// URL table. This is Vue Router-style route metadata for app UI and
/// policy; document head metadata belongs in a separate `PageMeta`
/// surface.
#[derive(Debug)]
pub struct RouteMetaKey<T: 'static> {
    name: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T: 'static> Clone for RouteMetaKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> Copy for RouteMetaKey<T> {}

impl<T: 'static> RouteMetaKey<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }

    pub const fn name(self) -> &'static str {
        self.name
    }

    fn id(self) -> RouteMetaId {
        RouteMetaId {
            name: self.name,
            type_id: TypeId::of::<T>(),
        }
    }
}

/// Typed route-record metadata.
///
/// Use this for data that belongs to the route graph: side-nav labels,
/// breadcrumbs, access hints, layout flags, analytics names, and other
/// route-adjacent UI policy. It is intentionally separate from page
/// metadata such as title, description, canonical URL, or Open Graph
/// tags.
#[derive(Clone, Default)]
pub struct RouteMeta {
    entries: Vec<RouteMetaEntry>,
}

impl fmt::Debug for RouteMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouteMeta")
            .field("len", &self.entries.len())
            .finish()
    }
}

#[derive(Clone)]
struct RouteMetaEntry {
    id: RouteMetaId,
    value: Rc<dyn Any>,
}

impl RouteMeta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<T: 'static>(&mut self, key: RouteMetaKey<T>, value: T) {
        let id = key.id();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.value = Rc::new(value);
            return;
        }
        self.entries.push(RouteMetaEntry {
            id,
            value: Rc::new(value),
        });
    }

    pub fn get<T: 'static>(&self, key: RouteMetaKey<T>) -> Option<&T> {
        let id = key.id();
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.value.as_ref().downcast_ref::<T>())
    }

    pub fn contains<T: 'static>(&self, key: RouteMetaKey<T>) -> bool {
        self.get(key).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Route prefetch policy.
///
/// The trigger controls how directives may schedule a prefetch. The
/// `loader` flag is opt-in because loader functions may be expensive
/// or touch app data; plain route/code prefetch is the conservative
/// default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefetch {
    trigger: PrefetchTrigger,
    loader: bool,
}

impl Prefetch {
    pub const fn none() -> Self {
        Self {
            trigger: PrefetchTrigger::Never,
            loader: false,
        }
    }

    pub const fn on_intent() -> Self {
        Self {
            trigger: PrefetchTrigger::Intent,
            loader: false,
        }
    }

    pub const fn on_visible() -> Self {
        Self {
            trigger: PrefetchTrigger::Visible,
            loader: false,
        }
    }

    pub const fn loader(mut self) -> Self {
        self.loader = true;
        self
    }

    pub const fn trigger(self) -> PrefetchTrigger {
        self.trigger
    }

    pub const fn includes_loader(self) -> bool {
        self.loader
    }
}

impl Default for Prefetch {
    fn default() -> Self {
        Self::none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefetchTrigger {
    Never,
    Intent,
    Visible,
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

/// Where in the navigation pipeline a [`RouteRejection`]
/// originated. Drives the stable identifiers fired in
/// `RouteNavigationFailed` so observers can tell guard-side and
/// loader-side rejections apart (RFC-078 §5.10.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RejectionSource {
    /// Synchronous guard (`RouteConfig::guard`) returned `Reject`
    /// or `Redirect`.
    Guard,
    /// Async loader (`RouteConfig::loader`) returned an `Err`
    /// that mapped to a `RouteRejection`.
    Loader,
}

impl RouteRejection {
    /// Stable closed-set `reason` identifier for this rejection
    /// **as observed from `source`**. The same `RouteRejection`
    /// produces `"guard_unauthorized"` when a guard fired it and
    /// `"loader_unauthorized"` when a loader fired it; observability
    /// pipelines that aggregate by `reason` can split traffic by
    /// origin without parsing the rejection enum.
    pub(crate) fn reason(&self, source: RejectionSource) -> &'static str {
        match (source, self) {
            (RejectionSource::Guard, RouteRejection::Unauthorized) => "guard_unauthorized",
            (RejectionSource::Loader, RouteRejection::Unauthorized) => "loader_unauthorized",
            (RejectionSource::Guard, RouteRejection::Forbidden(_)) => "guard_forbidden",
            (RejectionSource::Loader, RouteRejection::Forbidden(_)) => "loader_forbidden",
            (RejectionSource::Guard, RouteRejection::Blocked(_)) => "guard_blocked",
            (RejectionSource::Loader, RouteRejection::Blocked(_)) => "loader_blocked",
            (RejectionSource::Guard, RouteRejection::NotFound) => "guard_not_found",
            (RejectionSource::Loader, RouteRejection::NotFound) => "loader_not_found",
            (RejectionSource::Guard, RouteRejection::Server(_)) => "guard_server_error",
            (RejectionSource::Loader, RouteRejection::Server(_)) => "loader_server_error",
            // Custom rejections own their reason string; both
            // sources surface the user-supplied identifier
            // verbatim. Apps that want a source-distinguished
            // custom reason should encode the source into their
            // chosen identifier.
            (_, RouteRejection::Custom { reason }) => reason,
        }
    }
}

/// Decision returned by a sync route guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteGuardDecision {
    Allow,
    /// Guard cannot decide yet because an async client-side
    /// prerequisite is still hydrating. The router leaves the
    /// current outlet untouched and waits for the owner of that
    /// prerequisite to call [`crate::reevaluate_current`].
    Pending,
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
/// not depend on the loader checking. `abort_signal` is also
/// inherited automatically by generated `#[server]` calls made while
/// the loader future is being polled, so route supersession cancels
/// the underlying browser fetch.
#[derive(Clone, Debug)]
pub struct LoaderContext {
    pub path: String,
    pub params: HashMap<String, String>,
    pub query: HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
    pub navigation_token: crate::router::RouteToken,
    pub(crate) abort_signal: Option<web_sys::AbortSignal>,
}

impl LoaderContext {
    /// `true` when the navigation that started this loader is
    /// still the router's current one. Returning `false` means the
    /// user navigated away while the loader was in flight; the
    /// loader can early-exit (its result will be dropped anyway).
    pub fn is_navigation_active(&self) -> bool {
        crate::router::route_token_is_current(self.navigation_token)
    }

    /// Browser abort signal for this route navigation, when the
    /// platform supplied one. Generated `#[server]` stubs inherit this
    /// automatically inside loader futures; direct `fetch::call`
    /// users can also pass it through `FetchOptions`.
    pub fn abort_signal(&self) -> Option<web_sys::AbortSignal> {
        self.abort_signal.clone()
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
        let abort_signal = ctx.abort_signal();
        let fut = (self.f)(ctx);
        Box::pin(crate::fetch::with_abort_signal_future(
            abort_signal,
            async move { fut.await.map(|t| Box::new(t) as Box<dyn Any>) },
        ))
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
    fn from(ctx: LifecycleContext<'_>) -> Self {
        match crate::router::take_pending_loader_data::<T>(ctx.scope_id) {
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
    fn from(ctx: LifecycleContext<'_>) -> Self {
        crate::router::take_pending_loader_data::<T>(ctx.scope_id)
    }
}

impl<T: 'static> Loader<T> {
    pub(crate) fn from_rc(data: Rc<T>) -> Self {
        Self { data }
    }
}

/// Route-local configuration for component `C`.
#[derive(Clone)]
pub struct RouteConfig<C: Component> {
    pub(crate) name: Option<RouteName>,
    pub(crate) meta: RouteMeta,
    pub(crate) page_meta: Option<PageMetaFactory>,
    pub(crate) guards: Vec<Rc<dyn RouteGuard>>,
    pub(crate) loader: Option<Rc<dyn RouteLoader>>,
    pub(crate) prefetch: Prefetch,
    _component: PhantomData<fn() -> C>,
}

impl<C: Component> RouteConfig<C> {
    pub fn new() -> Self {
        Self {
            name: None,
            meta: RouteMeta::new(),
            page_meta: None,
            guards: Vec::new(),
            loader: None,
            prefetch: Prefetch::none(),
            _component: PhantomData,
        }
    }

    /// Give this route a stable Rust-side name for typed navigation.
    pub fn name(mut self, name: RouteName) -> Self {
        self.name = Some(name);
        self
    }

    /// Attach typed metadata to this route.
    ///
    /// Route metadata is route policy/configuration, not component
    /// state or document head metadata. Define static keys near the
    /// plugin or feature that consumes them, then attach values from
    /// `RouteComponent::config()`.
    pub fn meta<T: 'static>(mut self, key: RouteMetaKey<T>, value: T) -> Self {
        self.meta.insert(key, value);
        self
    }

    /// Attach document/head metadata for this route.
    ///
    /// The factory runs after a successful navigation and receives the
    /// resolved route params/query. Use this for page title,
    /// description, canonical URL, Open Graph tags, and similar
    /// browser-visible metadata.
    pub fn page_meta(
        mut self,
        meta: impl for<'a> Fn(&PageMetaContext<'a>) -> PageMeta + 'static,
    ) -> Self {
        self.page_meta = Some(Rc::new(meta));
        self
    }

    /// Attach static document/head metadata for this route.
    pub fn static_page_meta(mut self, meta: PageMeta) -> Self {
        self.page_meta = Some(Rc::new(move |_| meta.clone()));
        self
    }

    /// Attach a synchronous client-side guard.
    ///
    /// Guards are a paint/UX primitive, not an authorization
    /// boundary. Any sensitive data touched by the route must still be
    /// protected by server-side `#[server(guard = ...)]` checks.
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

    /// Configure route prefetch behavior.
    ///
    /// Route/code prefetch is conservative; loader prefetch happens
    /// only when [`Prefetch::loader`] is set. Guards still run before
    /// a loader prefetch, so rejected or pending routes do not fetch
    /// loader data speculatively.
    pub fn prefetch(mut self, prefetch: Prefetch) -> Self {
        self.prefetch = prefetch;
        self
    }

    pub(crate) fn into_runtime(self) -> router::RouteRuntimeConfig {
        router::RouteRuntimeConfig {
            name: self.name,
            meta: self.meta,
            page_meta: self.page_meta,
            guards: self.guards,
            loader: self.loader,
            prefetch: self.prefetch,
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

    // Hand-written: `#[derive(RouteComponent)]` expands to
    // `::pocopine::RouteComponent`, which core can't depend on.
    impl RouteComponent for TestRoute {}

    #[test]
    fn route_component_default_config_has_no_guards() {
        let config = TestRoute::config();
        assert!(config.guards.is_empty());
    }

    #[test]
    fn route_config_records_name_and_prefetch_policy() {
        const TEST: RouteName = RouteName::new("test");

        let config = RouteConfig::<TestRoute>::new()
            .name(TEST)
            .prefetch(Prefetch::on_intent().loader());

        assert_eq!(config.name, Some(TEST));
        assert_eq!(config.prefetch.trigger(), PrefetchTrigger::Intent);
        assert!(config.prefetch.includes_loader());
    }

    #[test]
    fn route_config_records_typed_metadata() {
        const REQUIRES_AUTH: RouteMetaKey<bool> = RouteMetaKey::new("requires_auth");
        const SECTION: RouteMetaKey<&'static str> = RouteMetaKey::new("section");

        let config = RouteConfig::<TestRoute>::new()
            .meta(REQUIRES_AUTH, true)
            .meta(SECTION, "admin");

        assert_eq!(config.meta.get(REQUIRES_AUTH), Some(&true));
        assert_eq!(config.meta.get(SECTION), Some(&"admin"));
        assert!(config.meta.contains(REQUIRES_AUTH));
    }

    #[test]
    fn page_meta_builder_records_head_tags() {
        let meta = PageMeta::new()
            .title("Dashboard")
            .description("Team dashboard")
            .canonical("/dashboard")
            .og_title("Dashboard");

        assert_eq!(meta.title_text(), Some("Dashboard"));
        assert_eq!(
            meta.meta_tags(),
            &[
                PageMetaTag::Name {
                    name: "description".into(),
                    content: "Team dashboard".into()
                },
                PageMetaTag::Property {
                    property: "og:title".into(),
                    content: "Dashboard".into()
                }
            ]
        );
        assert_eq!(
            meta.links(),
            &[PageLink {
                rel: "canonical".into(),
                href: "/dashboard".into()
            }]
        );
    }

    #[test]
    fn route_config_records_page_meta_factory() {
        let mut params = HashMap::new();
        params.insert("id".into(), "42".into());
        let query = HashMap::new();
        let config = RouteConfig::<TestRoute>::new()
            .page_meta(|ctx| PageMeta::new().title(format!("Story {}", ctx.params["id"])));
        let ctx = PageMetaContext {
            path: "/story/42",
            full_path: "/story/42",
            params: &params,
            query: &query,
            hash: None,
            route_pattern: Some("/story/:id"),
            component: Some("test-route"),
        };

        let page_meta = config.page_meta.as_ref().expect("page meta")(&ctx);

        assert_eq!(page_meta.title_text(), Some("Story 42"));
    }

    // ── Store-reference scanner (RFC: missing-store boot-error) ──

    #[test]
    fn collect_store_names_extracts_single_ref() {
        let mut into = Vec::new();
        super::collect_store_names("$store.preferences.theme", &mut into);
        assert_eq!(into, vec!["preferences".to_string()]);
    }

    #[test]
    fn collect_store_names_handles_multiple_and_dedupes() {
        let mut into = Vec::new();
        // Walks every path in the AST — both ternary branches and
        // the condition. The repeated `$store.a` is deduped.
        super::collect_store_names("$store.a == $store.b ? $store.a : $store.b", &mut into);
        assert_eq!(into, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn collect_store_names_ignores_bare_store_token() {
        // `$store` without a `.` is the container object, not a
        // specific store. Don't claim it as a missing reference.
        let mut into = Vec::new();
        super::collect_store_names("$store", &mut into);
        assert!(into.is_empty());
    }

    #[test]
    fn collect_store_names_ignores_string_literal_lookalike() {
        // The whole point of the AST-based scanner: text that looks
        // like `$store.X` inside a string literal must NOT be picked
        // up as a missing-store reference. A substring scanner would
        // false-positive here.
        let mut into = Vec::new();
        super::collect_store_names("'$store.example was renamed'", &mut into);
        assert!(into.is_empty());
    }

    #[test]
    fn collect_store_names_finds_refs_in_call_arguments() {
        // Refs nested inside call arguments / binops must still be
        // discovered.
        let mut into = Vec::new();
        super::collect_store_names("debug($store.flags.verbose)", &mut into);
        assert_eq!(into, vec!["flags".to_string()]);
    }

    #[test]
    fn collect_store_names_silent_on_parse_failure() {
        // Macro-time parse failures are surfaced by the template
        // pipeline as `compile_error!`. This scanner runs at boot
        // on `&'static str` plan sources that already survived the
        // macro, but stays defensive: a parse failure here just
        // means "no refs to add", never a panic.
        let mut into = Vec::new();
        super::collect_store_names("status === 'queued'", &mut into);
        assert!(into.is_empty());
    }

    #[test]
    fn check_store_registrations_passes_when_complete() {
        // No vtables → no scanned refs → no missing.
        let result = super::check_store_registrations(&[], &["preferences"]);
        assert!(result.is_ok());
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
        assert_eq!(
            RouteTarget::new("/_pocopine/server-fn"),
            Err(RouteTargetError::ReservedNamespace)
        );
        assert_eq!(
            RouteTarget::new("/_pocopine"),
            Err(RouteTargetError::ReservedNamespace)
        );
        assert_eq!(
            RouteTarget::new("/_pocopine-secret/admin"),
            Err(RouteTargetError::ReservedNamespace)
        );
        assert_eq!(RouteTarget::new(""), Err(RouteTargetError::Empty));
    }

    #[test]
    fn route_target_builds_query_strings() {
        let target = RouteTarget::path_with_query(
            "/search",
            RouteQuery::from([("q", "router api"), ("tab", "a&b")]),
        )
        .unwrap();

        assert_eq!(target.into_path(), "/search?q=router%20api&tab=a%26b");

        let target = RouteTarget::path_with_query("/search#results", [("q", "router")]).unwrap();

        assert_eq!(target.into_path(), "/search?q=router#results");
    }

    #[test]
    fn route_url_encodes_segments_query_and_hash() {
        let target = RouteUrl::new()
            .segment("users")
            .segment("user/42")
            .query("tab", "a b")
            .hash("top#section")
            .target()
            .unwrap();

        assert_eq!(
            target.into_path(),
            "/users/user%2F42?tab=a%20b#top%23section"
        );
    }

    #[test]
    fn route_url_replaces_existing_hash_when_hash_is_set() {
        let target = RouteUrl::path("/reports#old")
            .query("tab", "active")
            .hash("new")
            .target()
            .unwrap();

        assert_eq!(target.into_path(), "/reports?tab=active#new");
    }

    #[test]
    fn route_encoding_helpers_are_public_and_stable() {
        assert_eq!(encode_route_path_segment("a/b c"), "a%2Fb%20c");
        assert_eq!(encode_route_query_part("a&b c"), "a%26b%20c");
        assert_eq!(encode_route_fragment("top#2"), "top%232");
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
    fn loader_from_rc_constructs_loader_from_shared_rc() {
        // The router migrates the one-shot pending value into a
        // per-scope `Rc` slot on first read; subsequent extractors
        // construct `Loader<T>` directly from that shared `Rc`
        // through `Loader::from_rc`.
        let rc: Rc<String> = Rc::new("hello".to_string());
        let strong_count_before = Rc::strong_count(&rc);
        let loader = Loader::<String>::from_rc(rc.clone());
        assert_eq!(*loader.get(), "hello");
        // Loader holds its own clone of the Rc.
        assert!(Rc::strong_count(&rc) > strong_count_before);
        // Deref to `&T`.
        assert_eq!(loader.len(), 5);
    }

    #[test]
    fn app_records_route_error_component() {
        let app = App::new().route_error_component::<TestRoute>();
        assert_eq!(app.route_error_component, Some(TestRoute::NAME));
    }

    #[test]
    fn app_records_not_found_component() {
        let app = App::new().not_found_component::<TestRoute>();
        assert_eq!(app.not_found_component, Some(TestRoute::NAME));
    }

    #[test]
    fn app_overrides_default_to_none_for_route_error_and_not_found() {
        // Without configuration both slots are `None` so the router
        // falls back to its built-in surfaces (HTML banner for the
        // error, no-op for the 404).
        let app = App::new();
        assert!(app.route_error_component.is_none());
        assert!(app.not_found_component.is_none());
    }

    #[test]
    fn rejection_reason_distinguishes_guard_and_loader_source() {
        // Same rejection variant emits a different stable
        // identifier depending on whether a guard or a loader
        // produced it. RFC §5.10.7: closed-set reasons let
        // observability split traffic by origin without parsing
        // the rejection enum.
        assert_eq!(
            RouteRejection::Unauthorized.reason(RejectionSource::Guard),
            "guard_unauthorized"
        );
        assert_eq!(
            RouteRejection::Unauthorized.reason(RejectionSource::Loader),
            "loader_unauthorized"
        );
        assert_eq!(
            RouteRejection::Forbidden("policy_x").reason(RejectionSource::Guard),
            "guard_forbidden"
        );
        assert_eq!(
            RouteRejection::Forbidden("policy_x").reason(RejectionSource::Loader),
            "loader_forbidden"
        );
        assert_eq!(
            RouteRejection::NotFound.reason(RejectionSource::Guard),
            "guard_not_found"
        );
        assert_eq!(
            RouteRejection::NotFound.reason(RejectionSource::Loader),
            "loader_not_found"
        );
        assert_eq!(
            RouteRejection::Server("db_down").reason(RejectionSource::Guard),
            "guard_server_error"
        );
        assert_eq!(
            RouteRejection::Server("db_down").reason(RejectionSource::Loader),
            "loader_server_error"
        );
        assert_eq!(
            RouteRejection::Blocked("ab_test").reason(RejectionSource::Guard),
            "guard_blocked"
        );
        assert_eq!(
            RouteRejection::Blocked("ab_test").reason(RejectionSource::Loader),
            "loader_blocked"
        );
    }

    #[test]
    fn rejection_reason_custom_passes_user_string_through() {
        // Custom rejections own their reason string verbatim;
        // both sources surface the user-supplied identifier so
        // apps can encode whatever taxonomy they need.
        let custom = RouteRejection::Custom {
            reason: "tenant_quota_exceeded",
        };
        assert_eq!(
            custom.reason(RejectionSource::Guard),
            "tenant_quota_exceeded"
        );
        assert_eq!(
            custom.reason(RejectionSource::Loader),
            "tenant_quota_exceeded"
        );
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
    /// Component painted when a [`RouteRejection`] reaches the
    /// fallback (no rejection handler returned `Some(action)`).
    /// `None` falls back to the built-in
    /// [`paint_route_error_surface`](router::paint_route_error_surface)
    /// HTML banner.
    route_error_component: Option<&'static str>,
    /// Component painted when no route matches the URL and no
    /// wildcard route is registered. `None` keeps the prior
    /// behaviour (route-state updates, no component paint).
    not_found_component: Option<&'static str>,
    plugins: crate::plugin::PluginRegistry,
    installing_plugin: Option<&'static str>,
    devtools: bool,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a component. Delegates to the trait method and records the
    /// runtime name for introspection. Idempotent — calling
    /// `register::<C>` after `route::<C>(...)` (or vice versa) leaves
    /// `C::NAME` in the manifest exactly once.
    pub fn register<C: Component>(mut self) -> Self {
        C::register();
        self.record_component_name(C::NAME);
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
    /// segments (`"/blog/:id"`) or the 404 fallback `"*"`. `C` must
    /// be a `#[component]` that implements [`RouteComponent`];
    /// `C::config()` supplies any guards / loaders the route should
    /// honour. Matching routes paint the component into the
    /// `<pp-outlet>` with captured params passed through as
    /// attributes.
    ///
    /// Components that don't need guards or loaders add
    /// `#[derive(RouteComponent)]` on the struct — the derive emits
    /// the empty `impl RouteComponent for MyComponent {}` — and
    /// inherit the default empty `RouteConfig`. The bound exists
    /// specifically to
    /// guarantee that a component declaring guards
    /// (e.g. `require_auth`) cannot silently be mounted unguarded
    /// by the wrong builder method.
    pub fn route<C: RouteComponent>(self, pattern: &'static str) -> Self {
        self.route_with::<C>(pattern, C::config())
    }

    /// Source-compatible alias for [`Self::route`].
    ///
    /// Both resolve `C::config()` and register through `route_with`.
    /// Kept for app code that adopted the explicit name during the
    /// RFC-078 staging window.
    pub fn route_component<C: RouteComponent>(self, pattern: &'static str) -> Self {
        self.route::<C>(pattern)
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
        self.record_component_name(C::NAME);
        self
    }

    /// Push `name` into the component manifest if not already
    /// present. Routes silently call this so route-only direct-
    /// builder apps (`App::new().route::<C>(...).run()`) report
    /// the right `component_count` in `AppBootStarted` and surface
    /// the right names through `App::registered_components()`.
    fn record_component_name(&mut self, name: &'static str) {
        if !self.components.contains(&name) {
            self.components.push(name);
        }
    }

    /// Install a generic route rejection handler.
    ///
    /// Plugins should use this instead of extending the base [`App`]
    /// with auth-specific methods such as login-route configuration.
    pub fn route_rejection_handler<H: RouteRejectionHandler>(mut self, handler: H) -> Self {
        self.route_rejection_handlers.push(Rc::new(handler));
        self
    }

    /// Configure the component the router mounts when a
    /// [`RouteRejection`] reaches the fallback (no installed
    /// [`RouteRejectionHandler`] returned `Some(action)`).
    ///
    /// Without this configuration the router paints the built-in
    /// minimal [`RouteErrorSurface`] HTML banner — intentionally
    /// plain so production apps notice and override it. Setting a
    /// component lets the app paint branded error UI; the
    /// component is mounted into the outlet just like a normal
    /// route component, so it can use the full template / handler
    /// surface.
    ///
    /// Apps that want to read the rejection variant inside the
    /// error component can do so by registering a
    /// [`RouteRejectionHandler`] that captures the rejection into
    /// reactive state before falling through; the router does not
    /// pass rejection metadata into the error component
    /// implicitly (closed-set reasons only, per RFC-078 §5.10.7).
    pub fn route_error_component<C: Component>(mut self) -> Self {
        C::register();
        self.route_error_component = Some(C::NAME);
        self
    }

    /// Configure the component the router mounts when no
    /// registered route matches the URL **and** no `*` wildcard
    /// route is registered. With a wildcard route in place the
    /// wildcard wins (its guards / loader / config still run);
    /// this slot is the lower-friction alternative for apps that
    /// don't want to dedicate a routing slot to 404s.
    ///
    /// Without this configuration unmatched URLs leave the outlet
    /// in its previous state — the route-state is updated and any
    /// `$route.*` bindings re-evaluate, but no new component
    /// mounts.
    pub fn not_found_component<C: Component>(mut self) -> Self {
        C::register();
        self.not_found_component = Some(C::NAME);
        self
    }

    /// **Internal — invoked by the `app!{}` macro; do not call
    /// directly.** Records a route without eagerly calling
    /// `C::register()`. Safe only when paired with
    /// [`Self::run_with_registry`]: the static `&'static phf::Map`
    /// is the authoritative registry, and skipping the eager
    /// `register()` keeps it that way. Direct user calls would
    /// route to a component the registry never registered.
    ///
    /// Resolves `C::config()` so guards / loaders declared on
    /// route components flow through both the macro path and
    /// direct-builder path identically. The `RouteComponent` bound
    /// matches [`Self::route`] — every component used in
    /// `app!{ routes: [...] }` must implement `RouteComponent`
    /// (default empty body is fine).
    #[doc(hidden)]
    pub fn route_static<C: RouteComponent>(mut self, pattern: &'static str) -> Self {
        router::register_route_with_config(pattern, C::NAME, C::config().into_runtime());
        self.routes.push(pattern);
        self.record_component_name(C::NAME);
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
        self.record_component_name(name);
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
        // Install the panic-to-`console.error` hook before anything
        // else. The framework authors directive `.expect(...)`
        // messages throughout (`#[store]` "not registered — call
        // App::store::<_>() first" being the canonical example),
        // and without this hook a wasm panic surfaces as
        // "RuntimeError: unreachable executed" with no message.
        // `set_once()` is safe to call from every App::run().
        console_error_panic_hook::set_once();
        let Self {
            components,
            stores,
            routes,
            before_mount,
            after_mount,
            route_rejection_handlers,
            route_error_component,
            not_found_component,
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
            router::set_route_error_component(None);
            router::set_not_found_component(None);
            // Same reasoning for fetch middleware: a plugin may
            // have called `fetch::install_middleware` in its
            // `install` fn before validation discovered the
            // missing service. Refused-to-mount apps must not
            // leave privileged middleware live — clear it and
            // freeze so subsequent install attempts panic.
            crate::fetch::clear_and_freeze();
            crate::plugin::render_plugin_boot_error(&errors);
            return;
        }
        crate::plugin::activate(plugins);
        router::set_route_rejection_handlers(route_rejection_handlers);
        router::set_route_error_component(route_error_component);
        router::set_not_found_component(not_found_component);
        // Close the fetch-middleware install seam — RFC-078 §5.10.3
        // requires plugin install to run before the first
        // `App::run` so middleware can't be slipped in after the
        // trust boundary closed. This is the canonical close point.
        crate::fetch::freeze_middleware_chain();
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
        // Cross-check $store.X references in compiled component
        // templates against `App::store::<T>()` registrations.
        // Without this, a missing `.store::<T>()` call deferred-
        // panics at first $store access with a stack trace that
        // looks like a framework bug. Fail loud at boot with the
        // exact missing names instead.
        if let Err(missing) = check_store_registrations(&components, &stores) {
            crate::plugin::emit(crate::plugin::AppBootFailed {
                reason: "missing_store_registration",
            });
            render_missing_store_boot_error(&missing);
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

/// Parse a compile-time expression source with `pocopine_expr` and
/// collect every `$store.<name>` path reference into `into`.
///
/// AST-based (not substring-based) so the same literal text inside
/// a string literal (e.g. `pp-text="'$store.example missing'"`)
/// can't trip the boot check — the parser sees that occurrence as
/// a `Literal::String`, not an `Expr::Path`. A parse failure means
/// the macro-time pipeline already surfaced a compile error for
/// that expression, so silently skipping it here is fine.
fn collect_store_names(src: &str, into: &mut Vec<String>) {
    let Ok(ast) = pocopine_expr::parse(src) else {
        return;
    };
    visit_paths(&ast.value, &mut |segments| {
        if segments.len() >= 2 && segments[0] == "$store" {
            let name = segments[1].clone();
            if !into.contains(&name) {
                into.push(name);
            }
        }
    });
}

/// Walk every `Expr::Path` (including the LHS of `Expr::Assign`)
/// in the AST, including paths inside nested binops / ternaries /
/// call arguments / statement sequences. Used by
/// [`collect_store_names`] to find every `$store.X` reference
/// regardless of where it sits in the expression tree.
fn visit_paths(expr: &pocopine_expr::Expr, sink: &mut impl FnMut(&[String])) {
    use pocopine_expr::Expr;
    match expr {
        Expr::Literal(_) => {}
        Expr::Path(segs) => sink(segs),
        Expr::Not(inner) => visit_paths(&inner.value, sink),
        Expr::BinOp(_, lhs, rhs) => {
            visit_paths(&lhs.value, sink);
            visit_paths(&rhs.value, sink);
        }
        Expr::Ternary(cond, then_branch, else_branch) => {
            visit_paths(&cond.value, sink);
            visit_paths(&then_branch.value, sink);
            visit_paths(&else_branch.value, sink);
        }
        Expr::Call(_, args) => {
            for arg in args {
                visit_paths(&arg.value, sink);
            }
        }
        Expr::Assign(lhs_segs, rhs) => {
            sink(lhs_segs);
            visit_paths(&rhs.value, sink);
        }
        Expr::Seq(stmts) => {
            for stmt in stmts {
                visit_paths(&stmt.value, sink);
            }
        }
    }
}

/// Scan a compiled template plan's expression sources for
/// `$store.<name>` references, accumulating into `into`. Covers
/// the plan slices that carry plain `expr_src` strings —
/// bindings, listeners, `pp-if`, native models — which is where
/// store references appear in practice.
fn collect_plan_store_names(
    plan: &'static crate::templates_plan::StaticTemplatePlan,
    into: &mut Vec<String>,
) {
    for b in plan.bindings {
        collect_store_names(b.expr_src, into);
    }
    for l in plan.listeners {
        collect_store_names(l.expr_src, into);
    }
    for p in plan.if_plans {
        collect_store_names(p.expr_src, into);
        for b in p.else_if {
            collect_store_names(b.expr_src, into);
        }
    }
    for m in plan.match_plans {
        collect_store_names(m.expr_src, into);
    }
    for m in plan.native_models {
        collect_store_names(m.expr_src, into);
    }
}

/// Cross-check `$store.X` references across registered components'
/// plans against the stores actually registered with
/// `App::store::<T>()`. Returns the sorted, deduplicated list of
/// missing store names; an empty `Ok(())` means every reference
/// has a matching registration.
///
/// Plan lookup goes through
/// [`crate::templates_plan::template_plan_for`], which tries the
/// active PHF registry first (only set by
/// [`App::run_with_registry`]) and falls back to the
/// `TEMPLATE_PLANS` thread-local that every component's macro-
/// emitted `register()` populates. Without the fallback this
/// check is silently no-op for the canonical
/// `App::new()…run()` flow — that path never installs a PHF
/// registry, so every per-component vtable lookup returns `None`.
fn check_store_registrations(
    components: &[&'static str],
    stores: &[&'static str],
) -> Result<(), Vec<String>> {
    let mut needed: Vec<String> = Vec::new();
    for &name in components {
        if let Some(plan) = crate::templates_plan::template_plan_for(name) {
            collect_plan_store_names(plan, &mut needed);
        }
    }
    let mut missing: Vec<String> = needed
        .into_iter()
        .filter(|name| !stores.contains(&name.as_str()))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

fn render_missing_store_boot_error(missing: &[String]) {
    let list = missing.join(", ");
    let msg = format!(
        "pocopine: components reference store(s) not registered with \
         `App::store::<T>()`: {list}. Add the missing `.store::<T>()` calls \
         to your `App::new()…run()` chain before mount."
    );
    web_sys::console::error_1(&JsValue::from_str(&msg));
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Some(body) = doc.body() else { return };
    if let Ok(banner) = doc.create_element("div") {
        let _ = banner.set_attribute("data-pocopine-boot-error", "missing-store");
        let _ = banner.set_attribute(
            "style",
            "all: initial; display: block; background: #b00020; color: #fff; \
             font-family: system-ui, sans-serif; padding: 12px 16px; \
             font-size: 14px; line-height: 1.4;",
        );
        banner.set_text_content(Some(&msg));
        let _ = body.insert_before(banner.as_ref(), body.first_child().as_ref());
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
