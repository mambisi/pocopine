# RFC 098 - Core hardening: atomic effect lifecycle, non-reentrant dispatch, deterministic order

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-06-12 |
| **Related** | [`rfc-095-reactive-core-de-alpine.md`](./rfc-095-reactive-core-de-alpine.md) (W0 harness, W5 revert), [`rfc-096-signals-first-reactive-core.md`](./rfc-096-signals-first-reactive-core.md) (the engine being hardened) |

## 1. Summary

The reactive core's algorithm is settled and its performance is
proven (1.10× vanilla geomean; dispatch measures 0.0 ms on the
heaviest benchmark action). What remains is its **failure surface**:
effect state smeared across four tables that are permitted to
disagree, re-entrancy that is survived rather than impossible, and
nondeterministic dispatch order that makes rare bugs unreplayable.

This RFC hardens the core with four internal changes — **zero public
API change, zero authoring change** — judged by correctness and
maintainability, with performance required only to be *neutral*
(same-session A/B gate). Explicit non-goal, by direction: no
Solid/Leptos-style ownership tree — the DOM via `release_subtree`
**is** the ownership hierarchy, and a parallel one in the graph is
redundant.

The doctrine this encodes: the engine's core fits on one screen and
was verifiable by adversarial review; every change here must
preserve that property.

## 2. The four changes

### H1 — single-copy trigger dispatch

Today `trigger`/`trigger_signal` clone the subscriber `HashSet` to
escape the `SIGNAL_DEPS` borrow, then `dispatch_subs` copies that
clone into `TRIGGER_SCRATCH` — whose entire purpose was avoiding the
allocation the caller just made. `dispatch_subs` takes the
`SignalId` instead, fills the scratch inside a short borrow, drops
the borrow, iterates. One copy, zero per-trigger allocation.

### H2 — one `EffectEntry` in a generational slab

Effect state currently lives in `EFFECTS` + `SCHEDULERS` +
`CLEANUPS` + `SIGNAL_REVERSE` (plus residue in `QUEUE` /
`SIGNAL_DEPS`). The code copes by making staleness benign
("stale entries degrade to no-ops"), which also means the tables
may legally disagree and every lifecycle operation must visit all
of them. Consolidate:

```rust
struct EffectEntry {
    body: Rc<dyn Fn()>,
    scheduler: Option<SchedulerFn>,
    cleanups: Vec<CleanupFn>,
    deps: HashSet<SignalId>,      // SIGNAL_REVERSE moves home
}
static EFFECTS: RefCell<Slab<EffectEntry>>;   // EffectId = generation<<32 | slot
```

- `release(id)` removes **one** entry — lifecycle becomes atomic; a
  half-released effect is unrepresentable.
- Generational ids preserve today's forgetting-is-safe property:
  a stale id's generation mismatches and resolves to `None`,
  exactly like today's missing-key lookup — but O(1), with slot
  reuse instead of unbounded id growth.
- `EffectId` stays a `Copy` `u64` (generation in the high bits), so
  the DOM-expando teardown lists (`track_effect_on`) and devtools
  are untouched. `ScopeId`/`SignalId` keep the shared `NEXT_ID`
  counter; only effects move to slab addressing. The debugger
  convention changes from "globally unique integer" to
  "slot:generation", printed by the `Debug` impl.

### H3 — trampoline dispatch: re-entrancy made unrepresentable

`dispatch_subs` runs computed schedulers **inline**, and a
computed's scheduler calls `trigger_signal` on its own subscribers
— re-entering dispatch mid-iteration. The current defense
(`mem::take` of the scratch, capacity-preserving restore) is
correct but commemorates two real crashes in its comments.

Replace recursion with a worklist owned by the outermost dispatch:

```text
dispatch(sid):
    worklist = [sid]
    while let Some(s) = worklist.pop():
        for eid in subs(s):
            scheduler?  → run inline (dirty-marking must stay
                          synchronous for computed laziness);
                          triggers it fires APPEND to worklist
            no scheduler → QUEUE.insert(eid)
    …batch gate, schedule_flush as today…
```

Nested dispatch no longer exists, so the scratch dance — and the
bug class it defends against — is deleted, not defended. A
re-entrancy depth guard (debug assert) documents the invariant.

### H4 — deterministic dispatch and flush order

`HashSet` iteration makes effect execution order vary per run; an
order-dependent bug (always a bug, but bugs happen) manifests as an
unreplayable fuzz failure. Subscriber sets and `QUEUE` become
insertion-ordered (`IndexSet` or `Vec` + membership bitcheck): same
asymptotics, and **every W0 differential-fuzz failure replays from
its seed alone**. For an engine whose correctness story is built on
that fuzz harness, replayability outranks hash-order's nanoseconds.

## 3. Non-goals (binding)

1. **No ownership tree.** Rejected by direction as redundant: the
   DOM anchors effect lifetimes (`track_effect_on` →
   `release_subtree`), and stale queued ids no-op safely. No
   parallel disposal hierarchy in the graph, no topological flush.
2. **No performance objectives.** H1 removes an allocation as a
   side effect of removing a contradiction; if any H-change costs
   measurable time, neutrality is restored before merge or the
   change is dropped (the W5 discipline).
3. **No compile-time field indices** (string keys → dense ids).
   Typo-safety value is low on a macro-generated surface; the
   two-tier key contract would touch every consumer. Revisit only
   with evidence of real-world key typos.
4. **No public API motion.** `track`/`trigger`/`effect`/`release`
   signatures unchanged.

## 4. Verification

- **The W0 harness is the gate**: differential fuzz (now
  seed-replayable under H4) + keyed symmetry + the full wasm test
  battery, green at every phase.
- New tests: release-during-dispatch (effect released by another
  effect mid-flush), scheduler-triggers-scheduler chains ≥3 deep
  (the old crash shape, now structurally inert), slab generation
  reuse (stale id from a reused slot resolves to None).
- Same-session A/B benchmark pair: geomean within noise (±2%).
- One-screen check (soft): `track` + `track_signal` + `trigger` +
  `dispatch` remain ≤ ~120 lines combined.

## 5. Phasing

| phase | change | risk |
|---|---|---|
| 1 | H4 deterministic order (smallest, improves the harness used to verify the rest) | low |
| 2 | H1 single-copy dispatch | low |
| 3 | H3 trampoline (delete the scratch machinery) | medium |
| 4 | H2 slab consolidation (touches every effect-table consumer) | medium |

Each phase lands with the full battery + an A/B pair, in that order
— H4 first so phases 2–4 are verified by a replayable fuzzer.
