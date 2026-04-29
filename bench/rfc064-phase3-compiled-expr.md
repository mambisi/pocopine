# RFC 064 Phase 3 — compiled expression ABI evidence

Measured 2026-04-30 on `wip/rfc-064-phase-1ab` working tree.

Phase 3 adds a macro-emitted `StaticExpr` descriptor for the
safe expression envelope audited in `bench/expr-audit-rfc064.md`:
identifiers, one-field paths, literals, unary `!`, simple
comparisons, and `&&` / `||` combinations. Existing `expr_src`
strings remain in the static plan for diagnostics and for the
default-enabled `runtime-expr-fallback` compatibility feature.

## Code Surface

- `pocopine_core::expr::{StaticExpr, StaticLiteral, StaticBinOp}`
  is the shared descriptor ABI.
- Macro template-plan emission now sets `compiled:
  Some(&StaticExpr::...)` for in-envelope static bindings,
  child-host bindings, and `pp-if` controller expressions.
- Out-of-envelope expressions still emit `compiled: None` and
  route through `expr::parse_cached` / `expr::evaluate` while
  `runtime-expr-fallback` is enabled.
- Directive installers gained cleanup-safe `install_eval` entry
  points so generated plans can use either compiled descriptors
  or runtime-evaluator closures without duplicating DOM patch
  logic.

## Counter Wasm Size

Command:

```bash
wasm-pack build --release --target web examples/counter
wc -c examples/counter/pkg/counter_bg.wasm
gzip -c examples/counter/pkg/counter_bg.wasm | wc -c
```

| Measurement | RFC 062 baseline | Phase 2 | Phase 3 current | Delta vs RFC 062 | Delta vs Phase 2 |
|---|---:|---:|---:|---:|---:|
| Raw wasm bytes | 347,684 | 345,992 | 346,617 | -1,067 | +625 |
| Gzip wasm bytes | 147,045 | 146,763 | 146,821 | -224 | +58 |

Phase 3 adds a small amount of shared descriptor/evaluator
machinery. Counter remains below the RFC 062 baseline, but Phase
3 is slightly larger than Phase 2.

## Twiggy Top

Command:

```bash
twiggy top -n 30 examples/counter/pkg/counter_bg.wasm
```

Top entries after Phase 3:

```text
 Shallow Bytes │ Shallow % │ Item
───────────────┼───────────┼─────────────────────
         23309 ┊     6.72% ┊ code[567]
         17253 ┊     4.98% ┊ data[59]
         14136 ┊     4.08% ┊ code[386]
         13210 ┊     3.81% ┊ data[13]
         13018 ┊     3.76% ┊ code[504]
          8624 ┊     2.49% ┊ data[35]
          7055 ┊     2.04% ┊ code[56]
          5814 ┊     1.68% ┊ code[7]
          5130 ┊     1.48% ┊ data[0]
          4930 ┊     1.42% ┊ code[830]
          4816 ┊     1.39% ┊ code[0]
          4670 ┊     1.35% ┊ data[11]
          4116 ┊     1.19% ┊ code[5]
          3268 ┊     0.94% ┊ data[33]
          3232 ┊     0.93% ┊ code[46]
          3170 ┊     0.91% ┊ code[240]
          2987 ┊     0.86% ┊ code[866]
          2919 ┊     0.84% ┊ code[10]
          2891 ┊     0.83% ┊ code[117]
          2848 ┊     0.82% ┊ code[265]
          2535 ┊     0.73% ┊ code[28]
          2440 ┊     0.70% ┊ code[169]
          2427 ┊     0.70% ┊ code[1]
          2244 ┊     0.65% ┊ code[338]
          2231 ┊     0.64% ┊ code[272]
          2220 ┊     0.64% ┊ data[6]
          2169 ┊     0.63% ┊ code[156]
          2009 ┊     0.58% ┊ code[500]
          1973 ┊     0.57% ┊ code[17]
          1973 ┊     0.57% ┊ data[61]
        177000 ┊    51.07% ┊ ... and 1369 more.
        346617 ┊   100.00% ┊ Σ [1399 Total Rows]
```

## jsbench Context

Commands:

```bash
./jsbench/benchmark.sh --all --browser firefox
python3 jsbench/measure.py --browser firefox jsbench/vanilla
```

Mean milliseconds:

| action | pocopine | vanilla | vue | leptos | yew |
|---|---:|---:|---:|---:|---:|
| run(1000) | 311.59 | 216.85 | 270.33 | 306.08 | 325.31 |
| update every 10th | 192.91 | 156.36 | 166.06 | 150.45 | 192.95 |
| select | 177.20 | 142.65 | 188.34 | 1289.51 | 179.24 |
| swapRows | 255.43 | 159.78 | 175.64 | 159.22 | 199.18 |
| remove | 228.46 | 191.90 | 208.92 | 197.34 | 238.26 |
| clear | 293.48 | 241.07 | 265.89 | 321.93 | 381.16 |
| runLots(10000) | 1246.15 | 854.29 | 1016.13 | 1376.06 | 1282.60 |
| add(1000) | 381.13 | 258.59 | 346.03 | 355.44 | 392.93 |
| geomean | 310.61 | 230.24 | 270.90 | 361.83 | 317.23 |

This is recorded for comparability with the RFC 062 baseline and
Phase 2 evidence. The Phase 3 patch is primarily an ABI and parse
avoidance change for static-plan expressions; it does not change
`pp-for` reconciliation strategy.

## Correctness Gates

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
wasm-pack test --firefox --headless crates/pocopine
wasm-pack test --firefox --headless crates/pine --lib --test pine
```

All passed on this working tree. The focused
`crates/pocopine --test template_plan` browser suite also passed
with the RFC 064 fixture that asserts compiled descriptors for
all first-envelope forms and runtime fallback for an explicit
ternary expression.
