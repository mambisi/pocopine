# RFC-112: Dynamic component rendering (`<pp-component :is>`)

**Status:** Implemented
**Crates:** `pocopine-core` (mount, registry, reactive), `pocopine-macros` (template compiler)
**Relates to:** the SPA router (`<pp-outlet>` / `router.rs`), RFC-058 (slot fragments / mount ABI), RFC-101 (per-row mount), RFC-089 (router parity — nested outlets)

## Summary

Add a first-class **dynamic component** element — `<pp-component :is="expr">` —
that mounts, at runtime, whichever registered component the reactive `:is`
expression resolves to, swapping it when the expression changes. Rust code uses
`ComponentRef::of::<C>()`, so the compiler proves that `C` is a component and
the runtime key comes from `C::NAME`; application authors do not spell a tag
name. This is the Vue `<component :is>` primitive with a typed Rust selection
path. It closes the one gap that previously forced either a hand-written
`pp-if` switch (one arm per component) or imperative mounting.

## Motivation

A component whose identity is chosen at runtime is a routine SPA need:

- **In-place / modal navigation** — settings panels, wizards, multi-step forms,
  master → detail — where the screen is a component keyed off store state, with
  no URL change.
- **Tab panels** — render the active tab's component from a key.
- **Plugin / data-driven screens** — a registry of components resolved by name.

Today the only key → component → mount mechanism is the **URL router**
(`<pp-outlet>` in `router.rs`): it is URL-driven, single-outlet, and nested
outlets are unimplemented (RFC-089 Phase 2). Plain templates can only mount a
component by its **literal tag**. So anything "render component X, chosen at
runtime, not from the URL" has no declarative answer.

The machinery already exists internally — `App::mount_subtree::<C>(host)` (public)
does `C::register()` + `mount::mount_child_component(host, C::NAME)` +
`finalize_compiled_subtree(host)` and returns a `SubtreeHandle` with a clean
`unmount()`. An app can drive this imperatively today (thread-local handle,
`watch_scope_field` on the key, manual teardown on remount), but that is
per-app boilerplate with no `keep-alive`, no swap transition, and untested
hydration. It belongs in the framework.

## Design

```html
<!-- active: Option<ComponentRef>; swap on change -->
<pp-component :is="active"></pp-component>

<!-- forward props to the resolved component -->
<pp-component :is="tab_key" :param="$store.x.param"></pp-component>

<!-- cache instances instead of tearing down (state + scroll survive) -->
<pp-component :is="key" keep-alive></pp-component>

<!-- composes with transitions on the swap -->
<pp-component :is="key" pp-transition:in="fade"></pp-component>
```

- **`:is`** — required, reactive. The normal Rust value is a
  `ComponentRef` constructed with `ComponentRef::of::<C>()`. That constructor
  requires `C: Component`, registers `C`, and derives the canonical key from
  `C::NAME`. A registered name is accepted only for genuinely data-driven or
  plugin screens; use `ComponentRef::from_registered_name` to validate that
  boundary. Empty or unknown values render nothing (a slot fallback may be
  offered later).
- **Props** — bound attributes forward to the resolved component's declared
  props, exactly as on a static tag.
- **`keep-alive`** — opt-in. Off (default): leaving a component unmounts it →
  fresh state next time, enter transition re-fires. On: unmounted components are
  hidden and cached (keyed by resolved name) → state + scroll preserved, cheap
  re-show. This is the fresh-vs-preserved-state knob apps keep hand-rolling.
- **Transitions** — the swapped-in subtree carries any `pp-transition`, so
  enter/leave animate on change.

## Semantics

- On mount and on every `:is` change: if the resolved name equals the currently
  mounted one, no-op; else tear down the current subtree (or hide it, under
  `keep-alive`) and mount the new one into the sentinel host.
- Lifecycle hooks (`on_mount` / `on_ready` / `on_unmount`) fire per swap on the
  child, as with `pp-if`.
- Cleanup is bound to the `<pp-component>` scope: when it unmounts, its current
  child + any `keep-alive` cache are torn down.

## Implementation notes

`<pp-component>` is a reserved sentinel tag (like `<pp-outlet>`), recognised by
the template compiler, which emits a **reactive mount region**:

1. A reactive effect reads `:is` (tracking it as a dependency) and any bound
   props.
2. On change, resolve the name via the component registry
   (`registry::mount_template_for` / the same path `mount_child_component` uses)
   and mount into the sentinel host — reusing the exact pipeline `mount_subtree`
   already wraps (`register` → `mount_child_component` → `finalize_compiled_subtree`),
   with `SubtreeHandle`-style teardown for the outgoing child.
3. `keep-alive` holds a `HashMap<name, MountedScope>`; leaving hides
   (`SubtreeHandle::hide`), returning shows.

`Component` is intentionally not converted into a trait object. It has an
associated `NAME` and static registration/mount entry points, so it is not
object-safe. `ComponentRef` is the small erased token: its public constructor is
typed, while its private runtime payload is the already-validated canonical
registry name.

Net new surface is small: a sentinel tag + a reactive region generator; the
mount/registry/reactive primitives are all present and public.

### `<pp-outlet>` becomes a special case

The URL router's outlet is `<pp-component>` keyed on the matched route:
conceptually `<pp-component :is="$route.matched">` with the router supplying the
name + props. Re-expressing `router.rs`'s mount step on top of this shared region
removes a parallel mount path and gives the router `keep-alive` + nested outlets
(RFC-089 Phase 2) for free (a screen can itself contain a `<pp-component>`).

## Non-goals

- Async / lazily-loaded components (could layer on later via a loading slot).
- Mounting arbitrary un-registered components — `:is` names go through the
  registry.
- Replacing the URL router's guards/loaders — this is the mount primitive it
  can sit on, not its navigation policy.

## Tests

- Swap on `:is` change mounts/unmounts the right component; lifecycle hooks fire
  once per swap; no scope/DOM leak across N swaps.
- Prop forwarding to the resolved child (incl. reactive prop updates without a
  remount).
- `keep-alive`: state + scroll survive a round-trip; uncached path is fresh.
- `pp-transition` fires on swap.
- Unknown / empty `:is` renders nothing and cleans up.
- `<pp-outlet>` re-expressed on the shared region passes the existing router
  suite (parity check).
