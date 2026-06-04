---
title: "Animation"
description: "Animation presets, macro arguments, FLIP, and the WAAPI escape hatch."
---

# Animation

pocopine ships first-class motion. Every Pine primitive animates by
default — dialogs fade, menus slide, collapsibles grow, chips reorder
with layout animation. Nothing to wire; no handler code; no CSS to
copy.

Three knobs, declared on the component:

| Knob | Purpose |
| --- | --- |
| `transition = "<preset>"` | symmetric enter + leave |
| `transition_in = "<preset>"` / `transition_out = "<preset>"` | asymmetric |
| `animate = "flip"` | layout animation on keyed `pp-for` reorder |

## Preset catalogue

Ten built-ins. Pick the one that matches the motion you want; compose
with `pp-anchor` / `pp-resize` if you need more.

| Preset | Motion |
| --- | --- |
| `fade` | opacity 0 ↔ 1 |
| `scale` | scale 0.95 ↔ 1 + opacity |
| `fade-scale` | scale 0.94 ↔ 1 + opacity |
| `zoom` | scale 0.8 ↔ 1 + opacity |
| `slide-up` | translateY +8px ↔ 0 + opacity |
| `slide-down` | translateY -8px ↔ 0 + opacity |
| `slide-left` | translateX +8px ↔ 0 + opacity |
| `slide-right` | translateX -8px ↔ 0 + opacity |
| `collapse` | grid-template-rows 0fr ↔ 1fr + opacity (no JS measurement) |
| `none` | disables the primitive's default preset |

## Declaring motion on a component

```rust
use pocopine::prelude::*;

#[derive(Default)]
#[component(template = "MyPanel.poco", transition = "fade-scale")]
pub struct MyPanel {
    #[prop] pub open: bool,
}
```

That's it. Mount/unmount the component via `pp-if` and the
`fade-scale` enter + leave run automatically.

Asymmetric enter / leave (Svelte-style `in:` / `out:`):

```rust
#[component(
    template = "Toast.poco",
    transition_in = "slide-up",
    transition_out = "fade",
)]
pub struct Toast { … }
```

## Per-instance override

Authors override any primitive's default from the template using
`pp-transition`:

```html
<!-- Override the default slide-down with a zoom -->
<pine-popover-content pp-transition="zoom">…</pine-popover-content>

<!-- Disable animation entirely for this one use -->
<pine-dialog-content pp-transition="none">…</pine-dialog-content>

<!-- Asymmetric -->
<pine-tooltip-content pp-transition:in="scale" pp-transition:out="fade">…</pine-tooltip-content>
```

Precedence: per-instance `pp-transition` attributes > `#[component]` macro default.

## FLIP — keyed list reorder

Declare `animate = "flip"` on any component rendered inside a keyed
`pp-for`:

```rust
#[component(template = "Chip.poco", animate = "flip")]
pub struct Chip { #[prop] pub value: String }
```

When the list reorders, each reused chip animates from its old layout
position to its new one. The walker snapshots `DOMRect`s before the
mutation, inverts them with a `transform: translate(dx, dy)`, and
plays the transform to `translate(0, 0)`.

FLIP and mount/unmount transitions compose — a chip can fade in on
mount, slide around on reorder, and fade out on removal:

```rust
#[component(template = "Chip.poco", animate = "flip", transition = "scale")]
pub struct Chip { … }
```

## Programmatic escape hatch (WAAPI)

For motion the preset system can't express, drop to the
`pocopine::animate` API:

```rust
use pocopine::animate::{animate, AnimateOptions, Keyframe};

animate(
    &el,
    &[
        Keyframe::from_iter([("opacity", "0"), ("transform", "translateY(20px)")]),
        Keyframe::from_iter([("opacity", "1"), ("transform", "translateY(0)")]),
    ],
    AnimateOptions { duration_ms: 240.0, easing: "cubic-bezier(0.16, 1, 0.3, 1)".into(), ..Default::default() },
);
```

The returned `AnimationHandle` exposes `.cancel()`, `.finish()`, and
`.on_finish(cb)`.

## Registering a custom preset

One call at app boot. Registered names work everywhere the built-ins
do — macro args, template attributes, `apply_preset`.

Each `Phase` carries three CSS class strings: `base` (the
`transition-property` + duration + easing rule), `from` (the
visual extreme to start from), and `to` (the settled state).
Define the atom classes in your CSS, then reference them here:

```css
/* styles.css */
.brand-rise-base {
  transition: opacity 300ms ease-out, transform 300ms ease-out;
}
.brand-rise-from { opacity: 0; transform: translateY(1rem); }
.brand-rise-to   { opacity: 1; transform: translateY(0); }
```

```rust
use pocopine::animate::{register_preset, Preset, Phase};

pub fn main() {
    App::new()
        .before_mount(|| {
            let _ = register_preset("brand-rise", Preset {
                enter: Phase {
                    base: "brand-rise-base",
                    from: "brand-rise-from",
                    to:   "brand-rise-to",
                },
                leave: Phase {
                    base: "brand-rise-base",
                    from: "brand-rise-to",   // leave starts from the settled state
                    to:   "brand-rise-from", // and animates back to the extreme
                },
            });
        })
        .run();
}
```

`register_preset` returns `Err` if the name is already taken —
overriding a built-in is a deliberate, explicit step.

## Pine primitive defaults

| Primitive | Default motion |
| --- | --- |
| Dialog / AlertDialog content | `fade-scale` |
| Dialog / AlertDialog overlay | `fade` |
| Popover content | `slide-down` |
| HoverCard content | `fade-scale` |
| Tooltip content | `scale` in, `fade` out |
| DropdownMenu / ContextMenu content (+ sub-content) | `slide-down` |
| Command content | `fade-scale` |
| Command overlay | `fade` |
| Combobox / Select content | `fade` |
| TagsInput item | `animate = "flip"` + `transition = "scale"` |
| Command / Combobox item | `animate = "flip"` |
| Collapsible / Accordion / Tree content | `collapse` |

## Stagger (RFC-039)

`enter_subtree_staggered(root, stagger_ms, on_done)` dispatches
`enter` to every animated descendant in `root`'s subtree with
`i * stagger_ms` of delay. Use it for sequenced reveals on list
mounts, popover content, etc.:

```rust
use pocopine::animate::enter_subtree_staggered;

enter_subtree_staggered(&root, 30, || {
    // fires after the LAST element settles
});
```

`stagger_ms = 0` is equivalent to the (unstaggered)
`enter_subtree`.

## Layout animation anywhere — `pp-flip`

`pp-flip` opts an element into FLIP layout animation: any time the
DOM mutates and the element ends up in a new position, the runtime
animates it from its old spot to its new one. Inspired by Framer
Motion's `layout` prop.

```html
<ul>
  <template pp-for="item in items" pp-key="item">
    <li pp-flip>{item}</li>
  </template>
</ul>
```

A singleton MutationObserver on `document.body` watches for DOM
changes; on each batch, every registered `pp-flip` element checks
whether its `getBoundingClientRect` shifted and FLIP-animates the
delta. Honours `prefers-reduced-motion`.

Limitations: tracks layout shifts caused by DOM mutations.
Position changes from font load, scrollbar appearance, or container
resize need a paired `pp-resize` until a future ResizeObserver-
driven path lands.

## Reduced motion (RFC-039)

The runtime reads `(prefers-reduced-motion: reduce)` once at
install and listens for matchMedia change events. When set:

- `transition::enter` / `leave` short-circuit to sync `on_done()`.
- `animate()` clamps the WAAPI duration to ~1ms (so finished()
  callbacks fire on a microtask).
- The CSS atom sheet collapses every preset's duration to 1ms via
  a `@media (prefers-reduced-motion: reduce)` rule.

Per-element opt-out via `data-pp-motion="always"` (and
`data-pp-motion="reduce"` to opt-in for a subtree). Use for
motion-as-data UI:

```html
<pine-progress-root data-pp-motion="always" value="42"></pine-progress-root>
```

## `@starting-style` — the modern CSS-only path

For authors who want native CSS motion with no preset machinery
at all, modern browsers (Chrome 117+, Safari 17.5+, Firefox 129+)
ship `@starting-style`. It defines the style an element starts
from the moment it's first rendered — the browser handles the
transition from there. Much simpler than the preset system when
you control the author CSS.

Disable the preset on a specific instance via `pp-transition="none"`
and drive the animation from CSS:

```html
<pine-dialog-content pp-transition="none">…</pine-dialog-content>
```

```css
pine-dialog-content {
  opacity: 1;
  scale: 1;
  transition: opacity 180ms cubic-bezier(0.16, 1, 0.3, 1),
              scale 180ms cubic-bezier(0.16, 1, 0.3, 1);
}
@starting-style {
  pine-dialog-content {
    opacity: 0;
    scale: 0.94;
  }
}
```

The demo's Dialog / AlertDialog / Command CSS uses this pattern
— see `examples/website/styles.css`. Pairs naturally with Pine
components where `pp-if` fully unmounts: each mount is a fresh
insertion, so `@starting-style` always kicks in.

Leave is still handled by the preset system (or the state machine
waits for transitionend if present) — `@starting-style` has no
unmount analogue without `transition-behavior: allow-discrete`,
which requires staying in the DOM via `pp-show` rather than
`pp-if`.

## Collapsible / Accordion: pass ONE child, style that child

Pine's Collapsible and Accordion Content accept a single child
element and let authors own its styling entirely:

```html
<pine-accordion-content>
  <div class="faq-answer">
    …your content…
  </div>
</pine-accordion-content>
```

```css
.faq-answer {
  padding: 0.5rem 1rem;
  background: var(--bg);
  border: 1px solid var(--border);
}
```

The outer `.pine-accordion-content` is the `collapse` preset's
grid container — it must stay free of padding / border /
background so the `grid-template-rows` tween can reach true 0
at the end of close. Anything with physical dimensions on the
outer leaves a "ghost row" that pops when the element
unmounts.

Non-dimensional styling (font-size, color, `[data-state]`
hooks) is fine on the outer — only box-model properties need
to move inside.

## Gotcha: don't center with `transform` if you use scale / slide presets

The `scale`, `fade-scale`, `zoom`, and `slide-*` presets all
animate the `transform` CSS property. If your CSS centres the
animated element via `transform: translate(-50%, -50%)`, the
preset's transform clobbers the centering — the element animates
from an off-centre position, then snaps to centre when the
transition class clears. Visually that reads as a flicker.

Fix: use a non-`transform` centring technique on any element you
want to scale or slide. Common patterns:

```css
/* Modal-style: position fixed + inset 0 + margin auto. */
.my-content {
  position: fixed;
  inset: 0;
  margin: auto;
  width: 90vw;
  max-width: 420px;
  height: fit-content;
}

/* Or: a centring wrapper with flexbox / grid that doesn't move. */
.my-portal {
  position: fixed;
  inset: 0;
  display: grid;
  place-items: center;
  pointer-events: none;
}
.my-portal > .my-content { pointer-events: auto; }
```

Either keeps `transform` free for the preset to own. Pine
layout-bearing hosts default to `display: contents`, so for Dialog /
AlertDialog, center the rendered portal root and keep the rendered
`.pine-dialog-content` panel as the animated card:

```css
.pine-dialog-portal {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}
.pine-dialog-overlay { pointer-events: auto; }
.pine-dialog-content { pointer-events: auto; }
```

The website Dialog / AlertDialog showcase CSS is the reference
pattern for those primitives.

## Theming hooks (RFC-039)

atoms.css reads durations and easings via CSS custom properties:

```css
:root {
  --pp-tx-duration: 120ms;
  --pp-tx-easing: ease-out;
}
```

Per-preset overrides via `--pp-tx-fade-duration`,
`--pp-tx-flip-duration`, etc. fall back to the global
`--pp-tx-duration` when absent.

## Non-goals (for now)

- **Spring / physics easing** — WAAPI `cubic-bezier` covers v0.
- **`#[component(stagger_ms = N)]` macro arg** — call
  `enter_subtree_staggered` from author code; the macro arg
  will follow.
- **Scroll-triggered animation** — pair `pp-intersect` with
  `animate::animate` manually; a helper may follow.
- **Router / outlet transitions** — needs an RFC on `pp-outlet`.
