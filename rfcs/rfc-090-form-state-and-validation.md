# RFC 090 — Form state and validation

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-29 |
| **Builds on** | [RFC 009](./rfc-009-pp-model-components.md), [RFC 031](./rfc-031-prop-vs-state.md), [RFC 044](./rfc-044-model-fields.md), existing `pine-form-root` / `pine-field-root` primitives |
| **Related** | [RFC 091](./rfc-091-store-state-ownership.md), `crates/pine/src/form`, `crates/pine/src/field`, storage-browser connection dialog, signup showcase |

## 1. Summary

Add a first-party `pocopine-form` crate that gives components a
serializable, typed **form object** for values, field metadata,
validation errors, submit lifecycle, and server-error mapping.
The author experience should intentionally feel closer to React form
libraries such as React Hook Form or TanStack Form than to a plain
`HashMap` of errors:

- create a form from default values,
- register fields by name,
- validate through a resolver,
- read `meta`,
- call `handle_submit`,
- set/reset values and errors programmatically,
- and let form/field primitives consume the same field metadata.

The goal is not to replace native forms or Pine form primitives.
Instead:

- `pocopine-form` owns value state, validation state, error shape,
  and submit helpers.
- `pine-form-root` / `pine-field-root` own DOM, accessibility,
  native constraint-validation hooks, and visual state propagation.
- App components keep their form instance local unless the app
  deliberately promotes it into a store.

This lets Pocopine apps avoid the common pattern we are seeing in the
storage-browser work: many ad hoc component fields, custom error
strings, custom submit guards, custom "server error to field error"
mapping, and duplicated dirty/touched/submitting behavior.

## 2. Motivation

Pocopine already has the low-level pieces:

- `pp-model` moves input values into component state.
- `pine-form-root` intercepts submit and carries an `errors` map.
- `pine-field-root` wires label/control/error accessibility and flips
  `invalid` when its `name` appears in the form errors map.
- Server functions can return domain errors.

What is missing is the middle layer that normal applications need:

1. a stable form object shape that can be serialized in component state,
2. field errors with codes, messages, and source,
3. submit status and duplicate-submit protection,
4. dirty/touched/visited/pending metadata,
5. client validation before server submission,
6. server validation mapping back into the same field-error shape,
7. reset/reinitialize helpers for edit dialogs,
8. ergonomic tests that do not require a browser.

Without this layer, complex forms either become global-store blobs or
large component structs with every field and error hand-written.
That is manageable for one dialog, but it scales poorly for auth,
storage connections, settings, CRUD editors, and sync conflict
resolution forms.

## 3. Design Goals

- **Local by default.** A form object belongs in the component that owns
  the interaction. Stores should hold durable app state, not every
  transient draft field.
- **Typed values, generic errors.** Value structs are app-owned Rust
  types. Error transport is framework-owned and reusable.
- **Works with `.poco`.** Templates should bind to normal fields and
  maps; no magic form parser is required in the template language.
- **Browser and server compatible.** The core crate must compile on
  wasm and host targets.
- **No schema lock-in.** The first version should not force a Zod-like
  DSL, validator macro, ORM, or server framework.
- **React-form ergonomics.** The public API should resemble
  `useForm`, `register`, `handleSubmit`, `setValue`, `setError`,
  `reset`, `watch`, and `formState`, adapted to Rust and Pocopine's
  component model.
- **Accessible by construction.** Pine form primitives continue to own
  `aria-*`, labels, descriptions, and error visibility.
- **Testable without DOM.** Most validation and submit-state behavior
  should be unit-testable in Rust.

## 4. Non-goals

- Replacing native `<form>`, `FormData`, browser autofill, or native
  constraint validation.
- Building a full schema language in the first slice.
- Generating forms from database models.
- Owning business validation that belongs to the application or server.
- Solving every nested-field path shape immediately. Flat field names
  are enough for the first implementation; nested paths can layer on
  the same key type later.
- Moving form drafts into global stores automatically.

## 5. React-style Target Model

The target mental model is:

```tsx
const form = useForm({
  defaultValues,
  resolver,
});

<form onSubmit={form.handleSubmit(onValid)}>
  <input {...form.register("bucket")} />
  {form.formState.errors.bucket?.message}
</form>
```

The Pocopine equivalent should be explicit Rust, but preserve that
shape:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionValues {
    pub provider: String,
    pub name: String,
    pub bucket: String,
    pub endpoint_url: String,
}

pub struct ConnectionDialog {
    pub form: Form<ConnectionValues>,
}

impl Default for ConnectionDialog {
    fn default() -> Self {
        Self {
            form: use_form(ConnectionValues::default())
                .mode(ValidationMode::OnSubmit)
                .build(),
        }
    }
}

#[handlers]
impl ConnectionDialog {
    pub fn save(&mut self) {
        let decision = self.form.handle_submit(&ConnectionResolver);
        let Some(values) = decision.valid_values() else {
            return;
        };

        dispatch!(crate::save_connection(values).await, |s, result| {
            match result {
                Ok(saved) => {
                    s.form.finish_submit();
                    s.close(saved);
                }
                Err(err) => s.form.apply_failure(err.into()),
            }
        });
    }
}
```

The template should stay close to today's Pine primitives:

```html
<pine-form-root pp-bind:errors="form_first_errors"
                pp-bind:submitting="form_is_submitting"
                @submit="save">
  <pine-field-root name="bucket">
    <pine-field-label>Bucket</pine-field-label>
    <pine-field-control>
      <pine-input pp-model:value="form.values.bucket"
                  @blur="touch_field('bucket')"
                  required></pine-input>
    </pine-field-control>
    <pine-field-error pp-text="form_first_errors.bucket || 'Bucket is required.'"></pine-field-error>
  </pine-field-root>
</pine-form-root>
```

That is the minimum viable bridge. A later Pine bridge can reduce the
remaining event wiring:

```html
<pine-form-root pp-bind:form-meta="form.meta" @submit="save">
  <pine-form-field name="bucket">
    <pine-input pp-form-control pp-model:value="form.values.bucket"></pine-input>
  </pine-form-field>
</pine-form-root>
```

The important design choice is that the framework owns the same
concepts React form libraries expose, but Pocopine does not need React
hooks. The form object is ordinary component state.

The intended vocabulary mapping is:

| React form concept | Pocopine shape | Notes |
|---|---|---|
| `useForm({ defaultValues })` | `use_form(default_values).build()` | Constructor/builder, not a hook. |
| `register("field")` | `form.register("field")` | Tracks metadata and error participation by field name. |
| `handleSubmit(onValid)` | `form.handle_submit(&resolver)` | Returns a decision; the component dispatches async work. |
| `setValue` / `setError` | `set_value` / `set_error` | Explicit Rust methods on `Form`. |
| `reset` | `reset(values)` | Reinitializes values, errors, and field metadata. |
| `watch` | `watch(path)` | Reads a typed field descriptor. |
| `formState` | `form.meta` | Serializable status, validity, dirty, and submit flags. |
| `Controller` | `pine-form-controller` | Bridge for compound Pine controls. |

## 6. Proposed Crate

Add `crates/pocopine-form`.

The core crate should have no Pine dependency in its base module. Pine
integration can be either a feature module or direct integration from
`crates/pine`.

```rust
pub fn use_form<V>(default_values: V) -> FormBuilder<V>;

pub struct FormBuilder<V> {
    default_values: V,
    mode: ValidationMode,
}

impl<V> FormBuilder<V> {
    pub fn mode(self, mode: ValidationMode) -> Self;
    pub fn build(self) -> Form<V>;
}

pub struct Form<V> {
    pub values: V,
    pub initial: V,
    pub errors: FieldErrors,
    pub fields: FieldMetaMap,
    pub meta: FormMeta,
    pub registered: std::collections::BTreeSet<FieldName>,
}

pub struct FormMeta {
    pub status: FormStatus,
    pub is_dirty: bool,
    pub is_valid: bool,
    pub is_validating: bool,
    pub is_submitting: bool,
    pub is_submitted: bool,
    pub submit_count: u32,
    pub form_error: String,
}

pub type FieldName = String;
pub type FieldErrors = std::collections::BTreeMap<FieldName, Vec<FormError>>;
pub type FieldMetaMap = std::collections::BTreeMap<FieldName, FieldMeta>;

pub struct FieldMeta {
    pub dirty: bool,
    pub touched: bool,
    pub focused: bool,
    pub pending: bool,
    pub visited: bool,
}

pub struct FormError {
    pub code: String,
    pub message: String,
    pub source: ErrorSource,
}

pub enum ErrorSource {
    Client,
    Native,
    Server,
}

pub enum FormStatus {
    Idle,
    Validating,
    Submitting,
    Submitted,
    Failed,
}
```

All public structs must derive `Clone`, `Debug`, `Serialize`,
`Deserialize`, and `Default` where practical so they can live directly
inside component state.

The first slice should not store a resolver inside `Form`.
Resolvers can be closures, structs with dependencies, or async-adjacent
application objects, and those are not reliably serializable. Passing
the resolver to `handle_submit` keeps the form object ordinary state
while preserving the React `handleSubmit` mental model.

### 6.1 Registration and Field Paths

React form libraries return input props from `register("field")`.
Pocopine cannot literally return DOM props into a template, but it can
own the same concept:

```rust
impl<V> Form<V> {
    pub fn register(&mut self, name: impl Into<FieldName>);
    pub fn unregister(&mut self, name: &str);
    pub fn touch(&mut self, name: &str);
    pub fn focus(&mut self, name: &str);
    pub fn blur(&mut self, name: &str);
    pub fn set_error(&mut self, name: &str, error: FormError);
    pub fn clear_error(&mut self, name: &str);
}
```

For typed value reads/writes, use field descriptors rather than storing
non-serializable closures inside `Form`:

```rust
pub struct FieldPath<V, T> {
    pub name: &'static str,
    pub get: fn(&V) -> &T,
    pub set: fn(&mut V, T),
}

impl<V> Form<V> {
    pub fn value<T: Clone>(&self, path: FieldPath<V, T>) -> T;
    pub fn set_value<T>(&mut self, path: FieldPath<V, T>, value: T);
    pub fn watch<T: Clone>(&self, path: FieldPath<V, T>) -> T;
}
```

A later `#[derive(FormValues)]` can generate these descriptors, but the
core API should work without the derive.

## 7. Validation Model

The first version should use explicit Rust functions and traits.
Macros can come later.

```rust
pub trait FormResolver<V> {
    fn resolve(&self, values: &V, registered: &std::collections::BTreeSet<FieldName>) -> FormResolution;
}

pub struct FormResolution {
    pub errors: FieldErrors,
}

impl FormResolution {
    pub fn valid() -> Self;
    pub fn invalid(errors: FieldErrors) -> Self;
    pub fn is_valid(&self) -> bool;
}

pub trait ValidateField<V> {
    fn validate_field(&self, field: &str, values: &V, errors: &mut FieldErrors);
}
```

For app code, lightweight helpers should be enough:

```rust
pub fn required(errors: &mut FieldErrors, field: &str, value: &str, message: &str);
pub fn min_len(errors: &mut FieldErrors, field: &str, value: &str, min: usize, message: &str);
pub fn matches(errors: &mut FieldErrors, field: &str, left: &str, right: &str, message: &str);
```

The important contract is that all validators write into the same
`FieldErrors` map. `pine-form-root` already understands an errors map;
we can either pass a flattened first-message map into Pine or teach
Pine to accept the richer error type.

Validation mode should be configurable:

```rust
pub enum ValidationMode {
    OnSubmit,
    OnBlur,
    OnInput,
    Manual,
}
```

This mirrors React form libraries while keeping the implementation
simple: the component or Pine bridge calls `touch`, `set_value`, or
`validate_field` at the configured points.

## 8. Submit Lifecycle

`Form` should provide conservative submit helpers:

```rust
impl<V> Form<V> {
    pub fn handle_submit<R: FormResolver<V>>(&mut self, resolver: &R) -> SubmitDecision<V>
    where
        V: Clone;
    pub fn finish_submit(&mut self);
    pub fn apply_failure(&mut self, failure: FormFailure);
    pub fn reset(&mut self, values: V);
    pub fn set_values(&mut self, values: V);
    pub fn clear_errors(&mut self);
    pub fn first_error_map(&self) -> std::collections::HashMap<String, String>;
}
```

The decision type is deliberately small:

```rust
pub enum SubmitDecision<V> {
    Valid(V),
    Invalid(FieldErrors),
    Busy,
}

impl<V> SubmitDecision<V> {
    pub fn valid_values(self) -> Option<V>;
    pub fn is_valid(&self) -> bool;
}
```

`handle_submit` is the React-style entrypoint. It should:

1. guard duplicate submits,
2. increment submit count,
3. run the resolver,
4. set `is_submitting` only when values are valid,
5. return either valid cloned values or invalid errors.

```rust
pub fn save(&mut self) {
    let decision = self.form.handle_submit(&ConnectionResolver);
    let Some(input) = decision.valid_values() else {
        return;
    };
    dispatch!(crate::save_connection(input).await, |s, result| {
        match result {
            Ok(saved) => {
                s.form.finish_submit();
                s.close(saved);
            }
            Err(err) => s.form.apply_failure(err.into()),
        }
    });
}
```

The helper should leave async ownership to existing Pocopine patterns
instead of inventing a new async runtime abstraction.

## 9. Server Error Shape

Server functions need a conventional form-validation failure shape:

```rust
pub struct FormFailure {
    pub message: String,
    pub fields: FieldErrors,
}

pub type FormResult<T> = Result<T, FormFailure>;
```

Applications can still return domain-specific errors, but the form
crate should provide conversions from common server validation errors
into `FormFailure`.

The field-error source should be set to `ErrorSource::Server` when a
server response is applied.

## 10. Pine Integration

The existing Pine primitives remain the visual/accessibility layer.
This RFC proposes incremental upgrades:

### 10.1 `pine-form-root`

Add optional support for:

- `errors: HashMap<String, String>` remains supported.
- `field_errors: FieldErrors` may be added when `pine` depends on or
  feature-gates `pocopine-form`.
- `submitting: bool` can stamp `data-submitting` and block duplicate
  native submit emissions if configured.
- `meta: FormMeta` can stamp `data-dirty`, `data-valid`,
  `data-submitting`, and `data-submitted`.
- `pp:form:submit` can carry a small detail payload with `submit_count`
  and `valid` when supplied by a form object.

The existing `pp:submit` event should remain for compatibility.

### 10.2 `pine-field-root`

Continue observing the enclosing form's errors by `name`. Later, if
`FieldErrors` is accepted directly, the field can expose:

- first message,
- all messages,
- first code,
- source.

`pine-field-error` should be able to show slotted fallback text or the
current first error message.

### 10.3 Controller Components

React Hook Form uses `Controller` for controlled widgets. Pocopine
needs the same escape hatch for Pine inputs and compound controls that
do not expose a plain native input event.

The first bridge can be explicit:

```html
<pine-form-controller name="auth_mode"
                      pp-bind:form-meta="form.meta"
                      @field-blur="touch_field('auth_mode')">
  <pine-tabs-root pp-model:value="form.values.auth_mode">
    ...
  </pine-tabs-root>
</pine-form-controller>
```

The controller observes field meta/errors by `name` and stamps common
state onto its host. It does not own the value; the component still
binds value through `pp-model`.

## 11. Template Usage

First slice usage should remain explicit and readable:

```html
<pine-form-root pp-bind:errors="form_error_map"
                @submit="save">
  <pine-field-root name="bucket">
    <pine-field-label>Bucket</pine-field-label>
    <pine-field-control>
      <pine-input pp-model:value="form.values.bucket" required></pine-input>
    </pine-field-control>
    <pine-field-error pp-text="form_error_map.bucket || 'Bucket is required.'"></pine-field-error>
  </pine-field-root>

  <button type="submit" :disabled="form_submitting">Save</button>
</pine-form-root>
```

Because `.poco` expressions intentionally stay simple, components may
mirror derived view fields such as `form_error_map` and
`form_submitting` from the form object until expression support grows.

## 12. Example Target: Storage Connection Dialog

The storage-browser connection dialog should be a proving ground:

- `ConnectionFormValues` holds provider, name, bucket, endpoint,
  credentials, GCS auth mode, emulator flags, and root prefix.
- `ConnectionFormValidator` owns client-side checks:
  - bucket is required,
  - S3 region and access key are required,
  - new S3 connection requires secret key,
  - GCS service JSON is required for new service-json connections,
  - JSON paste extracts project id,
  - unsupported auth mode is rejected.
- Server-side save maps provider-specific errors back to the same
  `FieldErrors`.
- The dialog keeps local form state; the store only receives the
  saved connection summary.

This should remove most one-off fields from the dialog component and
make edit/new/reset behavior explicit.

## 13. Implementation Plan

### Phase 1 — Core crate

- Add `crates/pocopine-form`.
- Define `use_form`, `Form`, `FormMeta`, `FieldMeta`,
  `FormError`, `FieldErrors`, `FormStatus`, `FormFailure`,
  `SubmitDecision`, and resolver/validator helpers.
- Add unit tests for duplicate submit guards, reset, dirty/touched
  mutation, first-message flattening, and server-error application.

### Phase 2 — Pine bridge

- Keep current `pine-form-root` API working.
- Add opt-in helpers for richer errors if dependency direction is
  acceptable.
- Add tests proving `pine-field-root` reacts to field errors and clears
  when errors clear.
- Add a controller wrapper for non-native or compound controls.

### Phase 3 — Example migration

- Migrate the storage-browser connection dialog to `Form`.
- Keep the current visual UI and behavior.
- Use the migration to tune helper names before broad docs.

### Phase 4 — Docs and recipes

- Add a form guide under `docs/`.
- Add one simple signup example and one async edit-dialog example.
- Document when form state belongs in a component versus a store.

### Phase 5 — Optional derive/macro layer

Only after the explicit API stabilizes, consider:

```rust
#[derive(FormValues)]
pub struct ConnectionFormValues {
    #[field(required)]
    pub bucket: String,
}
```

This is deliberately deferred so the core contract is not blocked on a
macro design.

## 14. Open Questions

- Should `pine` depend on `pocopine-form`, or should integration live
  behind a feature to keep Pine primitives lighter?
- Should first-slice field names remain flat strings, or should we
  introduce `FieldPath` now?
- Should native constraint-validation failures be normalized into
  `FormError { source: Native }`, or should they remain a Pine-only
  visual state?
- Should `FormFailure` live in `pocopine-form` or in `pocopine` so
  server functions can use it without another explicit dependency?
- How much derived view state should `Form` expose for `.poco`
  templates given the current expression limitations?
- Is `use_form` the right exported name in Rust, or should it be
  `Form::builder` with `use_form` as a convenience alias?

## 15. Drawbacks

- Another crate adds API surface and maintenance cost.
- A generic form object can become too abstract if we try to solve
  nested schema validation too early.
- Pine integration creates dependency-direction pressure between UI
  primitives and framework helpers.
- A React-like API can feel alien if copied too literally into Rust.
  The implementation must preserve the mental model without forcing
  hooks or non-serializable closures into component state.
- If app authors put every form object into a global store, this will
  recreate the store-bloat problem in a new shape. Docs must steer the
  local-component default clearly.

## 16. Alternatives

### Keep only Pine form primitives

This is the current state. It is good for accessibility wiring, but it
does not solve typed values, submit status, error normalization, reset,
or server validation mapping.

### Adopt an external validation crate directly

Apps can do this today, but Pocopine still needs a common error shape
for Pine fields, server functions, and examples.

### Build a schema macro first

This may become useful, but it would lock the public shape before the
runtime contract has been proven in real forms.

### Treat forms as stores

This works for multi-page wizards, but it is too heavy for dialogs and
ordinary editors. The default should be local form objects, with stores
reserved for durable or cross-route draft state.

## 17. Acceptance Criteria

- A new `pocopine-form` crate can compile on host and wasm targets.
- A component can hold `Form<MyValues>` in local state.
- The public API includes React-form equivalents for `use_form`,
  `register`, `handle_submit`, `set_value`, `set_error`, `reset`,
  `watch`, and `meta`.
- Client validation can produce field errors without DOM.
- Server validation failures can map into the same field-error shape.
- Pine fields can display those errors by `name`.
- Pine has a controller path for compound controls.
- Duplicate submits are guarded consistently.
- The storage-browser connection dialog can be migrated without
  losing provider-specific behavior.
