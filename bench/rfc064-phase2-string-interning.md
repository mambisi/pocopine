# RFC 064 Phase 2 — string interning evidence

Measured 2026-04-30 on `wip/rfc-064-phase-1ab` working tree.

This is the Phase 2 start point after converting generated
static modifier and compiled `pp-for` handoffs from owned
`String` / `Vec<String>` values to static string slices where
the macro already emits framework-known values.

## Code surface

- `StaticListener.modifiers`, child-host listener modifiers,
  child-host model modifiers, and opaque directive modifiers now
  pass through generated install helpers as `&'static [&'static str]`.
- `directives::for_::install` now receives compiled
  `item_name`, `items_expr`, and `key_expr` as static strings from
  `StaticForPlan` instead of allocating owned strings in
  `install_static_for_plan`.
- The legacy thread-local template-plan registry now stores
  macro-emitted component tags as `&'static str` instead of cloning
  them into owned `String` keys.
- Dynamic user data remains owned where needed: handler names,
  runtime selector strings, model paths passed into native model
  effects, keyed row signatures, and registry-owned names were not
  folded into static lifetimes.

## Counter wasm size

Command:

```bash
wasm-pack build --release --target web examples/counter
wc -c examples/counter/pkg/counter_bg.wasm
gzip -c examples/counter/pkg/counter_bg.wasm | wc -c
```

| Measurement | RFC 062 baseline | Phase 2 current | Delta |
|---|---:|---:|---:|
| Raw wasm bytes | 347,684 | 345,992 | -1,692 |
| Gzip wasm bytes | 147,045 | 146,763 | -282 |

The change is a small size win. The main intent is allocation
hygiene in generated install paths, not a large size reduction.

## Twiggy top

Command:

```bash
twiggy top -n 30 examples/counter/pkg/counter_bg.wasm
```

Top entries after Phase 2:

```text
 Shallow Bytes │ Shallow % │ Item
───────────────┼───────────┼─────────────────────
         23556 ┊     6.81% ┊ code[564]
         17253 ┊     4.99% ┊ data[58]
         14136 ┊     4.08% ┊ code[384]
         13210 ┊     3.82% ┊ data[12]
         13018 ┊     3.76% ┊ code[500]
          8624 ┊     2.49% ┊ data[34]
          7055 ┊     2.04% ┊ code[55]
          5814 ┊     1.68% ┊ code[7]
          5130 ┊     1.48% ┊ data[0]
          4930 ┊     1.42% ┊ code[826]
          4816 ┊     1.39% ┊ code[0]
          4622 ┊     1.34% ┊ data[10]
          4116 ┊     1.19% ┊ code[5]
          3268 ┊     0.94% ┊ data[32]
          3232 ┊     0.93% ┊ code[45]
          3144 ┊     0.91% ┊ code[239]
          3044 ┊     0.88% ┊ code[862]
          2919 ┊     0.84% ┊ code[10]
          2891 ┊     0.84% ┊ code[116]
          2848 ┊     0.82% ┊ code[264]
          2535 ┊     0.73% ┊ code[28]
          2440 ┊     0.71% ┊ code[168]
          2427 ┊     0.70% ┊ code[1]
          2270 ┊     0.66% ┊ code[2]
          2242 ┊     0.65% ┊ code[336]
          2231 ┊     0.64% ┊ code[271]
          2212 ┊     0.64% ┊ data[5]
          2169 ┊     0.63% ┊ code[155]
          2009 ┊     0.58% ┊ code[496]
          1973 ┊     0.57% ┊ code[17]
        175858 ┊    50.83% ┊ ... and 1361 more.
        345992 ┊   100.00% ┊ Σ [1391 Total Rows]
```

Compared with the RFC 062 baseline top snapshot, the largest
entry moved from `code[564]` at 24,042 bytes to `code[564]` at
23,556 bytes. Symbol names remain stripped by `wasm-opt`, so this
is top-output evidence rather than named-symbol attribution.

## jsbench context

Command:

```bash
./jsbench/benchmark.sh --all --browser firefox
python3 jsbench/measure.py --browser firefox jsbench/vanilla
```

Mean milliseconds:

| action | pocopine | vanilla | vue | leptos | yew |
|---|---:|---:|---:|---:|---:|
| run(1000) | 319.83 | 221.89 | 260.76 | 338.04 | 327.65 |
| update every 10th | 194.36 | 156.09 | 167.79 | 152.72 | 191.61 |
| select | 183.55 | 143.34 | 193.43 | 1298.58 | 169.27 |
| swapRows | 260.78 | 157.19 | 173.65 | 153.98 | 192.29 |
| remove | 237.58 | 196.09 | 208.88 | 188.43 | 225.96 |
| clear | 304.24 | 252.00 | 258.37 | 332.89 | 365.57 |
| runLots(10000) | 1303.44 | 856.71 | 1046.79 | 1377.05 | 1391.83 |
| add(1000) | 376.37 | 287.90 | 331.31 | 365.17 | 407.90 |
| geomean | 318.35 | 235.65 | 269.11 | 366.51 | 314.55 |

The benchmark context is recorded for comparability only, not as
a Phase 2 speed claim. Phase 2 is expected to affect size and
install-time allocation hygiene, not `pp-for` algorithmic
throughput.

## Correctness gates

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
wasm-pack test --firefox --headless crates/pocopine
wasm-pack test --firefox --headless crates/pine --lib --test pine
```

All passed on this working tree.
