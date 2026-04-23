# RFC 044 — `#[model]` field role for two-way component contracts

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-23 |
| **Supersedes** | Extends [RFC 009](./rfc-009-pp-model-components.md) and [RFC 031](./rfc-031-prop-vs-state.md) |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 028](./rfc-028-emit.md), Vue 3 `defineModel()`, React controlled component patterns |

## 1. Summary

Add a third field role to `#[component]`:

- **`#[prop]`** — parent-writable, one-way *in* contract.
- **`#[model]`** — parent-writable **and** child-emittable,
  two-way contract.
- **state (unmarked)** — internal only; parent writes are dropped.

Today pocopine splits two-way component binding across **two
separate declarations**:

1. the field must be marked `#[prop]` so the parent can write it,
2. the component author must remember to manually emit
   `pp:update:<field>` with the right payload and at every state
   transition that should round-trip.

Concretely, today's author has to keep **four separate things**
aligned by hand:

1. field role,
2. field name,
3. event name,
4. payload normalization.

That split is easy to get wrong. The field role says "this is part
of the public contract", but the emit path is still ad hoc handler
code. Refactors then leave parent and child out of sync without any
single place in the codebase expressing the full contract.

`#[model]` makes the contract machine-readable:

```rust
#[component(template = "PineCalendarRoot.poco")]
pub struct PineCalendarRoot {
    #[model]
    pub value: Option<DateValue>,

    #[model]
    pub placeholder: Option<DateValue>,

    #[prop]
    pub min_value: Option<DateValue>,

    pub heading: String,
}
```

From that one declaration, the framework knows:

1. the field accepts parent writes,
2. the field participates in `pp-model:<field>`,
3. the field has an outbound model event name,
4. the field's wire format comes from its serde shape,
5. the field has a guaranteed dispatch origin that does not depend
   on handwritten `pp-ref="root"`,
6. devtools/docs can present it as a two-way public field rather
   than just a prop.

This RFC **does** make `#[model]` assignments advance the public
two-way contract, but only under explicit runtime rules:

- parent mirror-in writes do not echo back out,
- setup / initial seeding does not emit,
- multiple writes in one turn coalesce to the final value,
- emission uses the field's canonical serde shape.

So the author model becomes:

- `self.value = next` on a `#[model]` field means "update the field
  and publish the new public value",
- unmarked state and `#[prop]` fields keep today's plain local-write
  semantics.

## 2. Motivation

### 2.1 The current contract is split across unrelated code paths

RFC-031 made `#[prop]` explicit. That fixed the "parent can stomp
internal state" problem, but it intentionally stopped short of
describing **two-way** fields.

RFC-009 added `pp-model` on components, but the outward half of the
contract stayed convention-based:

- the component author chooses which field is model-like,
- manually calls `emit_model_field("name", payload)`,
- must remember the exact event name,
- must serialize the payload in the shape parent bindings expect,
- must re-emit on every transition where the parent's view of that
  field should stay current,
- and must ensure the event actually has a valid dispatch origin
  (today often `pp-ref="root"`).

That means the real public contract is fragmented across:

- the struct field declaration,
- the directive runtime,
- template usage,
- one or more handlers,
- and sometimes helper functions added later during bug fixes.

### 2.2 This failure mode is recurring, not hypothetical

Recent regressions in Pine exposed the same class of problem
multiple times:

- named `pp-model:<field>` support landed, but components that still
  emitted the generic `pp:update:model` silently desynced,
- child prop writes normalized kebab-case in one path but not
  another, because the "real contract" wasn't centralized,
- typed date-model migration updated the field types but missed
  `placeholder` re-emits, so parent state stayed stale and later
  prop changes appeared "ignored",
- wrapper components forgot `pp-ref="root"`, so even correctly named
  model emits had no DOM origin and silently vanished.

The common cause is not dates, popovers, or calendars. The cause is:

> **the field role and the event contract are declared separately,
> so refactors frequently update one without the other.**

### 2.3 Typed values alone are not enough

Migrating calendar props from `String` to `Option<DateValue>` was a
good change, but it did not prevent sync bugs. The type system could
prove "this field holds a date", yet still could not prove:

- whether the field should emit at all,
- which event name it should emit on,
- what payload shape should represent `None`,
- or whether a companion field like `placeholder` was also part of
  the two-way public contract.

So the missing type is not just the value type. The missing type is
the **model contract role**.

## 3. Non-goals

- **Replacing `#[prop]`.** `#[prop]` remains the one-way "public
  input" role. `#[model]` is additive, not a rename.
- **A second runtime channel beyond `pp-model`.** This RFC refines
  the existing `pp-model:<field>` contract; it does not introduce a
  separate "signal" or "store" system.
- **Computed / derived fields.** `#[computed]` remains a separate
  possible future RFC.
- **Removing backwards compatibility for string-authored attrs in
  v1.** Static HTML attrs still arrive as strings. Serde remains the
  boundary normalizer.
- **Unconditional echo on every model-field write.** `#[model]`
  assignment is only safe if the runtime tracks write origin,
  suppresses mirror-in echo, silences setup seeding, and coalesces
  multiple writes in one turn.

## 4. Surface

### 4.1 Attribute syntax

Bare marker in v1:

```rust
#[component(template = "…")]
pub struct Thing {
    #[model]
    pub value: String,
}
```

### 4.2 Optional rename for wire name

The default model name is the Rust field name, exactly as
`#[prop]` works today after kebab-case normalization at the template
boundary. For fields where the wire name should differ, allow:

```rust
#[model(name = "open")]
pub is_open: bool,
```

This is deliberately symmetric with future `#[prop(name = "…")]`
work if that ever lands.

### 4.3 Assignment semantics

For `#[model]` fields, plain assignment becomes the public-contract
advance point:

```rust
#[component(template = "PineCalendarRoot.poco")]
pub struct PineCalendarRoot {
    #[model]
    pub value: Option<DateValue>,
}

#[handlers]
impl PineCalendarRoot {
    pub fn select_date(&mut self, next: Option<DateValue>) {
        self.value = next;
    }
}
```

The runtime treats that write as:

1. update the field,
2. mark the model field dirty for this turn,
3. coalesce repeated writes,
4. emit `pp:update:value` from the captured component host/root when
   the turn completes,
5. skip that emit when the write origin says the change came from
   parent mirror-in or setup seeding.

So authors stop hand-writing `emit_model_field("value", …)` strings
throughout handlers, and stop depending on template-local refs to
make model emission work.

### 4.4 Interaction with `#[observe(KEY)]` (RFC-036)

`#[observe(KEY)]` and `#[model]` are orthogonal axes:

- `#[observe(KEY)]` is mirror-in from an injected `Handle<Root>` or
  other provided parent-scope context,
- `#[model]` is mirror-out to an author's `pp-model:<field>` binding.

They may coexist on the same field when a component intentionally
bridges both directions. Example: a compound input may observe a
root-owned query field while also exposing that same query as its
public two-way contract to the outside world.

This RFC does not change RFC-036 semantics. It only makes the
outbound half declarative when the field is also a model field.

## 5. Behavior

### 5.1 Role semantics

| role | parent can write | plain assignment publishes | participates in devtools as public contract |
|---|---:|---:|---:|
| `#[prop]` | yes | no | yes |
| `#[model]` | yes | yes | yes |
| state (default) | no | no | no |

`#[model]` is therefore a *strict superset* of `#[prop]` from the
runtime's perspective.

### 5.2 `pp-model:<field>` mirror-in

Unchanged in spirit from RFC-009 / RFC-031:

- the parent side of `pp-model:<field>` may only write into fields
  marked `#[model]` or `#[prop]`,
- state fields still reject mirror-in writes.

### 5.3 Outbound event naming

`#[model] pub value: T` reserves the event name
`pp:update:value`. `#[model(name = "open")]` reserves
`pp:update:open`.

The framework, not handwritten component code, should own that name.

### 5.4 Payload shape

Payloads use the field's serde representation directly.

That means:

- `String` emits a JS string,
- `bool` emits a JS boolean,
- `Option<T>` emits either the serde shape of `T` or `null`,
- `DateValue` can serialize to an ISO string because its serde impl
  chooses that representation.

Value-transforming serde attributes on the field are respected,
because the generated model helper serializes the real field
value through its natural serde impl rather than through a
hand-written per-component adapter. Specifically:

- `#[serde(serialize_with = "…")]` and `#[serde(with = "…")]`
  apply — they shape how the value is encoded, which is exactly
  what an emit needs.

Key-affecting serde attributes are semantically inapplicable here
and have **no effect** on model emission:

- `#[serde(rename = "…")]` — renames the field's KEY in a parent
  struct's output. Model emission sends the value as
  `CustomEvent.detail`, with the wire name set by the Rust field
  identifier (or `#[model(name = "…")]`). There is no key to
  rename.
- `#[serde(skip_serializing_if = "…")]` — controls whether the
  field is INCLUDED in a parent struct's output. Model emission
  always fires when a publishable write is flushed; RFC-044's
  §5.5 origin suppression is the mechanism for conditional
  emission. `Option<T>::None` canonicalises to `null` on the
  wire because that's the natural serde shape.

This keeps `#[model]` from needing manual
`maybe_date_string(...)`-style helpers to express "emit this
field's public value," while avoiding the footgun where a
key-affecting attr appears to work in the struct context but
produces `undefined` on the model wire.

### 5.5 Assignment-driven emission semantics

The runtime tracks the origin of every `#[model]` write. Minimum
origins for v1:

- **ParentModelIn** — a `pp-model` mirror-in write from the parent,
- **LocalHandler** — a write performed inside the component's own
  handler / lifecycle code,
- **SetupSeed** — mount/setup/initial hydration seeding,
- **ObserveMirror** — a write originating from `#[observe(KEY)]` or
  equivalent context mirroring.

Emission rules:

- **LocalHandler** writes are publishable,
- **ParentModelIn** writes do not echo back out,
- **SetupSeed** writes do not emit,
- **ObserveMirror** writes follow LocalHandler semantics unless a
  future RFC narrows that further.

Multiple writes to the same `#[model]` field in one turn coalesce to
the final value before the outbound event fires.

This is the core runtime cost that makes assignment-driven model
syntax safe rather than deceptively clean.

### 5.6 Dispatch origin

Generated model emit paths dispatch from the component's captured
host/root element, obtained from lifecycle context during mount or
ready. A missing `pp-ref="root"` in the template must not be able to
break `#[model]`.

This is a core part of the contract. Two-way fields should work from
their declaration alone; they should not depend on an additional
template convention that authors can forget.

### 5.7 Backwards compatibility: empty-string clear shim

Fields of type `Option<T>` accept an incoming `""` as `None` at the
proxy set trap. This is a migration affordance for static HTML and
older model flows that wrote values like `placeholder=""`.

That shim belongs in the **runtime deserialization boundary**, not
in every component's emit code.

This RFC therefore keeps:

- runtime: may interpret `""` → `None` when the destination field is
  `Option<T>`,
- outbound model helpers: should emit the field's canonical serde
  shape, i.e. `null` for `None`, not `""`.

This makes the contract more typed without breaking authored markup
overnight. The shim is explicitly scoped to be removable by a future
RFC once the ecosystem has migrated.

### 5.8 Author ergonomics

Before:

```rust
pub fn select_date(&mut self, iso: String) {
    let Some(date) = DateValue::parse_iso(&iso) else { return };
    let mut s = self.build_state();
    s.select_date(date);
    self.value = s.selected;
    self.placeholder = Some(s.placeholder);
}
```

After:

```rust
pub fn select_date(&mut self, iso: String) {
    let Some(date) = DateValue::parse_iso(&iso) else { return };
    let mut s = self.build_state();
    s.select_date(date);
    self.value = s.selected;
    self.placeholder = Some(s.placeholder);
}
```

Still explicit, but no stringly event names, no duplicated payload
normalization logic, and no extra helper vocabulary at the callsite.

## 6. Rationale

### 6.1 Why assignment-driven emission anyway?

Because it is the cleanest author-facing model.

The desired component code is:

- assign to the field,
- let the runtime publish the two-way contract once per turn.

That is easier to teach, easier to read, and much harder to forget
than a separate emit call or generated helper method.

The price is runtime machinery: origin tracking, setup silence, and
coalescing. This RFC accepts that price because the API surface is
worth it.

### 6.2 Why not just keep `#[prop]` + manual `emit_model_field`?

Because that is exactly the pattern that keeps regressing.

It asks the author to manually keep these four things in sync:

1. field role,
2. field name,
3. event name,
4. payload normalization.

The compiler sees no relationship between them. Refactors therefore
update two or three and forget the fourth.

### 6.3 Why not reuse `#[prop(two_way)]`?

Possible, but weaker as API language.

`#[model]` communicates intent directly:

- this is not just a prop,
- it is the field participating in `pp-model`.

That lines up with author vocabulary, RFC-009 terminology, and Vue's
`modelValue` / `defineModel` mental model.

## 7. Implementation sketch

### 7.1 `crates/pocopine-macros/src/lib.rs`

Extend field parsing to record a third role in addition to today's
`is_prop` boolean:

```rust
enum FieldRole {
    State,
    Prop,
    Model { public_name: String },
}
```

Generate:

- `is_prop(key)` returning true for `Prop` and `Model`,
- `is_model(key)` for devtools/runtime helpers,
- `model_name(key) -> Option<&'static str>`,
- hidden metadata tying each model field to its event name and
  serializer,
- write hooks or scope metadata that let the runtime mark a model
  field dirty when assignment happens.

### 7.2 `crates/pocopine-core/src/scope.rs`

Add default trait methods for model metadata:

```rust
fn is_model(&self, key: &str) -> bool { false }
fn model_name(&self, key: &str) -> Option<&'static str> { None }
```

### 7.3 `crates/pocopine-core/src/reactive.rs` / scope write path

Assignment-driven `#[model]` requires runtime involvement when the
field is written.

The minimum viable design is:

- the generated setter/write path marks model fields dirty with their
  current write origin,
- dirty model fields are queued for a per-turn flush,
- the flush serializes the final value once and emits one
  `pp:update:<field>` event,
- flush skips dirty entries whose last origin is `ParentModelIn` or
  `SetupSeed`.

This is where coalescing and loop suppression live.

### 7.4 `crates/pocopine-core/src/lifecycle.rs`

The generated model helper needs a stable dispatch origin captured
from lifecycle context. That policy belongs here, not in template
conventions.

The minimal design is:

- during generated `on_ready`, capture the component's host/root
  element from lifecycle context,
- store it in hidden component-managed state or a runtime side table,
- have the model-field flush emit through `emit_from(&captured_el, ...)`.

This makes model emission independent of `pp-ref="root"`.

### 7.5 `crates/pocopine-core/src/emit.rs`

Add a helper that emits the canonical serde shape of a model field
by name from the component's `pp-ref="root"`:

```rust
pub fn emit_model_key<S: ComponentState>(state: &S, key: &str) { ... }
```

Or equivalent generated code using the existing `emit_from` path and
the captured lifecycle element from §7.3.

The important part is centralizing the serialization and event-name
lookup. The runtime flush path, not handwritten component code, owns
when this helper is called.

### 7.6 `crates/pocopine-core/src/directives/model.rs`

No semantic rewrite required. It already listens on
`pp:update:<field>` for named model bindings. The main runtime
benefit is that mirror-in writes can explicitly tag their origin as
`ParentModelIn`, so assignment-driven publication does not echo them
back out.

### 7.7 Existing migration targets

The initial Pine migration targets are the components currently
hand-writing model emits:

- calendar roots (`value`, `placeholder`),
- range calendar roots (`start`, `end`, `placeholder`),
- date pickers / range pickers (`value`, `start`, `end`, `open`
  where publicly modeled),
- dialog / popover / collapsible / command roots (`open`),
- input-like primitives (`value`, `checked`, `pressed`, `state`,
  `values`, depending on the primitive).

Those are precisely the places where the split contract has already
caused regressions.

### 7.8 Devtools

Show three buckets:

- props,
- model fields,
- state.

That makes a component's contract immediately legible when debugging
why a field is or is not syncing. In the Scopes panel, model fields
should be tagged inline as **model (two-way)** so they are visually
distinct from one-way props at a glance.

## 8. Migration

### 8.1 Framework

1. Add `#[model]` support in macros/runtime.
2. Keep `emit_model_field` available for compatibility.
3. Document `#[model]` as the preferred path for any field meant to
   participate in `pp-model`.

### 8.2 Pine primitives

Migrate fields like:

- `value`,
- `open`,
- `checked`,
- `pressed`,
- `start`,
- `end`,
- `placeholder` where it intentionally round-trips,

from `#[prop]` + handwritten emit code to `#[model]`.

### 8.3 Compat strategy

Short term:

- `emit_model_field` remains supported,
- `#[prop]` + manual emit continues to work.

Long term:

- docs and examples stop teaching the manual pattern,
- code review should treat new manual `emit_model_field("known-field", …)`
  alongside a matching public field as a smell unless there is a good
  reason.

## 9. Drawbacks

- **Another field role increases surface area.** RFC-031 deliberately
  kept the model simple with `#[prop]` vs state. `#[model]` makes the
  contract more expressive, but also more complex.
- **Generated helper naming becomes part of the macro API.** We
  should choose a hidden/internal-friendly spelling if we want room
  to revise it later.
- **Outward `null` for `Option<T>` may surface mismatches in old
  parent code.** The transition shim should blunt most of this, but
  some examples/tests may need updating.

## 10. Alternatives considered

### 10.1 Status quo

Keep `#[prop]` + manual `emit_model_field`.

Rejected because it leaves the same recurring bug class intact and
does not give reviewers or tooling any machine-readable way to tell
that a field is supposed to round-trip.

### 10.2 Explicit helper-only emission

Generate `set_model_<field>(...)` helpers and require authors to call
them manually.

Rejected as the primary design because it keeps too much of the old
failure mode alive: authors still need to remember a second callsite
to advance the public contract. It is safer than handwritten
`emit_model_field`, but not as clean as assignment-driven semantics.

### 10.3 Watch-based automatic emit after mount

Have `#[handlers]` auto-install hidden field watchers for every
`#[model]` field and emit on all post-mount changes.

Rejected for v1 because the runtime needs to understand write origin
at the assignment boundary anyway. A watch-only approach adds another
layer while still needing cycle suppression and coalescing.

### 10.4 `#[prop(model)]` instead of `#[model]`

Viable, but less readable in structs and less legible in docs. The
main concept authors need to see is "this field participates in
`pp-model`".

## 11. Resolutions and remaining future work

This RFC takes the following positions:

1. **`#[model]` assignment is assignment-driven publication.**
   Plain writes to model fields advance the public contract subject
   to origin-aware suppression and per-turn coalescing.
2. **Outbound model payloads canonicalize immediately to serde
   shape.**
   For `Option<T>`, that means `null` for `None`. The inbound
   empty-string compatibility shim remains, but outbound shape is
   always canonical.
3. **Model-field metadata is runtime/devtools-facing, not a new
   userland reflection API.**
   Userland already declares intent with `#[model]` on the struct.
4. **Devtools should show `#[model]` as a distinct bucket.**
   It is not just "a prop that happens to emit"; it is a different
   public contract role.

Future work, intentionally deferred:

1. **Atomic model groups.**
   Some components conceptually advance multiple model fields
   together (`value` + `placeholder`, `start` + `end`). A future
   extension like `#[model(group = "date")]` could offer grouped
   emission semantics. Out of scope for v1.
2. **Richer origin / transaction semantics.**
   The v1 origin set is intentionally minimal. A future RFC may add
   explicit transaction scopes or atomic grouped model updates.

## 12. Why this helps others understand the system

Without this RFC, a reader has to mentally join three documents:

- RFC-009 for `pp-model`,
- RFC-031 for `#[prop]`,
- and implementation conventions around `emit_model_field`.

That is too much hidden coupling for a feature used by nearly every
interactive component.

This RFC gives a single sentence future contributors can hold onto:

> **`#[model]` is the declared two-way public field role for
> components.**

That is the level of clarity the current design is missing.
