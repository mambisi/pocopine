---
title: "Storage Browser State Refactor"
description: "The storage browser example is intentionally becoming a real app: connection profiles, S3/GCS browsing, route-backed prefixes, command search, dialogs, and…"
---

# Storage Browser State Refactor

The storage browser example is intentionally becoming a real app: connection
profiles, S3/GCS browsing, route-backed prefixes, command search, dialogs, and
uploads. That also means `StorageBrowserStore` has started to hold unrelated
state. The refactor should use Pocopine's component model instead of adding more
global fields.

This recipe is the target structure and migration order.

## Pocopine Tools To Use

Use the narrowest Pocopine primitive that matches the data lifetime:

| Need | Pocopine shape |
|---|---|
| One component owns it | local component fields on `#[component]` |
| Parent configures a child | `#[prop]` fields plus static attrs or `pp-bind:` |
| Many parent-to-child fields | `#[derive(Props)]` prop bag plus `#[prop(flatten)]` |
| Route params | route component `#[prop]` fields |
| Cross-subtree app state | one focused `#[store]` |
| Browser/server boundary | `#[pocopine::server]` functions with serializable DTOs |
| Repeated UI rows | keyed `pp-for` with small row DTOs |
| User-controlled Pine primitives | `pp-model:*` for open/value state when the parent controls it |

Two rules matter most:

- Stores are for durable, cross-subtree state. Dialog inputs, loading flags,
  command search results, upload expansion, and transient errors should usually
  live in the component that renders them.
- Props are an explicit component contract. Mark exposed fields with `#[prop]`;
  keep everything else as private component state.

## Current Problem Shape

`examples/file-browser/src/store/mod.rs` is doing all of these jobs:

- selected connection and route prefix
- connection list and selected connection metadata
- object listing, filters, derived counts, breadcrumbs, path labels
- connection modal form state for both S3 and GCS
- command palette state and all-bucket search results
- upload dock state and upload metadata
- new-folder dialog state
- loading, saving, and error flags for several independent workflows

That creates three symptoms:

- Unrelated UI actions invalidate the same global store.
- Components read many `$store.storage.*` fields they do not own.
- Derived labels such as `path_meta`, count labels, and prefix labels become
  cached state instead of local render facts.

## Target Ownership

Keep one small app store for navigation and shared identity:

```rust
#[derive(Default, Serialize, Deserialize)]
#[store(name = "storage")]
pub struct StorageBrowserStore {
    pub connections: Vec<StorageConnectionSummary>,
    pub selected_connection_id: String,
    pub current_prefix: String,
}
```

Everything else should move closer to the component that owns the UI.

| Concern | Owner after refactor |
|---|---|
| Connection list load/delete/reconnect | `FileBrowserSidebar` |
| Connection form fields/save errors | `FileBrowserConnectionDialog` |
| Current object listing/filter/counts | `FileBrowserFileList` or a new `FileBrowserObjectBrowser` |
| Breadcrumbs/path title/path subtitle | object browser/header local derived state |
| New folder name/error/saving | `FileBrowserNewFolderDialog` |
| Upload open/expanded/metadata | `FileBrowserUploadDock` |
| Command palette open/query/results/errors | `FileBrowserStorageCommand` |
| Route param synchronization | `FileBrowserRoute` |

## Prop Bags

When a child needs several fields from the selected connection, pass a typed prop
bag instead of adding more store fields.

```rust
#[derive(Props, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionBadgeProps {
    #[prop] pub id: String,
    #[prop] pub name: String,
    #[prop] pub provider_label: String,
    #[prop] pub bucket: String,
    #[prop] pub root_prefix: String,
    #[prop] pub favicon_url: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct FileBrowserHeader {
    #[prop(flatten)]
    pub connection: ConnectionBadgeProps,

    #[prop]
    pub prefix: String,
}
```

The caller still writes flat template bindings:

```html
<file-browser-header
  pp-bind:id="selected.id"
  pp-bind:name="selected.name"
  pp-bind:provider-label="selected.provider_label"
  pp-bind:bucket="selected.bucket"
  pp-bind:root-prefix="selected.root_prefix"
  pp-bind:favicon-url="selected.favicon_url"
  pp-bind:prefix="$store.storage.current_prefix" />
```

Use this for UI identity and display metadata. Do not use prop bags to smuggle
mutable workflow state across the tree.

## Server DTOs Stay Central

Keep provider DTOs and server functions in `examples/file-browser/src/storage_browser/`:

- `mod.rs` exposes serializable DTOs and `#[pocopine::server]` wrappers.
- `server.rs` owns local config persistence and provider-specific SDK calls.
- Components call the server functions directly and update their local fields
  from the response.

This keeps the S3/GCS boundary reusable while letting UI state move out of the
global store.

## Migration Order

1. Extract `FileBrowserConnectionDialog`.
   Move `connection_*`, `saving_connection`, and `modal_error` out of the store.
   Keep the dialog controlled by `pp-model:open` if the shell needs to open it.

2. Extract object-browser state.
   Move `entries`, `visible_entries`, filters, listing request id, breadcrumbs,
   counts, and listing errors into the file-list/browser component. Pass only
   `connection_id` and `prefix` as props.

3. Let route state stay global.
   Keep `selected_connection_id` and `current_prefix` in the store so sidebar,
   route sync, upload, and listing agree on the browser location.

4. Move command search into `FileBrowserStorageCommand`.
   The command palette already owns its visual shell. It should own query,
   loading, errors, and result rows too.

5. Move upload dock state into `FileBrowserUploadDock`.
   Pass `connection_id` and `prefix` as props and build upload metadata locally.
   Keep completion refresh as an explicit event or store update.

6. Delete derived global labels last.
   Once components own their data, recompute labels locally from the local state:
   count labels from entries, title from prefix, subtitle from connection props.

## Test Split

As fields move out of the store, move tests with the behavior:

- Store tests: route normalization and selected-connection changes.
- Connection dialog tests: defaults, service JSON parsing, auth-mode switches.
- Object browser tests: stale-listing rejection, filters, derived labels.
- Server tests: provider config, key safety, timestamp formatting, favicon
  domain mapping.

Avoid one huge "storage browser store" test module. Each owner should have its
own narrow invariants.

## Stop Criteria

The refactor is complete when:

- `StorageBrowserStore` is under roughly 200 lines and has one reason to change:
  browser location and shared connection identity.
- No dialog input is read from `$store.storage.*`.
- No loading flag in the store represents a component-local request.
- Main browser content can be reasoned about from its props plus its local
  listing state.
- Server DTOs remain stable and provider-specific code stays out of components.
