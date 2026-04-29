# RFC 064 Phase 4 — `pp-for` reconcile profile

Captured 2026-04-30 on `wip/rfc-064-phase-1ab` after the Phase 3
compiled-expression ABI commit.

This is the required Phase 4 precondition: profile keyed list
operations before adding keyed reconcile complexity. The normal
cross-framework jsbench plan is unchanged; this profile uses a
pocopine-only diagnostic plan with extra prepend and full-reorder
actions.

## Command

```bash
./jsbench/benchmark.sh --profile-bench --browser firefox
```

The command builds the pocopine jsbench harness with
`--features mount-profiler`, enables
`window.__POCOPINE_MOUNT_PROFILE`, and records the slowest-run
phase breakdown per action.

## Operation Summary

Wall-clock means from the profiled Firefox run:

| Operation | Mean ms | p50 | p95 | Max | Dominant profiled cost on slowest run |
|---|---:|---:|---:|---:|---|
| create 1,000 | 204.81 | 203.60 | 220.44 | 221.89 | reconcile 41 ms; row_iter 17 ms, reorder 23 ms |
| update every 10th | 155.18 | 155.28 | 158.70 | 159.18 | reconcile 10 ms; row_iter 6 ms |
| swap | 196.34 | 188.85 | 216.87 | 221.62 | DOM insertion/reorder 12 ms |
| remove | 216.00 | 202.22 | 278.65 | 294.38 | reconcile 12 ms; row_iter 8 ms |
| clear | 216.47 | 170.28 | 346.12 | 369.45 | leaver_drain 179 ms |
| append 1,000 | 267.82 | 263.72 | 281.99 | 284.14 | reconcile 60 ms; row_iter 24 ms, reorder 31 ms |
| prepend 1,000 | 287.36 | 281.68 | 301.51 | 305.15 | reconcile 74 ms; row_iter 38 ms, reorder 34 ms |
| full reorder | 331.96 | 326.26 | 344.19 | 344.78 | DOM insertion/reorder 43 ms; row_iter 20 ms |
| create 10,000 | 894.87 | 899.37 | 908.65 | 909.00 | reconcile 349 ms; row_iter 111 ms, reorder 231 ms |

## Slowest-Run Phase Details

```text
run(1000)              action_total_ms=  66.00
  mount                     14.00
  reconcile                 41.00
    reconcile.row_iter      17.00
    reconcile.reorder       23.00

update every 10th      action_total_ms=  22.00
  reconcile                 10.00
    reconcile.row_iter       6.00

swapRows               action_total_ms=  40.00
  mount.dom_insertion       12.00
  reconcile                 15.00
    reconcile.reorder       12.00

remove                 action_total_ms=  20.00
  reconcile                 12.00
    reconcile.row_iter       8.00

clear                  action_total_ms= 211.00
  reconcile                179.00
    reconcile.leaver_drain 179.00

add(1000)              action_total_ms=  96.00
  mount                     22.00
  reconcile                 60.00
    reconcile.row_iter      24.00
    reconcile.reorder       31.00

prepend(1000)          action_total_ms= 119.00
  mount                     32.00
  reconcile                 74.00
    reconcile.row_iter      38.00
    reconcile.reorder       34.00

reorder all            action_total_ms= 154.00
  mount.dom_insertion       43.00
  reconcile                 66.00
    reconcile.row_iter      20.00
    reconcile.reorder       43.00

runLots(10000)         action_total_ms= 625.00
  mount                    150.00
  reconcile                349.00
    mount.clone_template_body    61.00
    mount.initial_binding_apply  39.00
    mount.listener_installation  27.00
    reconcile.row_iter          111.00
    reconcile.reorder           231.00
```

The profiler totals overlap: `reconcile_total_ms` encloses row
mount work during create/append/prepend. The subphase values are
still useful for bottleneck direction, but the phase rows should
not be summed as independent costs.

## Decision

The profile justifies optimizing keyed reconcile, but not by
starting with an LIS-only pass:

- `swapRows` already moves a small amount of DOM; the slowest run
  spends 12 ms in insertion/reorder, not enough to justify LIS
  as the first patch.
- `reorder all` does show DOM movement cost, but a full reverse
  has little stable subsequence to preserve, so LIS would not
  remove much movement in that case.
- `append`, `prepend`, and initial create pay avoidable row-iter
  and reorder work over already-stable prefixes/suffixes.
- `clear` still has a leaver-drain cost spike and needs a
  narrower teardown profile before changing the reconcile
  algorithm around it.

Next Phase 4 implementation step: add a Vue-style head/tail
reconcile fast path for append/prepend/stable-prefix cases before
considering LIS. Keep full-reorder LIS as a follow-up only if a
non-reverse reorder profile shows DOM movement dominates.

## Phase 4 Checkpoint — Head/Tail Fast Path

Implemented after the profile:

- append fast path: when old row JS object identities still match
  the new array prefix, update existing loop metadata, create only
  the appended suffix, and batch one fragment insert before the
  controller template;
- prepend fast path: when old row JS object identities still match
  the new array suffix, create only the prepended prefix and insert
  it before the old first row;
- `prepend_list_inline`: targeted cache helper used by the
  diagnostic harness so prepend preserves existing tail object
  identities instead of reserializing the whole array;
- compiled-row bulk teardown cleanup now skips empty side tables
  and clears mount epochs in one thread-local borrow.

Counter size after this checkpoint:

| Measurement | RFC 062 baseline | Phase 3 | Phase 4 checkpoint |
|---|---:|---:|---:|
| Raw wasm bytes | 347,684 | 346,617 | 346,617 |
| Gzip wasm bytes | 147,045 | 146,821 | 146,828 |

Standard non-profile Firefox pocopine run:

```text
run(1000)                205.28
update every 10th        156.68
select                   129.22
swapRows                 196.79
remove                   214.11
clear                    202.77
runLots(10000)           873.03
add(1000)                263.24
geomean                  231.19
```

Profiled Firefox diagnostic run after the checkpoint:

| Operation | Before mean ms | After mean ms | Notes |
|---|---:|---:|---|
| create 1,000 | 204.81 | 207.71 | essentially unchanged |
| update every 10th | 155.18 | 156.59 | unchanged |
| swap | 196.34 | 194.57 | small win |
| remove | 216.00 | 210.75 | small win, still variable |
| clear | 216.47 | 213.65 | side-table cleanup did not move leaver_drain materially |
| append 1,000 | 267.82 | 264.34 | small win |
| prepend 1,000 | 287.36 | 270.93 | meaningful win once identity-preserving prepend is used |
| full reorder | 331.96 | 350.97 | worse; still not an LIS target from this reverse-only profile |
| create 10,000 | 894.87 | 899.92 | unchanged within run noise |

Decision after checkpoint: keep the append/prepend identity fast
path because it restores the standard Firefox jsbench geomean to
the RFC 062 baseline range and materially improves the targeted
prepend diagnostic. Do not add LIS yet: the profile still shows
reverse-order reorder as a DOM-movement case with little stable
subsequence to preserve, and the standard jsbench path is already
back at baseline.
