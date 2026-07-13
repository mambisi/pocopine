# Typed tags and chips

Enable `pine-richtext-extensions` with `tags` for the target-independent model
and commands. Add `view` when the browser editor should render the built-in
Pocopine chip.

```toml
pine-richtext-extensions = { workspace = true, features = ["tags", "view"] }
```

Register the same extension through the typed model/view lane. The runtime
proves that `TagNode` is paired with `PineRichTextTag`; application code never
stores a component name or custom-element tag.

```rust
use pine_richtext::runtime::RuntimeBuilder;
use pine_richtext_extensions::tags::TagsExtension;

let runtime = RuntimeBuilder::new()
    .with_view(TagsExtension)
    .try_build()?;
```

`TagsExtension` also contributes a typed native `NodeDomSpec`. Model-only
renderers emit an accessible `<span class="pine-richtext-tag
pine-richtext-tag--native">`; browser runtimes use the same structure as the
visible fallback if the Pocopine component cannot mount. The native tree binds
only the declared `id`, `label`, and closed `kind` token attrs. Pine's HTML
serializer escapes label text and attribute boundaries, and no component name,
remove button, handler, or arbitrary CSS enters the semantic output.

Insert a tag with a normal transaction command:

```rust
use pine_richtext::commands::Command;
use pine_richtext_extensions::tags::{TagAttrs, TagKind, insert_tag};

let attrs = TagAttrs::new("issue-272", "Framework")?
    .with_kind(TagKind::Info);
if let Some(transaction) = insert_tag(attrs).apply(&state) {
    state = state.apply(transaction)?;
}
```

The registered named commands are `insert_tag`, `update_tag`, `delete_tag`, and
`select_tag`. They are also available through
`CommandRequest::Custom { name, args }`. `insert_tag` accepts optional `from`
and `to` positions, allowing a suggestion picker to replace its trigger and
query after focus moves into the picker.

## Lightweight suggestions

`TagSuggestionMatcher` consumes `ChangeInfo::caret_prefix` and a
`SelectionSnapshot`. It does not serialize the document or inspect DOM text.
The trigger, boundary rule, query length, spaces, and accepted query characters
are configurable.

```rust
use pine_richtext_extensions::tags::{
    TagSuggestionConfig, TagSuggestionMatcher,
};

let matcher = TagSuggestionMatcher::new(TagSuggestionConfig {
    trigger: '#',
    maximum_query_chars: 32,
    ..TagSuggestionConfig::default()
});

let editor_for_change = editor.clone();
let suggestion_matcher = matcher.clone();
let subscription = editor.on_change(move |change| {
    let Ok(snapshot) = editor_for_change.selection_snapshot() else { return };
    if let Some(active) = suggestion_matcher.match_change(&change, &snapshot) {
        // Filter the application's tag catalog with active.query.
        // On acceptance dispatch insert_tag with active.from/active.to.
    }
});
```

Keep the returned subscription alive for as long as the picker is mounted.

## Serialization and clipboard

- JSON and Pine's normal slice clipboard preserve `id`, `label`, `kind`, and
  the typed node version.
- `TagClipboardPayload` plus `TAG_CLIPBOARD_MIME` provides a lossless format for
  integrations copying one chip. It includes a strict `type` discriminator and
  version and rejects unknown fields or mismatched payload versions.
- Plain text emits `#label`; Markdown emits the CommonMark-safe `\#label` so it
  remains ordinary text even at the start of a block. Those formats are
  intentionally lossy: external id and visual kind are editor metadata and are
  not encoded into an invented Markdown dialect.
- Backspace/Delete use Pine's standard inline-atom commands, so a chip is
  deleted as one unit. ArrowLeft/ArrowRight first select an adjacent tag and
  then move past a selected tag on the next press.

## Styling and the optional remove action

The built-in view exposes `.pine-richtext-tag`, `data-kind`, `data-selection`,
`data-editable`, and child classes. Override the
`--pine-richtext-tag-*` custom properties from an application stylesheet.
Arbitrary CSS is never persisted in the document.

The accessible remove button is present but hidden by default. Enable it for a
surface without changing semantic data:

```css
.my-editor .pine-richtext-tag {
  --pine-richtext-tag-border-radius: 0.45rem;
  --pine-richtext-tag-remove-display: inline-flex;
}
```
