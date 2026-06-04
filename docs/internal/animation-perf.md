# Animation performance characteristics

Baseline numbers + analysis for the RFC-038 / RFC-039 motion stack.
Run `python3 examples/website/e2e/test_motion_perf.py` to refresh
on the current machine; commit message updates can record drift.

## Stress fixture

`examples/website` exposes a "Mount 500-row stress fixture"
button that materialises a `pine-tags-input-root` containing 500
keyed `pine-tags-input-item` chips, each with `transition = "scale"`
and `animate = "flip"`. The "Shuffle" button rotates the list by
one — the worst case for FLIP because every chip's position
changes.

## Baseline (commit `0f16d63`, headless Firefox, dev build)

| Operation | Wall-clock |
| --- | --- |
| `stress_shuffle` reactive update (Vec rotate + pp-for re-run) | ~0 ms |
| Full shuffle round-trip (Playwright click → FLIP settled) | **mean 1133 ms, max 1210 ms** |
| `Animation` objects in flight after settle | 1 (clean) |

The reactive update itself is sub-millisecond. **The wall-clock cost
is dominated by creating 500 WAAPI `Animation` objects** in the FLIP
play loop — each `flip_from_snapshot` call invokes
`element.animate()` which round-trips through `Reflect::set` for
options + JS construction of a `KeyframeEffect`. WAAPI animation
creation is inherently expensive in Firefox (~2 ms each).

`getBoundingClientRect` reads are batched across the snapshot loop
and the play loop, so layout reflow cost is amortised to a small
constant (~2 reflows per shuffle, not 500).

## Why subtree-dispatch caching wouldn't help here

`collect_animated` (the `querySelectorAll` over 9 transition attrs)
is only called from `enter_subtree` / `leave_subtree`, which fire
on pp-if mount/unmount + pp-for add/remove — **never on pp-for
reorder**. For the stress fixture's shuffle, `collect_animated` is
not on the hot path. Caching it is therefore deferred until a
profile shows it actually matters (e.g. a Tree with many animated
descendants opened and closed in a tight loop).

## Where caching WOULD help (deferred)

- A `<pine-collapsible-content>` containing 100+ animated
  descendants, opened and closed repeatedly.
- A `pp-show`-toggled subtree with many `pp-transition:*` elements.

Both are uncommon today. Revisit when benchmark numbers justify
the maintenance cost of a cache + invalidation hook.

## Mitigations available today

- Wrap the stress section in `<… data-pp-motion="reduce">` to
  collapse all transitions to 1ms (RFC-039 §1).
- Set `--pp-tx-flip-duration: 0ms` per-instance to skip the FLIP
  animation entirely while keeping the reorder.
- Author-side: chunk pp-for items into windowed pages so only
  ~50 items are in flight at a time.

## Future investigations

- Does `requestIdleCallback`-batching the FLIP play loop reduce
  jank on slower devices?
- Can a single WAAPI `GroupEffect` (when standardised) replace
  the per-element `animate()` call?
- Is CSS-transition-based FLIP cheaper than WAAPI for bulk
  reorders despite the class-swap reflow tax?

Open a fresh RFC if you want to pursue these — they're all
performance optimisations, not correctness work.
