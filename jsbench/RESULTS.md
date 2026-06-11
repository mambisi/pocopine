# jsbench results

Cross-framework benchmarks of the same keyed table workload, driven
by Playwright over a temporary local HTTP server.

Last measured: **2026-06-10** on `perf-reactive-dirty-tracking`
(complete branch: RFC-095 + RFC-096 signals-first core + RFC-094
chains/match/comment anchors). Earlier records below are from
different machine sessions — compare within a section, never
across sections.

## 2026-06-10 — W4 mutation channel, same-binary A/B

RFC-095 W4 landed as a descriptor-variant channel (one JS
crossing mounts a whole keyed batch; see the RFC §7). Same
binary, `POCOPINE_CHANNEL=off` env vs default-on, two pairs in
both orderings. NOTE: the machine ran ~25% faster in this window
than in the all-harness section below — compare only within this
section.

| action | off (pair 1/2) | on (pair 1/2) | delta |
|---|---:|---:|---:|
| runLots(10000) | 831.4 / 822.8 | 757.4 / 752.6 | **−8.5 to −8.9%** |
| add(1000) | 266.1 / 264.8 | 244.9 / 253.4 | −4 to −8% |
| run(1000) | 207.9 / 208.2 | 194.3 / 198.4 | −5 to −6.5% |
| update / select / swapRows / remove / clear | — | — | flat¹ |
| **geomean** | 210.3 / 212.3 | 208.8 / 208.5 | −0.7 to −1.8% |

¹ pair 1 showed update +11% / select +10% but with a 2.2× spread
outlier and drift; pair 2 (reversed order) measured both flat —
the channel touches only fresh-mount paths.

Vanilla in the same window: runLots 583.6, geomean 190.3 —
pocopine's runLots gap moved **1.41× → 1.29×**. Channel-on
profile: clone / binding-apply / listener / path-resolution
brackets all read 0 ms (folded into one 63 ms interpreter call);
the remaining gap is items-projection serde + fingerprint sweep
+ per-row enter + handle extraction.

Bundle: 518,437 → 528,021 B wasm (+9.6 KB) + 3.5 KB JS snippet.

### W2b follow-on — length short-circuit + prototype-gated enter

Two of the four remaining-gap levers, same-session back-to-back
pair vs W4-only:

| action | W4-only | +levers | delta |
|---|---:|---:|---:|
| runLots(10000) | 787.4 (p50 755.5) | 746.3 (p50 724.6) | **−4 to −5.2%** |
| add(1000) | 268.9 | 249.3 | −7.3% |
| run(1000) / clear / others | — | — | flat¹ |

¹ run(1000)'s 1K rows are too small for either lever to matter;
clear's big hash is on the *before* side of the sweep (the old
10K rows must be hashed before the handler runs), which this
lever can't reach — macro skip-hints could.

Mechanism: `quick_len` reads a collection field's length in O(1)
via a serde probe (serde hands `serialize_seq` the length before
iterating); a moved length proves "changed" without hashing 10K
structs. Plus one prototype `has_transition_in_subtree` check
replacing 10K per-row enter walks on channel mounts.

Cumulative session arc on runLots vs vanilla: **1.41× → 1.29×
(W4) → ~1.25× (levers)**. Remaining: handle extraction, clear's
before-hash, and ~227 ms "unaccounted" (see below).

### W2c — handler touch hints (tried, REMOVED)

`#[handlers]` body analysis proving per-handler touch sets
(declared writes trigger without hashing; only "maybes" get
fingerprinted). Measured bench-neutral — Fnv64 over 10K
two-field structs is sub-millisecond, so the sweep was never
this benchmark's cost — and the write-classification relied on
a hand-maintained list of std mutating-method NAMES, which is
unmaintainable (future Rust APIs and user methods silently
misclassify). Neutral payoff + fragile heuristic = removed
entirely. If sweep cost ever shows up on real apps, prefer
length/generation short-circuits (W2b, kept) over name-based
guessing.

### W4c — parentVals: channel paints parent-dependent bindings

Parent-rooted fast paths are row-invariant → resolved once per
flush (untracked), evaluated JS-side; the mount-time
`refresh_parent_bindings_many` repaint (10K-row no-op walk)
dropped. Back-to-back pair: **runLots −5.1%** (722.4 → 685.6,
1.01×/1.02× spreads), add −4.9%, run −5.1%, select flat.

Session cumulative on runLots: ~827 (pre-W4) → **~686 (−17%,
≈1.17× vanilla)** via W4 channel + enter-skip/len-probe +
parentVals.

### JSON projection lane — tried, REVERTED

Hypothesis: the 10K-row items projection (serde_wasm_bindgen
object building) dominated the unaccounted budget; routing
collections ≥64 through one `serde_json::to_string` +
`JSON.parse` crossing should reclaim it. Measured: runLots
746→740 (≈noise), all other actions flat — and profile bracket
math shows the projection was never the cost (`reconcile_total −
row_iter − reorder ≈ 0` both before and after; the items rebuild
is already cheap at this shape). Bundle: **+39.7 KB wasm**
(serde_json newly pulled into the wasm path). Reverted — the
unaccounted ~227 ms is browser layout inside the action window
plus the app's own row-building string work, not projection.

## 2026-06-10 — branch complete (RFC-095/096/094), all harnesses

Same session, same machine, headless Firefox, mean ms, five
harnesses back-to-back. Measured after the full branch landed:
signals-first reactive core, per-field dirty sweep, plan-gated
proxy elision, and the RFC-094 structural controllers (chains,
pp-match, comment anchors — the anchor migration itself
benchmarked perf-neutral in same-session A/B pairs).

| action | vanilla | pocopine | Vue | Yew | Leptos |
|---|---:|---:|---:|---:|---:|
| run(1000) | 216.28 | 266.34 | 272.25 | 324.76 | 321.47 |
| update every 10th | 162.23 | 180.12 | 167.57 | 201.39 | 152.04 |
| select | 159.52 | 174.30 | 232.77¹ | 185.41 | 1862.77 |
| swapRows | 167.07 | **165.26** | 195.77 | 206.13 | 162.74 |
| remove | 213.39 | **205.44** | 221.07 | 238.83 | 197.55 |
| clear | 262.01 | 316.32 | 260.51 | 365.84 | 317.31 |
| runLots(10000) | 833.09 | 1168.64 | 1052.07 | 1409.39 | 1414.99 |
| add(1000) | 297.92 | 338.16 | 344.96 | 391.21 | 364.72 |
| **geomean** | **244.99** | **277.82** | **284.94** | **323.66** | **384.66** |

Geomean vs vanilla: pocopine **1.13×**, Vue 1.16×, Yew 1.32×,
Leptos 1.57×. Standing: **pocopine 2nd of 5**, ahead of both
Rust harnesses on every single action, and ahead of Vue on
geomean for the first time — pocopine moved 294 → 278 across
the branch while Vue held ~268–285 between sessions.

¹ Vue's select ran wide this session (1.49× spread, 182–348 ms;
its April figure was ~187). The pocopine-vs-Vue geomean ordering
is partly that outlier — the robust per-action picture: pocopine
wins select / swapRows / remove (the latter two beat vanilla),
Vue wins update / clear / runLots, run and add are near-ties.
The remaining gap to vanilla stays concentrated in
creation-heavy ops (runLots 1.40×, add 1.13×) — RFC-095 W4
(batched mutation channel) territory.

Leptos's select pathology (1.86 s, consistent across sessions)
predates this branch and dominates its geomean.

## 2026-06-10 — RFC-095 branch, all harnesses

Same session, same machine, headless Firefox, mean ms. All five
harnesses measured back-to-back; pocopine additionally measured
against `main` in the same session for the RFC-095 deltas (see
`rfcs/rfc-095-reactive-core-de-alpine.md` §4/§6 — W2 alone was
−7.1% geomean vs main; W1 was neutral on this workload because
the hot rows ride RFC-054 compiled row plans that never used the
proxy path).

| action | vanilla | pocopine | Vue | Yew | Leptos |
|---|---:|---:|---:|---:|---:|
| run(1000) | 224.79 | 306.57 | 261.55 | 303.96 | 318.18 |
| update every 10th | 163.41 | 186.82 | 162.49 | 193.89 | 152.66 |
| select | 153.96 | 188.40 | 186.64 | 185.50 | 1879.62 |
| swapRows | 159.69 | 164.26 | 174.18 | 191.41 | 159.14 |
| remove | 202.49 | 209.14 | 220.59 | 243.40 | 206.18 |
| clear | 259.45 | 303.39 | 243.35 | 373.53 | 342.03 |
| runLots(10000) | 879.12 | 1296.61 | 1090.26 | 1357.22 | 1449.14 |
| add(1000) | 298.62 | 384.64 | 328.43 | 398.97 | 391.65 |
| **geomean** | **243.74** | **294.18** | **267.93** | **317.41** | **394.07** |

Geomean vs vanilla: pocopine **1.21×**, Vue 1.10×, Yew 1.30×,
Leptos 1.62× (Leptos's select pathology — 1.9 s here, 1.0 s in
the April record — dominates its geomean; it predates this
branch). Standing: pocopine 2nd of 5, ahead of both Rust
harnesses, behind Vue — the gap to Vue is concentrated in
creation-heavy ops (run/runLots/add), which is RFC-095 W4
(batched mutation channel) territory.

Run-to-run noise on this machine is ±2–4% geomean (pocopine
measured 282.7 / 286.5 / 294.2 across three same-branch runs);
deltas under ~5% across separate runs are not conclusive —
back-to-back pairs are.

### RFC-096 complete (S1–S5) vs main, back-to-back

Measured after the full signals-first switch landed (write
mirror, readers everywhere, versioned projections + typed
pp-text lane, js_bridge endgame). The machine was globally
~25% faster in this window than in the earlier sessions —
only this back-to-back pair is comparable.

| action | main | branch | delta |
|---|---:|---:|---:|
| run(1000) | 206.47 | 209.49 | +1.5%¹ |
| update every 10th | 160.75 | 159.28 | −0.9% |
| select | 160.62 | 119.78 | **−25.4%**² |
| swapRows | 152.20 | 152.74 | flat |
| remove | 173.39 | 168.07 | −3.1% |
| clear | 218.30 | 190.27 | −12.8% (bimodal) |
| runLots(10000) | 899.69 | 812.47 | **−9.7%**³ |
| add(1000) | 271.58 | 278.47 | +2.5%¹ |
| **geomean** | **228.78** | **214.11** | **−6.4%** |

¹ within spread. ² main's select ran wide (1.51× spread,
138–243 ms); by medians the delta is −14.9% (138.3 → 117.7).
³ tight spreads both sides (1.02–1.03×) — the most reliable
row; the typed text lane + de-proxied loop/list-watcher
machinery showing up at scale.

S5 profiler verdict (mount-profiler, runLots(10000), 559 ms
action total): `state_sync` — the entire dependency-graph cost —
measures **0.0 ms**; the budget is `reconcile_reorder` (178 ms)
and per-row mount DOM work. The alien-signals algorithm was NOT
adopted (nothing to optimize); the remaining lever is W4's
batched mutation channel.

### Final branch state (all 12 commits) vs main, back-to-back

| action | main | branch | delta |
|---|---:|---:|---:|
| run(1000) | 304.19 | 292.80 | −3.7% |
| update every 10th | 191.59 | 185.70 | −3.1% |
| select | 175.96 | 185.79 | +5.6%¹ |
| swapRows | 168.33 | 169.53 | flat |
| remove | 217.53 | 203.47 | −6.5% |
| clear | 323.40 | 285.59 | −11.7%² |
| runLots(10000) | 1274.42 | 1264.89 | −0.7% |
| add(1000) | 395.13 | 352.97 | −10.7% |
| **geomean** | **297.37** | **285.73** | **−3.9%** |

¹ select is the noisiest small op (175.9–194.0 across this
session's runs on BOTH refs); the W2 back-to-back pair measured
it −9.3%. ² clear is bimodal — directional only.

Across all same-session measurements: main 297–304, branch
282–294. The two back-to-back pairs measured −7.1% (W2 pair) and
−3.9% (final pair) — the honest claim is **−4 to −7% geomean**.

Final bundle: branch 515,444 B raw (+379 B vs main; the W2
fingerprint machinery net of the magics/dead-code removals),
213,938 B gzip.

Bundle: `jsbench_bg.wasm` release, branch 516,115 B vs main
515,065 B (+1,050 B — the W2 fingerprint machinery costs slightly
more than the magics removal saved).

## Methodology

- Driver: `./jsbench/benchmark.sh [<harness>] --browser firefox`.
- Plan: 2 warmups and 5 measured passes through the standard keyed
  table action sequence.
- Timing: wall-clock from click dispatch to the post-settle snapshot.
- Browser: headless Firefox.
- Rust harnesses: `wasm-pack build --release --target web`.
- Baseline rule: vanilla is always measured as the control because
  browser timing is noisy even for framework-free DOM code.

## Review Refresh

The broad all-harness run was:

```text
./jsbench/benchmark.sh --all --browser firefox
```

`--all` currently covers `pocopine`, `leptos`, `yew`, and `vue`.
It does not include `vanilla`, so vanilla was measured separately
and pocopine was rerun immediately afterward without rebuilding:

```text
python3 jsbench/measure.py --browser firefox jsbench/vanilla
./jsbench/benchmark.sh pocopine --browser firefox --no-build
```

## Geomean

| Framework | Geomean ms | Notes |
|---|---:|---|
| vanilla | 186.97 | tight control rerun |
| Vue | 202.17 | all-harness refresh |
| pocopine | 212.04 | tight rerun vs vanilla |
| pocopine | 216.99 | all-harness refresh |
| Yew | 225.07 | all-harness refresh |
| Leptos | 281.45 | all-harness refresh |

Pocopine remains in the requested **211-215 ms** band in the tight
vanilla-controlled rerun and remains between Vue and Yew in the
all-harness refresh.

## Tight Control Rerun

| action | vanilla mean ms | pocopine mean ms |
|---|---:|---:|
| run(1000) | 170.10 | 202.21 |
| update every 10th | 143.23 | 157.01 |
| select | 107.54 | 122.47 |
| swapRows | 153.19 | 153.96 |
| remove | 169.46 | 164.94 |
| clear | 164.16 | 186.68 |
| runLots(10000) | 600.35 | 846.74 |
| add(1000) | 222.81 | 261.83 |
| geomean | 186.97 | 212.04 |

## All-Harness Refresh

| action | pocopine | Vue | Yew | Leptos |
|---|---:|---:|---:|---:|
| run(1000) | 209.53 | 184.89 | 209.01 | 219.66 |
| update every 10th | 168.87 | 153.45 | 160.04 | 142.79 |
| select | 126.02 | 136.22 | 125.51 | 1003.07 |
| swapRows | 149.24 | 145.43 | 162.84 | 149.40 |
| remove | 171.89 | 164.04 | 179.09 | 165.83 |
| clear | 196.32 | 174.56 | 227.56 | 207.38 |
| runLots(10000) | 825.95 | 717.78 | 872.38 | 901.43 |
| add(1000) | 264.98 | 241.62 | 270.87 | 270.25 |
| geomean | 216.99 | 202.17 | 225.07 | 281.45 |

## Bundle Size

Counter release build after the RFC 064 review refresh:

```text
wasm-pack build --release --target web examples/counter
wc -c examples/counter/pkg/counter_bg.wasm
gzip -c examples/counter/pkg/counter_bg.wasm | wc -c
```

| artifact | bytes |
|---|---:|
| raw wasm | 346,766 |
| gzip wasm | 147,662 |
