# Native code editor validation

Implementation: RFC-124, branch `feat/pine-code-editor`. This report separates
automated evidence from manual release checks. The code editor is implemented;
release acceptance remains pending the manual checks below.

## Automated checks

- Pure Rust: 24 tests covering Unicode offsets, normalized lines, change
  inversion/mapping, revisions, bounded history, commands, search and lexing.
- Firefox WASM library tests: 16 passing, including composition, injected
  rendering failures, reentrancy and live framework removal paths.
- Framework pre-detachment hook: 3 Firefox tests passing; conditional,
  keyed-row, compiled bulk-clear, dynamic component, owned-mount and canceled
  leave behavior are additionally covered by the editor's integration tests.
- `pine-code` builds without `view` for WASM and on the host; with `view`,
  WASM all-target Clippy passes with warnings denied.
- Full workspace formatting passes.

Release integration: **66 / 66 passing** (22 each in Chromium 148.0.7778.96,
Firefox 150.0.2, and WebKit 26.4). Cases cover native typing/history, Unicode
and backwards selection, clipboard/drop, readonly/disabled navigation,
composition guards and finalization, interrupted-draft policy, recovery,
line identity, highlighting fallback, dynamic accessible names and styling.
The release CLI build passes. The example bundle used for this run is
`code_editor_example_bg.3f817a15.wasm`.

On this Ubuntu 25.10 host, Playwright WebKit uses isolated Ubuntu 24.04
compatibility libraries extracted from a disposable container. No host
package versions were replaced. Results describe headless Linux browser
engines; they do not substitute for testing Safari on macOS or iOS.

## Measured performance and default limits

Recorded 2026-09-10 on an Intel Core Ultra 9 275HX (24 logical CPUs,
65,743,953,920 bytes RAM), Linux 6.17.0-41-generic, Chromium 148.0.7778.96.
Build: `pocopine build --path examples/code-editor --release`, Rust
1.95.0-nightly (2026-02-20), workspace `opt-level = "z"`, LTO enabled,
one codegen unit, wasm-opt enabled. The complete example WASM is 798,192
bytes, 338,024 bytes gzip, 258,670 bytes Brotli; this includes the demo and
its Pine components, not just the editor library.

Each dataset received 30 warmup edits followed by 200 native one-character
replacements rotating between beginning, middle and end. Replacements keep
exact-limit documents at their original size. Tests run serially without
trace/screenshot recording. Chromium EventTiming includes the next paint,
rounds to 8 ms, and omits durations below 16 ms; missing entries are assigned
16 ms in the reported estimate. Separate synchronous input and next-rAF
measurements are included in the raw data. A rAF callback is pre-paint and is
not represented as a physical display measurement.

| Dataset | Presentation | Synchronous input p95 | Event-to-paint p95 estimate | Gate |
| --- | --- | ---: | ---: | ---: |
| 10 KiB | Rust highlighting, 1,160 spans | 3.2 ms | 24 ms | 32 ms |
| 64 KiB | Rust highlighting, 7,445 spans | 19.6 ms | 32 ms | 32 ms |
| 100 KiB | Plain, span budget | 8.2 ms | 24 ms | 32 ms |
| 256 KiB | Plain, span budget | 20.2 ms | 32 ms | 50 ms |
| 10,000 lines | Plain, span budget | 28.3 ms | 40 ms | 50 ms |
| Single 32 KiB line | Plain, line highlight budget | 4.6 ms | 16 ms | 32 ms |

All p95 gates passed, and the 100 KiB run had no editing long task above
50 ms. The 64 KiB run had six input-overlapping long tasks and a 184 ms
maximum EventTiming duration: passing p95 does not guarantee every keystroke
avoids a stall on this shared reference machine. The raw report retains these
outliers. These are measured desktop bounds, not guarantees on mobile or
slower hardware.

Every ordinary edit read one logical line; there were zero full-DOM reads in
these typing runs. Retained history was 34,800 bytes after each measured edit
sequence. Paste, replace-all, undo and scrolling were exercised and timed
separately; results and DOM counts are in the raw artifact.

The [initial measurement](validation/initial-performance.json) missed gates
with 11,635 spans at 100 KiB (40 ms p95) and a single 256 KiB line (80 ms p95).
Accordingly, defaults are **256 KiB total, 10,000 lines, 32 KiB per logical
line, and 8,000 syntax spans**. Longer logical lines are rejected with
`SizeLimit`, preserving committed text/history. Excess syntax spans switch
the complete document to plain presentation until load or language change.
Custom document limits are accepted but extend beyond the measured envelope.

The [100-cycle measurements](validation/mount-cycles.json) passed. After
warmup, the two-editor page held 30 tracked editor listeners, two observers,
zero pending frames, 1,002 DOM nodes and 1,507,328 bytes of WASM memory.
Those counts stayed fixed from cycle 20 through 100. Collected JS heap grew
from 2,716,400 to 2,862,980 bytes over that interval, with growth tapering;
this is not a proof that every browser allocation is leak-free.

Raw artifacts: [final benchmark](validation/performance.json),
[initial benchmark](validation/initial-performance.json),
[mount cycles](validation/mount-cycles.json).

## Workspace-wide blockers

`cargo clippy --workspace --target wasm32-unknown-unknown` and the matching
workspace build fail because `mio` does not support this target with networking
enabled. The unchanged main checkout reproduces the Clippy failure with a
fresh target directory. This is distinct from the passing scoped editor WASM
checks.

`cargo test --workspace` did not complete. A run with socket access failed
compiling `pocopine-live` at `src/lib.rs:1820`, with incompatible `http` type
identities at `RequestContext::from_parts`. A preceding run also reported
missing compiled collaboration/realtime crates. These broad host failures have
not been attributed to a source regression in this change; a clean full
workspace host run remains outstanding.

## Manual release checks still required

Synthetic composition tests validate event ordering, ownership, selection and
undo contracts. They do not establish real platform IME or mobile support.
Before declaring those supported, record results for:

- Japanese and Chinese IME, Korean input, dead keys and rapid composition
  restart/cancel using actual operating-system input methods.
- Android Chrome and iOS Safari touch selection, clipboard, composition and
  read-only keyboard behavior.
- VoiceOver/Safari and a Windows screen-reader/browser combination: names,
  multiline editing, selection, read-only/disabled states and Tab escape.
- Human keyboard/caret inspection under zoom, font changes and forced colors.

The automated tests exercise equivalent DOM/keyboard contracts where possible.
The release status stays pending until these manual results are available.
