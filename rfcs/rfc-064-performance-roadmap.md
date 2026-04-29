# RFC 064 — Performance roadmap to community-credible benchmarks

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-29 |
| **Supersedes** | — |
| **Related** | [RFC 012](./rfc-012-expression-evaluator.md), [RFC 015](./rfc-015-pp-anchor.md), [RFC 016](./rfc-016-pp-resize-pp-intersect.md), [RFC 022](./rfc-022-pp-roving.md), [RFC 058 §5.10](./rfc-058-compiled-views-walker-removal.md), [RFC 060](./rfc-060-component-uses-registry.md), [RFC 061](./rfc-061-compiled-mount-only.md), [RFC 062](./rfc-062-per-component-mount-specialization.md) |
| **Depends on** | RFC 062 implemented |

## 1. Summary

Five architectural perf changes that take counter from ~130 KB
gzip (post-RFC-062) to **≤80 KB gzip minimal / ≤100 KB gzip
full-features**, and jsbench `runLots(10000)` from ~2.2× Solid
to **within 1.3× Solid**. Each is an independent PR; the
sequence below orders them by leverage, not schedule.

The community-acceptance threshold is "competitive with
Yew (~70 KB) and Leptos (~50 KB), faster than both." This RFC
commits to that threshold and ships the work to hit it.

## 2. Motivation

Post-RFC-062, pocopine sits at ~130 KB gzip counter — too big for
the JS-framework benchmark conversation (Solid ~7 KB, Vue 3 vapor
~10 KB) and only marginally smaller than Yew (~70 KB) / Leptos
(~50 KB). The reactivity choice (Vue 3 proxy + effect, ~25 KB)
and the bundled-by-default features (animations + transitions,
~15 KB) are architecturally locked. The remaining ~60 KB of
runtime tax is what this RFC targets.

There are exactly five places that tax lives, each a focused
change with measurable delivery:

- The runtime expression evaluator (`expr::parse_cached` + the
  recursive AST eval) for templates that could be const-folded
  at macro time.
- Per-route bundling — every app currently ships every component
  every route could reach. RFC 058 §5.10 specced clustering;
  RFC 060's `uses` graph supplies the data; this RFC implements
  the splitter.
- Lifted fragment bodies (slot / `pp-if` / `pp-for` row) still
  go through the generic `apply_static_plan` walker. RFC 062
  explicitly excluded them; this RFC closes the gap.
- The `pp-for` keyed reconcile loop allocates Rcs + HashMap
  entries per row.
- Macro-emitted code paths still allocate `String` for tag and
  attribute names that could be `&'static str`.

## 3. Non-goals

- **Not changing reactivity.** Proxy + effect stays. Signals are
  rejected by RFC 058 + RFC 059.
- **Not removing animations or transitions from core.** RFC 058
  Phase 6.5 + RFC 059 lock these as default-on.
- **Not the full WIT-bindgen split-delivery.** RFC 058 Phase 8
  remains toolchain-gated. This RFC ships the per-cluster wasm
  *generation*; runtime per-cluster fetch can layer on later.
- **Not a benchmark harness or community comms.** Those are
  parallel work tracked separately.

## 4. Design

Five changes, ordered by leverage:

### 4.1 Cluster algorithm — `uses` graph drives per-route splits

**Delivery**: ≥40% reduction in `examples/website` boot bundle;
counter unchanged (single-route).

RFC 058 §5.10.1 specs route-boundary clustering. RFC 060 Tier 4
already builds the static `&'static phf::Map` registry from the
transitive closure of `App::route::<C>(...)`. This change
extends the closure walk:

- Identify the **shell cluster** (components reachable from
  every route — present in every cluster's intersection).
- Identify each **route cluster** (components reachable from
  exactly one route, minus the shell).
- Emit per-cluster `&'static phf::Map` literals — one for the
  shell, one per route.
- Update `App::run_with_registry` to load the shell cluster
  at boot and the active route's cluster on first navigation.

The biggest impact is on apps with many routes (websites,
dashboards) where today the boot bundle ships every page's
components. Counter (1 component, 1 route) is unchanged; the
website example with 50 routes drops boot bundle by ~60%
(estimated).

**Implementation surface**: `crates/pocopine-macros/src/lib.rs`
(the `app!{}` macro grows a closure-walk pass), new tiny
`pocopine-core/src/clusters.rs` for the shell + per-route
loaders. No author code changes.

### 4.2 Const-fold the expression evaluator at macro time

**Delivery**: counter binary shrinks ≥8 KB gzip with the runtime
expression evaluator stripped. A pathological "lots of function
calls" fixture confirms the runtime evaluator still loads when
needed.

RFC 012's expression evaluator parses every directive value
string (`pp-text="message"`, `:title="some_title"`, etc.) at
runtime via `expr::parse_cached`, then recursively evaluates
the AST against the scope proxy on each render. The AST eval
is ~12 KB gzip in the runtime.

Most templates use simple expressions: bare identifiers
(`message`), field access (`user.name`), unary/binary ops
(`!open`, `count > 0`), method calls (`items.len()`). The
macro can analyze each `expr_src` at compile time and emit:

- **Statically resolvable** (≥80% of expressions in the
  workspace audit): a direct closure that reads the scope
  proxy field by name, no AST in the binary.
- **Dynamically resolvable** (the rest — function calls,
  arbitrary chains, future expressions): keep the runtime
  evaluator as a fallback, but make it opt-in via a feature
  flag so apps with no dynamic expressions don't ship it.

**Implementation surface**: extend the existing AST visitor in
`crates/pocopine-macros/src/template_plan.rs`; add a
`crates/pocopine-core/src/expr_compiled.rs` for the const-fold
helper signatures. The runtime expression evaluator
(`crates/pocopine-core/src/expr.rs`) becomes feature-gated.

**Estimated impact**: ~10 KB gzip when no dynamic expressions
are used. Most apps qualify.

### 4.3 Specialize lifted fragments (slot / `pp-if` / `pp-for` body)

**Delivery**: jsbench `runLots(10000)` improves ≥15%; the
`apply_static_plan` symbol does not appear in the binary.

RFC 062 explicitly deferred this (§3 non-goal). The macro
already emits per-component `__pocopine_mount` bodies; lifted
fragments (the bodies of `<template pp-if>` and `<template
pp-for>` and parent-supplied slot fragments) still produce a
`StaticTemplatePlan` that runtime `apply_static_plan` walks.

This change applies the same RFC 062 codegen shape to fragments:
- The macro emits a `__pocopine_fragment_<id>(host, scope)` per
  lifted fragment.
- Slot fragments, if-body fragments, and for-row fragments all
  consume the same emit shape.
- `apply_static_plan` becomes pure dead code and gets deleted.
- `StaticTemplatePlan` is purely a macro-internal IR; no runtime
  representation.

**Implementation surface**: extend the per-component specialize
emitter in `crates/pocopine-macros/src/template_plan.rs` to
also drive fragment emission. Delete `apply_static_plan` and
its helpers from `crates/pocopine-core/src/templates_plan.rs`
(net ~250 LOC removed).

**Estimated impact**: ~8–12 KB gzip from `apply_static_plan`
deletion + faster `pp-for` row mounts (no plan iteration per
row).

### 4.4 Tighten `pp-for` keyed reconcile

**Delivery**: jsbench `swap`, `partial_update`, and
`runLots(10000)` all improve measurably; existing pp-for tests
stay green; counter benchmark unchanged.

The current `for_::run_keyed` (`crates/pocopine-core/src/directives/for_.rs`)
has measurable overhead per row:

- Allocates `Rc<str>` keys + a `HashMap<Rc<str>, RowEntry>` per
  reconcile pass.
- Several `entry.key.clone()` + `RefCell::borrow_mut` round-trips
  per row.
- Builds a `pool` HashMap of removed rows then probes for reuse
  per new key — fine for swap, slow for full clear-and-reload.

Solid's keyed reconcile uses LIS (longest increasing subsequence)
on the new index→old index mapping, which gives optimal node
movements. Vue 3 vapor uses two-pointer reconcile + LIS for the
middle. Either is a tighter loop than the current pool-and-probe
approach.

**Implementation surface**: rewrite `run_keyed` in
`crates/pocopine-core/src/directives/for_.rs`. Reuse the row
plan (RFC 054) emit shape; no macro changes.

**Estimated impact**: jsbench `swap` and `partial_update` operations
improve 30–50%. Counter unchanged. `runLots(10000)` improves
~20% (mount throughput is dominated by per-row mount cost,
which RFC 062 + this RFC together attack).

### 4.5 String interning for tag + attribute names

**Delivery**: counter binary shrinks ≥1.5 KB gzip after the
sweep; all tests green.

The macro currently emits `String::from("pp-text")` and similar
in several plan-construction sites (inspect macro output via
`cargo expand`). Convert all framework-known names to `&'static
str` literals; reserve `String` for genuinely-dynamic content
(user template literals, dynamic class lists).

**Implementation surface**: audit macro emit sites in
`crates/pocopine-macros/src/template_plan.rs` + the directive
runtime helpers. Replace `String` with `&'static str` where the
value is known at expansion time.

**Estimated impact**: ~2–4 KB gzip from removed allocator usage
+ String formatting code in the binary. Smaller boot-time
allocations (counter mount has ~30 fewer heap allocs).

## 5. Implementation order + cumulative delivery

The five items are independent and can ship in five PRs. The
order below stages cumulative measurement cleanly — each PR's
delivery is a single isolated number on the dashboard:

| Order | Item | Cumulative impact (counter gzip) |
|---|---|---|
| 1 | §4.5 string interning | −2 KB → 128 KB |
| 2 | §4.3 fragment specialization | −10 KB → 118 KB |
| 3 | §4.2 const-fold expressions | −10 KB → 108 KB |
| 4 | §4.4 tightened reconcile | unchanged on counter; jsbench wins |
| 5 | §4.1 cluster algorithm | unchanged on counter; −40% on website |

Each PR is independently revertable; reviewers can take them
in any order though the listed sequence makes the cumulative
measurement story cleanest. PRs may run in parallel where
their files don't conflict.

## 6. Performance targets

After all five items land:

- **Counter raw**: ≤200 KB (currently ~290 KB post-RFC-062)
- **Counter gzip minimal** (no animations, no Pine): **≤80 KB**
- **Counter gzip full-features**: **≤100 KB**
- **Mount time**: ≤2 ms on a 2024 mid-tier laptop
- **jsbench `runLots(10000)`**: within 1.3× Solid
- **jsbench `swap` + `partial_update`**: within 1.2× Solid
- **website example boot bundle**: ≥40% smaller than today

These are commitments, not estimates. If a phase doesn't deliver
its share of the impact, the implementation gets revised before
moving to the next phase.

## 7. Testing requirements

Per phase:

- **§4.5**: existing tests + a `wasm_size_no_regression` integration
  test that asserts counter gzip is below the rolling target.
- **§4.3**: every existing template_plan test passes; new test
  asserts no `apply_static_plan` symbol in the release binary
  (`twiggy top counter.wasm | grep apply_static_plan` returns
  nothing).
- **§4.2**: new fixtures for each common expression shape
  (identifier, field access, binary op, method call, mixed)
  pass through the const-fold emitter; fallback fixture exercises
  the runtime evaluator.
- **§4.4**: existing pp-for tests + new fixtures for clear,
  partial swap, full reorder, append, prepend.
- **§4.1**: new website-sized fixture asserts boot bundle is
  ≤60% of the no-cluster baseline; per-route cluster files
  exist on disk; navigation triggers correct cluster load.

## 8. Measurement requirements

Each PR includes in its body:

- Counter raw + gzip delta vs the previous PR.
- jsbench delta for the operations the PR claims to improve.
- Twiggy diff highlighting the symbols removed/added.

After §4.1 lands, also:

- Website example boot bundle delta.
- Per-cluster bundle sizes.
- Navigation latency for cluster load (cold + warm cache).

## 9. Open questions

1. **§4.2 const-fold scope** — does the macro try to fold method
   calls (`items.len()`) and chain accesses (`user.profile.name`),
   or only single-step accesses? The first cuts the runtime
   evaluator dependency further; the second is faster to ship.
2. **§4.4 reconcile algorithm** — Solid-style LIS or Vue-vapor's
   two-pointer + LIS hybrid? LIS is cleaner; the hybrid is faster
   on the common "small change" case.
3. **§4.1 cluster boundaries** — does an `App::mount_subtree::<C>`
   call participate in clustering (its component must be in some
   cluster), or is it strictly app-shell-only? Affects how
   tooling-mounted components are bundled.
4. **§4.5 interning vs reuse** — should the macro emit a single
   `static PP_TEXT: &str = "pp-text"` constant referenced by
   every site, or rely on LLVM dedup of identical literals? The
   first is more obvious; the second is what LLVM does anyway.

## 10. Why this is enough

Pocopine doesn't need to beat Solid's 7 KB to be taken seriously.
It needs to **enter the Rust web framework conversation** that
Yew (~70 KB) and Leptos (~50 KB) currently own, with a credible
case that the size gap above Solid buys real ergonomics:

- Animations + transitions in core (Yew/Leptos: bring your own).
- Accessibility primitives (Reka UI parity via Pine).
- Typed compound contracts + scoped slots.
- Single-file SSR + hydration story (RFC 059).

After this RFC: pocopine is ~80 KB minimal, beats Yew on size,
beats Leptos on ergonomics, and runs the same krausest-style
benchmarks as the JS frameworks within 1.3× Solid. That's the
"we exist, we're credible" threshold.
