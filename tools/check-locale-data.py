#!/usr/bin/env python3
"""Release audit of CLI-generated locale data and translation pruning.

Build the CLI first. Application builds deliberately use `pocopine build`.
The isolated copy lives below target; no example source/catalog is modified.
"""
from pathlib import Path
import gzip
import json
import os
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
CLI = Path(os.environ.get("POCOPINE_CLI", TARGET / "debug/pocopine")).resolve()
PROJECT = TARGET / "locale-data-audit"
TAGS = ["en", "fr", "ar", "th", "fa", "ja", "zh-Hant", "es-AR"]
USED = "USED_CATALOG_GROWTH_SENTINEL"
UNUSED = "UNUSED_CATALOG_GROWTH_SENTINEL"


def run(args, log, extra_env=None):
    with log.open("w") as output:
        result = subprocess.run(args, cwd=ROOT, env={**os.environ,
            "CARGO_TARGET_DIR": str(TARGET), **(extra_env or {})}, stdout=output, stderr=subprocess.STDOUT)
    if result.returncode:
        raise SystemExit(f"command failed; see {log}\n{log.read_text()[-6000:]}")


def build(label, count, strict=False, growth=None):
    catalog = json.loads((ROOT / "examples/locale/locales/en.json").read_text())
    if growth == "used":
        catalog["common.welcome"] = f"Hello {{name}} " + (USED + " ") * 1000
    if growth == "unused":
        catalog.update({f"common.unused_{i}": (UNUSED + " ") * 100 for i in range(100)})
    for tag in TAGS[:count]:
        # Identical text isolates configured locale data from wording changes.
        (PROJECT / f"locales/{tag}.json").write_text(json.dumps(catalog, indent=2))
    (PROJECT / "pocopine.toml").write_text(
        '[locale]\ndefault = "en"\nrouting = "prefix-except-default"\n'
        f'locales = {json.dumps(TAGS[:count])}\nstrict_parity = {str(strict).lower()}\n')
    run([str(CLI), "build", "--path", str(PROJECT), "--release", "--no-bins",
         "--no-stylekit"], PROJECT / f"{label}.log")
    info = json.loads((PROJECT / "target/pocopine/locale/build.json").read_text())
    data = Path(info["runtime_data"])
    # Inspect the current emitted HTML, not an assumed bundle name.
    import re
    html = (PROJECT / "pkg/index.html").read_text()
    name = re.search(r'locale_demo\.([a-f0-9]+)\.js', html).group(1)
    wasm = (PROJECT / f"pkg/locale_demo_bg.{name}.wasm").read_bytes()
    (PROJECT / "measurements").mkdir(exist_ok=True)
    (PROJECT / "measurements" / f"{label}.wasm").write_bytes(wasm)
    for sentinel in [USED, UNUSED, "common.welcome", "common.unused_0",
                     "Hello {name}, welcome to Pocopine"]:
        assert sentinel.encode() not in wasm, f"{label}: catalog bytes leaked: {sentinel}"
    if strict:
        assert (b"janvier" in wasm) == (count > 1), "unconfigured French formatting data retained or configured data missing"
    public = b"".join((PROJECT / "pkg" / url.removeprefix("/pkg/")).read_bytes()
                      for url in info["manifest"]["catalogs"].values())
    assert UNUSED.encode() not in public, "unused message leaked into browser assets"
    if growth == "used":
        assert USED.encode() in public, "growth fixture did not retain the used message"
    result = dict(case=label, locales=count, strict=strict, wasm=len(wasm),
                  gzip=len(gzip.compress(wasm, mtime=0)),
                  plural_bytes=(data / "plural.rs").stat().st_size,
                  icu_bytes=(data / "formatting.blob").stat().st_size,
                  data_id=data.name, public_bytes=len(public))
    print(json.dumps(result), flush=True)
    return result


def check_runtime(data):
    env = {"POCOPINE_LOCALE_DATA_DIR": str(data),
           "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER": "wasm-bindgen-test-runner"}
    for label, flags in [("host", []), ("intl", ["--target", "wasm32-unknown-unknown"]),
                         ("strict", ["--target", "wasm32-unknown-unknown", "--features", "strict-parity"])]:
        run(["cargo", "test", "-p", "pocopine-locale", "--offline", "--test",
             "configured_data", *flags], PROJECT / f"configured-{label}.log", env)
        print(f"configured runtime: {label} passed", flush=True)


def main():
    PROJECT.mkdir(parents=True, exist_ok=True)
    source = ROOT / "examples/locale"
    shutil.copytree(source / "src", PROJECT / "src", dirs_exist_ok=True)
    (PROJECT / "locales").mkdir(exist_ok=True)
    for file in source.glob("*.html"):
        shutil.copyfile(file, PROJECT / file.name)
    manifest = (source / "Cargo.toml").read_text().replace(
        '../../crates/', (ROOT / 'crates').as_posix() + '/')
    (PROJECT / "Cargo.toml").write_text(manifest)
    shutil.copyfile(ROOT / "Cargo.lock", PROJECT / "Cargo.lock")
    rows = [build(f"intl-{n}", n) for n in [1, 3, 8]]
    rows += [build("intl-3-used", 3, growth="used"),
             build("intl-3-unused", 3, growth="unused")]
    baseline = rows[1]
    for row in rows[3:]:
        assert row["data_id"] == baseline["data_id"], "copy change regenerated runtime data"
        assert row["wasm"] == baseline["wasm"], "catalog growth changed wasm size"
    assert rows[3]["public_bytes"] > baseline["public_bytes"] + 50000
    assert rows[4]["public_bytes"] == baseline["public_bytes"]
    # Locale identity and plural predicates are small, but not literally free.
    assert abs(rows[2]["wasm"] - rows[0]["wasm"]) < 32000
    tree = subprocess.check_output(["cargo", "tree", "--manifest-path",
        str(PROJECT / "Cargo.toml"), "--target", "wasm32-unknown-unknown",
        "--edges", "normal", "--offline"], text=True)
    assert not any(name in tree for name in ["icu_datetime", "icu_decimal", "jiff v"])
    rows += [build(f"strict-{n}", n, strict=True) for n in [1, 3, 8]]
    assert rows[-1]["icu_bytes"] > rows[-3]["icu_bytes"]
    # A switch to all-locale baked constructors produces a much larger jump.
    assert abs(rows[-1]["wasm"] - rows[-3]["wasm"]) < 100000
    (PROJECT / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    check_runtime(PROJECT / "target/pocopine/locale/data" / baseline["data_id"])
    print(f"locale data audit passed; results: {PROJECT / 'results.json'}", flush=True)


if __name__ == "__main__":
    main()
