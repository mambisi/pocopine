# RFC 089 - SPA router parity and nested outlets

* **Status:** Accepted (Phases 0–1 typed target/location/navigation API + reserved-namespace enforcement landed; compiled `pp-route` links + nested outlets, Phases 2–5 pending)
* **Author:** pocopine team
* **Created:** 2026-05-28
* **Tracking branch:** `review/router-api-docs`
* **Supersedes:** the SPA-router non-goals in [RFC 003](./rfc-003-router.md) for nested layouts and the stale `pp-route` compiled-directive assumptions in RFC 003
* **Related:** [RFC 003 (client-side SPA router)](./rfc-003-router.md), [RFC 065 (route-cluster bundling)](./rfc-065-route-cluster-bundling.md), [RFC 078 (client route guards and loaders)](./rfc-078-client-route-guards-and-loaders.md), [Vue Router guide](https://router.vuejs.org/)

## Summary

Pocopine has a working SPA router core: `App::route::<C>(pattern)`,
`RouteComponent`, route guards, route loaders, `$route`, history
navigation, route events, and route error surfaces. The remaining gap is
not "routing exists"; it is that the author-facing SPA experience is
still v0:

* `pp-route` is still referenced by examples and RFC 003 but is not
  implemented on the compiled template path.
* `<pp-outlet>` is a single global outlet, so nested layouts are not
  representable.
* `navigate(&str)` accepts a raw string and returns no structured
  outcome.
* active link state, route meta/global guards, redirects/aliases,
  scroll behavior, and route docs are uneven compared with the baseline
  users expect from mature SPA routers such as Vue Router.

This RFC closes the SPA gap in phases while keeping server-side
rendering and route-cluster code splitting out of scope. RFC 065 owns
bundle partitioning; this RFC owns the runtime and author API that a
single-wasm SPA needs.

## Motivation

The router review found that the current code and docs disagree in ways
that directly affect app authors:

* Examples use `<a href="/about" pp-route>`, but the compiled template
  planner treats `pp-route` as a deferred directive. Those links degrade
  to browser navigation.
* RFC 003 promises a single outlet and explicitly defers nested layouts;
  modern apps need parent layouts, sub-navigation, and child pages.
* RFC 078 moved guards/loaders forward, but RFC 003 still lists loaders
  as non-goals and names walker-era implementation files that no longer
  exist.
* The reserved `/_pocopine/*` namespace is documented but not enforced
  by route matching.
* Redirect loop checks compare only `path`, not the full path plus query.

Vue Router is the comparison target because it sets the mainstream SPA
contract: declarative links, programmatic push/replace, dynamic params,
nested route records rendered into nested views, navigation guards,
route meta fields, redirects/aliases, navigation failures, scroll
behavior, and lazy route loading. Pocopine does not need to copy Vue's
component model, but the missing concepts are real product gaps for SPA
apps.

## Goals

* Restore declarative client-side link navigation on the compiled path.
* Introduce a typed route target/location API with `RouteName`,
  `RouteTarget`, `RouteQuery`, `push`, `replace`, `go`, `prefetch`,
  and structured navigation results.
* Support nested route records and nested `<pp-outlet>` rendering.
* Preserve the current flat `App::route::<C>("/path")` API for existing
  apps.
* Add first-class active link state for route-aware anchors.
* Add route meta and global guard hooks as generic router primitives.
* Add redirect and alias route configuration.
* Enforce the reserved `/_pocopine/*` namespace.
* Update router docs so shipped behavior, RFCs, examples, and tests
  agree.

## Non-goals

* **SSR and hydration.** RFC 059 remains the home for server-rendered
  initial HTML and route-data hydration.
* **Route-cluster bundling and lazy wasm chunks.** RFC 065 owns bundle
  metadata and code delivery. This RFC adds the runtime/API prefetch
  surface, but not multi-artifact loading.
* **File-system routing.** Pocopine keeps explicit route declaration.
* **A `#[route]` attribute macro.** RFC 003 rejected per-component
  route attributes. This RFC keeps URLs centralized in `App::route`
  while component-owned behavior lives in `RouteComponent::config()`.
* **A JavaScript-compatible Vue Router clone.** The comparison informs
  Pocopine's Rust API; it does not define the implementation shape.
* **Auth-specific router APIs.** Auth remains a consumer through
  RFC 078 guards, loaders, rejection handlers, and plugins.
* **Typed query decoding in v1.** This RFC keeps query values as strings
  in `$route` and the base router context.

## Comparison table

| Capability | Vue Router baseline | Pocopine today | RFC 089 target | Priority |
|---|---|---|---|---|
| Declarative links | `<RouterLink>` renders an anchor, intercepts navigation, and applies active classes. See [active links](https://router.vuejs.org/guide/essentials/active-links.html). | Examples use `pp-route`, but the compiled planner excludes it, so anchors can fall back to full browser navigation. | Implement compiled `pp-route`; add active/exact state; keep plain `<a>` semantics for accessibility and fallback. | P0 |
| Programmatic navigation | `router.push`, `router.replace`, and history traversal accept strings or route-location objects. See [programmatic navigation](https://router.vuejs.org/guide/essentials/navigation). | `pocopine::navigate(&str)` does push-only string navigation and returns `()`. | Add `router::push(RouteTarget)`, `router::replace(RouteTarget)`, `router::go(delta)`, and keep `navigate(&str)` as a push shorthand. | P0 |
| Dynamic params | Dynamic segments become route params and are exposed on route locations. | `/:name` params exist and pass to components as attributes. | Keep current behavior; add target builders that validate required params before producing a path. | P0 |
| Nested routes/views | Parent route records render parent layouts and children render into nested router views. See [nested routes](https://router.vuejs.org/guide/essentials/nested-routes.html). | One global `OUTLET`; every discovered `<pp-outlet>` overwrites the previous one. Matching returns one route. | Match a route chain; mount the root record into outlet depth 0; mount child records into depth 1+ outlets owned by parent route scopes. | P0 |
| Guards | Global, per-route, and in-component guards can allow, redirect, cancel, or await. See [navigation guards](https://router.vuejs.org/guide/advanced/navigation-guards.html). | Per-route synchronous guards, `Pending`, loaders, and rejection handlers exist through RFC 078. No global guard/meta layer. | Add global before/after hooks and route meta. Keep component-local `RouteComponent::config()` as the per-route guard source. | P1 |
| Route meta | Route records carry arbitrary meta for guards/layout policy. See [route meta fields](https://router.vuejs.org/guide/advanced/meta.html). | No route meta. Apps encode policy in closures or plugin state. | Add typed route meta entries backed by static keys. Expose merged meta across the matched chain. | P1 |
| Redirect and alias | Route records may redirect or expose aliases. See [redirect and alias](https://router.vuejs.org/guide/essentials/redirect-and-alias). | Redirects exist only as guard/rejection outcomes. No route-record redirect or alias. | Add `.redirect(...)` and `.alias(...)`; redirects rematch target and skip source guards. | P2 |
| Navigation outcome | Programmatic navigation reports duplicated, aborted, cancelled, errored, or redirected outcomes. See [navigation failures](https://router.vuejs.org/guide/advanced/navigation-failures.html). | Plugin route events exist, but direct navigation returns no result. | Return `NavigationResult` from push/replace and emit the same classification through existing route events. | P1 |
| Scroll behavior | Apps can configure scroll restoration/top/hash behavior. See [scroll behavior](https://router.vuejs.org/guide/advanced/scroll-behavior). | No router-owned scroll policy. | Add optional `App::scroll_behavior(...)`; default to browser-compatible popstate restoration and top-on-new-push. | P2 |
| Lazy route loading | Async route components and chunk loading are first-class. See [lazy loading routes](https://router.vuejs.org/guide/advanced/lazy-loading.html). | Single wasm bundle; RFC 065 sketches route-cluster metadata. | Defer loading mechanics to RFC 065; this RFC only ensures route chains and navigation results can represent async/pending route resolution later. | Later |

## Design

### 1. Route target and location

Current API:

```rust
pocopine::navigate("/blog/42?tab=comments");
```

Add a typed API without breaking that shorthand:

```rust
use pocopine::{
    encode_route_path_segment, RouteName, RouteQuery, RouteTarget, RouteUrl,
};

const BLOG_POST: RouteName = RouteName::new("blog.post");

let target = RouteTarget::new("/blog/42")?;
pocopine::push(target)?;

pocopine::replace(RouteTarget::path_with_query(
    "/search",
    RouteQuery::from([("q", "router")]),
)?)?;

let target = RouteTarget::named(BLOG_POST)
    .param("id", "42")
    .query("tab", "comments")
    .build()?;
pocopine::push(target)?;

let target = RouteUrl::new()
    .segment("blog")
    .segment("user/42")
    .query("tab", "comments")
    .hash("thread 1")
    .target()?;

let slug = encode_route_path_segment("user/42");
```

`RouteLocation` is the normalized, read-only shape used by guards,
loaders, hooks, route events, and `$route`:

```rust
pub struct RouteLocation {
    pub path: String,
    pub full_path: String,
    pub query: HashMap<String, String>,
    pub hash: Option<String>,
    pub params: HashMap<String, String>,
    pub matched: MatchedRouteChain,
    pub meta: RouteMeta,
}
```

`RouteTarget` is user input. It validates that navigation stays
app-local:

* starts with `/` or names a registered route;
* does not start with `//`;
* contains no backslash;
* does not target `/_pocopine/*`;
* carries all params required by a named route.

`RouteUrl` is the explicit URL creation API. It percent-encodes path
segments, query keys/values, and hash fragments. The public
`encode_route_path_segment`, `encode_route_query_part`, and
`encode_route_fragment` helpers expose the same encoding rules for code
that needs to build part of a URL before handing it to the router.

`navigate(&str)` remains as:

```rust
pub fn navigate(url: &str) {
    let _ = push(url);
}
```

### 2. Navigation methods and result

Add:

```rust
pub enum NavigationKind {
    Push,
    Replace,
    Pop,
}

pub enum NavigationFailure {
    Duplicated,
    Aborted { reason: &'static str },
    Redirected { to: RouteLocation },
    Cancelled,
    NotFound,
    InvalidTarget(RouteTargetError),
}

pub type NavigationResult = Result<RouteLocation, NavigationFailure>;

pub fn push(target: impl IntoRouteTarget) -> NavigationResult;
pub fn replace(target: impl IntoRouteTarget) -> NavigationResult;
pub fn go(delta: i32);
pub fn prefetch(target: impl IntoRouteTarget) -> PrefetchResult;
```

The first implementation can stay synchronous for no-loader routes.
Routes with async loaders keep the existing event-driven completion
path; `NavigationResult` returns the accepted target or immediate
failure, while completion/failure events still report async loader
outcomes. A future async navigation API can build on the same
classification without changing route matching.

Loop prevention compares `full_path` (`path + search + hash`), not only
`path`. Redirecting from `/login?next=/admin` to `/login?next=/admin`
is duplicated; redirecting from `/login` to `/login?next=/admin` is a
real target change.

### 3. Compiled `pp-route`

`pp-route` remains an attribute on anchors:

```html
<a href="/dashboard" pp-route>dashboard</a>
<a href="/blog/42" pp-route:replace>post</a>
```

The compiled template planner emits a cleanup-tracked click listener for
`pp-route` on native anchor elements. The listener reads the anchor's
current `href` attribute at click time so bindings like `:href="post_url"`
work.

The listener intercepts only when:

* the event is not already default-prevented;
* primary button;
* no ctrl/meta/shift/alt modifier;
* target is not `_blank`;
* `download` is absent;
* the URL is same-origin and app-local;
* the path is not under `/_pocopine/`.

Otherwise the browser handles the click. This preserves normal open in
new tab, copy-link, download, and external-link behavior.

`pp-route:replace` calls `replace`; plain `pp-route` calls `push`.

### 4. Active link state

Add optional active-state management to `pp-route`:

```html
<a href="/admin" pp-route pp-route-active-class="is-active">
  admin
</a>
<a href="/admin/users" pp-route pp-route-exact>
  users
</a>
```

Default classes:

* `pp-route-active`
* `pp-route-exact-active`

Active means same matched route record and same params. Exact active
means the normalized `full_path` route record chain ends at the same
record. Query values do not affect active by default, matching Vue
Router's behavior; an app can opt into exact full-path matching with
`pp-route-exact-path`.

The implementation subscribes each link to the router scope and updates
classes when `$route` changes. It must not trigger layout shifts beyond
normal CSS class effects.

### 5. Route tree and nested outlets

Keep flat routes source-compatible:

```rust
App::new()
    .route::<Home>("/")
    .route::<About>("/about")
    .run();
```

Add closure-scoped layout builders:

```rust
App::new()
    .route::<Home>("/")
    .layout::<AdminLayout>("/admin", |admin| {
        admin.index::<AdminHome>();
        admin.route::<AdminUsers>("users");
        admin.route::<AdminSettings>("settings");
    })
    .route::<Login>("/login")
    .run();
```

The closure form is the intended public API. It is more Rust-native
than a fluent `.child(...).end()` chain because the borrow scopes the
children and prevents accidentally continuing the parent builder while
still "inside" the layout.

The route tree compiles to route records:

```rust
pub struct RouteRecord {
    pub id: RouteRecordId,
    pub name: Option<&'static str>,
    pub path: RoutePath,
    pub component_name: Option<&'static str>,
    pub config: RouteRuntimeConfig,
    pub meta: RouteMeta,
    pub redirect: Option<RouteRedirect>,
    pub aliases: &'static [&'static str],
    pub children: &'static [RouteRecordId],
}

pub struct MatchedRoute {
    pub record_id: RouteRecordId,
    pub component_name: Option<&'static str>,
    pub route_pattern: &'static str,
    pub params: HashMap<String, String>,
    pub outlet_depth: usize,
}

pub struct MatchedRouteChain(Vec<MatchedRoute>);
```

Matching returns the deepest valid chain, not a single route. For
`/admin/users`:

```text
[
  { pattern: "/admin", component: "admin-layout", outlet_depth: 0 },
  { pattern: "users", component: "admin-users", outlet_depth: 1 },
]
```

Each route component in the chain mounts into the outlet owned by its
parent depth:

1. depth 0 mounts into the app root outlet discovered during boot;
2. while mounting depth 0, any `<pp-outlet>` inside that component is
   registered as depth 1 for that route scope;
3. depth 1 mounts into that outlet;
4. the process continues until the chain is complete.

No unrelated outlet can steal ownership. The router stores outlets by
route mount scope and depth, not as one global `Option<Element>`.

```rust
struct OutletKey {
    navigation_token: RouteToken,
    parent_scope: Option<ScopeId>,
    depth: usize,
}
```

The compiled mount path registers outlets as it creates them. When a
route subtree unmounts, existing scope cleanup drops its outlet entries.

Nested matching rules:

* Child paths are relative unless they start with `/`.
* `""` is the index child for the parent route.
* Params merge from parent to child; duplicate names are rejected at
  route registration.
* Wildcards may appear only as the last child in a sibling list.
* Parent guards run before child guards.
* Parent loaders run before child loaders in chain order.

### 6. Layout preservation

When navigating between two URLs whose matched chains share a prefix,
preserve the mounted prefix and remount only the divergent suffix.

Example:

* `/admin/users` -> `/admin/settings`
* `AdminLayout` remains mounted.
* `AdminUsers` unmounts.
* `AdminSettings` mounts into the existing depth-1 outlet.

This is the main UX reason for nested outlets. It also avoids losing
layout-owned reactive state, open sidebars, scroll containers, or
subscriptions on every child navigation.

The first implementation may remount the entire chain to reduce risk,
but the public API and tests must be written around the preservation
contract so the runtime can optimize without changing authors' code.

### 7. Guards, loaders, and meta across a chain

For a matched chain:

1. resolve route-record redirects;
2. build `to: RouteLocation` and `from: Option<RouteLocation>`;
3. run global before guards in registration order;
4. run route-record guards from parent to child;
5. run route loaders from parent to child;
6. mount or reuse chain components;
7. run global after hooks and emit route events.

Add:

```rust
pub trait RouteGlobalGuard: 'static {
    fn decide(&self, to: &RouteLocation, from: Option<&RouteLocation>)
        -> RouteGuardDecision;
}

pub trait RouteAfterHook: 'static {
    fn after(&self, to: &RouteLocation, from: Option<&RouteLocation>, result: &NavigationResult);
}

impl App {
    pub fn before_route<G: RouteGlobalGuard>(self, guard: G) -> Self;
    pub fn after_route<H: RouteAfterHook>(self, hook: H) -> Self;
}
```

Route meta is typed. The name intentionally follows the Vue Router
concept: route-record metadata for app UI and policy. It is separate
from page/head metadata; a future `PageMeta` surface should own title,
description, canonical URL, Open Graph tags, and similar document
metadata.

```rust
pub struct RouteMetaKey<T: 'static> {
    name: &'static str,
    _t: PhantomData<T>,
}

pub struct RouteMeta;

impl RouteMeta {
    pub fn get<T: 'static>(&self, key: RouteMetaKey<T>) -> Option<&T>;
}
```

Route components can attach meta from their local config:

```rust
static REQUIRES_AUTH: RouteMetaKey<bool> = RouteMetaKey::new("requires_auth");

impl RouteComponent for AdminLayout {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().meta(REQUIRES_AUTH, true)
    }
}

App::new()
    .route::<AdminLayout>("/admin")
    .before_route(|to, _from| {
        if to.meta.get(REQUIRES_AUTH).copied().unwrap_or(false) {
            // generic guard logic; auth plugin can supply this
        }
        RouteGuardDecision::Allow
    });
```

The merged meta view is parent-to-child. Child keys override parent
keys only when the type and key name match.

Page/head metadata uses a separate `PageMeta` shape:

```rust
pub struct PageMeta;

impl PageMeta {
    pub fn new() -> Self;
    pub fn title(self, title: impl Into<String>) -> Self;
    pub fn description(self, content: impl Into<String>) -> Self;
    pub fn canonical(self, href: impl Into<String>) -> Self;
    pub fn robots(self, content: impl Into<String>) -> Self;
    pub fn og_title(self, content: impl Into<String>) -> Self;
    pub fn og_description(self, content: impl Into<String>) -> Self;
    pub fn meta_name(self, name: impl Into<String>, content: impl Into<String>) -> Self;
    pub fn meta_property(self, property: impl Into<String>, content: impl Into<String>) -> Self;
}
```

Routes attach page metadata through component config:

```rust
impl RouteComponent for StoryPage {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().page_meta(|route| {
            let id = route.params.get("id").map(String::as_str).unwrap_or("");
            PageMeta::new()
                .title(format!("Story {id}"))
                .description("Story detail")
                .canonical(format!("/stories/{id}"))
                .og_title(format!("Story {id}"))
        })
    }
}
```

The router applies `PageMeta` only after a navigation succeeds, removes
previous Pocopine-managed page tags, and restores the original document
title for routes that do not provide a title.

### 8. Redirects and aliases

Add route-record redirects:

```rust
App::new()
    .redirect("/home", RouteTarget::path("/"))
    .route::<UserProfile>("/users/:id")
        .alias("/u/:id")
        .end();
```

Redirect rules:

* Redirect records can omit a component unless they have children.
* Source route guards and loaders do not run.
* The target is normalized and matched as a new navigation.
* Redirect depth is capped to prevent loops.

Alias rules:

* An alias keeps the browser URL unchanged.
* Matching uses the aliased route record.
* Params exposed to the component are parsed from the alias pattern.
* Active link state resolves against the route record, not just string
  path equality.

### 9. Scroll behavior

Add an optional app-level hook:

```rust
pub enum ScrollPosition {
    Top,
    Preserve,
    Selector(&'static str),
    Coordinates { left: f64, top: f64 },
}

impl App {
    pub fn scroll_behavior<F>(self, f: F) -> Self
    where
        F: Fn(&RouteLocation, Option<&RouteLocation>, Option<ScrollPosition>)
            -> ScrollPosition
            + 'static;
}
```

Default:

* `popstate`: preserve browser saved position when available;
* `push`/`replace`: scroll to top;
* hash target: prefer element id/name matching the hash.

This is P2 because it depends on the navigation-location model but does
not block nested outlets.

### 9.1 Prefetch policy

Prefetch is route-local and opt-in:

```rust
impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .name(RouteName::new("dashboard"))
            .guard(predicate_guard(require_auth()))
            .loader(load_dashboard)
            .prefetch(Prefetch::on_intent().loader())
    }
}
```

`Prefetch::on_intent()` is the default shape for route links: hover,
focus, or touch intent may schedule prefetch. `Prefetch::on_visible()`
is for larger route lists where visibility is a better signal.
`Prefetch::loader()` must be explicit because loader functions may be
expensive or touch app data. Guards run before loader prefetch; pending,
rejected, or redirected routes do not run speculative loader work.

Programmatic prefetch:

```rust
pocopine::prefetch(RouteTarget::named(RouteName::new("dashboard")).build()?);
```

Directive prefetch:

```html
<a href="/dashboard" pp-route pp-prefetch="intent">dashboard</a>
<a href="/reports" pp-route pp-prefetch="visible">reports</a>
```

In the single-wasm runtime, route/code prefetch is a readiness check.
When RFC 065 adds route clusters, the same API becomes the scheduling
surface for `ensure_route_cluster(...)`. Loader prefetch can cache the
loader result for the next exact URL navigation.

### 10. Reserved namespace

Route registration rejects app routes under `/_pocopine/`:

```rust
App::new().route::<Debug>("/_pocopine/debug"); // panic in debug, registry error in app!{}
```

Runtime matching also refuses the namespace as defense in depth. A
browser request to `/_pocopine/*` should never match a page route, even
if a route slipped through registration.

`pp-route` does not intercept `/_pocopine/*` links.

### 11. Docs

Update:

* RFC 003: mark nested layouts and loaders as superseded by RFC 078 and
  RFC 089; replace walker-era implementation notes with compiled-mount
  notes.
* RFC 078: align code snippets with the shipped `Pending` and
  `predicate_guard` APIs.
* `docs/guides/routing/route-guards-and-loaders.md`: include `Pending`, current
  `pocopine-auth-client` adapter names, route refresh semantics, and
  nested-route ordering once implemented.
* `examples/spa`: keep as the flat starter example and add a nested
  admin/settings example.
* `docs/router.md`: add an explanatory router guide. RFCs are the
  authoritative design; docs should be the author-facing surface.

## Rollout

### Phase 0 - Docs and invariants

* Add this RFC and update the RFC index.
* Update stale router/guard docs to match the current shipped API.
* Add tests that assert `/_pocopine/*` cannot be registered or matched.
* Fix redirect loop checks to compare full route keys.

### Phase 1 - Link and navigation API

* Implement compiled `pp-route`.
* Add route-link browser tests for normal click, modifier click,
  external URL, `_blank`, `download`, dynamic href, replace mode, and
  `/_pocopine/*` bypass.
* Add `RouteName`, `RouteQuery`, `RouteTarget`, `push`, `replace`,
  `go`, `prefetch`, `RouteLocation`, `NavigationResult`, `RouteUrl`,
  route-local `Prefetch`, typed route meta, and public route encoding
  helpers.
* Keep `navigate(&str)` source-compatible.

### Phase 2 - Nested route matching and outlet ownership

* Replace single `OUTLET` with outlet registrations keyed by navigation
  token, parent scope, and depth.
* Add a route tree and `MatchedRouteChain`.
* Add child-route builders.
* Mount matched chains in depth order.
* Add tests for index children, child params, wildcard child fallback,
  duplicate param rejection, and parent/child guard ordering.

### Phase 3 - Layout preservation

* Preserve common matched prefixes across sibling navigations.
* Add tests proving parent layout scope state survives child route
  changes.
* Ensure loaders for preserved records do not rerun unless their params
  or query dependencies change.

### Phase 4 - Meta, global hooks, active links

* Add typed route meta and merged-chain lookup.
* Add global before/after route hooks.
* Add active/exact link class management.
* Document guard/meta patterns for auth, feature flags, and analytics.

### Phase 5 - Redirects, aliases, scroll

* Add route-record redirect and alias support.
* Add scroll behavior hook.
* Add docs and examples for canonical redirects, legacy aliases, and
  nested route defaults.

## Testing

Host tests:

* route target validation;
* full-path duplicate and redirect-loop detection;
* route tree matching;
* route chain param merge;
* duplicate param rejection;
* wildcard child ordering;
* redirect depth limit;
* alias matching;
* route meta merge;
* reserved namespace registration and match rejection.

Wasm/browser tests:

* `pp-route` click interception and cleanup;
* active link state updates after push, replace, and popstate;
* nested outlets mount the correct child;
* sibling child navigation preserves parent layout scope;
* parent and child guards run in order;
* parent and child loader data reaches the correct component;
* unmount cleanup removes child outlet registrations;
* browser back/forward remounts the correct chain;
* hash and scroll behavior when Phase 5 lands.

Examples:

* flat starter SPA remains minimal;
* nested admin example demonstrates parent layout preservation,
  child navigation, active sub-nav links, and route loaders.

## Compatibility

Existing apps continue to compile:

* `App::route::<C>("/path")` remains valid.
* `impl RouteComponent for C {}` remains the empty config default.
* `pocopine::navigate("/path")` remains a push-style shorthand.
* A single `<pp-outlet>` app behaves as it does today, except
  `pp-route` works on the compiled path and `/_pocopine/*` is enforced.

New APIs are additive. The only intentional breaking behavior is that
routes under `/_pocopine/*` become invalid; RFC 003 already documented
that namespace as reserved.

## Alternatives considered

* **Dedicated `<pp-link>` component.** Rejected for now. Plain anchors
  with `pp-route` preserve browser semantics, SSR fallback, and existing
  examples. A component can be added later as sugar.
* **One global outlet plus manual layout components.** Rejected. It
  cannot preserve parent layout state or represent parent/child guard
  order.
* **String-only navigation forever.** Rejected. Named routes, params,
  redirect loop prevention, aliases, and structured failures all need a
  normalized target/location model.
* **Copy Vue Router's async navigation promises exactly.** Rejected.
  Pocopine can expose an async API later, but Rust/wasm ergonomics and
  current loader flow make a synchronous acceptance result plus route
  events the smaller first step.
* **Make route meta untyped JSON.** Rejected. Pocopine should keep
  type-safe app/plugin contracts where possible.

## Open questions

* Should child builders use `.end()` or should route tree construction
  move to a nested `routes! { ... }` macro for better type-state?
* Should parent loaders rerun on child-only navigation when query changes?
  The default proposed here is no unless the parent declares query
  dependencies.
* Should `pp-route` active matching ignore query forever, or should
  exact-path matching become the default for `pp-route-exact`?
* Should named routes be required for typed target builders, or can the
  macro derive stable names from component names?
* How should nested outlets interact with future SSR hydration markers?
  This RFC keeps the runtime shape compatible but leaves hydration to
  RFC 059.
