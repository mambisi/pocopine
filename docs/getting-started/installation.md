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

From a source checkout of the repository, use the helper script
instead — it installs the CLI from the local crate, ensures the
`wasm32-unknown-unknown` target is present, and reminds you if
`wasm-pack` is missing:

```bash
./install.sh
```

After either installation path, run `pocopine doctor` to verify the
tools the CLI depends on — `cargo`, `rustc`, and `wasm-pack`:

```bash
pocopine doctor --path .
```

It reports each check as `[ok]`, `[warn]`, or `[fail]`, and exits
non-zero if any failures are found. Pass `--strict` to also fail on
warnings.

## Pinning tools per project

If a project needs wrappers or pinned tool paths, add a
`.pocopine.toml` at the project root. The CLI reads this file on
every invocation and falls back to PATH when a key is absent:

```toml
[tools]
cargo = { command = "cargo", args = ["+stable"] }
rustc = { command = "rustc", args = ["+stable"] }
wasm-pack = "/opt/tools/wasm-pack"
package-manager = "pnpm"
node = "node"
tailwindcss = "tailwindcss"
```

Each value is either a plain string (the executable name or path) or
a `{ command, args }` table for prepending fixed arguments.

Next: **[Quickstart](./quickstart.md)**.
