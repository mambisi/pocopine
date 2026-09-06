#!/usr/bin/env python3
"""Verify generated locale APIs as real Rust/wasm consumers, including pruning."""
from pathlib import Path
import gzip
import os
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "crates/pocopine-locale/tests/fixtures/typed-api"
TARGET = ROOT / "target"
MANIFEST = FIXTURE / "Cargo.toml"


def cargo(command, *args, fail_contains=()):
    env = os.environ.copy()
    env["CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER"] = "wasm-bindgen-test-runner"
    env["CARGO_TARGET_DIR"] = str(TARGET)
    cmd = ["cargo", command, "--manifest-path", str(MANIFEST), "--offline", *([] if command == "test" else ["--quiet"]), *args]
    print(f"locale codegen: {command} {' '.join(args)}", flush=True)
    result = subprocess.run(cmd, cwd=ROOT, env=env, text=True, capture_output=True)
    if fail_contains:
        if result.returncode == 0 or any(part not in result.stderr for part in fail_contains):
            raise SystemExit(f"expected targeted compile failure {fail_contains!r}\n{result.stdout}\n{result.stderr}")
    elif result.returncode:
        raise SystemExit(f"{result.stdout}\n{result.stderr}")
    elif result.stdout:
        print(result.stdout.strip(), flush=True)
    return result.stdout


def main():
    # The fixture is an isolated workspace so compile-fail features never enter
    # normal workspace builds. Seed its ignored lockfile from the real workspace
    # to test the exact dependency versions under review, not newer registry data.
    shutil.copyfile(ROOT / "Cargo.lock", FIXTURE / "Cargo.lock")
    cargo("test")
    cargo("test", "--features", "catalog-update")
    cargo("test", "--features", "server-integration")
    cargo("check", "--features", "bad-argument", fail_contains=("mismatched types", "PluralArg", "not a number"))
    cargo("check", "--features", "missing-key", fail_contains=("cannot find function `unused`",))
    cargo("test", "--target", "wasm32-unknown-unknown", "--lib")
    cargo("check", "--target", "wasm32-unknown-unknown", "--features", "host-key-in-browser", fail_contains=("could not find `auth` in `t`",))
    cargo("build", "--target", "wasm32-unknown-unknown", "--release")
    wasm = (TARGET / "wasm32-unknown-unknown/release/locale_typed_api_contract.wasm").read_bytes()
    for sentinel in [b"BROWSER_COPY_SENTINEL", b"HOST_COPY_SENTINEL", b"UNUSED_COPY_SENTINEL", b"cart.items", b"cart.title", b"auth.denied", b"common.bad_request", b"Invalid request.", b"Something went wrong."]:
        if sentinel in wasm:
            raise SystemExit(f"locale key/message bytes leaked into release wasm: {sentinel!r}")
    tree = cargo("tree", "--target", "wasm32-unknown-unknown", "--edges", "normal")
    if any(name in tree for name in ("icu_datetime", "icu_decimal", "icu_experimental", "jiff v")):
        raise SystemExit("default browser locale runtime acquired ICU formatting or timezone data")
    print(f"locale codegen: verified; standalone release wasm {len(wasm)} bytes, gzip {len(gzip.compress(wasm, mtime=0))} bytes", flush=True)


if __name__ == "__main__":
    main()
