# Building a Real Pocopine App

This guide captures the development process behind the Keep example. It is
not a feature tour. It is the set of practices that kept the example from
turning into a tangle of duplicated state, brittle directives, and global CSS
collisions.

The short version:

- design the data contract first,
- keep durable UI state in one store,
- keep small interaction state inside the component that owns it,
- share semantic components instead of copying DOM,
- keep directive expressions simple,
- split CSS by owner, but remember that CSS is not scoped yet.

## 1. Start With Data Contracts

Before building the UI, define the synced payloads and streams. In this
example those live in `src/model.rs`:

- `KeepNote`
- `KeepTodo`
- `KeepTag`
- `KEEP_STREAM` / `KEEP_COLLECTION`
- `KEEP_TAGS_STREAM` / `KEEP_TAGS_COLLECTION`

The payload shape should answer product questions before component questions:

- Is a checklist a separate resource or part of a note?
- Are labels just strings on notes, or queryable synced rows?
- Does archive/pin belong to note state or view state?
- Which fields need to survive refresh and sync to another browser?

For Keep, todos are part of the note payload, but tags are a separate synced
stream. That makes the sidebar queryable and durable even when a label has no
visible notes in the current section.

## 2. Use One Store as the App State Spine

`KeepStore` owns durable app state:

- synced note rows,
- synced tag rows,
- derived visible lists (`pinned_notes`, `other_notes`),
- command/search state,
- sidebar section state,
- editor/composer open state,
- selected note ids,
- theme preference.

The store is split by responsibility:

- `store/mod.rs` contains the serialized state shape, defaults, and tests,
- `store/actions.rs` contains UI-facing commands,
- `store/mutations.rs` contains note/tag push plumbing,
- `store/derived.rs` builds visible rows and label registries,
- `store/labels.rs`, `store/theme.rs`, and `store/view.rs` hold pure helper
  types and functions.

Cross-cutting helpers live in `src/utils/`:

- `utils/time.rs` owns timestamp helpers,
- `utils/ui.rs` owns DOM focus and shared-layout transition helpers.

This matters because Pocopine templates are easiest to reason about when
views render already-shaped data. Avoid doing heavy filtering through nested
`pp-for` plus `pp-if` in the template. Prefer deriving a list in the store,
then rendering it directly:

```rust
store.rebuild_visible_notes();
```

Then the template can stay simple:

```html
<template pp-for="row in $store.keep.other_notes" pp-key="row.key">
  <keep-note-card ...></keep-note-card>
</template>
```

## 3. Keep Component State Local When It Is Truly Local

Not every field belongs in `KeepStore`.

Good local component state:

- whether a card label popover is open,
- the current query inside that card's label picker,
- the current color popover open flag,
- an active form edit buffer before save.

Good store state:

- the actual note list,
- the actual label registry,
- the active editor id,
- whether the composer is open,
- selected notes for multi-select,
- command palette state.

Rule of thumb: if another component needs to render or mutate it, put it in
the store. If it only describes a tiny interaction inside one component, keep
it local.

## 4. Split Components by Ownership

The current component split is intentional:

| Component | Owns |
| --- | --- |
| `KeepBoard` | App shell, sync wiring, command dialog, sections, selection bar |
| `KeepComposer` | Collapsed/expanded create-note shell |
| `KeepEditor` | Modal shell and editor focus behavior |
| `KeepNoteForm` | Shared active edit buffer and save/archive dispatch |
| `KeepNoteBody` | Text body vs checklist body editing |
| `KeepNoteCard` | One masonry card and its card-local actions |

The important part is that `KeepComposer` and `KeepEditor` both use the same
`KeepNoteForm`. They do not each define their own title field, body field,
label picker, color picker, and toolbar.

That avoids the classic bug where editing works in the modal but not the
composer, or labels work in cards but not in the editor.

## 5. Share Semantics, Not Layout Flags

The shared form needs to know whether it is creating a draft or updating an
existing note. That is semantic.

It should not need to know whether it is currently displayed inside a modal
or an inline composer. That is layout.

Good:

```rust
KeepNoteFormContext { mode: "draft" }
KeepNoteFormContext { mode: "editor" }
```

Avoid:

```rust
KeepNoteFormContext { surface: "modal" }
KeepNoteFormContext { surface: "composer" }
```

The shell should provide size, placement, backdrop, and available scroll
space. The form should provide the same fields and commands everywhere.

## 6. Use Providers for Stable Semantic Context

Providers are useful for static configuration at component setup time. In
this example:

- `KeepComposer` provides `mode = "draft"`,
- `KeepEditor` provides `mode = "editor"`,
- `KeepNoteForm` injects that mode once during setup.

Do not use providers as a replacement for active payload updates. If the
active note changes, use store watchers or props/models. The form watches
`editor_open`, `editor_id`, and `editor_pinned` because those are live app
state changes.

## 7. Prefer One Edit Buffer

The form owns one local edit buffer:

- `title`
- `body`
- `kind`
- `color`
- `todos`
- `labels`
- `pinned`

On save it emits a typed `KeepFormNote` into `KeepStore`.

This is cleaner than binding every input directly to global store fields.
Direct global binding makes cancel/rollback harder and tends to leak partial
input state into unrelated UI.

The flow is:

1. Open composer or editor.
2. Load the local form buffer.
3. Let fields edit local component state.
4. On save, collect a `KeepFormNote`.
5. Let `KeepStore` create/update/archive and push the sync mutation.

## 8. Keep Directive Expressions Small

Pocopine directive expressions are Rust-oriented, not JavaScript template
expressions. Keep complex logic in handlers and store methods.

Prefer:

```html
<button @click="toggle_kind">...</button>
```

with Rust:

```rust
pub fn toggle_kind(&mut self) {
    ...
}
```

Avoid packing workflow logic into template expressions. It becomes harder to
debug, harder to type-check mentally, and more likely to expose parser/runtime
edge cases.

## 9. Use `pp-model:value` With Pine Inputs

Pine form primitives expose named component models. For `pine-input` and
`pine-textarea`, bind their `value` model:

```html
<pine-input pp-model:value="title"></pine-input>
<pine-textarea pp-model:value="body"></pine-textarea>
```

For native inputs inside a template, `pp-model` is still fine:

```html
<input pp-model="$store.keep.command_label_query">
```

If a form opens but fields are blank, inspect the rendered DOM and confirm
that the actual native `input` or `textarea` exists and has the expected
value. In this app, that caught several form-split mistakes quickly.

## 10. Derive Lists Before Rendering

The cards originally looked tempting to render as:

```html
<template pp-for="row in rows">
  <template pp-if="row.value.pinned">...</template>
</template>
```

That puts filtering, grouping, and rendering in the template. It also makes
blank-list bugs harder to diagnose because a row may exist but fail an inner
condition.

The current pattern is better:

- `KeepStore::rebuild_visible_notes` computes `pinned_notes` and
  `other_notes`,
- the template renders each list directly,
- tests cover the section/archive/label filtering rules.

## 11. Labels Must Be Data, Not Decorations

Labels appear as chips on notes, but they are also navigation and search
data. That means they cannot only live inside a note payload.

The Keep example uses:

- labels attached to notes,
- a synced tag stream,
- a registry rebuilt from both sources,
- backfill for labels found on hydrated notes but missing from the tag stream.

This prevents the sidebar from losing labels after a refresh.

## 12. Local Storage Is for Preferences, SQLite Is for Sync Data

Use typed `LocalStorage` for tiny preferences:

- theme,
- small UI preferences,
- values that are safe to lose or reset.

Use the sync local store for sync data:

- rows,
- cursors,
- pending mutations,
- durable offline state.

Do not put synced rows in browser `localStorage`. Keep that path typed and
small.

## 13. CSS Split Pattern

Use the same three-file component shape:

```text
components/note_card/
  KeepNoteCard.poco
  KeepNoteCard.css
  mod.rs
```

Then point the component macro at the CSS:

```rust
#[component(style = "KeepNoteCard.css")]
pub struct KeepNoteCard { ... }
```

Keep board-owned app styles in `components/board/KeepBoard.css`:

- theme variables,
- body/app shell,
- topbar,
- sidebar,
- command dialog,
- masonry grid,
- selection bar,
- shared utility classes.

Move component-owned styles into component CSS:

- composer chrome in `KeepComposer.css`,
- form fields/toolbars in `KeepNoteForm.css`,
- checklist/body editing in `KeepNoteBody.css`,
- card layout/actions in `KeepNoteCard.css`,
- modal shell in `KeepEditor.css`.

## 14. CSS Is Split, Not Scoped

Important: component CSS is not isolated today.

`#[component(style = "...")]` injects a normal global `<style>` tag. Splitting
CSS improves ownership, but selectors can still collide.

Avoid generic component selectors:

```css
.button { ... }
.title { ... }
.body { ... }
```

Prefer component-specific classes:

```css
.note-form-foot { ... }
.note-select { ... }
.cmd-inline-input { ... }
```

or host-qualified selectors when that makes ownership clearer:

```css
keep-note-card .note-actions { ... }
```

Scoped component CSS is tracked separately in:

- <https://github.com/mambisi/pocopine/issues/79>
- `docs/poco/03-scoped-styles.md`

Until that lands, CSS split is an organization tool, not a safety boundary.

## 15. Popovers, Dropdowns, and Portals

Pine popovers/dropdowns may render content through portals. Any selector that
depends on the DOM ancestry of the trigger can break when content is portaled.

Prefer styling portal content through stable classes on the content itself:

```html
<pine-popover-content class="label-pop">...</pine-popover-content>
```

and:

```css
.label-pop { ... }
```

This is another reason scoped CSS needs an explicit portal story before it
becomes the default.

## 16. Click-Through and Toolbar Rules

Cards open on click, but toolbar buttons inside cards should not open the
card. Use `@pointerdown.stop` and `@click.stop` on card toolbars and action
buttons that should consume the event. Pine overlay primitives already isolate
their own triggers and content, so only the surrounding toolbar or non-overlay
actions need app-level stops.

The pattern is:

```html
<div class="note-actions" @pointerdown.stop @click.stop>
  ...
</div>
```

Use this for:

- toolbar groups that mix overlay and non-overlay actions,
- archive/delete actions,
- multi-select controls.

## 17. Verify With the Browser

For this app, Rust checks are necessary but not enough. Several real bugs
were only visible after mount:

- blank editor fields after component extraction,
- click-through from card toolbar buttons,
- missing labels after refresh,
- CSS selector misses after splitting Pine input/textarea styles,
- OPFS failures without cross-origin isolation headers.

Minimum verification after UI changes:

```bash
cargo check -p keep-example
cargo check -p keep-example --target wasm32-unknown-unknown
cargo test -p keep-example
wasm-pack build --target web --dev
```

Then run the example:

```bash
cargo run -p pocopine-cli -- dev --path examples/keep
```

Browser smoke checklist:

- create a text note,
- create a checklist note,
- open and close the editor,
- verify title/body/todos populate,
- change color,
- add/remove a label,
- refresh the page and confirm notes and labels remain,
- open a second tab and confirm live wake-up pulls changes,
- use the command dialog for search and inline label creation,
- try multi-select actions,
- inspect the console for wasm panics.

## 18. When Something Goes Blank

Use this order:

1. Inspect the rendered DOM. Is the element missing, hidden, or empty?
2. Check whether the store has the data.
3. Check whether the component local buffer loaded from the store.
4. Check whether the template binds the right model name.
5. Check whether a `pp-if` destroyed and recreated the subtree.
6. Check whether a CSS selector is targeting the custom element instead of
   the native input rendered inside it, or the reverse.

Do not guess from the Rust code alone. In Pocopine, the generated DOM is part
of the truth.

## 19. Commit Boundaries

Good commit boundaries for an app like this:

- data model and sync stream,
- store state and derived lists,
- component shells,
- shared form extraction,
- local persistence,
- labels/tag stream,
- command dialog,
- multi-select,
- CSS split,
- docs.

Keep each commit runnable. If a refactor breaks editor population or card
rendering halfway through, finish the repair before committing.
