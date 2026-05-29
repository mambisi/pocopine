# Form State

Use `Form<V>` for form drafts, validation, field metadata, submit
status, and server-error mapping. Keep it local to the component that
renders the form.

## When To Use A Form

Use `Form<V>` when a workflow needs:

- default values,
- editable draft values,
- field registration,
- dirty/touched/focused metadata,
- client validation,
- server validation mapped back to fields,
- submit guards,
- reset/reinitialize behavior.

Do not put those draft fields into a global store unless the draft must
survive route changes or be shared by unrelated subtrees.

## Basic Shape

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionValues {
    pub name: String,
    pub bucket: String,
    pub region: String,
}

#[derive(Serialize, Deserialize)]
#[component(template = "ConnectionDialog.poco")]
pub struct ConnectionDialog {
    pub form: Form<ConnectionValues>,
    pub error: String,
}
```

Initialize it with defaults:

```rust
impl Default for ConnectionDialog {
    fn default() -> Self {
        Self {
            form: use_form(ConnectionValues::default())
                .mode(ValidationMode::OnSubmit)
                .build(),
            error: String::new(),
        }
    }
}
```

## Submit Flow

```rust
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

The form object handles validation and submit phase. The component still
owns the async dispatch because Pocopine handlers stay synchronous.

## Template Bridge

First-slice templates can stay explicit:

```html
<pine-form-root pp-bind:errors="form_errors"
                pp-bind:submitting="form_submitting"
                @submit="save">
  <pine-field-root name="bucket">
    <pine-field-label>Bucket</pine-field-label>
    <pine-field-control>
      <pine-input pp-model:value="form.values.bucket"
                  @blur="touch_bucket"
                  required></pine-input>
    </pine-field-control>
    <pine-field-error pp-text="form_errors.bucket || 'Bucket is required.'"></pine-field-error>
  </pine-field-root>
</pine-form-root>
```

The component can expose simple view fields such as `form_errors` and
`form_submitting` until Pine grows a richer `form-meta` bridge.

## Controller Controls

For compound controls, use a controller wrapper instead of forcing a
fake input event:

```html
<pine-form-controller name="auth_mode"
                      pp-bind:form-meta="form.meta"
                      @field-blur="touch_auth_mode">
  <pine-tabs-root pp-model:value="form.values.auth_mode">
    ...
  </pine-tabs-root>
</pine-form-controller>
```

The controller observes field meta/errors and stamps state. The value
still flows through the component's `pp-model` binding.

## Store Boundary

Good:

```rust
pub struct ConnectionDialog {
    pub form: Form<ConnectionValues>,
}

impl ConnectionDialog {
    pub fn close(&mut self, saved: StorageConnectionSummary) {
        pocopine::store::<StorageStore>().update(move |store| {
            store.upsert_connection(saved);
        });
    }
}
```

Bad:

```rust
pub struct StorageStore {
    pub connection_name_input: String,
    pub connection_bucket_input: String,
    pub connection_secret_input: String,
    pub connection_dialog_error: String,
}
```

The store should receive saved domain state. It should not own every
keystroke in the dialog.

## References

- RFC 090: `rfcs/rfc-090-form-state-and-validation.md`
- RFC 091: `rfcs/rfc-091-store-state-ownership.md`
- Store recipe: `docs/recipes/state-ownership.md`
