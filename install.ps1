#!/usr/bin/env pwsh
# Windows installer for the pocopine CLI. Mirrors install.sh: installs the
# `pocopine` binary, ensures the wasm target, and optionally adds a `pp`
# shorthand (a pp.cmd shim next to pocopine.exe). Set $env:POCOPINE_PP_ALIAS
# to 1/0 to skip the prompt in non-interactive installs.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSCommandPath

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found on PATH. Install Rust from https://rustup.rs/ and re-run."
    exit 1
}

Write-Host "==> installing pocopine CLI from $root"
cargo install --locked --path "$root/crates/pocopine-cli" --bin pocopine --force

if (Get-Command rustup -ErrorAction SilentlyContinue) {
    Write-Host "==> ensuring wasm32-unknown-unknown target"
    rustup target add wasm32-unknown-unknown
}
if (-not (Get-Command wasm-pack -ErrorAction SilentlyContinue)) {
    Write-Host "==> wasm-pack not found; install it with: cargo install wasm-pack"
}

# Optional 'pp' shorthand — a pp.cmd shim that forwards to pocopine.exe.
$binDir =
    if ($env:CARGO_INSTALL_ROOT) { Join-Path $env:CARGO_INSTALL_ROOT "bin" }
    elseif ($env:CARGO_HOME) { Join-Path $env:CARGO_HOME "bin" }
    else { Join-Path $env:USERPROFILE ".cargo\bin" }

$want = $env:POCOPINE_PP_ALIAS
if (-not $want) {
    $ans = Read-Host "==> add a 'pp' shorthand for 'pocopine'? [y/N]"
    if ($ans -match '^(y|yes)$') { $want = "1" } else { $want = "0" }
}
if ($want -eq "1") {
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    $ppCmd = Join-Path $binDir "pp.cmd"
    # %~dp0 = the directory of pp.cmd (the cargo bin dir); %* forwards all args.
    Set-Content -Path $ppCmd -Value '@"%~dp0pocopine.exe" %*' -Encoding Ascii
    Write-Host "    wrote $ppCmd  (pp -> pocopine)"
}

Write-Host "==> done. run: pocopine doctor --path ."
if ($want -eq "1") { Write-Host "    or:  pp doctor --path ." }
