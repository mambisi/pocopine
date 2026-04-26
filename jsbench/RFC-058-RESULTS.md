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
