# Issue Draft — Add `pocopine-form` form state and validation

## Title

Add `pocopine-form`: React-style typed form state and validation

## Body

### Problem

Complex Pocopine forms currently require every component to hand-roll
the same mechanics:

- default values and reset/reinitialize behavior,
- field registration,
- local draft values,
- dirty/touched/visited/submitting flags,
- `formState`-style derived status,
- duplicate-submit guards,
- client validation,
- server validation mapping,
- field error maps for `pine-field-root`,
- programmatic `setValue`, `setError`, `clearError`, and `watch`
  behavior.

The existing Pine primitives are useful but intentionally low-level:

- `pine-form-root` intercepts submit and propagates an error map,
- `pine-field-root` wires label/control/error accessibility,
- `pp-model` moves values into component state.

There is no reusable middle layer that owns a typed form instance and
normalizes validation behavior with the ergonomics people expect from
React Hook Form or TanStack Form. The storage-browser connection dialog
is already showing the failure mode: provider-specific fields,
modal-local errors, save state, validation hints, and reset behavior
are all custom code.

### Proposal

Implement the draft design in [RFC 090 — Form state and validation](../../rfcs/rfc-090-form-state-and-validation.md).

Add a new `crates/pocopine-form` crate with:

- `use_form(default_values)` / `FormBuilder<V>` as the Pocopine
  equivalent of `useForm({ defaultValues })`,
- `Form<V>` for typed values, initial values, `FormMeta`,
  submit count, field metadata, registered fields, and errors,
- `register`, `unregister`, `set_value`, `set_error`, `clear_error`,
  `reset`, and `watch` methods,
- `handle_submit` returning a `SubmitDecision<V>` so components can
  follow the React `handleSubmit(onValid)` mental model without
  introducing hooks or a new async runtime,
- `FormResolver<V>` and `ValidationMode` for resolver-style validation
  on submit, blur, input, or manual triggers,
- `FieldErrors` / `FormError` / `FormFailure` shared shapes,
- sync validation traits and lightweight validators,
- server-error application helpers,
- duplicate-submit guards,
- reset/reinitialize helpers,
- first-message flattening for Pine field integration.

Keep `pine-form-root` and `pine-field-root` as the DOM/accessibility
layer. The new crate should feed them error state; it should not
replace native forms or invent a schema DSL in the first slice.

Add a controller bridge for compound controls that cannot be wired like
a plain input:

- `pine-form-controller` observes field metadata/errors by `name`,
  stamps common state on its host, and lets the component keep binding
  values with `pp-model`.
- This gives Pocopine the same escape hatch React Hook Form's
  `Controller` gives React apps.

### Suggested Phases

1. Add `pocopine-form` core types and tests.
2. Add React-form-equivalent helpers: `use_form`, `register`,
   `handle_submit`, `set_value`, `set_error`, `reset`, `watch`, and
   `meta`.
3. Bridge `FieldErrors` to existing Pine field error maps.
4. Add a `pine-form-controller` path for compound Pine controls.
5. Migrate the storage-browser connection dialog to prove the API.
6. Add docs and a simple signup/edit-dialog recipe.
7. Consider a derive/macro layer only after the explicit API settles.

### Acceptance Criteria

- `pocopine-form` compiles on host and `wasm32-unknown-unknown`.
- A component can store `Form<MyValues>` locally.
- The public API has Pocopine-native equivalents for React forms:
  `useForm`, `register`, `handleSubmit`, `setValue`, `setError`,
  `reset`, `watch`, and `formState`.
- Client validators can produce field errors without a DOM.
- Server validation failures can map into the same error shape.
- Pine fields can display errors by field name.
- Pine has a controller path for tabs, selects, uploads, and other
  compound controls.
- Duplicate submits are guarded consistently.
- The storage-browser connection dialog can use the form object without
  moving transient draft state back into `StorageBrowserStore`.

### Non-goals

- No schema macro in the first implementation.
- No ORM or database-model form generation.
- No replacement for browser-native forms or autofill.
- No automatic global-store form state.
- No nested field-path system unless the flat field-name API proves
  insufficient during implementation.
- No direct clone of React hooks or non-serializable closures inside
  component state; the API should preserve the mental model while
  staying Rust/Pocopine-native.

### Open Questions

- Should `pine` depend on `pocopine-form`, or should the bridge be
  feature-gated?
- Should native constraint-validation failures become
  `FormError { source: Native }`?
- Should `FormFailure` live in `pocopine-form` or be re-exported from
  `pocopine` for server-function ergonomics?
- Should the exported constructor be named `use_form` for familiarity,
  or should `Form::builder` be primary with `use_form` as a small
  alias?

## Labels

- `enhancement`
- `forms`
- `pine`
- `rfc`

## Links

- RFC draft: `rfcs/rfc-090-form-state-and-validation.md`
- Existing primitives: `crates/pine/src/form`, `crates/pine/src/field`
- Proving-ground form: `examples/file-browser/src/components/connection_dialog`
