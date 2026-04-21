# RFC-038 — Native animations

Status: **Implemented** (PRs 1–3 landed). See also RFC-005
(pp-transition, the directive this RFC builds on).

## Problem

Pine's interactive primitives (Dialog, Popover, Tooltip, HoverCard,
DropdownMenu, ContextMenu, Command, Combobox, Select, Collapsible,
Accordion, Tabs, Tree) mount and unmount content via `pp-if` /
`pp-show` with **no transition attached**. RFC-005 shipped the
`pp-transition` directive as a six-attribute CSS-class-swap machine,
but it's not ergonomically usable by default — every author has to
wire:

```html
<template pp-if="open"
          pp-transition:enter="transition duration-200 ease-out"
          pp-transition:enter-start="opacity-0 scale-95"
          pp-transition:enter-end="opacity-100 scale-100"
          pp-transition:leave="transition duration-150 ease-in"
          pp-transition:leave-start="opacity-100 scale-100"
          pp-transition:leave-end="opacity-0 scale-95">
```

…on every mounted portal, dialog content, and expanding panel.
Result: pocopine apps snap and pop instead of animating. UX polish
felt in every competing framework — Svelte's `transition:`, Vue's
`<Transition>`, Radix's `data-state`-driven CSS, Framer Motion —
isn't available without hand-wiring.

Additionally: `pp-for`'s keyed reconcile reorders items instantly
with **no animation callbacks**. RFC-005 §10 deferred list
transitions to "a follow-up RFC once keyed diff lands". Keyed diff
landed in RFC-007; this RFC is that follow-up.

## Design

Three knobs, mirroring Svelte's surface:

| Svelte | pocopine |
| --- | --- |
| `transition:fade` | `#[component(transition = "fade")]` or `<el pp-transition="fade">` |
| `in:scale out:fade` | `transition_in`/`transition_out` macro args, or `pp-transition:in` / `pp-transition:out` attrs |
| `animate:flip` | `#[component(animate = "flip")]` on keyed pp-for items |

### Preset catalogue (atoms-based, CSS-class-swap driven)

Ships with ten presets: `fade`, `scale`, `fade-scale`, `zoom`,
`slide-up`, `slide-down`, `slide-left`, `slide-right`, `collapse`,
`none`. Each maps to three CSS classes (`pp-tx-<name>-base`,
`pp-tx-<name>-from`, `pp-tx-<name>-to`) declared in a stylesheet
injected once at `App::run()` time via
`crate::styles::inject_style("pocopine-animate-atoms", …)`.

`apply_preset(el, in_name, out_name)` stamps the preset's atom
class strings onto the six `pp-transition:*` attrs the existing
RFC-005 state machine already consumes. No rewrite of that state
machine.

Registry is extensible — authors call
`pocopine::animate::register_preset("brand-slide", preset)` at app
boot; the new name is usable everywhere the built-in ones are.

### Programmatic escape hatch — WAAPI wrapper

`pocopine::animate::animate(el, keyframes, options) -> AnimationHandle`
wraps `Element.animate()`. Handle exposes `cancel()`, `finish()`,
`on_finish(cb)`, and raw access. This is what the `flip` and
`collapse` helpers use under the hood, and what authors reach for
when preset+CSS isn't enough.

### FLIP — `pp-for` list-reorder animation

`animate::flip_from_snapshot(el, old_rect, opts)` is the primitive
driver. The FLIP algorithm is classic: snapshot rects before the
DOM mutates, compute `(dx, dy)` after, animate a transform from
`translate(dx, dy)` to `translate(0, 0)` via WAAPI with
`fill: "none"` so the element's post-animation transform clears
cleanly.

`pp-for`'s keyed path hooks this by snapshotting rects for each
reused clone before the `insert_before` reorder loop, then calling
`flip_from_snapshot` on every clone whose component declared
`animate = "flip"`. This is PR 2 of this RFC.

### Collapse — height 0 ↔ auto

`animate::collapse_to(el, open, opts)` measures `scrollHeight` at
animation start and tweens `height` via WAAPI. `overflow: hidden`
is applied for the duration so overflowing children don't bleed.
On expand, `fill: "none"` lets the element return to intrinsic
sizing after the animation; on collapse, `fill: "forwards"` keeps
the `0px` height committed.

Used by the `"collapse"` preset; Collapsible/Accordion/Tree
delegate their expand/collapse transitions through here.

## Macro integration

`#[component(…)]` gains:

- `transition = "name"` — symmetric default.
- `transition_in = "name"` — enter-only override.
- `transition_out = "name"` — leave-only override.
- `animate = "flip"` — enable FLIP on keyed-pp-for reorders.

The macro synthesises `#[prop]` fields (so authors override at
use-site via tag attributes) and appends a tail to the generated
`on_setup` that resolves the effective preset and calls
`apply_preset` on the rendered root.

## Author usage

Zero-config:
```html
<pine-dialog-root pp-model:open="dialog_open">…</pine-dialog-root>
```
Every Pine primitive ships with a sensible default preset.

Per-instance override:
```html
<pine-dialog-content transition="slide-up">…</pine-dialog-content>
<pine-tooltip-content transition-in="scale" transition-out="fade">…</pine-tooltip-content>
<pine-popover-content transition="none">…</pine-popover-content>
```

Programmatic:
```rust
use pocopine::animate::{animate, AnimateOptions, Keyframe};

let handle = animate(
    &el,
    &[
        Keyframe::from_iter([("opacity", "0"), ("transform", "scale(0.9)")]),
        Keyframe::from_iter([("opacity", "1"), ("transform", "scale(1)")]),
    ],
    AnimateOptions { duration_ms: 180.0, ..Default::default() },
);
```

Custom preset:
```rust
pocopine::animate::register_preset(
    "brand-slide",
    Preset::symmetric("my-brand-base", "my-brand-from", "my-brand-to"),
).unwrap();
```

## Non-goals

- **Spring / physics easing.** WAAPI's `cubic-bezier(…)` covers
  every taste goal in practice; a spring keyframe generator is a
  non-breaking follow-up if demand surfaces.
- **Scroll-triggered animation.** `pp-intersect` already gives you
  the event; pairing it with `animate()` is author-side glue
  available the moment PR 1 lands.
- **Stagger helper on `pp-for`.** Per-item `animation-delay` via
  CSS works today; a macro arg for stagger is a later iteration.
- **Router / outlet transitions.** Needs dedicated thought on
  `pp-outlet`'s mount semantics; separate RFC.

## Migration

- Existing `pp-transition:*` six-attr author markup keeps working
  unchanged — presets are additive.
- Existing Pine components that mount instantly today gain default
  animations. Authors who prefer the old snap-to behaviour set
  `transition="none"` on the affected component tag.

## Compatibility

- Needs browsers with Web Animations API for the programmatic
  `animate()`, FLIP, and collapse helpers (every modern evergreen
  — Chrome 36+, Firefox 48+, Safari 13.1+). The CSS-class-swap
  preset path works in older browsers back to CSS Transitions
  support (IE 10+).
- `Element.animate` absence triggers the fallback `AnimationHandle`
  which is inert — no crash, no motion.

## Verification

- `wasm-pack test --firefox --headless crates/pine` —
  `animate_preset_*`, `animate_flip_*` tests (PR 1).
- `wasm-pack test` adds per-primitive transition assertions in PR 3.
- `python3 examples/pine-demo/e2e/test_pine_demos.py` — Playwright
  visual checks: Dialog opacity transition, Collapsible height
  growth, TagsInput chip reorder transform delta.

## Implementation status

- **PR 1** — `068da09` `feat(core): pocopine::animate — WAAPI wrapper
  + preset catalogue`. The `animate` module (waapi / presets / flip /
  collapse / atoms.css), `pp-transition="<preset>"` /
  `pp-transition:in` / `pp-transition:out` shorthand expansion in
  `directives/transition.rs`, atoms stylesheet auto-injected at
  `App::run()`. Five wasm tests cover preset stamping, the
  asymmetric in/out split, the `none` clear, custom-preset
  registration, and `flip_from_snapshot` translate math.
- **PR 2** — `89d640f` `feat(core+macros): #[component] transition/
  animate args + pp-for FLIP`. Parses `transition` / `transition_in`
  / `transition_out` / `animate` on `#[component(…)]`; emits trait
  methods on `ComponentState` / `HandlerDispatch` (defaulting to `""`
  on the trait so existing components stay untouched). Walker
  `mount_component` reads the methods and stamps preset attrs +
  `data-pp-animate="<kind>"` on the **outer** custom-element tag —
  the element `pp-if` / `pp-show` hand to `transition::enter` /
  `leave`. `directives/for_.rs::run_keyed` snapshots
  `getBoundingClientRect` for reused clones with
  `data-pp-animate="flip"` and dispatches `flip_from_snapshot` after
  the reorder. Two macro tests added.
- **PR 3** — `4d4dd9e` `feat(pine): native transitions on 14
  compound primitives (RFC-038 PR 3)`. One-line `#[component]` edits:
  Dialog/AlertDialog content `fade-scale`, their overlays `fade`;
  Popover / DropdownMenu (+ SubContent) / ContextMenu content
  `slide-down`; HoverCard / Command content + Command overlay match
  Dialog; Tooltip content asymmetric (`scale` in, `fade` out);
  Combobox / Select content `fade`; TagsInput item gets `animate =
  "flip"` + `transition = "scale"`; Command / Combobox items get
  `animate = "flip"`. `docs/animation.md` ships the user-facing
  preset catalogue + override + custom-preset recipes; demo header
  explains the no-config story.

## Deferred follow-ups

- **Collapse runtime wiring.** The `"collapse"` preset routes
  through `animate::collapse_to`, but Collapsible / Accordion / Tree
  still rely on the CSS-class-swap path. Wiring `collapse_to` into
  their open/close transitions (so height tweens to measured
  `scrollHeight` instead of relying on author CSS) is the next
  iteration.
- **Per-primitive transition wasm assertions.** The macro-level
  test in PR 2 covers the stamping mechanism; per-primitive smoke
  tests asserting each Pine compound's outer tag carries the
  expected preset attrs would harden the integration but aren't
  load-bearing.
- **e2e visual checks.** Playwright assertions on the demo (Dialog
  opacity transition, Collapsible height growth, TagsInput chip
  reorder transform delta) — `examples/pine-demo/e2e/`.
