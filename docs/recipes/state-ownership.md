# State Ownership

Use the narrowest owner that can complete the workflow. Stores are not
the default place for state; they are the place for durable state shared
across unrelated subtrees.

## Decision Table

| Need | Use |
|---|---|
| One component renders and mutates it | component field |
| A dialog/editor has draft values and validation | `Form<V>` local to that component |
| A parent configures a child | `#[prop]` or `#[model]` |
| A route provides identity | route props or a tiny shared route field |
| Shell/sidebar/content need the same durable value | `#[store]` |
| Server/source data has a protocol owner | server DTO, sync collection, or query state |
| It can be calculated from other state | derived function/computed value |

## Good Store Candidates

- current session identity,
- app theme/preferences,
- cart contents,
- selected account/project/connection id,
- route identity that multiple subtrees must observe,
- synced collections,
- long-lived app capability metadata.

## Poor Store Candidates

- form inputs before save,
- dialog-local errors,
- upload dock expansion,
- command palette query/results,
- loading flags for one component request,
- labels such as "12 files" or "Root / Videos",
- selected object copies that duplicate a selected id.

## Shape Shared Stores By Domain

Prefer:

```rust
#[derive(Default, Serialize, Deserialize)]
#[store(name = "storage")]
pub struct StorageStore {
    pub connections: Vec<StorageConnectionSummary>,
    pub selected_connection_id: String,
    pub current_prefix: String,
    pub phase: LoadPhase,
    pub error: String,
}
```

Avoid:

```rust
pub struct StorageStore {
    pub connection_dialog_name: String,
    pub command_query: String,
    pub upload_dock_expanded: bool,
    pub listed_size_label: String,
    pub selected_connection: StorageConnectionSummary,
}
```

The first store represents durable browser state. The second store is a
collection of component internals and derived labels.

## Use Phase Enums

Avoid contradictory boolean clusters:

```rust
pub loading: bool,
pub saving: bool,
pub saved: bool,
pub failed: bool,
```

Prefer one workflow phase:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum LoadPhase {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed,
}
```

Templates can still read helper booleans from a component or store
method when needed.

## Keep Derived Data Derived

Do not store values that can be calculated cheaply:

- count labels,
- filtered count labels,
- path titles,
- breadcrumbs,
- selected connection name,
- capability badges.

Store the canonical values:

- entries,
- selected id,
- current prefix,
- connection summaries.

Compute the labels near the component that renders them.

## Normalize Shared Collections

If many components need the same collection, keep one copy and store
ids for selection.

```rust
pub struct NotesStore {
    pub notes: Vec<NoteRow>,
    pub selected_note_ids: Vec<String>,
}
```

Do not keep duplicate selected rows unless there is a measured
performance reason and a clear invalidation point.

## Mutate Through Store Actions

Put multi-field transitions in Rust methods:

```rust
#[handlers]
impl StorageStore {
    pub fn select_connection(&mut self, id: String) {
        self.selected_connection_id = id;
        self.current_prefix.clear();
        self.phase = LoadPhase::Idle;
        self.error.clear();
    }
}
```

Then call the method from a component handler. Avoid templates that
assign several `$store.*` fields to complete one workflow.

## Reset Explicitly

If a store has semantic defaults, expose the reset operation by name:

```rust
impl PreferencesStore {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
```

If some durable identity must survive reset, make that visible in the
method name and body.

## Migration Checklist

When a store is getting too large:

1. List every field and the component that renders it.
2. Move dialog/editor fields into a local `Form<V>`.
3. Move upload/search/popover fields into their rendering component.
4. Replace derived labels with local helper methods.
5. Keep route/shared identity in the store.
6. Normalize duplicated selected objects into selected ids.
7. Replace boolean clusters with phase enums.
8. Keep server DTOs and provider SDK calls outside components.

## References

- RFC 091: `rfcs/rfc-091-store-state-ownership.md`
- RFC 090: `rfcs/rfc-090-form-state-and-validation.md`
- Existing guide: `docs/components/02-state.md`
- Storage-browser recipe: `docs/recipes/storage-browser-state-refactor.md`
