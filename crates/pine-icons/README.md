# pine-icons

[Tabler Icons][tabler] (MIT) packaged for pocopine / Pine apps.
Two call sites, one sanctioned path each.

## Rust handlers — compile-time literal

```rust
use pine_icons::icon;

let user   = icon!("user");                  // outline, default
let star   = icon!(filled / "star");
let chev   = icon!(outline / "chevron-down");
```

Unknown names produce a compile-time error with a
[jaro-winkler][strsim] "did you mean …?" suggestion. Each call
emits its own `include_str!` — unused icons stay out of the WASM
binary.

## Template primitive — `<pine-icon>`

```rust
#[wasm_bindgen(start)]
fn main() {
    pine_icons::register_icons![
        "user",
        "chevron-down",
        filled / "star",
    ];
    App::new()
        .register::<pine_icons::PineIcon>()
        .run();
}
```

```html
<pine-icon name="user"></pine-icon>
<pine-icon name="star" variant="filled" size="16"></pine-icon>
<pine-icon :name="current_icon"></pine-icon>   <!-- reactive -->
```

Only icons enumerated in `register_icons![…]` end up in the
binary. Rendering uses `pp-html` under the hood; `name`, `variant`,
and `size` are reactive — prop changes re-render the SVG in place.

## Syncing upstream

```sh
scripts/sync-tabler-icons.sh                # HEAD of main
scripts/sync-tabler-icons.sh v3.17.0        # pinned tag / sha
```

Vendored files land under `assets/tabler/outline/` +
`assets/tabler/filled/`, plus a `MANIFEST.json` pinning the
upstream commit so rebuilds stay deterministic. The script
refuses to run on a dirty tree under `assets/`.

## License

Tabler Icons is MIT — see `LICENSE.tabler-icons`. This crate
itself is MIT OR Apache-2.0 (workspace default).

[tabler]: https://github.com/tabler/tabler-icons
[strsim]: https://crates.io/crates/strsim
