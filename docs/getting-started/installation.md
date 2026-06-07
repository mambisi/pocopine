---
title: "Installation"
description: "Install the pocopine CLI and verify your toolchain."
---

# Installation

The `pocopine` CLI handles building, serving, and hot-reload — one
install covers all three. The quickest way is the install script, which
downloads a prebuilt binary for your platform — no Rust toolchain
required:

```bash
curl -fsSL https://pocopine.dev/install.sh | sh
```

On Windows, run the PowerShell installer instead:

```powershell
irm https://github.com/mambisi/pocopine/releases/latest/download/pocopine-cli-installer.ps1 | iex
```

Prefer to build from source with Cargo? Once the crate is published you
can also `cargo install pocopine-cli`. From a source checkout of the
repository, use the helper script instead — it installs the CLI from the
local crate, ensures the `wasm32-unknown-unknown` target is present, and
reminds you if `wasm-pack` is missing:

```bash
./install.sh
```

Then scaffold your first app:

```bash
pocopine new my-app
cd my-app
just dev
```

After installing, run `pocopine doctor` to verify the
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
