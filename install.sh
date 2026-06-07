#!/usr/bin/env sh
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found on PATH" >&2
  echo "install Rust from https://rustup.rs/ and rerun ./install.sh" >&2
  exit 1
fi

echo "==> installing pocopine CLI from $ROOT (installs both 'pocopine' and the 'pp' shorthand)"
cargo install --locked --path "$ROOT/crates/pocopine-cli" --bins --force

if command -v rustup >/dev/null 2>&1; then
  echo "==> ensuring wasm32-unknown-unknown target"
  rustup target add wasm32-unknown-unknown
else
  echo "==> rustup not found; skipping wasm32 target installation"
fi

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "==> wasm-pack not found"
  echo "    install it with: cargo install wasm-pack"
fi

echo "==> done"
echo "    run: pocopine doctor --path .   (or the 'pp' shorthand: pp doctor)"
