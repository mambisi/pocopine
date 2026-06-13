# RFC 064 — Performance roadmap to community-credible benchmarks

| Field | Value |
|---|---|
| **Status** | Implemented (roadmap delivered across RFC-094/095/096/101) |
| **Author** | pocopine team |
| **Created** | 2026-04-29 |
| **Supersedes** | — |
| **Related** | [RFC 012](./rfc-012-expression-evaluator.md), [RFC 015](./rfc-015-pp-anchor.md), [RFC 016](./rfc-016-pp-resize-pp-intersect.md), [RFC 022](./rfc-022-pp-roving.md), [RFC 060](./rfc-060-component-uses-registry.md), [RFC 061](./rfc-061-compiled-mount-only.md), [RFC 062](./rfc-062-per-component-mount-specialization.md), [RFC 065](./rfc-065-route-cluster-bundling.md) |
| **Depends on** | RFC 062 implemented; baseline measurement captured (see §3) |

## 1. Summary

Four runtime perf changes that take counter from a measured
post-RFC-062 baseline toward Yew/Leptos-tier size and Solid-class
speed. Each phase ships with raw/gzip/jsbench/twiggy deltas
attached to its PR; nothing claims a target without measured
evidence.

The community-acceptance threshold is "competitive with Yew and
Leptos on size, faster than both on the standard JS-framework
benchmark suite." This RFC commits to the *measurement
discipline* required to demonstrate that, with directional
targets that are **gated by measurement** at each phase
boundary.

Bundle-splitting / route-cluster work moved to **RFC 065** —
it's an architectural concern (artifact generation, route load
behavior) rather than runtime perf.

## 2. Motivation

Post-RFC-062, pocopine's counter benchmark sits in the
~120-130 KB gzip range (estimate; needs fresh measurement post
the perf-trim + fragment-fallback collapse work that just
landed). The reactivity choice (Vue 3 proxy + effect) and the
bundled-by-default features (animations + transitions) are
architecturally locked. The remaining runtime tax is
addressable; this RFC targets four places it lives.

The four changes, ordered by leverage and by architectural
debt already known:

1. **Lifted fragment bodies** still go through the generic
   `apply_static_plan` walker. RFC 062 explicitly excluded them;
   this RFC closes the gap. Cleanest continuation of RFC 062.
2. **String interning for tag + attribute names** in
   macro-emitted code paths. Low-risk allocation hygiene.
3. **Compiled expression ABI** that const-folds the common
   subset of template expressions at macro time, leaving the
   runtime AST evaluator as an opt-in fallback for the genuinely-
   dynamic tail.
4. **`pp-for` keyed reconcile** loop tightening — gated on
   profiling evidence about where row mount/reuse/move cost
   actually lands after the fragment specialization above.

## 3. Baseline requirement

**This RFC is not actionable until a fresh baseline measurement
is captured.** Before the first phase PR opens, capture and
commit to `bench/baseline-rfc062.md`:

- **Counter raw + gzip wasm size** (release build, all current
  defaults).
- **Counter mount time** (mid-tier 2024 laptop, median of 10
  cold runs).
- **jsbench full operation matrix** (`runLots(10000)`, `swap`,
  `partial_update`, `clear`, `select`, `append`).
- **Twiggy top-50 symbol report** for the release counter
  binary.
- **Cross-framework reference numbers** for the same fixtures
  on Solid, Vue 3 vapor, Svelte, Yew, Leptos. Either run
  ourselves or cite published numbers with date.

Without these numbers, every "X KB" or "Y× Solid" claim in this
RFC is speculative. The phase deliveries below quote target
deltas, not absolute targets — the absolutes get filled in
once the baseline lands.

## 4. Non-goals

- **Not changing reactivity.** Proxy + effect stays. Signals are
  rejected by RFC 058 + RFC 059.
- **Not removing animations or transitions from core.** RFC 058
  Phase 6.5 + RFC 059 lock these as default-on.
- **Not bundle splitting / route clusters.** Moved to RFC 065.
- **Not a benchmark harness or community comms.** Tracked
  separately.
- **Not a guarantee of any specific final size.** Targets are
  measurement-gated; if a phase under-delivers, scope changes
  before the next phase starts.

## 5. Design

### 5.1 Phase 1 — Specialize lifted fragments

**Why first**: closes the architectural debt RFC 062 explicitly
left open. The principle "no fragmented runtime — one compiled
mount path" already passed council in RFC 062; this is the
extension.

RFC 062 emits per-component `__pocopine_mount` bodies; lifted
fragments (the bodies of `<template pp-if>`, `<template
pp-for>`, and parent-supplied slot fragments) still produce a
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
(estimated ~250 LOC removed, to be confirmed).

**Phase delivery (gated)**:

- The `apply_static_plan` symbol does not appear in the release
  binary (twiggy verification).
- jsbench `runLots(10000)` improves measurably vs the §3
  baseline.
- All existing tests pass.
- PR body includes raw/gzip/jsbench/twiggy delta vs §3 baseline.

### 5.2 Phase 2 — String interning for tag + attribute names

**Why second**: low-risk cleanup that reduces noise in the
twiggy output before the bigger Phase 3 change. Allocation
hygiene improves clarity for downstream profiling.

The macro currently emits `String::from("pp-text")` and similar
in several plan-construction sites (inspect via `cargo expand`).
Convert all framework-known names to `&'static str` literals;
reserve `String` for genuinely-dynamic content (user template
literals, dynamic class lists).

**Implementation surface**: audit macro emit sites in
`crates/pocopine-macros/src/template_plan.rs` + the directive
runtime helpers. Replace `String` with `&'static str` where the
value is known at expansion time.

**Phase delivery (gated)**:

- Counter binary shrinks measurably vs Phase 1 endpoint.
- All tests pass.
- PR body documents the size delta + lists every macro emit
  site that changed.

### 5.3 Phase 3 — Compiled expression ABI

**Why third**: highest reward, highest spec-precision required.
Council flagged that "method calls (`items.len()`)" is not
const-folding — it's a compiled expression ABI. This phase
ships **a conservative first envelope** of the ABI; broader
expression coverage layers on later RFCs.

#### 5.3.1 Capability matrix — first envelope

The macro statically compiles each template expression
(`pp-text="…"`, `:title="…"`, etc.) according to this matrix.
Anything in the **In** column gets emitted as a direct
closure that reads the scope proxy without going through the
runtime AST evaluator. Anything in the **Out** column keeps
the runtime evaluator path and triggers a feature-flag
dependency on `pocopine-core`'s `expr` module.

| Capability | First envelope |
|---|---|
| Bare identifier (`message`) | **In** |
| Single-step field access (`user.name`) | **In** |
| Nested chain access (`user.profile.name`) | **Out** (Phase 3.5 candidate) |
| Boolean negation (`!open`) | **In** |
| Comparison ops with literal RHS (`count > 0`, `state == "loading"`) | **In** |
| Boolean ops between simple terms (`!disabled && open`) | **In** |
| Number / string / bool literals | **In** |
| Optional / null fallback semantics | **Out** (specify in own RFC) |
| Method calls (`items.len()`) | **Out** (needs typed-binding RFC) |
| Array indexing (`tags[0]`) | **Out** |
| Handler / function calls | **Out** (these are listeners, not expressions — already handled) |
| Arbitrary chained method calls | **Out** |

**Rationale for the split**: the In column covers the majority
of `pp-text` / `pp-show` / `:attr` directives in the workspace
audit. Method calls and chain access need a real ABI design
(typed bindings, optional semantics) that doesn't fit in a
runtime-perf RFC. Shipping the In column alone strips the
runtime evaluator from apps that don't use the Out column —
which the workspace audit suggests is most of them.

#### 5.3.2 Workspace audit prerequisite

Before this phase opens, capture and commit to
`bench/expr-audit-rfc064.md`:

- Total count of template expressions across the workspace
  (`crates/pocopine`, `crates/pine`, `examples/`,
  `jsbench/`).
- Per-capability breakdown (how many fall in each row of the
  §5.3.1 matrix).
- List every expression in the **Out** rows verbatim, with
  file + line, so the council can sanity-check that the
  envelope is correctly drawn.

This audit is what justifies (or invalidates) the first-envelope
choice. If method calls turn out to be 30% of the workspace,
the envelope is wrong and Phase 3 gets re-scoped.

#### 5.3.3 Implementation surface

- `crates/pocopine-macros/src/template_plan.rs` — extend the AST
  visitor to classify each expression by §5.3.1 row.
- New `crates/pocopine-core/src/expr_compiled.rs` — typed
  installer signatures for In-column expression shapes.
- `crates/pocopine-core/src/expr.rs` — existing runtime evaluator
  becomes opt-in via cargo feature (`expr-runtime`); apps with
  no Out-column expressions don't ship it.

#### 5.3.4 Phase delivery (gated)

- Workspace audit committed at `bench/expr-audit-rfc064.md`
  before PR opens.
- Counter binary (no Out-column expressions in fixture)
  shrinks measurably with `expr-runtime` feature off.
- A pathological "method calls + chains" fixture confirms the
  runtime evaluator still loads and behaves correctly when
  `expr-runtime` is on.
- jsbench unchanged (this phase is size, not speed).
- PR body includes raw/gzip/twiggy delta + a link to the
  workspace audit.

### 5.4 Phase 4 — `pp-for` keyed reconcile (profile-first)

**Why fourth + last**: the win is real but the *shape* of the
win depends on profiling evidence not yet collected. Council
flagged that jumping straight to LIS without profiling is
premature.

#### 5.4.1 Profiling prerequisite

Before this phase opens, run a profiling pass against the
post-Phase-1 binary on jsbench `swap`, `partial_update`,
`clear`, `select`, `runLots(10000)`. Capture and commit to
`bench/for-profile-rfc064.md`:

- **Per-row breakdown** of mount cost: how much is template
  cloning, how much is scope creation, how much is binding
  install, how much is DOM insert.
- **Per-operation breakdown** of reconcile cost: how much is
  pool/probe HashMap work, how much is `Rc<str>` allocation,
  how much is the actual DOM movement.
- **Comparison hot paths**: which jsbench operation is
  furthest from Solid's number, and what dominates that gap?

#### 5.4.2 Algorithm choice gated on data

The reconcile rewrite picks one algorithm based on the
profiling data:

- **Solid-style LIS** if mount cost dominates and reconcile
  movements are a small fraction. LIS gives optimal node
  movements; cleaner code; one algorithm to maintain.
- **Vue-vapor's two-pointer + LIS hybrid** if `swap` and
  `partial_update` dominate. The hybrid is faster on the
  common "small change" case.
- **A different shape entirely** if profiling reveals neither
  movement nor allocation is the bottleneck — possible if
  RFC 062 already absorbed the per-row mount cost.

The PR opening this phase **must cite the profile** and
explain the choice. No algorithm gets implemented before the
profile justifies it.

#### 5.4.3 Implementation surface

`crates/pocopine-core/src/directives/for_.rs::run_keyed`. Reuse
the row plan (RFC 054) emit shape; no macro changes.

#### 5.4.4 Phase delivery (gated)

- Profile committed at `bench/for-profile-rfc064.md` before
  PR opens.
- jsbench operations the PR claims to improve **all** improve
  measurably vs Phase 3 endpoint.
- jsbench operations the PR doesn't claim to improve are
  unchanged or improved (no regressions).
- All existing pp-for tests pass.

## 6. Implementation order

The four phases must ship in order:

1. **Phase 1** (§5.1) — fragment specialization. Closes RFC 062
   debt; baseline cleanup before measurement claims layer on.
2. **Phase 2** (§5.2) — string interning. Reduces noise before
   the bigger Phase 3 change.
3. **Phase 3** (§5.3) — compiled expression ABI. Requires
   workspace audit prerequisite.
4. **Phase 4** (§5.4) — keyed reconcile. Requires profiling
   prerequisite.

Each phase blocks on its prerequisite (baseline for Phase 1;
audit for Phase 3; profile for Phase 4) and on the previous
phase's PR being merged + measurements published.

## 7. Performance targets — gated by measurement

After all four phases land, the directional targets are:

- **Counter raw**: meaningful reduction vs §3 baseline (target
  TBD post-baseline; aspirational ≤200 KB if baseline supports
  it).
- **Counter gzip minimal** (no animations, no Pine):
  aspirational **≤80 KB** — **gated by baseline + Phase 1+2+3
  measurements**.
- **Counter gzip full-features**: aspirational **≤100 KB** —
  same gating.
- **Mount time**: aspirational ≤2 ms on a 2024 mid-tier laptop.
- **jsbench `runLots(10000)`**: aspirational within 1.3× Solid.
- **jsbench `swap` + `partial_update`**: aspirational within
  1.2× Solid.

**These are targets, not commitments.** Each target lands as a
commitment only when its phase's measurements demonstrate the
work delivers. If a phase under-delivers, this section gets
revised before the next phase starts; "we promised X KB" is not
a reason to merge under-performing work.

The dashboard at `bench/dashboard.md` (created with the §3
baseline, updated by every phase PR) tracks delivered vs
aspirational and is the source of truth.

## 8. Testing requirements

Per phase:

- **Phase 1** (§5.1): every existing template_plan test passes;
  new test asserts no `apply_static_plan` symbol in the release
  binary (`twiggy top counter.wasm | grep apply_static_plan`
  returns nothing).
- **Phase 2** (§5.2): existing tests + a `wasm_size_no_regression`
  integration test asserting counter gzip stays below the
  rolling target.
- **Phase 3** (§5.3): a fixture per row of the §5.3.1 capability
  matrix exercises the const-fold emitter; a separate fixture
  exercises the runtime evaluator with `expr-runtime` feature
  on.
- **Phase 4** (§5.4): existing pp-for tests + new fixtures for
  clear, partial swap, full reorder, append, prepend, all
  passing under the new algorithm.

## 9. Measurement requirements

Each PR includes in its body:

- Counter raw + gzip delta vs the previous phase's endpoint.
- jsbench delta for the operations the PR claims to improve
  (or "unchanged" with the operation list, if size-only).
- Twiggy diff highlighting the symbols removed/added.
- Updated `bench/dashboard.md` row.

Phases with prerequisites (§5.3 audit, §5.4 profile) include a
link to the prerequisite document and a one-paragraph summary
of how it shaped the implementation choice.

## 10. Open questions

1. **Phase 3 envelope size** — the §5.3.1 matrix's In column is
   conservative. Should "nested chain access" promote into the
   first envelope on the strength of the workspace audit, or
   stay deferred to a follow-up RFC even if the audit shows it
   covers ≥80% of remaining Out cases?
2. **Phase 4 algorithm choice** — the council's question: how
   much is the algorithm choice allowed to shift the public
   targets in §7? Is "Solid-style LIS gets us within 1.4×, Vue-
   vapor hybrid gets us within 1.2× but is more code" a phase-
   internal decision, or does it surface for council review?
3. **§3 baseline timing** — capture once before Phase 1, or
   re-capture after every phase? The latter is more rigorous
   but more work.
4. **`expr-runtime` feature default** — on (preserves today's
   behavior; apps don't break) or off (forces apps with
   Out-column expressions to opt in explicitly)? On is safer;
   off is cleaner.

## 11. Why this is enough

Pocopine doesn't need to beat Solid's 7 KB to be taken
seriously. It needs to **enter the Rust web framework
conversation** that Yew (~70 KB) and Leptos (~50 KB) currently
own, with a credible, *measurement-backed* case that any size
gap above Solid buys real ergonomics:

- Animations + transitions in core (Yew/Leptos: bring your own).
- Accessibility primitives (Reka UI parity via Pine).
- Typed compound contracts + scoped slots.
- Single-file SSR + hydration story (RFC 059).

After this RFC + RFC 065 land, pocopine can demonstrate (not
just claim) competitive numbers against Yew/Leptos with
Solid-class ergonomics. The discipline this RFC commits to —
baselines before claims, audits before envelopes, profiles
before algorithms — is what makes the demonstration credible.
