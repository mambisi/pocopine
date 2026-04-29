# RFC 064 / RFC 065 measurement dashboard

Shared dashboard for phase PRs that touch runtime performance or
bundle shape. `bench/baseline-rfc062.md` remains the baseline.

| Phase | Commit/branch | Counter raw | Counter gzip | jsbench context | Notes |
|---|---|---:|---:|---|---|
| RFC 064 Phase 2 string interning | `wip/rfc-064-phase-1ab` working tree, 2026-04-30 | 345,992 | 146,763 | Firefox all-harness matrix in `bench/rfc064-phase2-string-interning.md` | Static modifier and compiled `pp-for` names now avoid install-time `String`/`Vec<String>` allocation. |
| RFC 064 Phase 3 compiled expression ABI | `wip/rfc-064-phase-1ab` working tree, 2026-04-30 | 346,617 | 146,821 | Firefox all-harness matrix in `bench/rfc064-phase3-compiled-expr.md` | Static template bindings, child-host bindings, and `pp-if` expressions use `StaticExpr` when in-envelope; default runtime fallback remains for ternary/call/concat/nested forms. |
