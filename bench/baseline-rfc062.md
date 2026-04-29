# RFC 064 §3 baseline — post-RFC-062 / RFC-063 Tier 1

**Captured**: 2026-04-29 on commit `d67f925` (`main`, immediately
after PR #18 merged: RFC 062 implementation + RFC 063 Tier 1 +
RFC 064/065 drafts).

**Hardware**: Linux 6.17, single-machine local. Headless
browser runs via the `jsbench/benchmark.sh` Playwright driver.
**Absolute numbers will drift on other hardware** — relative
ordering and deltas across phases are what matter.

This document is the source of truth every RFC 064 phase PR
compares against. Per RFC 064 §3, no phase PR opens until the
baseline is committed; per §7, every phase target is **gated
by measurement** against this file.

## 1. Counter wasm size

`wasm-pack build --release --target web examples/counter`,
post-`wasm-opt`:

| Measurement | Bytes | KB |
|---|---|---|
| **Raw** | 347,684 | **339 KB** |
| **Gzip** | 147,045 | **143 KB** |

Build flags: workspace defaults (no `panic = "abort"`, no
`build-std`, no `--strip-debug` beyond `wasm-opt`'s default).

### What this means against RFC 064 targets

RFC 064 §7 quotes aspirational targets that were drafted before
this baseline was captured. Re-baselining:

| Aspirational target (RFC 064 §7) | Baseline today | Status |
|---|---|---|
| Counter gzip minimal ≤80 KB | 143 KB (full features) | Gap: 63 KB |
| Counter gzip full-features ≤100 KB | 143 KB | Gap: 43 KB |
| Counter raw ≤200 KB | 339 KB | Gap: 139 KB |

Notes:
- "Minimal" is currently undefined — feature-flagging
  animations/transitions/Pine primitives is its own RFC track.
- The gaps above assume the four RFC 064 phases land. They're
  bigger than RFC 064's draft estimates implied — those
  estimates were speculative. The §7 targets are now treated as
  **directional only**; the dashboard updates incrementally.

## 2. jsbench operation matrix

Driver: `jsbench/benchmark.sh pocopine --browser <name>`. Plan
per run: 2 warmups + 5 measured passes through the action
sequence `run · clear · run · update · swaprows · clear ·
runlots · update · add · clear`. Reported as **mean ms**.

### Firefox (Spidermonkey, headless)

| Action | mean | p50 | p95 | max | spread | n |
|---|---:|---:|---:|---:|---:|---:|
| run(1000) | 208.23 | 215.34 | 230.66 | 242.13 | 1.16× | 15 |
| update every 10th | 153.69 | 155.11 | 166.90 | 168.58 | 1.10× | 10 |
| select | 132.35 | 122.00 | 164.47 | 174.89 | 1.32× | 5 |
| swapRows | 227.67 | 187.55 | 349.88 | 388.39 | 1.71× | 5 |
| remove | 183.11 | 186.05 | 188.45 | 188.70 | 1.03× | 5 |
| clear | 205.40 | 146.89 | 375.03 | 415.79 | 2.02× | 15 |
| **runLots(10000)** | **835.55** | 835.23 | 841.32 | 842.72 | 1.01× | 5 |
| add(1000) | 268.60 | 267.04 | 277.65 | 278.89 | 1.04× | 5 |
| **geomean** | **231.11** | | | | | 8 |

### Chromium (V8, headless)

| Action | mean | p50 | p95 | max | spread | n |
|---|---:|---:|---:|---:|---:|---:|
| run(1000) | 167.37 | 163.12 | 184.06 | 185.26 | 1.11× | 15 |
| update every 10th | 135.77 | 136.39 | 145.92 | 146.17 | 1.08× | 10 |
| select | 88.47 | 87.94 | 95.86 | 97.21 | 1.10× | 5 |
| swapRows | 167.31 | 164.83 | 179.48 | 182.91 | 1.09× | 5 |
| remove | 144.94 | 145.08 | 149.52 | 150.59 | 1.04× | 5 |
| clear | 160.80 | 135.13 | 285.31 | 290.79 | 1.81× | 15 |
| **runLots(10000)** | **732.70** | 708.89 | 796.97 | 812.10 | 1.11× | 5 |
| add(1000) | 236.05 | 236.27 | 241.64 | 242.30 | 1.03× | 5 |
| **geomean** | **184.73** | | | | | 8 |

## 3. Cross-framework reference numbers (Chromium)

From `jsbench/RESULTS.md` (last measured 2026-04-25 at commit
`0ed2ad5`). Other frameworks' numbers haven't been re-measured
post-RFC-062; only pocopine has changed.

| Action | pocopine (today) | vanilla | vue | leptos | yew |
|---|---:|---:|---:|---:|---:|
| run(1000) | 167 | 153 | 153 | 167 | 163 |
| **runLots(10000)** | **733** | 628 | 674 | 739 | 740 |
| add(1000) | 236 | 377 | 389 | 401 | 437 |
| update every 10th | 136 | 237 | 240 | 213 | 245 |
| swapRows | 167 | 133 | 136 | 131 | 134 |
| clear | 161 | 133 | 144 | 149 | 148 |

Notable as of 2026-04-29:
- **runLots**: pocopine 733 ms vs leptos 739, yew 740. Already
  at parity with the two Rust competitors. ~17% behind vanilla.
- **add(1000)**: pocopine 236 ms — fastest of all five frameworks.
  No regression against vanilla.
- **update every 10th**: pocopine 136 ms — fastest of all five.
  RFC-054 row plans still paying.
- **swapRows + clear**: pocopine 26-21% slower than the others.
  These are the two RFC 064 §5.4 (keyed reconcile) targets.

Solid is not in the existing jsbench harness; reference numbers
must be cited from upstream measurements. (RFC 064 §3 follow-up
work item.)

## 4. Twiggy top contributions

`twiggy top counter_bg.wasm`, top 50 entries by shallow bytes.
Symbol names are stripped by `wasm-opt`; entries reported by
`code[N]` / `data[N]` index. Used for delta tracking via
`twiggy diff` against this snapshot.

```
 Shallow Bytes │ Shallow % │ Item
───────────────┼───────────┼──────────────────────
         24042 ┊     6.91% ┊ code[564]
         17253 ┊     4.96% ┊ data[58]
         14136 ┊     4.07% ┊ code[385]
         13058 ┊     3.76% ┊ data[12]
         13052 ┊     3.75% ┊ code[501]
          8624 ┊     2.48% ┊ data[34]
          7055 ┊     2.03% ┊ code[55]
          5814 ┊     1.67% ┊ code[8]
          5338 ┊     1.54% ┊ data[0]
          4930 ┊     1.42% ┊ code[824]
          4816 ┊     1.39% ┊ code[0]
          4794 ┊     1.38% ┊ data[10]
          4116 ┊     1.18% ┊ code[6]
          3268 ┊     0.94% ┊ data[32]
          3232 ┊     0.93% ┊ code[45]
          3144 ┊     0.90% ┊ code[239]
          3044 ┊     0.88% ┊ code[860]
          2919 ┊     0.84% ┊ code[10]
          2891 ┊     0.83% ┊ code[118]
          2848 ┊     0.82% ┊ code[265]
          2535 ┊     0.73% ┊ code[28]
          2440 ┊     0.70% ┊ code[168]
          2427 ┊     0.70% ┊ code[1]
          2270 ┊     0.65% ┊ code[2]
          2262 ┊     0.65% ┊ code[5]
          2242 ┊     0.64% ┊ code[337]
          2231 ┊     0.64% ┊ code[272]
          2212 ┊     0.64% ┊ data[5]
          2169 ┊     0.62% ┊ code[155]
          2009 ┊     0.58% ┊ code[497]
          1973 ┊     0.57% ┊ code[17]
          1973 ┊     0.57% ┊ data[60]
          1853 ┊     0.53% ┊ code[29]
          1779 ┊     0.51% ┊ code[84]
          1758 ┊     0.51% ┊ code[180]
          1715 ┊     0.49% ┊ code[23]
          1707 ┊     0.49% ┊ code[3]
          1409 ┊     0.41% ┊ code[4]
          1302 ┊     0.37% ┊ code[9]
          1295 ┊     0.37% ┊ code[11]
          1157 ┊     0.33% ┊ code[7]
          1137 ┊     0.33% ┊ code[37]
          1058 ┊     0.30% ┊ code[88]
          1015 ┊     0.29% ┊ data[26]
           989 ┊     0.28% ┊ code[241]
           977 ┊     0.28% ┊ code[475]
           969 ┊     0.28% ┊ code[70]
           962 ┊     0.28% ┊ code[154]
           959 ┊     0.28% ┊ code[68]
           953 ┊     0.27% ┊ code[240]
           917 ┊     0.26% ┊ code[34]
           910 ┊     0.26% ┊ code[209]
```

## 5. Counter mount time

The counter project does not currently emit a standalone
mount-time number; jsbench `run(1000)` (mount 1000 rows in
`pp-for`) is the closest proxy. Per-component mount time is
mediated by the RFC 062 specialized `__pocopine_mount` body
and depends on plan size — measuring it isolated requires a
dedicated harness. Tracking via jsbench `run(1000)` for now;
RFC 064 phase PRs that claim "mount time improved" must
report this number explicitly.

Current `run(1000)` reading: **167 ms (Chromium) / 208 ms
(Firefox)**.

## 6. How phase PRs use this baseline

Each RFC 064 phase PR's body must include:

1. **Counter raw + gzip delta** vs the §1 numbers above
   (signed; negative is shrinkage).
2. **jsbench delta** for the operations the PR claims to
   improve, vs the §2 tables (Chromium; Firefox optional).
3. **Twiggy diff** highlighting the symbols added or removed.
4. **Cross-framework comparison row** updated only if the
   measurement shifts pocopine's position vs Solid / Yew /
   Leptos / Vue / vanilla.
5. **Updated `bench/dashboard.md` row** (created with the next
   phase PR; not in this baseline commit).

The PR body must cite this file by commit hash so the
comparison is reproducible.

## 7. Open follow-ups

- **Solid jsbench harness** — not present in `jsbench/`. RFC
  064 §10 Q3 + §3 list this as a gap. Either add a `solid`
  directory mirroring the existing pattern, or cite published
  Solid numbers from a fixed source.
- **Counter mount-time isolation** — add a `data-pp-mount-bench`
  hook in `examples/counter/index.html` that records `mount`
  start → first paint and surfaces the number for the harness
  to scrape.
- **Minimal-build profile** — RFC 064 §7 references a "minimal"
  counter (no animations / no Pine). No such build profile
  exists yet; "minimal" needs definition before its target
  becomes measurable.
- **`wasm-opt --strip-debug --strip-producers`** — current
  `wasm-pack` invocation may not pass the maximum-strip flags.
  A one-liner experiment can confirm whether more bytes are
  recoverable before any RFC 064 phase work starts.
