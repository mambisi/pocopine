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
- [`logging-tracing-observer.md`](./logging-tracing-observer.md) —
  browser console logging, backend logging, structured observed
  events, analytics sinks, privacy labels, and target filtering.
- [`postmortems/`](./postmortems/) — write-ups of subtle bugs + the
  invariants new code should preserve.

Formal design decisions live one level up in [`../rfcs/`](../rfcs/).
Example apps:

- [`examples/counter/`](../examples/counter/) — a single component.
- [`examples/todo/`](../examples/todo/) — multi-component, slots, and a store.
- [`examples/blog/`](../examples/blog/) — `App` + `#[server]` + axum server bin.
- [`examples/observability-smoke/`](../examples/observability-smoke/) — OTLP trace export smoke server.
- [`examples/spa/`](../examples/spa/) — `App::route` + `<pp-outlet>` + `pp-route` + `$route`.
- [`examples/site/`](../examples/site/) — the marketing page, dogfooded.
