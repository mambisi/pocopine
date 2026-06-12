# Road to 0.2.5 — SSR ships complete

0.2.0 made it fast (signals-first core, 1.10× vanilla, chains/match/
anchors, mutation channel). The 0.2.x line makes it solid and ends
with **full SSR** as the 0.2.5 headline. One release per workstream,
each independently shippable, each gated by the full battery + an
A/B perf-neutrality pair.

| version | ships | spec | gate |
|---|---|---|---|
| **0.2.1** | Core hardening: deterministic dispatch (H4), single-copy trigger (H1), trampoline dispatch (H3), generational-slab effect lifecycle (H2) — in that order, H4 first so the fuzzer is seed-replayable before the riskier phases. Plus: publishing pipeline (S3 static-files deploy config, blog + blogs section committed, 0.2.0 announcement goes live). | [RFC-098](../../rfcs/rfc-098-core-hardening.md) | battery + A/B neutrality per H-phase; fuzz replayable from seed |
| **0.2.2** | Field handles: `FieldHandle<T>` off `Handle<T>` (sweep-free single-field async writes), `&self` handlers skip the sweep, interior-mutability field rejection. | [RFC-097](../../rfcs/rfc-097-field-handles.md) | `fingerprint_count` pinned zero on handle sets and `&self` invokes; trybuild diagnostics |
| **0.2.3** | SSR parity foundations: pure-Rust JS-number formatter (replaces the client's js_sys call too — ONE formatter), `pocopine-expr` host backend over `serde_json::Value`, server-vs-client differential render harness. No user-facing SSR yet. | [RFC-099](../../rfcs/rfc-099-ssr-hydration.md) phase 1 | formatter differential-fuzzed vs JS; expr parity on the W0 corpus |
| **0.2.4** | Static SSR + SSG: full-document plan-stamper, state islands, hydration for bindings/interps/listeners (structural controllers still resolve client-side). Dev-mode hydration verification. | RFC-099 phase 2 | content page FCPs with wasm disabled; hydration DOM writes counter-pinned to zero |
| **0.2.5** | **SSR complete**: structural hydration (decision-labeled anchors; claim paths for chains/match/keyed pp-for) + two-tier templates (page templates leave the wasm). The headline release. | RFC-099 phases 3–4 | SSR+hydrate ≡ client mount, byte-equal under fuzz; bundle delta measured and recorded |

## Standing constraints (binding, from the RFC non-goals)

- The server renders; it never reacts. No engine server-side.
- One SSR mode; SSG = same renderer at build time; no islands/
  streaming/event-replay in 0.2.x (deferred behind 0.3).
- No ownership tree — the DOM is the lifecycle hierarchy.
- Core changes judged by correctness/maintainability; perf gate is
  *neutrality* (±2%, same-session A/B), not wins.
- Authoring surface frozen: `self.x = 1`, `.poco` templates-only,
  no signal types in user structs.

## Out of 0.2.x entirely

Streaming/suspense/islands (0.3+, only after SSR holds in
production); wasm code-splitting (parked on the trampoline/
ABI-metadata design — see the wip/experimenta-wasm-split
post-mortem); per-leaf flatten field handles, computed read-handles
(RFC-097 §7, by demand).
