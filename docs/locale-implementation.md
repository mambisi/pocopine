# RFC-120 implementation

Working branch: `feat/locale`. The specification is
[RFC-120](../rfcs/rfc-120-i18n.md), including server errors and recipient messages
in section 5.5. An unchecked item remains required; this checklist does not
replace or narrow the RFC.

## Shared runtime and catalogs

- [x] Leaf `pocopine-locale` crate; explicit, validated locale values; no ambient
  server locale; host/client dependencies gated at module boundaries.
- [x] Vendored, reproducible CLDR cardinal rules and parent chains; exact decimal
  plural operands; host ICU4X differential oracle and regeneration/drift check.
- [ ] ICU MF1 closed-subset parser and formatter: interpolation, plural/exact
  selectors, select, number/percent, date/time, apostrophe escaping, bounded
  nesting, and positional element placeholders.
- [x] Default-locale argument contracts, safe fallback, validated immutable
  catalogs, dense IDs, explicit artifact versions and build-ID rejection.
- [ ] Host number/date formatting, browser Intl rendering, optional strict
  formatting parity; shared plural selection without browser ICU4X by default.

## Compiler and authoring

- [ ] `[locale]` configuration; deterministic `.poco`, inline `poco!`, and Rust
  extraction with source diagnostics and target reachability.
- [ ] Static keys; module locality and `common.*`; duplicate/leaf-branch errors;
  default-locale missing-key and argument/type validation; orphan reporting.
- [ ] Typed generated Rust functions with explicit locale; completion metadata;
  host-only message retention and separation from browser assets.
- [ ] Compiled `pp-t` plans, named arguments, positional element preservation,
  compiled `$t` paths for attributes; diagnostics reject unsupported syntax.

## Browser and delivery

- [ ] Build produces fingerprinted per-locale catalogs, mapping metadata, and
  generated API before wasm/server compilation. Message text stays out of wasm.
- [ ] HTML shell starts catalog loading alongside wasm; splash waits for both;
  load failures and stale build IDs fail visibly and recover deliberately.
- [ ] Reactive text/attribute translation; cancellation-safe atomic locale
  switching; cleanup on unmount; no remount needed.
- [ ] Locale routing modes, precedence, explicit picker, persistence,
  `lang`/`dir`, and locale-aware links.

## Server and workers

- [ ] Negotiated typed request locale available before framework rejections;
  RPC metadata propagation; locale fixed for streaming calls.
- [ ] Public error/validation text translated without changing classification;
  internal diagnostics separated from public payload; network errors localized
  on the client. Preserve existing wire compatibility.
- [ ] Server/worker catalog initialization, standalone use, per-recipient locale
  snapshots, retries, stable semantic job inputs across catalog deployments.
- [ ] SSR stamping and hydration claims, structural plural parity, metadata and
  build-ID state. Current base has no `pocopine-ssr` crate: inspect the actual SSR
  architecture/history before choosing an integration; the draft's old PR #209
  status is not evidence that this exists.

## Tooling and verification

- [ ] `pocopine locale` (`i18n` alias): check, extract, merge, stats, XLIFF 2.0
  import/export; deterministic sorted files and diagnostics.
- [ ] LSP translation-key completion and default-locale hover.
- [ ] Runnable example exercising browser, server errors, locale switching,
  plurals, attributes, and recipient messages; run through the Pocopine CLI.
- [ ] Parser failure cases, CLDR oracle, catalog compatibility, compile-fail
  diagnostics, concurrent server locales, pre-handler errors, streaming,
  worker retries/deployments, browser geometry/interaction, and SSR parity.
- [ ] Bundle inspection/size regression proves message and unused-key removal;
  configured locale and catalog growth measured, not assumed.
- [ ] Required gates: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --target wasm32-unknown-unknown`,
  `cargo build --workspace --target wasm32-unknown-unknown`,
  `cargo test --workspace` on host, plus focused relevant checks.

## Decisions to settle during implementation

- Pin CLDR data and its source/license; regeneration is a reviewed maintenance
  change, with an oracle gate. Default fallback uses CLDR parents before the
  configured default locale.
- Keep ordinals outside the initial MF1 subset as specified in section 2;
  reject `selectordinal` explicitly instead of silently treating it as cardinal.
- Runtime catalog decoding/one-time parsing at initialization is compatible
  with section 10's ban on source parsing on every request.
- The unconditional no-ICU4X-browser rule and explicit `strict_parity` exception
  must be reconciled in documentation when implementing the opt-in path.

## Evidence

2026-09-06: Worktree inspected at base `08665567`; RFC is present and untracked.
No locale crate or SSR crate exists at this base. Existing `ServerError` uses
string payloads; `RequestContext` and `Extension<T>` provide typed host inputs.
Implementation and verification are in progress.

2026-09-06, shared runtime/compiler checkpoint:

- Added `crates/pocopine-locale`: exact decimal operands, validated locale values,
  CLDR text fallback, component-specific plural inheritance, MF1 parsing and
  branch selection, structured element/number/date parts, and immutable catalogs.
- Vendored CLDR 48.2.0 with Unicode License V3 and a deterministic generator.
  The ICU4X oracle checks 86,016 cases across 224 locales using ICU4X's own rule
  parser/evaluator. Its default baked data omits some CLDR locales, so testing
  those through the baked fallback would incorrectly compare against `other`.
- The host compiler accepts supplied source/reference records, validates flat
  JSON and default contracts with Stylekit's diagnostic/span types, assigns IDs,
  hashes semantic inputs via `pocopine-crypto`, resolves fallback, and generates
  separate host/browser catalogs. Source discovery and generated Rust call sites
  are still pending; this is not yet wired into app builds.
- Number/date nodes are typed rendering requests, not final formatted text yet.
  Browser bindings, CLI/LSP integration, server middleware, workers, and SSR
  remain unchecked above. No end-to-end implementation claim is made.
- After the compiler addition: all 18 unit tests and the 86,016-case ICU4X oracle
  pass. Host `--all-targets` and wasm strict Clippy (`-D warnings`), wasm build,
  regeneration fixed-point check, `git diff --check`, and workspace formatting
  pass. The wasm normal-dependency tree contains no ICU4X or host compiler.
  Full workspace host/wasm gates and browser verification remain pending.
