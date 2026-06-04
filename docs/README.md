# pocopine documentation

User-facing guides and tutorials for [pocopine](../README.md), the
full-stack Rust application framework. This tree is the source for the
documentation site — [`site.toml`](./site.toml) defines the navigation
and each page carries `title` / `description` front-matter.

New here? Start with **[Getting Started](./getting-started/introduction.md)**.

## Getting started

- [Introduction](./getting-started/introduction.md) — what pocopine is and how the pieces fit.
- [Installation](./getting-started/installation.md) — install the CLI and check your toolchain.
- [Quickstart](./getting-started/quickstart.md) — scaffold an app, write a component, run it.

## Guides

**Core**

- [Components](./guides/components/README.md) — structure, state, and composition.
- [Reactivity](./guides/reactivity/README.md) — effects, dep tracking, signals, the `Proxy` bridge.
- [Templates (`.poco`)](./guides/poco/README.md) — format, compilation, scoped styles, expressions.

**Styling & UI**

- [Pine Stylekit](./guides/styling/stylekit.md) — the utility-CSS compiler and `@theme` tokens.
- [Animation](./guides/styling/animation.md) — presets, FLIP, and the WAAPI escape hatch.
- [Icons](./guides/styling/icons.md) — tree-shaken Tabler icons.
- [Charts](./guides/styling/charts/README.md) — SVG-first chart primitives.

**Routing**

- [Route guards & loaders](./guides/routing/route-guards-and-loaders.md) — sync guards, async loaders, and fetch middleware for the SPA router.

**Server**

- [Server plugins](./guides/server/server-plugins.md) — host-side plugin lifecycle, tower middleware, and the `Server` builder.
- [Client modules](./guides/server/client-modules.md) — optional typed `.client.ts` modules and npm package imports.

**Data & sync**

- [Sync (client)](./guides/data/sync-client.md) · [Sync (server)](./guides/data/sync-server.md)
- [Object-storage uploads](./guides/data/storage-uploads.md) · [Browser storage](./guides/data/browser-storage.md)

**Auth**

- [Credentials](./guides/auth/credentials.md) · [JWT providers](./guides/auth/jwt-providers.md) · [Client bridge](./guides/auth/client.md)

**Operations**

- [Background jobs](./guides/jobs/jobs.md)
- [Logging & tracing](./guides/observability/logging-tracing.md)
- [App plugins](./guides/plugins/app-plugins.md)

**Recipes**

- [Recipes index](./guides/recipes/README.md) — applied patterns for real app structure.

## Tutorials

End-to-end builds:

- [Build an issue tracker (sync)](./tutorials/issue-tracker-sync.md)
- [Live invalidation](./tutorials/live-invalidation.md)
- [Phone OTP auth](./tutorials/phone-otp-auth.md)
- [Firebase Auth](./tutorials/firebase-auth.md)

## For contributors

- [`internal/`](./internal/) — design notes, roadmaps, performance
  retrospectives, and postmortems. These explain *why* the code looks
  the way it does and are **not** part of the published site.
- [`../rfcs/`](../rfcs/) — authoritative design decisions. Non-trivial
  features open an RFC first.

## Building the site

The documentation site is a pocopine app that renders this markdown as
static pages. [`site.toml`](./site.toml) is the navigation contract:
each entry maps a sidebar label to a markdown file, grouped under the
**Docs** and **Tutorials** tabs. To add a page, drop a markdown file
with `title` / `description` front-matter under the right folder and
add it to `site.toml`.
