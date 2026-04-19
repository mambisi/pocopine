# pocopine + Tailwind CSS

Minimal pocopine app styled with Tailwind utility classes, compiled
locally by `pocopine-cli`.

## Run

```bash
cargo run -p pocopine-cli -- dev --path examples/tailwind
```

On the first run, `pocopine-cli` downloads the Tailwind standalone
CLI to `target/pocopine/bin/tailwindcss` and spawns it in watch mode
alongside `wasm-pack`. Subsequent runs reuse the cached binary.

## Config

The Tailwind integration is opt-in per project via
`Cargo.toml`:

```toml
[package.metadata.pocopine.tailwind]
input = "app.css"          # entry CSS
output = "pkg/tailwind.css" # compiled bundle
# version = "v4.0.0"        # optional, pins the upstream release
# binary = "./tailwindcss"  # optional, uses a local binary instead
```

`app.css` is a normal Tailwind entry:

```css
@import "tailwindcss";
@source "./src/**/*.poco";
```

`@source` teaches Tailwind to scan `.poco` templates (Tailwind
parses raw text, so the extension doesn't matter — only the glob
does).

## DaisyUI

Add one line to `app.css`:

```css
@import "tailwindcss";
@plugin "daisyui";
@source "./src/**/*.poco";
```

`<button class="btn btn-primary">` then works the same as any other
utility.

## What about Node?

You don't need it. Tailwind v4 ships a Rust-backed standalone
binary; `pocopine-cli` downloads it on demand. If you'd rather manage
the binary yourself, set `binary = "..."` in the config.
