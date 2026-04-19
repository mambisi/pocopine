# pocopine + Tailwind CSS

Minimal pocopine app styled entirely with Tailwind utility classes.
Two build modes are supported.

## 1. CDN mode (the default here — zero setup)

`index.html` loads Tailwind v4's browser build via
`https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4`, which scans the
live DOM at runtime and generates CSS on the fly. Run:

```bash
cargo run -p pocopine-cli -- dev --path examples/tailwind
```

Open the URL it prints. That's the whole story — no `npm install`,
no separate CSS build step. Good for demos and prototypes.

## 2. Local build (production)

The CDN build is convenient but ships the Tailwind engine to every
visitor. For production, run the Tailwind CLI locally and link the
compiled CSS.

### Install the standalone CLI

```bash
npm install -D tailwindcss @tailwindcss/cli
```

(or download the
[standalone binary](https://github.com/tailwindlabs/tailwindcss/releases)
if you want to stay off npm — same flags).

### Point Tailwind at your templates

`app.css`:

```css
@import "tailwindcss";
@source "./src/**/*.poco";
```

The `@source` line teaches Tailwind to scan `.poco` files. Tailwind
parses raw text and regex-matches class-name-shaped tokens, so the
extension doesn't matter as long as it's in a source glob.

### Build the stylesheet

```bash
npx @tailwindcss/cli -i ./app.css -o ./tailwind.css --watch
```

Swap the CDN `<script>` tag in `index.html` for:

```html
<link rel="stylesheet" href="/tailwind.css" />
```

Now ship just the utilities your templates actually use.

## DaisyUI

Once Tailwind is in, DaisyUI is one line:

```css
@import "tailwindcss";
@plugin "daisyui";
@source "./src/**/*.poco";
```

`<button class="btn btn-primary">` then works the same way any
utility does.

## What to scan

pocopine writes class names in three places:

- **`.poco` templates** — most classes.
- **`.rs` handlers** — e.g. `pp-bind:class="classes_for_row"` where
  the string comes out of a handler. Scan these too:
  ```css
  @source "./src/**/*.{poco,rs}";
  ```
- **`index.html`** — classes on the root element. Scanned by default.
