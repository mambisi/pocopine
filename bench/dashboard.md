# RFC 064 / RFC 065 measurement dashboard

Shared dashboard for phase PRs that touch runtime performance or
bundle shape. `bench/baseline-rfc062.md` remains the baseline.

| Phase | Commit/branch | Counter raw | Counter gzip | jsbench context | Notes |
|---|---|---:|---:|---|---|
| RFC 064 Phase 2 string interning | `wip/rfc-064-phase-1ab` working tree, 2026-04-30 | 345,992 | 146,763 | Firefox all-harness matrix in `bench/rfc064-phase2-string-interning.md` | Static modifier and compiled `pp-for` names now avoid install-time `String`/`Vec<String>` allocation. |
| RFC 064 Phase 3 compiled expression ABI | `wip/rfc-064-phase-1ab` working tree, 2026-04-30 | 346,617 | 146,821 | Firefox all-harness matrix in `bench/rfc064-phase3-compiled-expr.md` | Static template bindings, child-host bindings, and `pp-if` expressions use `StaticExpr` when in-envelope; default runtime fallback remains for ternary/call/concat/nested forms. |
| RFC 064 Phase 4 `pp-for` profile | `wip/rfc-064-phase-1ab` working tree, 2026-04-30 | 346,617 | 146,821 | Firefox profiler matrix in `bench/for-profile-rfc064.md` | Diagnostic profile covers create, update, swap, remove, clear, append, prepend, full reorder, and runLots; next implementation target is head/tail keyed reconcile rather than LIS-first. |
| RFC 064 Phase 4 head/tail keyed reconcile | `wip/rfc-064-phase-1ab` working tree, 2026-04-30 | 346,617 | 146,828 | Firefox standard pocopine geomean 231.19 ms; diagnostic profiler update in `bench/for-profile-rfc064.md` | Append/prepend preserve stable prefix/suffix identities and avoid full keyed-map/reorder work for the stable side. Standard Firefox jsbench is back at RFC 062 baseline range. |
