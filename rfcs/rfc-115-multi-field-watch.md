# RFC-115: Multi-field watch — `#[watch(a, b, c)]`

**Status:** Proposed
**Crates:** `pocopine-macros` (`#[handlers]`), `pocopine-core` (reactive watch primitives)
**Relates to:** RFC-026 (`#[watch(field)]` sugar), RFC-036 (watch install machinery), RFC-044 §5.10.5 (flattened-container watch), PR #279 (watch signature contract)

## Summary

Let one handler watch several fields:

```rust
#[watch(preset, mode, start_date, start_day, start_time, end_date, end_day, end_time)]
fn on_when_changed(&mut self) {
    self.recompute();
}
```

- **One field** in the list keeps today's typed contract unchanged:
  `&mut self, (next: V, prev: Option<V>)`.
- **Two or more fields** require the payload-less shape: `&mut self`
  and nothing else. With heterogeneous field types there is no
  coherent single `(next, prev)`, and the dominant real-world use
  (recompute/derive) never reads the payload.
- **Coalesced dispatch:** when several watched fields change in the
  same reactive flush, the handler runs **once**, deferred to the
  tick boundary — not once per field.

Both shapes are enforced with spanned compile errors, extending the
PR #279 contract (a mismatched watch handler is never silently
skipped).

## Motivation

The motivating shape appears verbatim in application code (the event
editor's When control) — eight watch handlers, all identical:

```rust
#[watch(preset)]
fn on_preset(&mut self, _next: WhenPreset, _prev: Option<WhenPreset>) {
    self.recompute();
}
#[watch(mode)]
fn on_mode(&mut self, _next: WhenMode, _prev: Option<WhenMode>) {
    self.recompute();
}
#[watch(start_date)]
fn on_start_date(&mut self, _next: Option<DateValue>, _prev: Option<Option<DateValue>>) {
    self.recompute();
}
// … five more, byte-identical except the field name and types
```

Problems with the status quo:

1. **Boilerplate scales linearly** with the field count, and every
   handler's `(next, prev)` types must be kept in sync with the
   struct — the payload is dead weight (`_next`, `_prev`) in every
   one of them. This is also exactly the boilerplate that produced
   the original no-arg-watch bug class PR #279 hardened against:
   authors trimmed the unused args and the watch silently died.
2. **Redundant recomputation.** A preset change that rewrites
   `start_*` and `end_*` in one handler currently re-runs
   `recompute()` up to eight times in a single flush — once per
   per-field watch. The work is idempotent, so it's waste, not a
   bug, but it's waste the author cannot avoid today.

## Design

### Attribute grammar

`#[watch(...)]` takes a comma-separated list of **field idents** —
the same tokens the single-field form takes today, one or more of
them. No new vocabulary:

- `#[watch(field)]` — unchanged, typed contract.
- `#[watch(a, b, c)]` — multi-field, payload-less contract.

The `Either::Of3(a, b, c)` spelling considered during design is
rejected: it introduces arity-suffixed variants (`Of2`…`OfN`), it
looks like a Rust path expression but is checked by nothing, and it
expresses nothing a comma list doesn't.

### Handler contracts (compile-enforced)

| List arity | Required signature | Rationale |
|---|---|---|
| 1 | `fn f(&mut self, next: V, prev: Option<V>)` | unchanged (RFC-026 / PR #279) |
| ≥ 2 | `fn f(&mut self)` | no single `(next, prev)` exists; the coalesced call has no one triggering value |

Mismatches are spanned compile errors, symmetric with the PR #279
messages:

- `#[watch(a, b)]` on a handler with value args →
  ``multi-field #[watch] handlers take `&mut self` only — the
  coalesced call has no single (next, prev); read the fields off
  self``
- `#[watch(a)]` on a no-arg handler → the existing
  ``#[watch(a)] handler must take `&mut self` and `(next: V,
  prev: Option<V>)`…`` error, now with a trailing hint: ``…or list
  every watched field: #[watch(a, b)] with a no-arg handler``.
- Stacked `#[watch(a)] #[watch(b)]` attributes remain an error; the
  message points at the list form.
- Duplicate idents in one list (`#[watch(a, a)]`) are an error.

### Dispatch semantics

- **Coalescing:** the macro installs one subscription per listed
  field, all sharing a pending cell + generation ticket (the same
  mechanism the single-field install already uses for its initial
  seed). The first triggering field in a flush schedules the handler
  via `tick::next`; subsequent triggers in the same flush bump the
  ticket and stay collapsed into that one scheduled call.
- **Initial seed:** parity with single-field watch — one coalesced
  initial invocation after `on_ready` wiring, regardless of how many
  fields are listed. (For the motivating component this replaces the
  manual `recompute()` seed in `on_mount`.)
- **Ordering:** relative order between a multi-field handler and
  single-field handlers on the same flush is unspecified, same as
  between any two watches today.
- **Reentrancy/borrow:** the handler is invoked through the same
  `Handle::new(...).update(...)` path as single-field watches, so the
  `&mut self` acquisition and the tick-deferred initial call keep the
  RFC-026 guarantees.

### Core primitive

The typed install (`watch_scope_field_now::<V>`) needs `V`, which the
`#[handlers]` macro cannot know for arbitrary fields (it sees only
the impl block; the typed single-field form recovers `V` from the
handler's own signature). Multi-field watch needs a **payload-less
subscription**: `pocopine-core` grows a
`watch_scope_field_changed(scope, name, cb)` primitive — same dirty
propagation as the typed watch (including the RFC-044 dual-key
container triggering), no value read, no downcast. Per the
core-owns-engines rule this lands in `pocopine-core`;
`pocopine-macros` stays a thin consumer.

## Alternative considered: group the fields and watch the container

RFC-044 §5.10.5 already supports this today:

```rust
#[prop(flatten)]
common: WhenFields,        // one struct holding the eight fields

#[watch(common)]
fn on_common(&mut self, next: WhenFields, _prev: Option<WhenFields>) {
    self.recompute();
}
```

Dual-key triggering fires the container watch whenever any leaf
changes, and this remains the **right answer when the group is a
real domain object** — a value the component receives or exposes as
a unit.

It is the wrong general answer for the motivating case:

- The eight fields are **internal editor state** bound via
  `pp-model` — flatten is prop machinery; regrouping forces the
  fields into the component's prop surface just to satisfy a watch.
- Every template binding and every Rust access site churns
  (`start_time` → `common.start_time`), i.e. the watch tail wags the
  data-model dog.
- The container watch delivers a cloned struct payload per leaf
  change; the recompute pattern throws it away.

The RFC therefore adds the list form and keeps container-watch as
the documented alternative; the guide gets a "which one when" note.

## Non-goals

- **Per-field payloads in multi-watch** (an enum of
  `WhichField(next, prev)` variants). Codegen-heavy, and every known
  consumer ignores the payload. A handler that needs the values reads
  them off `self`; one that needs *which* field changed should use
  single-field watches — that need is the signal the fields have
  diverging behavior.
- **Watching computed fields or cross-scope fields** — same
  restrictions as single-field `#[watch]`.
- **Predicates/filters** (`#[watch(a, if = ...)]`) — out of scope.

## Compatibility

Purely additive. Every existing `#[watch(field)]` compiles
unchanged. The PR #279 hardening errors gain one hint line. The
motivating component collapses eight handlers plus a manual seed
into one three-line handler, and stops running `recompute()` eight
times per preset change.

## Open questions

1. Should the coalesced handler also be the vehicle for a future
   `#[watch(*)]`/whole-state watch? (Deliberately unanswered; the
   list form neither enables nor blocks it.)
2. Cap on list length? None proposed — the install cost is linear
   and small.
