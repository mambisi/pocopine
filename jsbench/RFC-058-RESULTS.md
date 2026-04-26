# RFC-058 Phase 2 — bundle-size accounting

Static accounting of the bytes Phase 2 (compiled view plans
behind a runtime fast-path; walker still drives the rest) adds
or removes against the immediate pre-Phase-2 baseline.
Maintained alongside `jsbench/RESULTS.md`, which tracks the
runtime numbers across frameworks.

## Method

Two `wasm-pack build --release --target web examples/counter`
runs around `d279244 feat(macros): emit + register template
plans (RFC-058 Phase 2.3)`:

- **baseline:** `9bf983a` — Phase 2.1/2.2/2.4/2.5 already in
  place (registry shape + classifier + runtime applier +
  walker fast-path) but the macro is not yet emitting plans,
  so the applier never has anything to consume. This isolates
  Phase 2.3's emission from the surrounding scaffolding.
- **head:** `d279244` (same checkpoint plus the Phase 2.6
  evidence tests added in this commit, which don't ship in
  release).

`wasm-opt -O` runs as part of `wasm-pack` for both builds.
Gzip column is `gzip -c <pkg>/counter_bg.wasm | wc -c` (no
flags — same as what cdn defaults publish).

## Counter (`examples/counter`)

One component, ~14 lines of `.poco`. The template is small and
mostly plan-eligible — six pp-on:click + two pp-text + one
pp-show + one pp-bind:title; one pp-model.number stays
walker-owned per RFC-058 §6.1.

| build         | raw wasm bytes | gzip wasm bytes |
|---------------|---------------:|----------------:|
| baseline      |      690 142   |       262 207   |
| head          |      691 131   |       262 471   |
| **delta**     |   **+989** (+0.14 %) |  **+264** (+0.10 %) |

### Why a tiny regression on counter

Phase 2 v1 ships **additive** code — the runtime applier
(`apply_static_plan`), registry, plan-failure counter, and the
re-exports through `__private` all link in regardless of
whether a given app uses them. The walker fast-path adds one
`template_plan_for(tag)` HashMap probe per mount. The macro
emits one `&'static StaticTemplatePlan` literal per
plan-eligible component plus a `register_template_plan(name,
&PLAN)` call.

On counter that adds bytes; the only offsetting saving is the
cleaned HTML being slightly smaller than the raw `.poco` bytes
the macro previously fed to `compile_template`, and that delta
is dominated by the plan-metadata cost on a template this size.

### Where the wins are deferred to

The two phase milestones that turn this neutral-on-tiny-apps
delta net-negative across the board:

- **§6.2 layering follow-up.** v1 skips template-plan
  compilation for any template carrying `pp-for` row plans
  (the row-plan stamper and the template-plan classifier
  compete for the serialised bytes; layering them is a v2
  follow-up). Today's example set leans heavily on `pp-for`
  (jsbench, hn, todo, blog) so the §6.1 envelope misses
  precisely the templates that would profit most from
  cleaned-HTML savings.
- **Phase 6 — walker quarantine behind `legacy-dom`.** Phase
  2's runtime is additive on top of the walker. Phase 6 moves
  the walker's `start` / `walk` / `bind` and the directive
  `run` shims behind `#[cfg(feature = "legacy-dom")]`, at
  which point default builds drop the walker entirely and the
  per-mount fast-path becomes the only path. The bytes saved
  there are the walker's full attribute-scan + DirectiveCall
  dispatch surface — order-of-magnitude bigger than the v1
  metadata cost this column tracks.

## Phase 6 v1 — observer quarantine via `legacy-dom`

The runtime walker's `MutationObserver`-based auto-discovery
(catches DOM nodes inserted after the initial `start()` walk
— router page changes via the slow path, markdown render
output, externally adopted DOM) graduates behind a
`legacy-dom` feature flag. Default-on, so existing apps keep
working unchanged. Apps that bootstrap entirely through
compiled-view mounts opt out via `default-features = false`.

Measured on counter (devtools held constant — only the
`legacy-dom` flag flips):

| build (devtools on)         | raw wasm bytes | gzip wasm bytes |
|-----------------------------|---------------:|----------------:|
| default (`+ legacy-dom`)    |      695 628   |       263 970   |
| `legacy-dom` off            |      692 853   |       263 365   |
| **delta**                   |   **-2 775** (-0.40 %) |   **-605** (-0.23 %) |

Modest — the v1 cfg gates only `install_observer` and its
helpers (the ~200-line `MutationRecord` callback closure plus
the `clear_private` helper used by the bulk-clear short-
circuit). The walker's `walk` / `bind` / `mount_component`
themselves stay always-on because Phase 4's controllers
(`pp-if` / `pp-for` / `pp-teleport`) call them for body
content rendering.

The bigger drops land when:

* **Phase 4.1d / 4.2c / 4.3 follow-ups** — body fragment
  lifting for `pp-if` / `pp-for` / `pp-teleport`. Once the
  controllers stamp their bodies via fragment fns instead of
  cloning a `<template>` and walking it, `walk` / `bind` /
  `mount_component` move behind `legacy-dom` too.
* **Phase 3.5c** — dynamic slot fragments. `materialize_slot`'s
  legacy capture/replay path also walks captured DOM; once
  every slot site has a fragment, the legacy path can move
  behind the flag.
* Walker dispatch shims in `directives/{text,html,bind,show,
  on,init,model,route,if_,for_,teleport}.rs` (the `pub fn run`
  entry points called via `bind`'s directive dispatch) all
  go behind the flag at the same time as `bind` itself.

Cumulatively those bring the default `pocopine` wasm down by
~10-15× more than the v1 observer quarantine alone.

## What Phase 2 got out of the door

The acceptance bar for Phase 2 was parity, not a size win:
- walker browser suite — **50/50 green** with the plan
  fast-path active;
- pine browser suite — **102/102 green** (compound primitive
  set unchanged);
- new `crates/pocopine/tests/template_plan.rs` evidence suite
  — **4/4 green** covering plan registration, fail-fast
  counter parity, brace-payload non-interpolation, and the
  v1 row-plan / template-plan layering trade-off.

`plan_failure_count()` stays at 0 across mount/unmount cycles
for every plan-eligible test fixture, confirming that the
macro is emitting node paths that resolve and expression
sources that parse on the runtime side.

## Rebuilding

```bash
# pre-Phase-2.3 baseline
git checkout 9bf983a -- crates/pocopine-macros crates/pocopine-core
wasm-pack build --release --target web examples/counter
ls -la examples/counter/pkg/counter_bg.wasm
gzip -c examples/counter/pkg/counter_bg.wasm | wc -c

# restore HEAD
git checkout HEAD -- crates/pocopine-macros crates/pocopine-core
wasm-pack build --release --target web examples/counter
ls -la examples/counter/pkg/counter_bg.wasm
gzip -c examples/counter/pkg/counter_bg.wasm | wc -c
```

## Phase 6.2 + 6.3 — feature decomposition snapshot

Measured at `wip/rfc/58-phase6` HEAD (Phase 6.2 `{{expr}}`
text-interpolation lift + Phase 6.3 walker recursion skip for
plan-clean subtrees). Same `wasm-pack build --release --target
web examples/counter` method; the four feature combos came from
toggling the `pocopine` dependency line in
`examples/counter/Cargo.toml` between runs and restoring after.

| Build (counter)                           | Raw wasm | Gzip wasm |
|-------------------------------------------|---------:|----------:|
| `default` (`devtools` + `legacy-dom`)     |  712 477 |  270 806  |
| `default-features = false` + `devtools`   |  710 570 |  269 988  |
| `default-features = false` + `legacy-dom` |  595 587 |  226 866  |
| `default-features = false` (lean)         |  593 447 |  226 368  |

Two readings:

* **`devtools` is the heavy feature.** Dropping it alone saves
  **-114 983 raw / -43 122 gzip** (about -16 % of the binary).
  Authors who want a small bundle should opt out of `devtools`
  before anything else.
* **`legacy-dom` saves comparatively little — by design.**
  Toggling `legacy-dom` off (with `devtools` matching) saves
  **-1 907 raw / -818 gzip** (-0.27 %). The feature today gates
  only the `MutationObserver` install + its callback closure;
  every other walker entry point (`walk` / `bind` / the
  directive `run` shims / `interp::scan_children` /
  `mount_component`'s scan path) stays always-on because
  router pages, controller-body fallbacks, and externally-
  injected DOM still need walker discovery.

### Why Phase 6.2 + 6.3 don't move the size needle (yet)

Both phases reduce **runtime** overhead (fewer redundant
descendant binds, parser cost moved to compile time) without
changing the always-on code surface:

* Phase 6.2 added `interp::install_planned` + the planned
  segment plumbing (always linked) in exchange for the
  walker still shipping `parse_segments` / `scan_children`
  unchanged.
* Phase 6.3 added a 3-line `walker_clean_plan` check + the
  call into `finalize_compiled_subtree` (already always-on)
  in exchange for skipping recursive `walk` calls. Neither
  side dropped any code.

The structural binary win is gated on **moving `walk` / `bind`
/ the directive `run` shims behind `legacy-dom`**, which
requires a compiled-only entry point that walks the tree for
registered component tags without invoking `bind` on every
descendant. The `walker_clean_plan` check from Phase 6.3
already proves the descend-without-bind shape is safe for
compiled subtrees; the remaining work is exposing it as an
alternative to `start` for lean builds.

### Verification

* `wasm-pack test --firefox --headless crates/pocopine` — 28
  template_plan + 50 walker + 0 rfc_056_emit (ignored). All
  green.
* `wasm-pack test --firefox --headless crates/pine` —
  103/103 green; `pine_template_plan_fallback_audit`
  continues to assert the exact-zero gate.
* `cargo test -p pocopine-macros` — 82 unit tests including
  the new `parse_interp_segments_handles_documented_shapes`
  parser-drift guard.

## Phase 6.4 experiment — DOM-builder code-gen (negative result)

**Premise:** The static template plan resolves nodes by
`node_path` and installs every binding/listener/init. The
HTML literal is now only there to feed
`el.set_inner_html(&html)` so the browser builds the DOM the
plan walks. If the macro emits explicit
`create_element` / `set_attribute` / `append_child` code
instead, the per-template HTML bytes drop out of the wasm —
analogous to Solid / SwiftUI's view-builder code-gen.

**What we measured.** Implemented `emit_builder_tokens` in the
macro (mirrors the cleaned-HTML serializer one-for-one,
respects `is_stripped` / `is_text_managed` /
`is_interp_managed` / `row_plan_id`, handles synthetic
elements + void elements). Skipped emission for templates
with `<template>` descendants (browser parser routes
`<template>` children into `HTMLTemplateElement.content`,
which `create_element` + `append_child` doesn't reproduce)
and for templates with `role = "..."` (runtime
`compile_template` rewrites `<root>` to the role's actual
HTML tag, which the builder doesn't yet replicate). Kept the
HTML registration in place to preserve `pp-as` semantics
(any host can mount any component via `pp-as`, and that
sandbox path needs the HTML string).

| Build (counter)                   | Pre-experiment | + builder    | Delta            |
|-----------------------------------|---------------:|-------------:|-----------------:|
| `default` (devtools + legacy-dom) | 712 477 / 270 806 | 719 408 / 272 937 | **+6 931 / +2 131** |
| lean (`default-features = false`) | 593 447 / 226 368 | 599 984 / 228 431 | **+6 537 / +2 063** |

**Conclusion: code-gen builders cost more bytes than they
save for templates of this size.** The cleaned HTML for
`Counter.poco` is roughly 500 bytes; the builder code-gen
needed ~6.5 KB raw / ~2 KB gzip on top of it. Even if we
could fully replace the HTML registration (we can't without
breaking `pp-as`), the builder code is already ~13× larger
than the HTML it would replace.

Why the budget blew out:

* Each `create_element("div").expect("create_element")` is
  ~30 bytes of wasm (call shim + tag string + error string).
  A counter template with 12 elements is ~360 bytes just for
  element creation.
* `set_attribute` calls add ~25 bytes each. ~20 attrs across
  the template ≈ 500 bytes.
* `Text` and `Comment` node calls each cost a similar shim.
* The generic `builder_append<P: AsRef<Node>, C: AsRef<Node>>`
  helper monomorphises across `(&Element, &Element) /
  (&Element, &Text) / (Element, &Element) / …` combinations,
  multiplying the link-time cost.
* Strings (`"div"`, `"class"`, `"data-pp-text-managed"`, …)
  do dedup across templates, but the codegen overhead per
  template dominates.

**The structural Phase 6 binary win remains gated on moving
`walk` / `bind` / the directive `run` shims behind
`legacy-dom`** (per the previous section). Code-gen builders
are not the lever to pull. The experiment is reverted from
the tree; this section preserves the negative result so we
don't re-explore the path without a substantially different
plan (e.g. data-driven `BuildOp` arrays consumed by a single
runtime interpreter, which trades wasm code per template for
wasm data per template — possibly more competitive but a
separate experiment).

## Phase 6.4 — cloneNode-from-`<template>` mount path

The Solid/Lit-style mount-perf trick: at first use, parse the
registered HTML once into a cached `HTMLTemplateElement`.
Every subsequent mount calls `template.content.cloneNode(true)`
instead of re-parsing the HTML via `set_inner_html` per
mount. Browser HTML parser stays out of the per-mount path
(C++ `cloneNode` is materially faster than HTML parsing).

| Build (counter) | Pre-cloneNode | + cloneNode | Delta            |
|-----------------|--------------:|------------:|-----------------:|
| `default`       | 712 477 / 270 806 | 714 229 / 271 437 | +1 752 / +631   |
| lean            | 593 447 / 226 368 | 594 665 / 226 869 | +1 218 / +501   |

Bundle cost: ~1.2-1.7 KB raw (cache + the lazy-parse fast
path). The per-template HTML strings stay in the bundle
(unchanged) — the win is mount throughput, not size.

### jsbench (chromium, mean ms over 5 runs)

| Action          | Pre-cloneNode | + cloneNode | Delta  |
|-----------------|--------------:|------------:|-------:|
| run(1000)       |     230.13    |   229.27    | -0.4%  |
| update every 10th | 156.26      |   153.64    | -1.7%  |
| select          |     107.37    |   110.55    | +3.0%  |
| swapRows        |     205.62    |   199.44    | -3.0%  |
| remove          |     172.27    |   163.78    | -4.9%  |
| clear           |     217.92    |   218.99    | +0.5%  |
| runLots(10000)  |    1118.00    |  1104.01    | -1.3%  |
| add(1000)       |     298.45    |   290.44    | -2.7%  |
| **geomean**     |     236.97    |   233.81    | -1.3%  |

The jsbench harness exercises `pp-for` row creation, which
already clones rows from the row-plan template via
`clone_template_body` — the cloneNode path. Component
mounts (`<my-component>`) are a small fraction of this
benchmark, so the visible delta is in the noise (1-2% across
actions; geomean -1.3%).

Where the win actually shows up: workloads that mount many
component instances (router page transitions, list-of-
components patterns, modal/popover open/close cycles).
`pine_template_plan_fallback_audit` confirms every Pine
plan is walker-clean, so all 167 component mounts route
through this path. We don't have a dedicated micro-benchmark
for "mount N instances of the same component" — adding one
would surface the cloneNode win directly.

### Verification

* All in-tree suites green: pocopine template_plan 28/28,
  walker 50/50, pine 103/103.
* `template_clone_for` falls back to `set_inner_html` when
  the document isn't available or the cache miss can't
  populate (parse refusal, etc.) — behaviour stays
  parity-correct.
