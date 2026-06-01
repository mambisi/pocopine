# RFC 054 — Compiled `pp-for` row plans

Status: Implemented

Author: Codex

Created: 2026-04-25

## 1. Summary

Pocopine should add a specialized fast path for simple keyed
`pp-for` templates.

Instead of treating each cloned row as a fresh mini template that must
be walked, parsed, and bound through the generic directive runtime,
the framework should:

1. analyze the `<template pp-for>` body once,
2. compile a compact row plan,
3. clone and patch rows directly from that plan.

The initial implementation target is benchmark-style flat tables and
lists, where this RFC should materially reduce:

- `run(1000)`
- `runLots(10000)`
- `add(1000)`
- `update every 10th`

## 2. Motivation

Current keyed `pp-for` is correct but too generic for very large lists.

Today, mounting or extending a large keyed list still pays per-row cost
for:

- runtime attribute discovery,
- directive parsing,
- recursive walker dispatch,
- effect setup on every dynamic binding,
- generic handler binding,
- full row-scope retriggering on reuse.

This is workable for ordinary UI, but it is not competitive for
`10,000`-row workloads. A benchmark-shaped app shows the problem
clearly:

- `run(1000)` can be improved with local optimizations,
- but `runLots(10000)` remains dominated by per-row setup cost,
- and `update every 10th` still pays too much generic row machinery.

Rust gives pocopine a better route than “just do more wasm”:

- compile more ahead of time,
- store denser row metadata,
- avoid runtime interpretation inside hot loops,
- patch known DOM nodes directly.

## 3. Goals

- Greatly reduce the per-row mount/bind cost for simple keyed lists.
- Keep the generic walker as the fallback for complex templates.
- Lean into Rust-side precomputation rather than more runtime parsing.
- Preserve existing template syntax; authors should not need a new API.

## 4. Non-goals

- Replacing generic `pp-for` entirely.
- Supporting every directive in the compiled fast path on day one.
- Changing user-facing `pp-for` syntax.
- Turning pocopine into a vnode/render-function framework.

## 5. Proposal

### 5.1 Add a compiled row-plan fast path

When a keyed `pp-for` body is simple enough, pocopine should compile a
row plan once and reuse it for every row instance.

Conceptually:

```rust
struct CompiledForPlan {
    key: KeyPlan,
    bindings: &'static [BindingPlan],
    listeners: &'static [ListenerPlan],
    template_shape: TemplateShape,
}
```

The generic keyed path remains available for any template that is not
eligible.

### 5.2 Eligibility

The initial fast path should only apply when the `<template pp-for>`
body satisfies all of these:

- exactly one element root,
- no registered child components,
- no `<slot>`,
- no nested `pp-for`,
- no `pp-if`, `pp-show`, `pp-teleport`, `pp-model`, `pp-init`,
- only simple supported bindings/listeners in the subtree.

Supported first-pass dynamic forms:

- `pp-text="..."`
- `:class="..."`
- `pp-bind:class="..."`
- `@click="handler(...)"`

This matches the common flat-row case well and directly targets
benchmark-style tables.

### 5.3 Compiled row plans are Rust-side metadata

The plan should not be a JS object graph assembled on every mount.
Instead, it should be a compact Rust-side structure describing:

- where dynamic nodes live,
- what expression or field they depend on,
- what kind of DOM patch they need,
- which event listeners to install.

Example binding forms:

```rust
enum BindingPlan {
    Text {
        node_path: NodePath,
        source: ValuePlan,
    },
    Class {
        node_path: NodePath,
        source: ValuePlan,
    },
}

enum ListenerPlan {
    Click {
        node_path: NodePath,
        action: ActionPlan,
    },
}
```

### 5.4 Clone once, patch directly

For eligible rows, mount should look like:

1. clone the prepared row DOM,
2. locate the precomputed dynamic nodes,
3. patch text/class values directly,
4. install direct listeners,
5. append into a `DocumentFragment`,
6. insert the fragment once.

This avoids the generic subtree walker for every row clone.

### 5.5 Reuse should skip unchanged rows cheaply

On keyed reuse, the row plan path should compare only the minimal row
state it needs and avoid re-running bindings for unchanged rows.

This RFC prefers compact cached row state over generic scope triggering.

Conceptually:

```rust
struct RowInstance {
    key: Rc<str>,
    root: Element,
    cache: RowCache,
}
```

where `RowCache` stores the last rendered values needed by the plan:

- text values,
- class values,
- maybe a compact row revision later.

### 5.6 Generic fallback remains authoritative

If a row template is not eligible, pocopine continues using the current
generic keyed `pp-for` implementation.

Correctness stays anchored in the existing runtime; the compiled plan is
an optimization path.

## 6. Design rationale

### 6.1 Why this instead of a vnode system?

Because pocopine’s strengths are:

- macro-time analysis,
- Rust-side data structures,
- direct DOM patching,
- explicit templates.

Introducing a full vnode/runtime rendering layer would move the
framework away from its current model and duplicate machinery the
generic walker already handles.

### 6.2 Why this fits Rust better than generic wasm optimism

Wasm does not make browser DOM work free.

Rust helps when used to:

- precompute row plans,
- reduce dynamic dispatch,
- minimize per-row allocations,
- keep hot loop data compact.

That is what this RFC focuses on.

### 6.3 Why keep the generic path?

Because pocopine supports much richer templates than a benchmark row.
The generic path remains necessary for:

- nested components,
- slot materialization,
- compound primitives,
- advanced directives,
- future features.

The fast path is not a semantic fork; it is a performance specialization.

## 7. Initial implementation plan

Phase 1:

- add the row-plan RFC and target shape,
- continue incremental runtime wins in the current keyed path,
- add local caches and skipped rerenders for unchanged rows,
- batch suffix insertion for bulk mounts.

Phase 2:

- compile eligible row templates into a plan,
- bypass subtree walker for eligible row clones,
- patch direct text/class/listener nodes from the plan.

Phase 3:

- add denser row cache/revision tracking,
- reduce per-row effect creation further,
- widen eligibility carefully.

## 8. Current partial implementation status

The current codebase already contains phase-1-aligned work:

- memoized `pp-text`,
- expression parse cache,
- keyed row reuse that skips retriggering unchanged rows,
- batched suffix insertion with `DocumentFragment`,
- fast no-transition removal path.

These are not the full RFC implementation, but they move the runtime
toward the compiled-row-plan direction and provide a baseline for
measuring further gains.

## 9. Example target workload

This RFC is intentionally motivated by rows like:

```html
<template pp-for="row in rows" pp-key="row.id">
  <tr :class="selected_id == row.id ? 'danger' : ''">
    <td pp-text="row.id"></td>
    <td><a @click="select_row(row.id)" pp-text="row.label"></a></td>
    <td><a @click="remove_row(row.id)">
      <span class="glyphicon glyphicon-remove" aria-hidden="true"></span>
    </a></td>
    <td></td>
  </tr>
</template>
```

This shape should become the “easy win” case for pocopine.

## 10. Drawbacks

- Adds a second internal execution path for `pp-for`.
- Requires careful eligibility checks to avoid semantic drift.
- Increases implementation complexity in a hot part of the runtime.

## 11. Alternatives considered

### 11.1 Keep only incremental micro-optimizations

Rejected. Useful, but insufficient for `10,000`-row competitiveness.

### 11.2 Full compiled templates everywhere

Rejected for now. Too large a shift in architecture. `pp-for` rows are
the hottest and narrowest target.

### 11.3 Accept large-list weakness as a wasm tradeoff

Rejected. The current gap is too large, and Rust gives us better tools
than that.

## 12. Unresolved questions

1. Should row plans be produced at macro expansion time, runtime bind
   time, or a hybrid of both?
2. Should listener plans stay on the generic `pp-on` closure path at
   first, or get their own lighter fast path immediately?
3. What is the best stable equality/revision model for reused row data
   without paying a serialization cost per item?

