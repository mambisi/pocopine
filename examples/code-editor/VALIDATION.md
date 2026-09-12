# Native code editor validation

Implementation: RFC-124, branch `feat/pine-code-editor`. This report separates
automated evidence from manual release checks. The code editor is implemented;
release acceptance remains pending the manual checks below.

After rebasing onto `origin/main` at `a3c55fbb` on 2026-09-12, the 28 host
tests, 84 browser checks, and full workspace formatting/WASM Clippy/build
checks were rerun. The Firefox WASM library tests and performance measurements
below remain evidence from the preceding implementation runs.

## Automated checks

- Pure Rust: 28 tests covering Unicode offsets, normalized lines, change
  inversion/mapping, revisions, bounded history, commands, search, lexing,
  language registration, all four grammar packages, incremental parsing,
  custom queries and parser-budget recovery.
- Firefox WASM library tests: 20 passing, including composition, injected
  rendering failures, reentrancy, live framework removal paths, stale syntax
  responses and worker-request coalescing.
- Framework pre-detachment hook: 3 Firefox tests passing; conditional,
  keyed-row, compiled bulk-clear, dynamic component, owned-mount and canceled
  leave behavior are additionally covered by the editor's integration tests.
- `pine-code` builds without `view` for WASM and on the host; with `view`,
  WASM all-target Clippy passes with warnings denied.
- Full workspace formatting passes.
- The CLI's focused WASM-header discovery test passes, including dependency
  reachability so an unrelated workspace package cannot supply the headers.

Release integration, refreshed 2026-09-12: **84 / 84 passing** (28 each in Chromium 148.0.7778.96,
Firefox 150.0.2, and WebKit 26.4). Cases cover native typing/history, Unicode
and backwards selection, clipboard/drop, readonly/disabled navigation,
composition guards and finalization, interrupted-draft policy, recovery,
line identity, highlighting fallback, dynamic accessible names and styling.
The real module worker is exercised with Rust, JSON, Python, JavaScript and a
custom configuration grammar. Tests also cover language changes and stale
results, worker startup failure and recovery, worker disposal/remounting, and
the native language picker with language-specific indentation.
The release CLI build passes. The example bundle used for this run is
`code_editor_example_bg.a329a228.wasm`.

The refreshed run verifies each actual browser engine and records its version
in the [engine-check artifact](validation/browser-engines.json). Validation
found that a global `browserName: 'chromium'` setting overrode the other device
presets; the earlier project names alone did not establish Firefox/WebKit
coverage. Removing that override and explicitly supplying Firefox's synthetic
clipboard fixture payload produced the verified 84/84 run above. The Chromium
performance measurements below are unaffected by the project override.

On this Ubuntu 25.10 host, Playwright WebKit uses isolated Ubuntu 24.04
compatibility libraries extracted from a disposable container. No host
package versions were replaced. Results describe headless Linux browser
engines; they do not substitute for testing Safari on macOS or iOS.

## Measured performance and default limits

These measurements describe the preceding
`code_editor_example_bg.5d51e60f.wasm` release bundle; they were not repeated
as part of rebase verification.

Recorded 2026-09-10 on an Intel Core Ultra 9 275HX (24 logical CPUs,
65,743,953,920 bytes RAM), Linux 6.17.0-41-generic, Chromium 148.0.7778.96.
Build: `pocopine build --path examples/code-editor --release`, Rust
1.95.0-nightly (2026-02-20), workspace `opt-level = "z"`, LTO enabled,
one codegen unit, wasm-opt enabled. The complete example WASM is 3,317,456
bytes, 1,035,822 bytes gzip, 723,431 bytes Brotli. This includes all four
grammar packages, the demo and its Pine components, including the language
picker. The preceding handwritten-tokenizer example was 798,771 bytes raw.
Applications can enable only their required grammar features. Each active
syntax worker initializes a separate instance of the application WASM module.

Each dataset received 30 warmup edits followed by 200 native one-character
replacements rotating between beginning, middle and end. Replacements keep
exact-limit documents at their original size. Tests run serially without
trace/screenshot recording. Chromium EventTiming includes the next paint,
rounds to 8 ms, and omits durations below 16 ms; missing entries are assigned
16 ms in the reported estimate. Separate synchronous input and next-rAF
measurements are included in the raw data. A rAF callback is pre-paint and is
not represented as a physical display measurement. These timings measure input
responsiveness, not the time until asynchronous syntax coloring finishes.

| Dataset | Presentation | Synchronous input p95 | Event-to-paint p95 estimate | Gate |
| --- | --- | ---: | ---: | ---: |
| 10 KiB | Rust highlighting, 928 spans | 2.9 ms | 24 ms | 32 ms |
| 64 KiB | Rust highlighting, 5,956 spans | 16.2 ms | 24 ms | 32 ms |
| 100 KiB | Plain, span budget | 8.9 ms | 24 ms | 32 ms |
| 256 KiB | Plain, span budget | 22.4 ms | 32 ms | 50 ms |
| 10,000 lines | Plain, span budget | 29.2 ms | 40 ms | 50 ms |
| Single 32 KiB line | Plain, line highlight budget | 5.1 ms | 16 ms | 32 ms |

All p95 gates passed. None of the six datasets recorded an input-overlapping
long task above 50 ms. The 64 KiB run had a maximum EventTiming duration of
32 ms. Passing these gates does not guarantee every keystroke avoids a stall
on this shared reference machine or establish performance on mobile or slower
hardware.

Every ordinary edit read one logical line; there were zero full-DOM reads in
these typing runs. Retained history was 34,800 bytes after each measured edit
sequence. Paste, replace-all, undo and scrolling were exercised and timed
separately; results and DOM counts are in the raw artifact. Bulk commands can
exceed the typing gates: replacing across 10,000 lines took 170.4 ms and its
undo took 97.5 ms in this run.

The [initial measurement](validation/initial-performance.json) missed gates
with 11,635 spans at 100 KiB (40 ms p95) and a single 256 KiB line (80 ms p95).
Accordingly, defaults are **256 KiB total, 10,000 lines, 32 KiB per logical
line, and 8,000 syntax spans**. Longer logical lines are rejected with
`SizeLimit`, preserving committed text/history. Excess syntax spans switch
the complete document to plain presentation until load or language change.
Custom document limits are accepted but extend beyond the measured envelope.

The [100-cycle measurements](validation/mount-cycles.json) passed. After
warmup, the two-editor page held two workers, 30 tracked editor listeners,
two observers, zero pending frames and 1,219 DOM nodes. Those counts stayed
fixed from cycle 20 through 100. Main-thread WASM memory grew from 3,801,088
bytes at cycle 20 to 3,866,624 at cycle 40 and stayed fixed through cycle 100.
Collected main-thread JS heap grew from 2,951,420 to 3,106,752 bytes between
cycles 20 and 100, with growth tapering. Worker heaps are not included in
those memory measurements; stable worker counts and disposal checks do not
prove that every browser allocation is leak-free.

Raw artifacts: [final benchmark](validation/performance.json),
[initial benchmark](validation/initial-performance.json),
[mount cycles](validation/mount-cycles.json).

## Workspace-wide blockers

Resolved on 2026-09-12: `cargo clippy --workspace --target
wasm32-unknown-unknown` and the matching workspace build both pass. The CLI,
asset client and S3/Azure/GCS storage adapters now gate their implementations
and dependencies to host targets. `mio` no longer appears in the workspace's
WASM dependency graph. CI includes these five crates in its WASM check, and
253 affected host tests passed, including the storage emulator integrations.
Clippy still reports existing workspace warnings.

The PR's initial CI run exposed additional configuration gaps: raw Cargo
needed the Tree-sitter package's WASM headers, the CLI fingerprint integration
test needed a host-only gate, and a WASM-only `Cell` import caused a host
warning. These are corrected. Both WASM Clippy commands from CI now pass
locally with `RUSTFLAGS="-D warnings"`, `--all-targets`, and Clippy warnings
denied. The full workspace formatting, WASM Clippy, and WASM build commands
also pass after these corrections.

The exact CI host command, `cargo test --workspace --exclude pine --exclude
observability-smoke --exclude pocopine-sync-query-macros --tests`, passes
with **3,694 tests**, using `RUSTFLAGS="-D warnings"` and an isolated Cargo
target directory. CI's separate sync Query macro and storage image-compression
steps also pass. The full run uncovered Render OpenAPI checksum drift; the
adapter's live request/response contracts were reviewed before refreshing the
pin, and the dedicated drift test passes with offline skipping disabled.

`cargo test --workspace` did not complete. After rebasing onto `a3c55fbb` on
2026-09-12, it failed with incompatible compiled `pocopine_core` identities and
a missing `pocopine_server` dependency. Clearing build artifacts for the core,
live, server and sync packages and retrying still reported colliding
`libpocopine_core` output filenames, then failed to load `pocopine_sync` from
`pocopine-sync-query/src/client.rs:20`. Earlier attempts failed at
`pocopine-live/src/lib.rs:1820` with incompatible `http` type identities.
After the CI repair, repeating the broader command with warnings denied in
the same isolated target directory used by the passing CI command failed
while linking `pocopine-auth-jwt`'s `jwks_resolver_wiremock` test, with undefined
symbols from dependencies including `tracing` and `h2`. No passing run of
the broader command is claimed; CI's package selection and isolated macro
job pass as recorded above.

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

## Keyword-span regression

Typing `let fu` slowly could extend the existing keyword span to include
` fu`. The tokenizer still produced the same keyword range for `let`, so
the renderer's cached-range shortcut skipped repairing the native DOM.
The renderer now verifies actual text/span boundaries before reusing that
cache. The new browser regression types character by character across frames
and checks both keyword/string styling and the resulting caret position.
All three engines pass with Tree-sitter worker output; the release benchmark
above also passes. Native DOM repair remains necessary regardless of which
parser produces the token ranges.
