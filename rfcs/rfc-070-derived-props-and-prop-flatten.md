# RFC 070 - Derived props and prop flattening

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-03 |
| **Related** | [RFC 031](./rfc-031-prop-vs-state.md), [RFC 044](./rfc-044-model-fields.md), [RFC 056](./rfc-056-component-interaction-safety-batch.md) |
| **Supersedes** | - |

## 1. Summary

Add a `#[derive(Props)]` macro for reusable prop groups and allow components
to expose those groups through `#[prop(flatten)]`.

Authors can keep related configuration grouped in Rust:

```rust
#[derive(Default, Clone, pocopine::Props)]
pub struct AxisProps {
    #[prop]
    pub x_label: String,

    #[prop]
    pub y_label: String,
}

#[component(template = "PineLineChart.poco")]
pub struct PineLineChart {
    #[prop(flatten)]
    pub axis: AxisProps,
}
```

while the public template/API surface remains flat:

```html
<pine-line-chart x_label="Week" y_label="Revenue"></pine-line-chart>
```

This is the prop-side counterpart to RFC 044's model flattening, but with a
stricter contract: fields are exposed only when the props struct explicitly
marks them with `#[prop]`. The framework must not auto-expose every struct
field.

## 2. Motivation

Unstyled UI primitives tend to repeat the same prop groups across several
components:

- chart dimensions;
- margins;
- axis labels;
- grid options;
- legend options;
- hover or tooltip configuration;
- marker styling and behavior.

Without grouping, every component repeats a flat list of fields and default
initializers. That keeps the public API simple but makes component internals
noisy. With a naive nested prop, component authors get nicer Rust internals
but users must write a nested object-like prop, which is not the way Pocopine
HTML templates normally read.

The desired shape is:

- grouped Rust internals;
- flat HTML attributes and bindings;
- explicit public surface;
- compile-time duplicate detection;
- no hidden runtime reflection.

This is especially useful for Pine Charts, but it is a framework-level feature
because reusable prop groups are common across UI primitives.

## 3. Design

### 3.1 `#[derive(Props)]`

`#[derive(Props)]` can be applied to a named-field struct:

```rust
#[derive(Default, Clone, pocopine::Props)]
pub struct ChartSize {
    #[prop]
    pub width: f64,

    #[prop]
    pub height: f64,

    #[state]
    pub measured_width: Option<f64>,
}
```

Only fields annotated with `#[prop]` become part of the props contract.
Fields annotated `#[state]` remain internal to the grouped Rust value and are
not flattened into the public component API. Unannotated fields are a compile
error. This keeps hidden view-model/source-model fields possible without
making accidental invisibility the default.

The derive emits an implementation of a framework-private trait, exposed in
public API as `pocopine::Props`:

```rust
pub trait Props {
    const KEYS: &'static [&'static str];

    fn get_prop(&self, key: &str) -> Option<wasm_bindgen::JsValue>;
    fn set_prop(&mut self, key: &str, value: wasm_bindgen::JsValue) -> bool;
}
```

The exact trait shape may use existing internal conversion helpers instead of
this literal signature, but the contract is:

- enumerate public prop keys statically;
- read a leaf value by key;
- write a leaf value by key;
- return failure for unknown keys or conversion failures through the existing
  component prop error path.

### 3.2 `#[prop(flatten)]` on components

A component field may flatten any type that implements `pocopine::Props`:

```rust
#[component(template = "PineBarChart.poco")]
pub struct PineBarChart {
    #[prop(flatten)]
    pub size: ChartSize,

    #[prop(flatten)]
    pub axis: AxisProps,
}
```

The container field itself is not a public prop key. Its derived leaf keys are
spliced into the component public prop surface:

```html
<pine-bar-chart width="720" height="320" x_label="Bucket"></pine-bar-chart>
```

Inside component code, authors keep the grouped form:

```rust
self.size.width
self.axis.x_label
```

### 3.3 Explicit includes, not excludes

V1 uses explicit inclusion. There is no `exclude` form.

The default include list is the set of fields marked `#[prop]` inside the
derived props struct. That means adding a plain field to a props struct does
not change the component's public API.

When a component wants only a subset of a reusable props struct, it may use an
explicit include list:

```rust
#[prop(flatten = ["x_label"])]
pub axis: AxisProps,
```

Every listed key must exist in the `Props::KEYS` list for the flattened type.
Unknown keys are compile errors.

`exclude` is intentionally omitted. It is a footgun for an opinionated
framework: if a shared props struct grows a new public field, every component
using `exclude = [...]` would expose that new field unless the author notices
and updates the exclusion list. With explicit includes, growth is opt-in.

### 3.4 Duplicate keys are compile errors

The component macro must reject duplicate public prop keys after flattening:

```rust
#[derive(Default, pocopine::Props)]
pub struct AxisProps {
    #[prop]
    pub label: String,
}

#[component(template = "Chart.poco")]
pub struct Chart {
    #[prop]
    pub label: String,

    #[prop(flatten)]
    pub axis: AxisProps,
}
```

This fails because `label` is exposed twice.

Duplicate detection runs after applying any explicit include list.

### 3.5 Scope of field attributes

V1 supports only plain `#[prop]` on `#[derive(Props)]` fields.

Unsupported in v1:

- `#[model]` inside a props struct;
- `#[state]` inside a props struct;
- per-leaf prop rename;
- default values from field attributes;
- validators;
- nested `#[prop(flatten)]` inside a props struct;
- tuple structs and enum props groups.

These are future extensions. The first version should stay small enough to be
reviewed against the existing component macro.

### 3.6 Serialization and conversion

Flattened prop leaves use the same conversion path as normal component
`#[prop]` fields.

This keeps behavior aligned for:

- static attributes;
- `pp-bind:*`;
- bool-like attributes;
- numeric parsing;
- string fields;
- serde-backed structs where already supported by ordinary props.

The `Props` derive must not invent a separate conversion system.

## 4. Runtime Semantics

Flattened props are props only.

- They are parent-writable.
- They are readable through the component's normal generated prop getter.
- They are not model fields.
- They do not emit `pp:update:*` events.
- They do not participate in `pp-model`.

This differs from RFC 044 `#[model(flatten = [...])]`, where each flattened
leaf is both a prop and a model key. `#[prop(flatten)]` is intentionally
one-way.

## 5. Macro Integration

The component macro currently builds generated match arms for:

- `get`;
- `set`;
- `keys`;
- `is_prop`;
- `is_model`;
- `model_name`.

`#[prop(flatten)]` should splice derived leaf arms into the prop-only parts of
that generated surface.

For a component field:

```rust
#[prop(flatten)]
pub axis: AxisProps,
```

the macro behaves as if the component declared synthetic prop leaves:

```rust
// public key: "x_label"
self.axis.x_label

// public key: "y_label"
self.axis.y_label
```

The real container field remains part of component state so user code can
mutate it normally.

## 6. Error Messages

Required compile errors:

1. `#[derive(Props)]` on non-struct input:

   ```text
   #[derive(Props)] can only be applied to named-field structs
   ```

2. `#[prop(flatten)]` on a type without a `Props` implementation:

   ```text
   #[prop(flatten)] requires a type that implements pocopine::Props
   ```

3. Duplicate public key after flattening:

   ```text
   duplicate prop key `label` after flattening; rename or remove one prop
   ```

4. Unknown explicit include:

   ```text
   flattened prop key `foo` is not declared by AxisProps
   ```

5. Nested flatten in v1:

   ```text
   nested #[prop(flatten)] is not supported yet
   ```

## 7. Pine Charts Example

Pine Charts should be able to factor repeated configuration like this:

```rust
#[derive(Default, Clone, pocopine::Props)]
pub struct ChartSizeProps {
    #[prop]
    pub width: f64,

    #[prop]
    pub height: f64,
}

#[derive(Default, Clone, pocopine::Props)]
pub struct AxisLabelProps {
    #[prop]
    pub x_label: String,

    #[prop]
    pub y_label: String,
}

#[component(template = "PineLineChart.poco")]
pub struct PineLineChart {
    #[prop(flatten)]
    pub size: ChartSizeProps,

    #[prop(flatten)]
    pub axis: AxisLabelProps,
}
```

User-facing HTML stays unchanged:

```html
<pine-line-chart
  width="720"
  height="360"
  x_label="Week"
  y_label="Revenue">
</pine-line-chart>
```

This gives charts grouped internals without making users learn nested prop
objects for common HTML-like configuration.

## 8. Implementation Plan

1. Add a `pocopine::Props` trait in the public crate, with conversion helpers
   hidden behind `__private` as needed.
2. Re-export `#[derive(Props)]` from `pocopine`.
3. Implement the derive for named-field structs.
4. Extend `#[component]` field parsing to accept `#[prop(flatten)]` and
   `#[prop(flatten = ["..."])]`.
5. Splice flattened prop leaves into generated get/set/key/is_prop arms.
6. Reject duplicate flattened keys and unsupported nested flattening.
7. Add trybuild coverage for every required compile error.
8. Migrate Pine Charts repeated props as the first real consumer.

## 9. Test Plan

- Unit/macro tests:
  - deriving `Props` on a named struct exposes only `#[prop]` fields;
  - `#[state]` fields are not visible as flattened prop leaves;
  - unannotated fields fail at compile time;
  - `#[prop(flatten)]` imports derived keys into a component;
  - `#[prop(flatten = ["..."])]` narrows the imported key set;
  - duplicate keys fail at compile time;
  - unknown include keys fail at compile time;
  - non-`Props` flattened types fail at compile time;
  - nested flatten fails at compile time.
- Browser/runtime tests:
  - static attributes write flattened leaves;
  - `pp-bind:*` updates flattened leaves;
  - ordinary sibling props still work;
  - flattened leaves do not emit `pp:update:*`;
  - `pp-model:*` rejects flattened prop-only leaves.
- Pine Charts integration:
  - line, area, and bar charts accept flattened size and axis-label props;
  - existing chart examples keep the same public HTML attributes.

## 10. Alternatives

### 10.1 Exclude lists

Rejected for v1.

`exclude` makes API growth implicit. If a shared props struct adds a new
public field, every consumer using `exclude` exposes it unless the consumer is
updated. That is not acceptable for a framework that prefers explicit safe
contracts.

### 10.2 Auto-expose every public field

Rejected.

This copies the most dangerous part of serde-style flattening into component
APIs. Component props are a public user-facing contract, not a serialization
detail. Authors must mark fields intentionally as `#[prop]` or `#[state]`.

### 10.3 Only use `#[prop(flatten = ["..."])]` without `Props`

Rejected as the primary design.

It avoids a derive macro but forces every component to repeat the leaf list.
The whole point of reusable prop groups is that the group owns its public
surface once. Use-site include lists remain useful for narrowing, not for the
main path.

## 11. Open Questions

- Should a future version support field-level rename, for example
  `#[prop(name = "x-label")]`, or should Pocopine keep Rust-ident wire names
  for props?
- Should `Props` derive require `Default`, or should only flattened component
  fields require whatever initialization the component already uses?
- Should nested flatten become legal after v1, or should repeated groups stay
  one level deep permanently for clearer diagnostics?
