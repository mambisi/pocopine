# Native autocomplete and composition validation

Recorded 2026-09-12 on branch `feat/pine-code-editor`. The rich-text fixes are
implemented and pass the automated checks below. Physical phone keyboard
acceptance remains pending.

## Behavior corrected

Autocomplete and IME can change the DOM even when the editor cannot cancel
`beforeinput`, and some corrections omit that event entirely. This is
documented by [MDN's beforeinput reference](https://developer.mozilla.org/en-US/docs/Web/API/Element/beforeinput_event).
Previously the rich-text editor had no `input` readback path, and replacement
text without target ranges was inserted at the caret. The rendered text and
Rust document could consequently disagree or contain a duplicated word.

The editor now reads the affected textblock after native input and commits its
text changes through a Rust transaction. Missing replacement ranges or payloads
wait for this readback. Composition owns its DOM until completion, including
when a render was queued before composition began. Reconciliation compares
actual DOM text with the committed model so it does not apply a native edit
twice. A final input delivered after `compositionend` in the same browser event
turn shares that composition's undo event.

Readback preserves Unicode caret positions, existing marks, and inline atoms
such as hard breaks. An incompatible native structure or a concurrent document
replacement restores the authoritative document. A canceled composition also
resumes deferred rendering. This is textblock reconciliation, not a general
DOM-to-document importer; arbitrary native structural edits are not accepted.

## Automated evidence

The demo was built and served through the Pocopine CLI in the debug profile.
All browser checks used `richtext.e241088b.js` / `richtext_bg.e241088b.wasm`.

| Project | Actual engine and version | Native-input checks |
| --- | --- | ---: |
| Desktop Chromium | Chromium 148.0.7778.96 | 11 / 11 |
| Desktop Firefox | Firefox 150.0.2 | 11 / 11 |
| Desktop WebKit | WebKit 26.4 | 11 / 11 |
| Pixel 7 emulation | Chromium 148.0.7778.96 | 11 / 11 |
| iPhone 13 emulation | WebKit 26.4 | 11 / 11 |

All **55 checks passed**, with no skipped or flaky cases. The suite checks the
actual engine against each project configuration and records its version and
loaded bundle. During validation, the shared configuration's global Chromium
override was found to override Firefox/WebKit device presets. That override
was removed; the table records the subsequent run using verified engines.

The 11 scenarios cover non-cancelable corrections, missing `beforeinput`,
missing target ranges, missing payloads, cancelable targeted replacements,
final input before and after `compositionend`, deletion of all composing text,
marks and inline breaks, external document replacement during composition,
and recovery from unsupported native structure. Assertions check the Rust
document against the DOM, following typing/caret placement, and undo/redo.
Tests mutate the mounted editor's DOM between synthetic native events.

Additional checks passed:

- 27 existing rich-text browser smoke tests in Chromium.
- 14 Firefox WASM library tests with the `view` feature.
- 488 host tests across `pine-richtext`, `pine-richtext-extensions`, and
  `pine-richtext-collab`, including composition undo grouping.
- `pine-richtext` WASM all-target Clippy with warnings denied.
- Full workspace formatting, WASM Clippy, and WASM build. Workspace Clippy
  still reports existing warnings.

The compact [browser result artifact](validation/native-input.json) records
individual cases, projects, engines, versions, and run times.

## Repeating the checks

From the worktree root, with the Pocopine CLI and Playwright browsers available:

```sh
pocopine run --path examples/richtext --port 5245
```

In another terminal:

```sh
RICHTEXT_INPUT_MATRIX=1 npx playwright test richtext-native-input.spec.mjs --workers=3
RICHTEXT_INPUT_MATRIX=1 npx playwright test 'richtext\.spec\.mjs$' --project=richtext-chromium --workers=2
cargo test -p pine-richtext -p pine-richtext-extensions -p pine-richtext-collab
wasm-pack test --firefox --headless crates/pine-richtext --features view
cargo clippy -p pine-richtext --features view --target wasm32-unknown-unknown --all-targets -- -D warnings
```

This host used the existing isolated Playwright browser installation at
`tmp/pine-code-browsers` in the main checkout, selected with
`PLAYWRIGHT_BROWSERS_PATH`. WebKit uses isolated Ubuntu 24.04 compatibility
libraries on this Ubuntu 25.10 host. No host packages were replaced.

## Remaining verification

Mobile emulation does not run Gboard, the iOS keyboard, or a real operating
system IME. The original reporter's phone, browser, keyboard, and exact
autocomplete sequence have not been identified. Verify on an actual Android
phone and iPhone: accept and cancel suggestions, replace a previous word,
compose Chinese/Japanese/Korean text, move the caret, continue typing, undo,
and export or save. Confirm the saved text agrees with the visible text and
that formatting and caret placement survive. Touch selection and screen-reader
acceptance also remain open.

The full host `cargo test --workspace` attempt on 2026-09-12 is blocked while
compiling `pocopine-live/src/lib.rs:1820`: incompatible `http` type identities
at `RequestContext::from_parts`. No complete host workspace pass is claimed.
The separate `mio` WASM blocker has been resolved by host-gating CLI, asset,
and cloud-storage implementations and dependencies; see the
[workspace validation report](../code-editor/VALIDATION.md#workspace-wide-blockers).
