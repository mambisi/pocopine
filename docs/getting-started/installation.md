---
title: "Installation"
description: "Install the pocopine CLI and verify your toolchain."
---

# Installation

The `pocopine` CLI handles building, serving, and hot-reload — one
install covers all three.

```bash
cargo install pocopine-cli
```

From a source checkout of the repository, use the helper script:

```bash
./install.sh
pocopine doctor --path .
```

`pocopine doctor` checks that the toolchain it needs (Rust, the
`wasm32-unknown-unknown` target, and `wasm-pack`) is present and
reports anything missing.

## Pinning tools per project

If a project needs wrappers or pinned tool paths, add a local
`.pocopine.toml`. Pocopine reads this file instead of guessing from
global tools, and never shells out through npm scripts:

```toml
[tools]
cargo = { command = "cargo", args = ["+stable"] }
rustc = { command = "rustc", args = ["+stable"] }
wasm-pack = "/opt/tools/wasm-pack"
package-manager = "pnpm"
node = "node"
tailwindcss = "tailwindcss"
```

Next: **[Quickstart](./quickstart.md)**.
