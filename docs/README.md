# pocopine design docs

Working notes, plans, and code sketches. Source of truth for *why* the code
looks the way it does — the code itself tells you *what*.

- [`components/`](./components/) — opinionated structure for building
  components and managing state. Read first.
- [`reactivity/`](./reactivity/) — the reactive core: effects, dep tracking,
  the JS `Proxy` bridge, and everything we want to bolt on next.
- [`poco/`](./poco/) — the `.poco` template format (HTML + directives),
  paired with sibling `.rs` + `.css` files. No mixed-language SFCs.
- [`client-modules.md`](./client-modules.md) — optional typed
  `.client.ts` modules, npm package imports, node_modules ownership,
  package-manager selection, and dev-watch behavior.
- [`integration-firebase.md`](./integration-firebase.md) — Firebase Auth
  tutorial using Pocopine client modules, an app-owned auth extension,
  `pocopine-auth-client`, Pine avatar/popover UI, and server-side
  verification boundaries.
- [`animation.md`](./animation.md) — preset catalogue, macro args,
  FLIP, and the WAAPI escape hatch. See RFC-038 for the design notes.
- [`animation-perf.md`](./animation-perf.md) — perf characteristics
  + baseline numbers for the motion stack. Refresh via
  `examples/website/e2e/test_motion_perf.py`.
- [`performance-strategy.md`](./performance-strategy.md) — current
  large-list performance strategy, including why Vue / Svelte are
  ahead and what pocopine should optimize next.
- [`jobs.md`](./jobs.md) — background-job runtime: Redis Streams +
  sorted-set scheduler + Lua-scripted state transitions, periodic
  firings, reclaim, and the memory backend. Includes redis-cli
  recipes for validating a live deployment.
- [`app-plugins.md`](./app-plugins.md) — app plugin architecture:
  install-time setup, lifecycle hook ordering, `app! { plugins: [...] }`,
  and the ownership boundary for observability/live/auth integrations.
- [`browser-storage.md`](./browser-storage.md) — typed `localStorage`
  helpers for small browser preferences, plus the auth-token and SSR
  boundaries.
- [`route-guards-and-loaders.md`](./route-guards-and-loaders.md) —
  client-side route guards, async loaders, fetch middleware, and the
  rejection-handler chain. Covers `ReturnTo` validation,
  `reevaluate_current` for sign-out, and the privacy invariants from
  RFC-078 §5.10.
- [`server-plugins.md`](./server-plugins.md) — host-side plugin
  lifecycle: `Server` builder, `request_event_layer`, server-function
  typed hooks, validation diagnostics, and the privacy invariant on
  framework events.
- [`live.md`](./live.md) — live invalidation tutorial: SSE streams,
  collection/query refresh callbacks, server topic policies, and the
  example/test files that must stay in sync with live API changes.
- [`sync.md`](./sync.md) — sync tutorial: explicit `pocopine-sync`
  extension setup, server shapes, cursor pulls, live wake-up wiring, and
  the current memory-backed example.
- [`sync-local-store-plan.md`](./sync-local-store-plan.md) — local-store
  implementation plan and status: `SyncLocalStore`, durable client
  identity, SQLite browser/native storage, and SQLx as a later
  host/server adapter.
- [`sync-local-first-roadmap.md`](./sync-local-first-roadmap.md) —
  current roadmap for turning the shipped sync protocol, local store, and
  SQLite backend into a database-agnostic local-first sync engine without
  becoming a sync database.
- [`sync-local-first-architecture-review.md`](./sync-local-first-architecture-review.md) —
  architecture review against established local-first systems, with the
  phase plan for the next resource-runtime slice.
- [`sync-conflict-architecture.md`](./sync-conflict-architecture.md) —
  deep dive on the sync layers, canonical-vs-rendered local state,
  pending overlays, push outcomes, conflict resolution direction, and the
  invariants future sync/resource authors must preserve.
- [`sync-crud.md`](./sync-crud.md) — `pocopine-sync-crud` helper layer:
  `CrudSource`, `ResourceId`, CRUD mutation payloads, write policies,
  transaction binding, the non-macro runtime adapter, planned proc-macro
  generated typed CRUD methods, and the boundary that keeps Pocopine out
  of ORM territory.
- [`sync-crud-macro-contract.md`](./sync-crud-macro-contract.md) —
  concrete generated CRUD macro API contract, including the server
  resource module shape, client binding helpers, queued/online handling,
  and conflict outcome semantics.
- [`logging-tracing-observer.md`](./logging-tracing-observer.md) —
  browser console logging, backend logging, structured observed
  events, analytics sinks, privacy labels, and target filtering.
- [`auth-jwt-providers.md`](./auth-jwt-providers.md) — contract
  for adding a JWT identity provider preset (in-tree or
  community-maintained), with the mandatory integration-test
  shape and the bundled-providers list.
- [`auth-credentials.md`](./auth-credentials.md) — first-party
  email + password tutorial: implement `UserStore`/`TokenStore`
  against your database (Postgres + `sqlx` walkthrough),
  plug `Credentials` in as a `ServerPlugin`, pair the issuer
  with `JwtVerifier::custom`. The crate ships only the trait
  shapes — no bundled in-memory backend.
- [`auth-client.md`](./auth-client.md) — wasm-side tutorial:
  install the bearer fetch middleware, wire `auth_plugin()` to
  install `AuthSession` + `Unauthorized → /login` redirect, and
  build route guards from `Predicate` values via `predicate_guard`.
- [`auth-phone-otp-tutorial.md`](./auth-phone-otp-tutorial.md) —
  build phone OTP auth today with Twilio + Postgres on top of
  the credentials primitives, until the official
  `pocopine-auth-otp` crate ships. Schema, sender, rate limiting,
  attempt limits, and the migration path.
- [`postmortems/`](./postmortems/) — write-ups of subtle bugs + the
  invariants new code should preserve.

Formal design decisions live one level up in [`../rfcs/`](../rfcs/).
Example apps:

- [`examples/counter/`](../examples/counter/) — a single component.
- [`examples/todo/`](../examples/todo/) — multi-component, slots, and a store.
- [`examples/blog/`](../examples/blog/) — `App` + `#[server]` + axum server bin.
- [`examples/live/`](../examples/live/) — `pocopine-live` SSE invalidation
  with collection/query refetch callbacks.
- [`examples/sync/`](../examples/sync/) — `pocopine-sync` cursor pulls
  with `pocopine-live` wake-ups.
- [`examples/observability-smoke/`](../examples/observability-smoke/) — OTLP trace export and JSON-lines analytics exporter smoke paths.
- [`examples/spa/`](../examples/spa/) — `App::route` + `<pp-outlet>` + `pp-route` + `$route`.
- [`examples/site/`](../examples/site/) — the marketing page, dogfooded.
