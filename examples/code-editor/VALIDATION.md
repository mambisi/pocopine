# Native code editor validation

Implementation: RFC-124, branch `feat/pine-code-editor`. This report separates
automated evidence from manual release checks. The code editor is implemented;
release acceptance remains pending the manual checks below.

## Automated checks

- Pure Rust: 23 tests covering Unicode offsets, normalized lines, change
  inversion/mapping, revisions, bounded history, commands, search and lexing.
- Firefox WASM library tests: 16 passing, including composition, injected
  rendering failures, reentrancy and live framework removal paths.
- Framework pre-detachment hook: 3 Firefox tests passing; conditional,
  keyed-row, compiled bulk-clear, dynamic component, owned-mount and canceled
  leave behavior are additionally covered by the editor's integration tests.
- `pine-code` builds without `view` for WASM and on the host; with `view`,
  WASM all-target Clippy passes with warnings denied.
- Full workspace formatting passes.

The release browser run and performance measurements are recorded below after
executing the checked-in Playwright fixtures.

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
