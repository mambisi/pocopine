---
title: "Storage Browser State Refactor"
description: "How to decompose an over-extended global store into focused component state, prop bags, and server DTOs — using the file-browser example as the reference."
---

# Storage Browser State Refactor

`StorageBrowserStore` has grown to cover connection profiles, S3/GCS browsing,
route-backed prefixes, command search, dialogs, and uploads in a single global
scope. This recipe describes the target structure and migration order for moving
unrelated state out of the store and into the components that own the
corresponding UI.

## Pocopine Primitives to Use

Use the narrowest Pocopine primitive that matches the data lifetime:

| Need | Pocopine shape |
|---|---|
| One component owns it | plain fields on a `#[component]` struct |
| Parent configures a child | `#[prop]` fields, bound with static attrs or `pp-bind:` |
| Many parent-to-child fields | `#[derive(Props)]` prop bag with `#[prop(flatten)]` |
| Route params | `#[prop]` fields on a route component |
| Cross-subtree app state | one focused `#[store]` |
| Browser/server boundary | `#[pocopine::server]` functions with serializable DTOs |
| Repeated UI rows | `pp-for` with `pp-key` and small row DTOs |
| Parent-controlled open/value state | `#[model]` field + `pp-model:*` at the call site |

Two rules matter most:

- Stores are for durable, cross-subtree state. Dialog inputs, loading flags,
  command search results, upload expansion, and transient errors should live in
  the component that renders them.
- Props are an explicit component contract. Mark inbound fields with `#[prop]`
  (or `#[model]` for two-way binding); keep everything else as private
  component state.

## Current Problem Shape

`examples/file-browser/src/store/mod.rs` carries all of these responsibilities:

- selected connection and route prefix
- connection list and selected connection metadata
- object listing, filters, derived counts, breadcrumbs, and path labels
- connection modal form state for both S3 and GCS
- command palette state and all-bucket search results
- upload dock state and upload metadata
- new-folder dialog state
- loading, saving, and error flags for several independent workflows

That produces three symptoms:

- Unrelated UI actions invalidate the same reactive scope.
- Components read `$store.storage.*` fields that belong to a different workflow.
- Derived labels (`path_meta`, count labels, prefix labels) are stored as
  cached state rather than computed at render time from local data.

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

Everything else moves closer to the component that owns the corresponding UI.

| Concern | Owner after refactor |
|---|---|
| Connection list load/delete/reconnect | `FileBrowserSidebar` |
| Connection form fields/save errors | `FileBrowserConnectionDialog` |
| Current object listing/filter/counts | `FileBrowserFileList` or a new `FileBrowserObjectBrowser` |
| Breadcrumbs/path title/path subtitle | local derived state inside the object browser or header |
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

`#[prop(flatten)]` maps the leaves of `ConnectionBadgeProps` directly to the
host element's attributes. The caller still writes flat `pp-bind:` bindings:

```poco
<file-browser-header
  pp-bind:id="selected.id"
  pp-bind:name="selected.name"
  pp-bind:provider-label="selected.provider_label"
  pp-bind:bucket="selected.bucket"
  pp-bind:root-prefix="selected.root_prefix"
  pp-bind:favicon-url="selected.favicon_url"
  pp-bind:prefix="$store.storage.current_prefix" />
```

Use prop bags for UI identity and display metadata. Do not use them to pass
mutable workflow state across the component tree.

## Server DTOs Stay Central

Keep provider DTOs and server functions in `examples/file-browser/src/storage_browser/`:

- `mod.rs` exposes serializable DTOs and `#[pocopine::server]` wrappers.
- `server/mod.rs` owns local config persistence and provider-specific SDK calls.
- Components call the server functions directly and update their local fields
  from the response.

This keeps the S3/GCS boundary reusable while letting UI state move out of the
global store.

## Migration Order

1. Extract `FileBrowserConnectionDialog`.
   Move `connection_*`, `saving_connection`, and `modal_error` out of the store.
   Declare `#[model] pub open: bool` on the dialog so the shell can control it
   with `pp-model:open` when it needs to open the dialog programmatically.

2. Extract object-browser state.
   Move `entries`, `visible_entries`, filters, `listing_request_id`, breadcrumbs,
   counts, and listing errors into `FileBrowserFileList` (or a new
   `FileBrowserObjectBrowser`). Pass only `connection_id` and `prefix` as
   `#[prop]` fields.

3. Keep route state global.
   `selected_connection_id` and `current_prefix` stay in the store so the
   sidebar, `FileBrowserRoute`, upload dock, and file list all agree on the
   current browser location.

4. Move command search into `FileBrowserStorageCommand`.
   The command palette already owns its visual shell. Move `query`, loading
   flag, errors, and result rows into the component as plain fields.

5. Move upload dock state into `FileBrowserUploadDock`.
   Pass `connection_id` and `prefix` as `#[prop]` fields and build upload
   metadata locally. Trigger a listing refresh via a store handler or direct
   server call on completion.

6. Delete derived global labels last.
   Once components own their data, recompute labels locally: count labels from
   entries, title from prefix, subtitle from connection props.

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
