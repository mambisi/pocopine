---
title: "Router and nested routes"
description: "Configure flat and nested SPA routes, render route chains through owned outlets, and preserve parent layouts across child navigation."
---

# Router and nested routes

The app shell owns the root outlet:

```html
<main>
  <pp-outlet></pp-outlet>
</main>
```

Flat routes continue to use `App::route`:

```rust
App::new()
    .route::<Home>("/")
    .route::<Post>("/posts/:post_id")
    .route::<NotFound>("*")
    .run();
```

Captured params are passed to matching `#[prop]` fields and are available from
`$route.params` and `RouteContext`.

## Nested layouts

A layout route is a route component whose template contains the next outlet:

```html
<section class="admin-layout">
  <nav>...</nav>
  <pp-outlet></pp-outlet>
</section>
```

Declare its children in a scoped builder:

```rust
App::new()
    .layout::<AdminLayout>("/admin", |admin| {
        admin.index::<AdminOverview>();
        admin.route::<AdminUser>("users/:user_id");
        admin.layout::<TeamLayout>("teams/:team_id", |team| {
            team.index::<TeamOverview>();
            team.route::<TeamSettings>("settings");
        });
        admin.route::<AdminNotFound>("*");
    })
    .run();
```

Child paths are relative unless they begin with `/`. `index::<C>()` is the
empty child path, so it matches the parent URL itself. A wildcard or named rest
parameter must be the final sibling, and a child cannot reuse an ancestor's
parameter name.

For `/admin/users/42`, the router produces a parent-to-child
`MatchedRouteChain`: `AdminLayout` at depth 0 and `AdminUser` at depth 1. Each
nested `<pp-outlet>` is registered against the route scope that owns it, so an
unrelated outlet cannot replace the app root or capture another branch.

## Layout preservation

Navigation only remounts the divergent suffix of two matched chains. Moving
from `/admin/users/42` to `/admin/settings` preserves the existing
`AdminLayout` DOM node and scope, unmounts `AdminUser`, then mounts
`AdminSettings` into the layout's outlet. Layout-owned state, subscriptions,
and scroll containers survive.

Parents are remounted when their record or captured params change. A query-only
navigation remounts the leaf while retaining its parent layouts.

## Guards, loaders, and locations

Route guards and loaders run from parent to child. A non-`Allow` parent guard
stops the child, and loader results are delivered to the corresponding route
component's `Loader<T>` extractor. Loaders for a preserved parent are not rerun
during sibling navigation.

`push` and `replace` return a `RouteLocation`. The `matched` field exposes the
normalized chain for breadcrumbs, analytics, and debugging:

```rust
let location = pocopine::push("/admin/settings")?;
for record in location.matched.iter() {
    log::debug!("{} at depth {}", record.route_pattern, record.outlet_depth);
}
```

See [Route guards, loaders, and fetch middleware](./route-guards-and-loaders.md)
for route-local policy and async data loading.
