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
| `fade-scale` | both simultaneously |
| `zoom` | scale 0.8 ↔ 1 + opacity |
| `slide-up` | translateY +8px ↔ 0 + opacity |
| `slide-down` | translateY -8px ↔ 0 + opacity |
| `slide-left` | translateX +8px ↔ 0 + opacity |
| `slide-right` | translateX -8px ↔ 0 + opacity |
| `collapse` | height 0 ↔ scrollHeight (measured at runtime) |
| `none` | disables the primitive's default preset |

## Declaring motion on a component

```rust
use pocopine::prelude::*;

#[derive(Default, Serialize, Deserialize)]
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

Authors override any primitive's default from the template:

```html
<!-- Override the default slide-down with a zoom -->
<pine-popover-content transition="zoom">…</pine-popover-content>

<!-- Disable animation entirely for this one use -->
<pine-dialog-content transition="none">…</pine-dialog-content>

<!-- Asymmetric -->
<pine-tooltip-content transition:in="scale" transition:out="fade">…</pine-tooltip-content>
```

Precedence: per-instance attributes > `#[component]` macro default.

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
    AnimateOptions { duration_ms: 240, easing: "cubic-bezier(0.16, 1, 0.3, 1)", ..Default::default() },
);
```

The returned `AnimationHandle` exposes `.cancel()`, `.finish()`, and
`.on_finish(cb)`.

## Registering a custom preset

One call at app boot. Registered names work everywhere the built-ins
do — macro args, template attributes, `apply_preset`:

```rust
use pocopine::animate::{register_preset, Preset, Phase};

pub fn main() {
    App::new()
        .init(|| {
            register_preset("brand-rise", Preset {
                enter: Phase {
                    base: "transition duration-300 ease-out".into(),
                    from: "opacity-0 translate-y-4".into(),
                    to:   "opacity-100 translate-y-0".into(),
                },
                leave: Phase {
                    base: "transition duration-200 ease-in".into(),
                    from: "opacity-100 translate-y-0".into(),
                    to:   "opacity-0 translate-y-4".into(),
                },
            });
        })
        .run();
}
```

Collisions error — overriding a built-in is a deliberate opt-in.

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

## Non-goals (for now)

- **Spring / physics easing** — WAAPI `cubic-bezier` covers v0.
- **Stagger helpers on `pp-for` enter** — CSS `animation-delay`
  works today; macro-level stagger may come later if demand is real.
- **Scroll-triggered animation** — pair `pp-intersect` with
  `animate::animate` manually; a helper may follow.
- **Router / outlet transitions** — needs an RFC on `pp-outlet`.
