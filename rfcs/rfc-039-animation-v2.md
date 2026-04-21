# RFC-039 — Animation v2

Status: **Implemented** (PRs 1–3 landed; macro `stagger_ms` arg
deferred). Builds on RFC-005 (pp-transition) and RFC-038
(animation v1).

## Problem

RFC-038 shipped CSS-class-based transitions on 14 Pine primitives.
The implementation surfaced five interlocking bugs (`@click`
setAttribute crash, single-element pp-if dispatch, missing pp-for
hooks, same-task class swap, FLIP zero-rect) and an audit found
several gaps that would compound as the surface grows:

- Zero `prefers-reduced-motion` respect — every primitive that
  ships without it adds an a11y debt to retrofit.
- `schedule_end` reads only the first `transition-duration` and
  fires `on_done` early on mixed-duration presets.
- Hard-coded durations / easings in atoms.css — no global theming.
- `AnimationHandle` is a callback-only API; `await`-ing or
  composing animations requires reaching through `.raw()` to web_sys.
- `collect_animated` runs `querySelectorAll` on every toggle
  (caching deferred — profiling showed it isn't the hot path,
  but we ship a 500-row stress fixture so future regressions are
  visible).
- Stagger and "layout animation anywhere" are common-pattern asks
  — both are small extensions of primitives we already shipped,
  cheaper to design in now than retrofit.

## Design — eight additive upgrades

Strictly additive. The existing six-attr `pp-transition`, preset
catalogue, and `#[component(transition = …)]` macro args all keep
working unchanged.

### §1 — `prefers-reduced-motion` respect

- `pocopine::animate::motion::install` reads
  `(prefers-reduced-motion: reduce)` once at boot + listens for
  matchMedia change events.
- `pocopine::animate::motion::effective_for(el)` walks ancestors
  for `data-pp-motion="always"` / `"reduce"` overrides; falls
  back to system preference.
- `transition::enter` / `leave` short-circuit to sync `on_done()`
  under reduced motion.
- `animate()` clamps `duration_ms` to ~1ms (so `finished()` fires
  on a microtask, preserving any composed-sequence timing).
  Opt-out via `respect_motion_preference: false` on
  `AnimateOptions` for motion-as-data UI.
- `atoms.css` has a `@media (prefers-reduced-motion: reduce)`
  block that collapses every preset duration to 1ms via the
  `--pp-tx-*-duration` custom properties.
- Per-element opt-in / opt-out via `data-pp-motion`. (Macro arg
  `motion = "always"` deferred — author the attribute directly.)

### §2 — CSS custom-property theming hooks

`atoms.css` reads every duration and easing through CSS vars:

```css
:root {
  --pp-tx-duration: 200ms;
  --pp-tx-easing: cubic-bezier(0, 0, 0.2, 1);
  --pp-tx-flip-duration: 260ms;
  --pp-tx-flip-easing: cubic-bezier(0.2, 0.8, 0.2, 1);
}
```

Per-preset overrides (`--pp-tx-fade-duration`,
`--pp-tx-slide-up-easing`, …) fall back to the global. Authors
retune all motion in one place.

### §3 — Subtree dispatch caching

**Deferred.** The plan committed to a cache. Profiling on the new
500-row stress fixture showed the bottleneck is **WAAPI animation
creation** (~2 ms × N in Firefox), not `querySelectorAll`. The
cache wouldn't move the needle on this benchmark and isn't justified
without one that does. Documented where it would help (heavy
pp-show toggle subtrees) so a future profile can reopen the
question with evidence. See `docs/animation-perf.md`.

### §4 — `transitionend` event for completion (`max`-duration parser)

Minimal fix shipped: `parse_duration` now takes the MAX of
comma-separated values instead of the first. Avoids the bug where
`opacity 100ms, transform 250ms` fires `on_done` at 100ms and
yanks the element mid-transform. The full `transitionend`-listener
upgrade is unnecessary in practice — the longest-duration timeout
covers every shipping preset and every author preset we've seen.
Revisit if mixed-property delays become common.

### §5 — `AnimationHandle::finished() -> impl Future`

`AnimationHandle` gains:

- `finished() -> impl Future<Output = ()>` — wraps
  `Animation.finished` so authors can `.await` an animation or
  compose with other Futures.
- `pause()` / `play()` / `set_playback_rate(f)` /
  `current_time() -> Option<f64>` — first-class playback control,
  no more reaching through `.raw()` for these.

### §6 — Stagger primitive

`pocopine::animate::enter_subtree_staggered(root, stagger_ms,
on_done)` dispatches `enter` to every animated descendant with
`i * stagger_ms` of delay. `on_done` fires after the LAST item
settles. Equivalent to `enter_subtree` when `stagger_ms = 0`.

`#[component(stagger_ms = N)]` macro arg deferred — call the
helper directly until that lands.

### §7 — `pp-flip` — layout animation anywhere

`<el pp-flip>` opts an element into FLIP layout animation. A
singleton MutationObserver on `document.body` watches childList
mutations; on each batch, every registered pp-flip element checks
whether its `getBoundingClientRect` shifted (≥ 2px) and
FLIP-animates the delta. Cleaned up automatically on element
removal.

Limitations: tracks DOM-mutation-driven shifts. Layout changes
caused by font load, scrollbar appearance, or CSS-only resizes
need a paired `pp-resize` until a future ResizeObserver-driven
path lands.

### §8 — `will-change` lifecycle

`transition::enter` / `leave` set `will-change: transform, opacity`
on the element at dispatch and clear it on settle. Keeps animated
content on the compositor's fast path on Firefox. FLIP already had
`will-change: transform` on `pp-tx-flip-base`.

## Implementation status

- **PR 1** — `0f16d63` `feat(core): RFC-039 PR 1 — motion preference
  + WAAPI Future + theming`. §1 §2 §4 §5 §8 + setattr fuzz test.
- **PR 2** — `afdc52e` `feat(pine-demo): RFC-039 PR 2 — 500-row
  stress fixture + perf baseline`. Stress fixture +
  `docs/animation-perf.md`. §3 deferred (justified by profile).
- **PR 3** — `170dc19` `feat(core): RFC-039 PR 3 — pp-flip directive
  + enter_subtree_staggered`. §6 §7.

## Deferred follow-ups

- **Subtree dispatch cache.** Reopen if a benchmark shows
  querySelectorAll is the bottleneck.
- **`#[component(motion = "...")]` and `stagger_ms` macro args.**
  Both are mechanical — emit `data-pp-motion` and call
  `enter_subtree_staggered` from the generated `on_setup` tail.
- **ResizeObserver-driven pp-flip.** Catches non-mutation layout
  shifts.
- **`transitionend`-event-based completion.** Strictly more
  correct than the longest-duration timeout but unnecessary on
  current presets.
- **View Transitions API integration.** Browser support reaching
  parity (Chrome 111+, Safari 18+); needs a Firefox fallback path
  and an integration story for route changes / mode swaps. Own
  RFC.
- **Spring / physics easing.** No demand yet.

## Verification

1. `wasm-pack test --firefox --headless crates/pine` — 86/86
   green (78 RFC-038 + 6 RFC-039 PR 1 + 2 PR 3).
2. `python3 examples/pine-demo/e2e/test_animations.py` — Dialog
   fade-scale enter, deferred leave removal, TagsInput chip FLIP
   on shuffle still pass end-to-end.
3. `python3 examples/pine-demo/e2e/test_motion_perf.py` — 500-row
   stress fixture baseline (mean 1133 ms shuffle on this machine).
4. Manual: set system to "reduce motion", reload demo — every
   primitive snaps without animating; `data-pp-motion="always"`
   subtrees still animate.
