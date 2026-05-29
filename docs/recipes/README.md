# Pocopine Recipes

Applied implementation recipes for turning the framework primitives into
maintainable application structure.

- [Storage browser state refactor](./storage-browser-state-refactor.md) -
  split a large app store into local component state, prop surfaces, route
  props, and server-function modules.
- [State ownership](./state-ownership.md) - choose between component
  state, `Form<V>`, props/models, route identity, stores, sync/query
  state, and derived values.
- [Form state](./form-state.md) - keep form drafts local with
  `pocopine-form`, submit through resolvers, and apply server errors
  without moving draft fields into stores.
