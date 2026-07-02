# RFC-113: Nested signals — leaf-granular reactivity

**Status:** IMPLEMENTED, v1 (this branch)
**Crates:** `pocopine-core` (`reactive`, `scope`, `path`)
**Relates to:** RFC-112 (the substrate), RFC-095 W2 (the dirty sweep this extends), RFC-096 (signals-first core), RFC-054 (row plans — own Vec element identity)

## Summary

Reactivity granularity widens from top-level fields to **paths**:
a binding on `settings.theme` subscribes to the signal keyed
`(scope, "settings.theme")`; a handler mutating `settings.size`
re-runs only `size` subscribers. The "group store fields by update
cadence, not by theme" constraint dissolves for types that derive
`PathAccess` — nesting becomes free.

Not a fingerprint replacement: detection (how change is learned)
and addressing (how precisely it's named) are independent axes.
Handlers stay plain Rust, so detection stays **diffing** — the
sweep just asks finer questions, and each hash is cheaper (scalar
leaves instead of whole structs).

## Design

The signal graph was already string-keyed (`(ScopeId, Key)`), so
this is parameter-widening plus one new piece of logic:

1. **Leaf tracking (read side).** `resolve_path_with` offers nested
   paths to `ScopeAccess::read_path`, which tracks the FULL dotted
   key and resolves the container **untracked**
   (`read_field_untracked` — the tracked read's body minus the
   subscription). Eligibility gates, each falling back to today's
   root-tracked walk: real component/store scopes only (derived
   loop/slot scopes compose from parents), no `$`-roots, **no
   numeric segments** (positions aren't identity — keyed row plans
   own Vec elements), and `path_fingerprint` must have reach
   (without it the sweep couldn't gate the key and every handler
   would re-run the subscriber).
2. **The trigger lattice (door 1 — explicit writes).** A native
   leaf write (`write_path_tracked`) fires: the exact key,
   subscribed keys *under* it, and subscribed *ancestors*
   (whole-container readers, `#[watch(container)]`) —
   **sibling leaves stay quiet**, which is the point. A whole-field
   write down-fans to subscribed dotted keys under it
   (`trigger_nested_under` — v1 unconditional, matching today's
   effective granularity; fingerprint-filtered descent is v2).
   The flat path pays nothing: a `has_nested_keys` scope marker
   (set at dotted-key interning, cleared at scope teardown) makes
   the down-fan a single `HashSet` miss for flat-only scopes — the
   neutrality gate.
3. **The sweep (door 2 — handler mutations).** Dotted tracked keys
   route through `ComponentState::path_fingerprint`
   (`sweep_fingerprint`); roots keep `field_fingerprint`. A changed
   dotted key also invalidates its **root's projection** — the
   container snapshot leaf reads walk through must not survive a
   leaf change even when the root key itself isn't tracked.
   No-net-change, length probes, `patch_*` marks, read-only-handler
   skip: all unchanged.
4. **Invalidations.** Native leaf writes invalidate the container
   projection (they bypassed serde entirely); flatten containers
   ride the existing rails.

## What stays deliberately out (v1 → v2 lines)

- **Leaf projections / typed leaf lane** — leaf reads still walk
  the container projection; the granularity win is in *triggering*.
  v2: serialize just the leaf, scalar leaves zero-serde.
- **Fingerprint-filtered down-fan** — container replaces currently
  over-trigger nested subscribers (exactly today's behavior).
- **Vec index signals** — never: one reorder and every index key
  lies. Row plans own element identity.
- **Persistent fingerprints on signal slots** (skip `begin`
  snapshots) — widens the door-1/door-2 invariant surface; only
  with a profile demanding it.

## The invariant to keep fuzzing

Door 1 and door 2 must agree: the same value arriving via
`path_set` vs via handler mutation leaves identical fingerprint
state and fires identical keys. The `nested_signals` suite pins the
directions individually; extending the RFC-096 differential fuzz
with nested ops is the standing follow-up.

## Tests

`pocopine-core/tests/nested_signals.rs`: sibling-leaf isolation
(native write + handler sweep), container-write down-fan, ancestor
firing with fresh container projection, no-net-change leaf,
no-`PathAccess` degradation to root granularity, flat-field
neutrality. Full core battery + pine's 126-test wasm suite green
(no pine type derives `PathAccess` — zero behavior change without
opt-in).
