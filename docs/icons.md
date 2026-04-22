# Icons (`pine-icons`)

pocopine ships a dedicated crate — `pine-icons` — that vendors
[Tabler Icons][tabler] (MIT, ~6000 icons across outline and
filled sets) with a tree-shaking-friendly API for both Rust
handlers and `.poco` templates.

## The two call sites

### Rust code — `icon!("name")`

The `icon!` proc macro returns the SVG body as a `&'static str`.
Each call is its own `include_str!`, so unused icons never ship
in the WASM binary.

```rust
use pine_icons::icon;

fn toolbar_class_for(busy: bool) -> &'static str {
    if busy { icon!("loader-2") } else { icon!("check") }
}

// Explicit variant:
let star_filled   = icon!(filled / "star");
let star_outline  = icon!(outline / "star");      // same as icon!("star")
```

Typos become compile errors:

```
error: unknown tabler outline icon `usre` — did you mean `user`?
```

### Templates — `<pine-icon>` + `register_icons!`

```rust
#[wasm_bindgen(start)]
fn main() {
    pine_icons::register_icons![
        "search",
        "chevron-down",
        "x",
        filled / "star",
    ];
    App::new()
        .register::<pine_icons::PineIcon>()
        .run();
}
```

```html
<!-- static -->
<pine-icon name="search" size="16"></pine-icon>

<!-- reactive: icon flips with `theme` -->
<pine-icon :name="theme == 'dark' ? 'sun' : 'moon'"></pine-icon>

<!-- filled variant -->
<pine-icon name="star" variant="filled"></pine-icon>
```

Only names listed in `register_icons![…]` end up in the binary.
Using an unregistered name renders nothing and logs a one-line
warning in dev builds.

## When to pick which

| Case | Use |
|---|---|
| Icon driven by a Rust `match` or `if` branch | `icon!("name")` in the handler, store in a field, bind via `pp-html`. |
| Icon baked directly into a template | `<pine-icon name="…">` |
| Icon chosen at runtime from data | `<pine-icon :name="data.icon">` (+ register every possible value upfront) |

Both mechanisms compose — a component can expose an `icon!`
result as a field and render a `<pine-icon>` in the same
template.

## Styling

`<pine-icon>` renders as a `<span>` (role=`visual`) containing
the raw SVG. Default CSS inherits `currentColor` for both
`fill` and `stroke`, so icons take on the surrounding text
color with no extra work:

```css
button { color: var(--brand); }
/* any <pine-icon> inside the button inherits brand color */
```

Size comes from the `size` prop (pixels, default 20). Override
with CSS if you want em-relative sizing:

```css
.inline-glyph { font-size: 1em; }
.inline-glyph .pine-icon,
.inline-glyph svg {
    width: 1em;
    height: 1em;
}
```

## Syncing with upstream

Tabler ships new icons continuously. The vendored set pins a
specific upstream commit recorded in
`crates/pine-icons/assets/tabler/MANIFEST.json`. Update via:

```sh
scripts/sync-tabler-icons.sh              # HEAD of main
scripts/sync-tabler-icons.sh v3.17.0      # specific tag
scripts/sync-tabler-icons.sh abc1234      # specific sha
```

The script:

1. Refuses to run if `assets/tabler/` has uncommitted changes.
2. Clones Tabler at the requested ref, resolves the commit SHA.
3. Copies `icons/outline/*.svg` + `icons/filled/*.svg` into the
   vendored tree.
4. Strips each file's leading `<!-- tags: … -->` metadata block
   (~150 bytes per icon × thousands of icons = meaningful saving
   once registered).
5. Rewrites `MANIFEST.json` + `LICENSE.tabler-icons`.
6. Prints a `git status` of the vendored directory.

Review the diff, bump `pine-icons`'s version if the public API
was affected (icons renamed upstream, etc.), commit.

## Attribution

Tabler Icons is MIT licensed — `crates/pine-icons/LICENSE.tabler-icons`
carries the upstream notice. When redistributing a Pine app that
uses `pine-icons`, keep that file alongside your own license.

[tabler]: https://github.com/tabler/tabler-icons
