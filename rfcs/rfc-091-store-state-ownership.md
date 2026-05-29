# RFC 091 - Store state ownership and durable app state

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-29 |
| **Builds on** | [RFC 002](./rfc-002-app-stores-servers.md), [RFC 031](./rfc-031-prop-vs-state.md), [RFC 044](./rfc-044-model-fields.md), [RFC 090](./rfc-090-form-state-and-validation.md) |
| **Related** | [`docs/components/02-state.md`](../docs/components/02-state.md), [`docs/recipes/state-ownership.md`](../docs/recipes/state-ownership.md), [`docs/recipes/storage-browser-state-refactor.md`](../docs/recipes/storage-browser-state-refactor.md) |

## 1. Summary

`#[store]` is already the framework primitive for shared state. This
RFC defines the design contract for when a store should exist, what it
should contain, and which state belongs closer to components, route
props, forms, or sync/query collections.

The core rule:

> Stores hold durable, cross-subtree app state. They should not become
> component-local draft state, async request scratch space, or cached
> render labels.

This RFC does not replace `#[store]`. It tightens its intended use so
Pocopine examples and app scaffolds do not drift toward one giant
global state object.

## 2. Motivation

The storage-browser example has become a useful stress test. It needs:

- connection profiles,
- selected connection and route prefix,
- object listing,
- search and command palette state,
- upload dock state,
- new-folder and connection dialogs,
- provider credentials,
- loading, saving, and error states.

The first implementation naturally put much of that into
`StorageBrowserStore`. That worked, but it produced a store with many
unrelated reasons to change. A connection dialog edit could invalidate
the same store as a listing request, an upload dock interaction, a
command search, or a breadcrumb label.

Other frameworks converge on the same lesson:

- keep transient interaction state local,
- shape global state by domain/data, not by component tree,
- avoid duplicated and redundant state,
- normalize collections that are shared or relational,
- model status as finite phases instead of contradictory booleans,
- keep derived render facts derived.

Pocopine should encode that as framework guidance and, where practical,
as helpers.

## 3. Design Goals

- **Local first.** Component fields own the state for UI that only the
  component renders or submits.
- **Forms stay local.** `Form<V>` from RFC 090 is the default for
  dialogs and editors. A store should receive the saved result, not
  every draft field.
- **Stores are domain-shaped.** Store names and fields should describe
  durable app concepts such as `session`, `preferences`, `cart`,
  `storage`, or `keep`, not components such as `top_bar` or
  `connection_dialog`.
- **Normalize shared collections.** Shared entity-like data should
  prefer maps/rows plus selected IDs over duplicated selected objects.
- **Avoid impossible states.** Workflow status should use enums such as
  `LoadPhase` or `SubmitPhase` instead of unrelated `loading`, `saved`,
  `failed`, and `error` flags that can contradict each other.
- **Derived data stays derived.** Counts, subtitles, breadcrumbs,
  labels, filtered rows, and capability badges should be recomputed
  from canonical state unless caching is needed for performance.
- **Typed store actions.** Multi-field changes should live in Rust
  methods on the store, not in template assignment chains.
- **Reset is explicit.** Stores with semantic defaults should expose a
  reset/reinitialize path.
- **SSR-safe future.** Store initialization must remain instance-based
  enough to support per-request store factories when SSR lands.

## 4. Non-goals

- Replacing `#[store]` with a Redux/Pinia clone.
- Requiring reducers, action enums, or state-machine configs for every
  store.
- Preventing direct Rust mutation inside `Handle::update`.
- Moving all component state into stores.
- Making stores persistent by default.
- Solving all local-first sync/query state; `pocopine-sync` and
  `pocopine-sync-query` own those layers.

## 5. Ownership Model

Pocopine should document and use this ownership ladder:

| State kind | Owner | Example |
|---|---|---|
| Single component interaction | component fields | popover open state, local filter input |
| Form draft and validation | `Form<V>` in the component | connection dialog credentials |
| Parent-configured child API | `#[prop]` / `#[model]` | selected tab value, dialog open |
| Route identity | route props or a small route store field | `connection_id`, `prefix` |
| Durable cross-subtree state | `#[store]` | selected connection id, session user, theme |
| Server/source data | server DTOs, sync collections, query state | object listing response, notes collection |
| Render facts | computed/derived local values | count labels, breadcrumbs, subtitles |

When multiple owners look plausible, choose the narrowest owner that
can still express the workflow without cross-component reach-through.

## 6. Store Shape

A good store has one reason to change. It should read like a domain
model, not a dump of active widgets.

```rust
#[derive(Default, Serialize, Deserialize)]
#[store(name = "storage")]
pub struct StorageStore {
    pub connections: Vec<StorageConnectionSummary>,
    pub selected_connection_id: String,
    pub current_prefix: String,
    pub phase: StoragePhase,
    pub error: String,
}
```

The store should not own:

- `connection_name_input`,
- `saving_connection`,
- `command_query`,
- `upload_dock_expanded`,
- `new_folder_name`,
- `listed_size_label`,
- `breadcrumbs`.

Those belong to the component that renders the workflow or to derived
view helpers.

## 7. Status as Phases

Loose booleans create contradictory states. Prefer an enum that names
the workflow.

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum LoadPhase {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum SubmitPhase {
    #[default]
    Idle,
    Submitting,
    Submitted,
    Failed,
}
```

Templates can still consume derived booleans:

```rust
impl StorageStore {
    pub fn loading(&self) -> bool {
        self.phase == LoadPhase::Loading
    }
}
```

The important invariant is that the canonical state cannot represent
both "submitting" and "submitted" at the same time.

## 8. Collections and Selection

Shared collections should avoid duplicating rows.

Preferred:

```rust
pub struct NotesStore {
    pub notes: Vec<NoteRow>,
    pub selected_note_ids: Vec<String>,
}
```

Avoid:

```rust
pub struct NotesStore {
    pub notes: Vec<NoteRow>,
    pub selected_notes: Vec<NoteRow>,
    pub active_note: NoteRow,
}
```

If lookup by id becomes important, move to an indexed shape:

```rust
pub struct EntityMap<T> {
    pub order: Vec<String>,
    pub by_id: std::collections::BTreeMap<String, T>,
}
```

The first implementation does not need a framework-owned entity store,
but the recipes should teach the normalized pattern.

## 9. Derived State and Selectors

Derived values should be functions or computed fields, not manually
maintained state.

Examples:

- `listed_size_label` derives from entries,
- `visible_entry_count_label` derives from filtered entries,
- `path_title` derives from current prefix,
- `selected_connection_name` derives from selected id plus
  connections,
- `can_upload` derives from selected connection capabilities.

When a derived value is expensive enough to cache, the owner must name
the invalidation key and update it in one action. Caching is a
performance decision, not the default state shape.

## 10. Store Actions

Stores should expose typed Rust methods for workflow transitions:

```rust
#[handlers]
impl StorageStore {
    pub fn select_connection(&mut self, id: String) {
        self.selected_connection_id = id;
        self.current_prefix.clear();
        self.phase = LoadPhase::Idle;
        self.error.clear();
    }

    pub fn apply_connections(&mut self, connections: Vec<StorageConnectionSummary>) {
        self.connections = connections;
        self.reconcile_selection();
    }
}
```

Template code should call a handler. It should not perform multi-field
assignment sequences against `$store.*`.

## 11. Reset and Reinitialize

Stores with semantic defaults should have explicit reset methods:

```rust
impl PreferencesStore {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
```

When the reset should preserve durable identity, make that visible:

```rust
impl StorageStore {
    pub fn reset_browser_state(&mut self) {
        let connections = std::mem::take(&mut self.connections);
        *self = Self {
            connections,
            ..Self::default()
        };
    }
}
```

A later helper trait may standardize this:

```rust
pub trait ResetStore {
    fn reset(&mut self);
}
```

This RFC does not require a new trait in the first slice; explicit
store methods are enough.

## 12. Store/Component Boundary

Use a store when:

- two unrelated subtrees need the same value,
- the value must outlive a mounted component,
- route/shell/sidebar/content must agree on identity,
- the state is a durable app preference or session,
- a sync collection or source boundary needs one canonical owner.

Do not use a store when:

- only one component renders the field,
- the value is a form draft,
- the value is an upload/dialog/search transient,
- the value can be derived from existing fields,
- the value exists only to drive CSS in one component.

## 13. Storage Browser Target

The storage-browser example should eventually converge on this split:

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

Local owners:

| Concern | Owner |
|---|---|
| Connection form | `FileBrowserConnectionDialog` with `Form<ConnectionValues>` |
| Object listing rows | object browser/file-list component |
| Command palette query/results | command component |
| Upload dock open/expanded/metadata | upload dock component |
| New-folder draft/errors | new-folder dialog |
| Breadcrumb labels/count labels | object browser derived state |

The store remains the shared browser location and connection identity,
not the whole UI.

## 14. Implementation Plan

### Phase 1 - Docs and recipes

- Add this RFC.
- Add a general state-ownership recipe.
- Add a form-locality recipe that uses `Form<V>` from RFC 090.
- Update the storage-browser refactor recipe to reference both RFCs.

### Phase 2 - Example migration

- Continue the storage-browser refactor in narrow slices.
- Move dialog, upload, command, listing, and derived labels out of
  `StorageBrowserStore`.
- Keep route and selected connection identity in the store.

### Phase 3 - Helper APIs

Consider small helpers only after the migration proves repeated need:

- `ResetStore` trait,
- selector/computed helper conventions,
- normalized collection helper,
- devtools grouping for canonical vs derived fields.

### Phase 4 - Diagnostics

Consider optional diagnostics:

- boot/devtools hints for missing store registration already exist,
- future devtools can surface store size and subscribers,
- docs/lints can flag template assignment chains to `$store.*`.

## 15. Open Questions

- Should `#[store]` generate a default `reset` helper when `Default`
  is available?
- Should Pocopine expose selector helpers before `#[computed]` is
  settled?
- Should stores have explicit read/write capability wrappers for
  component APIs, or is `Handle<T>::with/update` enough?
- How should route loader state and store state split once nested route
  loaders mature?
- Should devtools show "canonical", "workflow", and "derived" field
  groups based on optional annotations?

## 16. Drawbacks

- Stronger guidance can feel restrictive if an app is small and a
  large store is temporarily convenient.
- Moving state local can require more prop/event wiring.
- Store actions can become too broad if authors treat them as service
  objects instead of state transitions.
- Normalized data shapes are more explicit than duplicating selected
  objects, especially in small examples.

## 17. Alternatives

### Keep stores as generic singleton components

This is today's runtime model and remains correct mechanically, but it
does not teach ownership boundaries. Examples will keep drifting into
global state blobs.

### Adopt reducers/actions everywhere

Reducers are useful for some workflows, but Pocopine's handler model
already gives typed Rust mutation methods. Requiring action enums for
all stores would add ceremony without enough benefit.

### Make everything local

This avoids global bloat, but breaks shell/sidebar/content workflows
that genuinely need shared identity and durable app state.

### Add a full state-machine crate first

State machines are valuable for complex finite workflows. They are too
heavy as the default store model. The first step is to model status as
enums and keep store ownership narrow.

## 18. Acceptance Criteria

- The docs describe when to use local component state, `Form<V>`,
  props/models, route state, stores, and sync/query state.
- Store examples use phase enums instead of contradictory boolean
  clusters for workflows.
- Recipes show how to split a large store without losing route/shared
  identity.
- Storage-browser refactor work can cite this RFC for why dialog,
  upload, command, listing, and derived labels move out of the store.
- No runtime change is required for the first slice.
