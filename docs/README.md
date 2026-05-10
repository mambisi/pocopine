# pocopine design docs

Working notes, plans, and code sketches. Source of truth for *why* the code
looks the way it does — the code itself tells you *what*.

- [`components/`](./components/) — opinionated structure for building
  components and managing state. Read first.
- [`reactivity/`](./reactivity/) — the reactive core: effects, dep tracking,
  the JS `Proxy` bridge, and everything we want to bolt on next.
- [`poco/`](./poco/) — the `.poco` template format (HTML + directives),
  paired with sibling `.rs` + `.css` files. No mixed-language SFCs.
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
- [`sync-local-store-plan.md`](./sync-local-store-plan.md) — next sync
  phase plan: `SyncLocalStore`, durable client identity, SQLite-first
  browser storage, and SQLx as a later host/server adapter.
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
