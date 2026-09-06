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
- [x] ICU MF1 closed-subset parser and formatter: interpolation, plural/exact
  selectors, select, number/percent, date/time, apostrophe escaping, bounded
  nesting, and positional element placeholders.
- [x] Default-locale argument contracts, safe fallback, validated immutable
  catalogs, dense IDs, explicit artifact versions and build-ID rejection.
- [x] Host number/date formatting, browser Intl rendering, optional strict
  formatting parity; shared plural selection without browser ICU4X by default.
- [ ] Slice ICU formatting data and generated plural tables to configured
  locales; wire project `strict_parity` to the matching Cargo feature. Current
  ICU formatting uses the pinned release's complete baked data.

## Compiler and authoring

- [x] `[locale]` configuration; deterministic `.poco`, inline `poco!`, and Rust
  discovery from supplied target roots/cfg, with original source diagnostics.
- [ ] Resolve Cargo browser/server/worker targets, features and build-script
  cfg; wire discovery into the CLI and application build pipeline.
- [x] Static keys; module locality and `common.*`; duplicate/leaf-branch errors;
  default-locale missing-key and argument/type validation; orphan reporting.
- [x] Typed generated Rust functions with explicit locale; Rust completion/docs;
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

- [x] Shared explicit-preference negotiation, CLDR parent matching, bounded
  Accept-Language weights/exclusions and selection-source metadata.
- [ ] Negotiated typed request locale available before framework rejections;
  RPC metadata propagation; locale fixed for streaming calls.
- [ ] Public error/validation text translated without changing classification;
  internal diagnostics separated from public payload; network errors localized
  on the client. Preserve existing wire compatibility.
- [x] Server/worker catalog initialization, standalone use, per-recipient locale
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
- ICU4X is excluded from runtime message plural selection and the default
  browser backend. Host text rendering and opt-in browser `strict_parity` use
  ICU4X as explicitly distinguished in RFC sections 6 and 10.

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

2026-09-06, source discovery checkpoint:

- Added strict `[locale]` configuration and `server::discover_project`. The
  caller supplies selected Rust roots with the actual target cfg/feature set.
  Discovery follows active modules, literal includes, component template paths
  and inline templates; unrelated source files do not contribute references.
  Source tables, diagnostics and catalog outputs are deterministic.
- Rust extraction handles nested `cfg`/`cfg_attr`, `#[server]` bodies, import
  aliases and function re-exports. Namespace imports retain no unused messages.
  `client`/`server` module wrappers preserve the feature namespace; crate-root
  references use `app.*`. Rust module directories and `#[path]` resolution follow
  the [Rust Reference](https://doc.rust-lang.org/reference/items/modules.html).
- Conditional references inside opaque macros, components declared inside
  opaque macros, translation glob imports and unresolved generated includes
  produce diagnostics. The generated `OUT_DIR/pocopine_locale.rs` include is
  reserved for the upcoming locale code generator. General macro expansion and
  Cargo target/feature resolution are not claimed by this source-only API.
- Shared interpolation parsing fixes multibyte text corruption in compiled
  static segments. Inline literal decoding is shared with the real component
  macro; extraction maps decoded text back to original Rust byte locations.
- Verification: 31 locale unit tests, the 86,016-case ICU4X oracle, 56 shared
  parser tests, 134 macro unit tests, and all 9 existing inline macro expansion
  integration tests pass. Focused host strict Clippy, wasm locale strict Clippy
  and build, workspace formatting and diff whitespace checks pass. Full workspace gates,
  generated APIs, CLI/build wiring and runtime integration remain pending.

2026-09-06, formatting and generated API checkpoint:

- Added prepared host ICU4X and default wasm Intl formatters, plus the explicit
  `strict-parity` Cargo feature. Plural operands retain exact integer/fraction
  semantics before text rounding. Dates require a validated timestamp and IANA
  timezone; host/strict wasm use the pinned bundled timezone database. Browser
  number formatters are prepared once and date formatters use a bounded cache.
- Host catalogs initialize completely before use, validate identity and required
  slots, and support private content-addressed files or generated embedded data.
  They carry no current language and can be shared across threads. Browser
  catalog installation validates before replacing cache entries; unloaded
  locales and stale build IDs produce explicit errors.
- `server::generate_rust` emits typed `t::module::message(locale, args...)`
  functions, default-copy Rust docs, explicit initialization, and separate
  host/browser modules. Rust references to element-bearing messages fail with a
  `pp-t` diagnostic. App build wiring and compiled template plans remain pending.
- `tools/check-locale-codegen.py` builds a real isolated consumer. Host and wasm
  calls work; wrong argument types, missing keys and browser calls to host-only
  keys fail compilation. The same persisted recipient jobs pass before/after a
  catalog change that shifts dense IDs and edits wording. Retries keep locale,
  timezone and typed semantic inputs; the application owns its delivery queue.
- Verification: 35 host unit tests, six host formatting tests and the 86,016-case
  plural oracle pass. Default and strict-parity wasm each pass the catalog cache
  test and five formatting tests in Node. The generated fixture passes two host
  tests per catalog build and its wasm runtime test. Focused host/default-wasm/
  strict-wasm Clippy uses `--all-targets -D warnings`.
- The fixture's default release wasm is 421,906 bytes raw / 106,702 gzip, with
  actual generated calls exported. Sentinel audits find no catalog message text,
  static key strings or host-only copy; its normal wasm dependency tree has no
  ICU formatting or Jiff. These are whole-fixture sizes, not incremental locale
  cost or configured-locale growth measurements. The broader size gate remains
  unchecked, as do real-browser UI, CLI/build, HTTP/error, routing and SSR work.

2026-09-06, boundary negotiation checkpoint:

- Added `LocalePreferences`, `NegotiatedLocale`, `LocaleSource` and
  `Locales::negotiate` for recognized route locale, explicit RPC preference,
  cookie, passive language list and final configured fallback, in that order.
  Unsupported or malformed preferences do not prematurely force the default.
  Selection uses CLDR parents, including script boundaries, consistently with
  recipient and catalog fallback; it is not a separate RFC 4647 lookup policy.
- The passive list uses [HTTP quality weights](https://www.rfc-editor.org/rfc/rfc9110.html#name-quality-values),
  stable preference order and zero-weight exclusions. Parsing is bounded at
  8 KiB/64 ranges. If no acceptable configured locale exists, the application
  deliberately falls back to its default instead of HTTP 406. The shared
  metadata names are `pocopine-locale` and `pocopine_locale`; transport wiring
  and cookie persistence remain pending.
- Three behavioral tests cover conflicting preferences, malformed weights,
  regional/script fallback and oversized input on host and wasm. This is the
  pure boundary policy; no HTTP middleware, redirects or RPC injection has been
  installed yet, and the integration checklist remains unchecked.
