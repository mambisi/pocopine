# jsbench results

Cross-framework benchmarks of the same keyed table workload, driven
by Playwright over a temporary local HTTP server.

Last measured: **2026-04-30** on `wip/rfc-064-phase-1ab`.

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
