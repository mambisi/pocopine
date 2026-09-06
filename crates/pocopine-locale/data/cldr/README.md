# Unicode CLDR snapshot

Source: <https://github.com/unicode-org/cldr-json/tree/48.2.0>

- `cldr-json/cldr-core/supplemental/plurals.json`
- `cldr-json/cldr-core/supplemental/parentLocales.json`
- Upstream `LICENSE` (Unicode License V3).

These source files are build/maintenance inputs, not browser assets. Generate
the checked-in Rust table with `python3 tools/gen-locale-cldr.py`; check drift
with `python3 tools/gen-locale-cldr.py --check`. No network is used by generation.

To update, replace this snapshot from a pinned Unicode release, update
`CLDR_VERSION`, regenerate, and run `cargo test -p pocopine-locale`, including the
ICU4X oracle. Locale maintainers review source and generated diffs together;
do not silently replace the table with rules from the machine's browser/ICU.
