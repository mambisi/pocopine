#!/usr/bin/env sh
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found on PATH" >&2
  echo "install Rust from https://rustup.rs/ and rerun ./install.sh" >&2
  exit 1
fi

echo "==> installing pocopine CLI from $ROOT"
cargo install --locked --path "$ROOT/crates/pocopine-cli" --bin pocopine --force

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

# Optional 'pp' shorthand for 'pocopine', handled at the OS level: a symlink on
# Unix/macOS, a pp.cmd shim under Git-Bash / MSYS / Cygwin on Windows. Set
# POCOPINE_PP_ALIAS=1 (or 0) to skip the prompt in non-interactive installs.
BIN_DIR="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
case "$(uname -s 2>/dev/null)" in
  MINGW* | MSYS* | CYGWIN*) PP_OS=windows ;;
  *) PP_OS=unix ;;
esac

PP_ALIAS="${POCOPINE_PP_ALIAS:-}"
if [ -z "$PP_ALIAS" ] && [ -t 0 ]; then
  printf "==> add a 'pp' shorthand for 'pocopine'? [y/N] "
  read -r _reply || _reply=""
  case "$_reply" in [Yy]*) PP_ALIAS=1 ;; *) PP_ALIAS=0 ;; esac
fi

if [ "${PP_ALIAS:-0}" = 1 ]; then
  mkdir -p "$BIN_DIR"
  if [ "$PP_OS" = windows ]; then
    printf '@"%%~dp0pocopine.exe" %%*\r\n' > "$BIN_DIR/pp.cmd"
    echo "    wrote $BIN_DIR/pp.cmd  (pp -> pocopine)"
  else
    ln -sf pocopine "$BIN_DIR/pp"
    echo "    linked $BIN_DIR/pp -> pocopine"
  fi
fi

echo "==> done"
echo "    run: pocopine doctor --path ."
if [ "${PP_ALIAS:-0}" = 1 ]; then
  echo "    or:  pp doctor --path ."
fi
