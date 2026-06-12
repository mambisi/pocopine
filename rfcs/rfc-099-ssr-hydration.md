# RFC 099 - Server-side rendering: stamp on the server, claim on the client

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-06-12 |
| **Related** | [`rfc-058`] (compiled plans — the shared artifact), [`rfc-094-conditional-chains-and-enum-matching.md`](./rfc-094-conditional-chains-and-enum-matching.md) (comment anchors = hydration claim positions), [`rfc-092`] (Stylekit — inlined critical CSS), [`rfc-080`] (deploy: the `web` process), [`project_template_size_strategy`] (two-tier templates) |

## 1. Summary

One SSR mode, no menus: **the server executes the same compiled
plans the client does — stamping HTML strings instead of DOM — ships
a render-complete document with serialized state and decision-labeled
anchors, and the client hydrates by *claiming* that DOM, writing
nothing.** SSG is the same renderer run at build time, not a second
mode. Streaming, suspense, islands, and event replay are explicitly
deferred refinements.

Binding doctrine (from first principles, recorded): **the server
renders; it never reacts.** No reactive engine boots server-side —
loaders are `async fn`s, rendering is string-stamping, and
reactivity exists only where the DOM can anchor effect lifecycles.
(This is also why no ownership tree is needed: hydration attaches
effects to real elements, same as a client mount.)

## 2. The pipeline

```text
BUILD    #[component] emits compiled plan + cleaned HTML   (exists today)

SERVER   route loader runs → structs built, on_setup runs (plain Rust)
         plan-stamper walks the SAME plans → HTML string:
           · bindings/interps filled from state
           · chains/match RESOLVED; anchor carries the decision
             (<!--pp:cond:1-->, <!--pp:match:Ready-->)
           · pp-for expanded: N rows + <!--pp:for-->
         + <script type="application/json" data-pp-state> per root
         + Stylekit CSS inlined
         → FIRST PAINT requires zero JS/wasm

CLIENT   wasm deferred + preloaded → hydrate:
           · Deserialize state; on_setup does NOT re-run;
             on_mount / on_ready fire here (they touch DOM/timers)
           · plans BIND instead of stamp: resolve node paths against
             existing DOM, install effects + listeners, write nothing
           · controllers claim labeled anchors + standing clones;
             pp-for rebuilds its keyed pool from the rows in place
```

## 3. Why this shape

**Paint speed.** FCP is pure HTML+CSS — the pixels are in the
payload. TTI is attacked through the bundle, not tricks: hydration
is O(bindings) with zero initial DOM writes, and SSR is what unlocks
the two-tier template plan (page templates leave the wasm because
the server delivered them as DOM). Zero layout shift and zero
hydration flash are *consequences of the parity invariant*, not
aspirations.

**Correctness.** Both sides execute the same artifact — the same
`StaticTemplatePlan`, node paths, cleaned HTML, and expression ASTs.
Hydration is not a second renderer trying to agree with the first;
it is the same plan in a different output mode. Correctness reduces
to a finite parity checklist (§4), most of which 0.2.0 already
built.

## 4. Parity invariants

| invariant | mechanism |
|---|---|
| same template structure | cleaned HTML is the shared artifact |
| stable claim positions | RFC-094 comment anchors; the server appends the decision to the label so the client claims without re-evaluating |
| same state | scope state serialized into the document; client deserializes. `on_setup` server-only; `on_mount`/`on_ready` client-only |
| same text for same value | **the gating risk**: client text uses JS `String()` number semantics (`js_number_string` via js_sys); the server needs a pure-Rust implementation of JS number formatting (shortest round-trip + JS exponent thresholds). Lands FIRST (phase 1) with exhaustive differential tests |
| same expression results | `pocopine-expr` gains a host backend over `serde_json::Value`; the two interpreters become a differential-fuzz target (render server-side, hydrate, compare to a pure-client mount — byte-equal or fail; the W0 pattern) |
| keyed rows claimable | keys recomputed from deserialized items; rows claimed in document order; count/key mismatch ⇒ per-list re-stamp fallback |

**Divergence policy (opinionated):** hydration mismatches are build
bugs. Dev: verify (compare claimed DOM against a client-side stamp)
and fail loudly with a diff. Prod: trust, with per-subtree re-stamp
fallback on structural mismatch — degraded, never wrong.

## 5. What's new (the cost)

1. **Server plan-stamper** in `pocopine-server`: walks a plan +
   state to an HTML string. Mechanical except for:
2. **`pocopine-expr` host backend** — evaluate the existing AST
   against `serde_json::Value`. Same parser, second evaluator,
   differential-tested against the wasm one.
3. **JS-number formatting in pure Rust** — shared by the stamper
   and (replacing the js_sys call) optionally by the client, so
   there is literally one formatter.
4. **Claim-mode installs** in controllers and binding setup: a
   hydrate flag routing "resolve + subscribe, don't write" and
   "claim anchor + adopt clone" beside the existing create paths.
5. **State serialization**: one JSON island per root scope;
   contract = the component's existing serde surface (what the
   sweep and templates already see — `#[serde(skip)]` is invisible
   here too, consistently).

## 6. Phasing

Phases map to the 0.2.x release ladder
(`docs/internal/roadmap-0.2.x.md`): phase 1 → 0.2.3, phase 2 →
0.2.4, phases 3–4 → **0.2.5, the headline release: SSR ships
complete**.

| phase | delivers | gate |
|---|---|---|
| 1 | parity foundations: pure-Rust JS number formatting; expr host backend; server-vs-client differential render harness | formatter differential-fuzzed; expr eval parity on the W0 corpus |
| 2 | static SSR + SSG: full-document stamp, state islands, hydration for bindings/interps/listeners (structural controllers re-resolve client-side) | a content page FCPs with wasm disabled; hydration writes zero DOM mutations (counter-pinned) |
| 3 | structural hydration: decision-labeled anchors; claim paths for chains/match/keyed pp-for | differential: SSR+hydrate ≡ client mount, byte-equal, fuzzed |
| 4 | two-tier integration: page templates dropped from wasm | bundle delta measured and recorded |
| — | deferred: streaming, suspense, event replay, islands | each only after 1–4 hold in production |

## 7. Non-goals (binding)

1. **No server-side reactive engine** — ever, in this RFC's scope.
2. **No mode matrix** — one SSR mode; SSG is the same renderer at
   build time; no per-component/per-route rendering options.
3. **No islands/partial hydration** in v1 — full-document hydrate;
   the bundle work makes "hydrate everything" cheap before
   selective hydration earns its complexity.
4. **No second template language** — the server renders compiled
   plans, never re-parses `.poco` at request time.
