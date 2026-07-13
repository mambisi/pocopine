# Pine rich-text BubbleMenu

Enable the local browser feature and register the component with the app:

```toml
[dependencies]
pine-richtext-extensions = { workspace = true, features = ["bubble-menu"] }
```

```rust
use pine_richtext_extensions::bubble_menu::PineRichTextBubbleMenu;

app.register::<PineRichTextBubbleMenu>();
```

The declarative form keeps the menu mounted, follows text/node/cell selection
geometry, and hides for empty, blurred, detached, or read-only selections by
default:

```html
<section class="editor-with-menu">
  <pine-rich-text-root id="article-editor"></pine-rich-text-root>

  <pine-rich-text-bubble-menu
    editor="#article-editor"
    placement="top"
    align="center"
    offset="8"
    viewport_padding="12"
    debounce_ms="0">
    <button type="button" aria-label="Bold">B</button>
    <button type="button" aria-label="Italic"><em>I</em></button>
    <button type="button" aria-label="Add link">Link</button>
  </pine-rich-text-bubble-menu>
</section>
```

Each mounted menu receives a unique `data-plugin-key`. Stable styling hooks are
`data-state`, `data-placement`, `data-align`, `data-anchor-kind`, and:

- `--pine-richtext-bubble-menu-background`
- `--pine-richtext-bubble-menu-color`
- `--pine-richtext-bubble-menu-border`
- `--pine-richtext-bubble-menu-radius`
- `--pine-richtext-bubble-menu-shadow`
- `--pine-richtext-bubble-menu-padding`
- `--pine-richtext-bubble-menu-gap`
- `--pine-richtext-bubble-menu-z-index`

For application-specific visibility, attach the controller directly. The
predicate can narrow the safety defaults but cannot make a detached or
geometry-less menu visible:

```rust,ignore
use pine_richtext_extensions::bubble_menu::{
    BubbleMenuController, BubbleMenuOptions,
};
use pine_richtext::view::Editor;

let options = BubbleMenuOptions::default().with_should_show(|context| {
    !context.snapshot.enclosing_block_types.iter().any(|name| name == "code_block")
});
let controller = BubbleMenuController::attach(editor, menu_element, options)?;
// Keep `controller` for exactly as long as the menu is mounted.
```

## Searchable command surfaces

Set `BubbleMenuOptions::searchable = true`, then call
`controller.begin_search(query)`. The returned token identifies exactly one
request. While it is pending, committed editor steps map the preserved
`SelectionBookmark`. Pass the token and provider value to
`controller.search().unwrap().finish(token, value)`:

- a current result returns `MappedSearchResult { value, selection }`;
- an older result is rejected as `StaleSearchResult`;
- starting a new query or dropping the controller invalidates earlier work.

The application resolves/dispatches the returned bookmark in its command. This
keeps network/provider concerns outside the editor UI while preventing a slow
response from applying to the wrong text after the document changed.

## Focus and dismissal

Moving focus into menu buttons or inputs keeps the menu open. Pointer-down is
captured before editor blur, so click commands still run even for controls that
do not take focus. Escape hides the menu for the current selection and, by
default, refocuses the editor. Set `escape_refocus="false"` to leave focus where
the application places it.
