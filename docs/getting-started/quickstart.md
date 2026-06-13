---
title: "Quickstart"
description: "Scaffold an app, write your first component, and run it."
---

# Quickstart

This walks you from an empty directory to a running component. It
assumes the [CLI is installed](./installation.md).

## 1. Scaffold an app

`pocopine new` scaffolds a small welcome app — Cargo manifest,
`index.html`, and a few composed components — so you skip the manual
crate setup:

```bash
pocopine new hello-pine
cd hello-pine
```

The generated crate is a Rust library compiled to WebAssembly: its
`Cargo.toml` already depends on `pocopine` and sets
`crate-type = ["cdylib", "rlib"]`, and `src/lib.rs` registers the
starter components. (Want the framework's agent guides for your editor?
`pocopine new --skills`, or `pocopine skills install` later.)

## 2. Anatomy of a component

A component is a Rust struct plus a `.poco` template file. The struct
holds state; `#[handlers]` methods mutate it. The scaffold ships a few;
here's the shape of one — add your own the same way:

```rust
// src/lib.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub n: u32,
}

#[handlers]
impl Counter {
    pub fn bump(&mut self) {
        self.n += 1;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<Counter>().run();
}
```

```poco
<!-- src/Counter.poco -->
<button @click="bump">
  clicked <strong pp-text="n"></strong> times
</button>
```

By default `#[component]` looks for a template named after the struct
(`Counter.poco`) in the same directory as the `.rs` file.

The scaffold's `index.html` already has a `pp-app` root — `App::run()`
scans for that attribute and mounts all registered components it finds
inside it. To add a component, register it in `main()` and drop its tag
in:

```html
<!-- index.html -->
<!doctype html>
<html>
<body>
  <div pp-app>
    <counter></counter>
  </div>
  <script type="module">
    import init from "/pkg/hello_pine.js";
    init();
  </script>
</body>
</html>
```

After a build the script reference reads `/pkg/hello_pine.<hash>.js` —
`pocopine build` content-hashes the JS + wasm pair (one hash, the
wasm's) and rewrites this line, so browsers and CDNs can cache the
bundle forever (`immutable`) while `index.html` itself always
revalidates. Leave the hashed name alone; every build keeps it current.

## 3. Run it

`just dev` (which wraps `pocopine dev`) builds the wasm bundle, serves it
on a local port, and rebuilds on save.

```bash
just dev
# → listening on http://127.0.0.1:5243
```

Ship a release build with `pocopine build --release`, then deploy with
`pocopine deploy`.

## 4. Editor support

Install the **Poco LSP** extension for `.poco` syntax highlighting, completion,
diagnostics, hover, and goto-definition:

- **VS Code** — the [Marketplace](https://marketplace.visualstudio.com/items?itemName=pocopine.vscode-poco), or from a terminal:
  ```bash
  code --install-extension pocopine.vscode-poco
  ```
- **VSCodium / Cursor / Windsurf** — [Open VSX](https://open-vsx.org/extension/pocopine/vscode-poco)

With the `pocopine` CLI installed, the extension automatically runs the
framework's own `pocopine lsp` server, so diagnostics match the compiler;
without it, a bundled server is used. Run `pocopine doctor` to check both your
toolchain and whether the extension is installed.

## Next steps

- **[Components](../guides/components/README.md)** — structure and state, in depth.
- **[Templates](../guides/poco/README.md)** — the full `.poco` directive set.
- **[Tutorials](../tutorials/issue-tracker-sync.md)** — build a real feature end to end.
