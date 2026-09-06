# RFC-120 implementation

Working branch: `feat/locale`. The specification is
[RFC-120](../rfcs/rfc-120-i18n.md), including server errors and recipient messages
in section 5.5. An unchecked item remains required; this checklist does not
replace or narrow the RFC.

## Confirmed authoring decision

2026-09-06: `$t` is the only template translation surface. Use existing
`pp-text`, attribute bindings and interpolation. `$t.module.key` is shorthand
for a message without arguments; `$t('common.welcome', name)` supplies values
in the generated signature's alphabetical argument-name order. There is no
`pp-t` or `pp-t:*` directive. A direct `$t` binding in `pp-text` preserves rich
message child elements. This clarification supersedes the original directive
examples. Compiled template support and CLI/build delivery are verified below;
routing and authoring tools remain in progress.

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
- [x] Resolve Cargo browser/server/worker targets, features and build-script
  cfg; wire discovery into the CLI and application build pipeline.
- [x] Static keys; module locality and `common.*`; duplicate/leaf-branch errors;
  default-locale missing-key and argument/type validation; orphan reporting.
- [x] Typed generated Rust functions with explicit locale; Rust completion/docs;
  host-only message retention and separation from browser assets.
- [x] Compiled `$t` paths and calls in existing bindings; positional values,
  rich child-element preservation, and diagnostics for unsupported syntax.

## Browser and delivery

- [x] Build produces fingerprinted per-locale catalogs, mapping metadata, and
  generated API before wasm/server compilation. Message text stays out of wasm.
- [x] HTML shell starts catalog loading alongside wasm; splash waits for both;
  load failures and stale build IDs fail visibly and recover deliberately.
- [x] Shared generated-API/browser cache and read-only committed locale signal;
  cancellation-safe atomic selection after catalog validation.
- [x] Reactive template text/attribute translation; cleanup on unmount;
  locale/argument updates preserve native placeholder identity without remounting.
- [ ] Locale routing modes, precedence, explicit picker, persistence,
  `lang`/`dir`, and locale-aware links.

## Server and workers

- [x] Shared explicit-preference negotiation, CLDR parent matching, bounded
  Accept-Language weights/exclusions and selection-source metadata.
- [x] Negotiated typed request locale available before framework rejections;
  locale fixed for streaming calls, with incoming explicit RPC preference.
- [x] Outgoing buffered/replayed/streaming RPC metadata from committed UI locale.
- [x] Catalog-backed guard/body/extractor rejection payloads, stable error
  variants, and public payload access without diagnostic Display prefixes.
- [x] Generated public error/validation functions and a client display adapter
  preserve classification and wire compatibility. Server payloads stay verbatim;
  network diagnostics use an application-generated localized public message.
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
- [x] Runnable example exercising browser, server errors, locale switching,
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
  rich-message diagnostic. App build wiring and compiled template plans remain pending.
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

2026-09-06, HTTP integration checkpoint:

- The server `locale` feature exposes `ServerLocale` and `FrameworkMessages`.
  Applications initialize their generated host catalogs and bind four generated
  functions for unauthorized, forbidden, malformed-request and internal public
  messages. `Server::with_locale` prepares the boundary outside auth and plugin
  layers at finalization, including routes added after configuration.
- Requests receive typed `Locale` and negotiation metadata before guards, body
  decoding and extractors. Explicit `pocopine-locale` metadata precedes cookie
  and weighted language detection; conflicting duplicate explicit values are
  ignored. RPCs never redirect. Responses preserve existing Content-Language
  and Vary values while adding negotiated language/cache variation when needed.
  This boundary currently handles unprefixed RPC paths; URL routing is pending.
- Generated macro rejection sites apply the prepared public text after existing
  diagnostic logging. Existing ServerError variants/wire shapes remain intact,
  handler-returned messages stay verbatim, and missing required extensions use
  generic internal copy. `ServerError::public_message()` avoids diagnostic
  prefixes and returns None for client-side network diagnostics; client network
  localization remains pending.
- The real generated-API fixture includes auth ordering, late routes, locale
  conflicts/fallback, malformed and oversized bodies, guard and extractor
  failures, application errors, and concurrent English/French SSE streams.
  It also exercises unchanged legacy behavior without locale configuration.
- Verification against the workspace lock: the complete generated-API verifier
  passes, including its HTTP fixture and expanded host-message byte audit.
  Existing two streaming-server-function and three auth-layer-ordering tests
  pass, as do all 134 macro unit tests. Focused host server/macro strict Clippy,
  wasm core/server strict Clippy and build, server feature-disabled check,
  workspace formatting and whitespace checks pass. The fixture is now 421,963
  bytes raw / 106,699 gzip; this remains a whole-fixture measurement. Full
  workspace gates and the unchecked integration/size requirements remain open.

The server wiring uses application-authored catalog keys; the framework does
not introduce a second message format or hardcode a translation namespace:

```rust
// Host startup, after the application build has generated `t`.
t::initialize()?;
let messages = pocopine_server::locale::FrameworkMessages {
    unauthorized: t::common::unauthorized,
    forbidden: t::common::forbidden,
    bad_request: t::common::bad_request,
    internal: t::common::internal,
};
let locale = pocopine_server::locale::ServerLocale::new(locales, messages);
let server = pocopine_server::Server::new(router).with_locale(locale);
```

Handlers extract `pocopine_server::Extension<pocopine_locale::Locale>` and pass
the value to generated functions. Domain adapters choose translated public
application messages; the framework adapter handles pre-handler rejections.

2026-09-06, browser state and outgoing RPC checkpoint:

- The opt-in core/umbrella `locale` feature exposes `locale::client::LocaleController`.
  `t::catalogs()` shares the generated API's cache with it. Initialization
  requires the exact selected catalog to be ready, and the public locale signal
  is read-only. The `locale-strict-parity` feature forwards to the leaf backend;
  project configuration/build selection still needs wiring.
- Switch tickets publish only after validation. New selections supersede old
  work, including selecting the currently displayed language. Dropped futures,
  failed/stale catalogs, and late superseded failures leave the visible language
  intact. Cached selections avoid the loader. This is the selection contract;
  the actual HTML preloader, HTTP catalog transport, URL/cookie persistence and
  document metadata are still pending.
- Browser RPCs capture an untracked committed locale before awaiting transport.
  Middleware replays retain that header and SSE requests carry one snapshot for
  the stream lifetime. Applications without an active locale retain prior
  behavior. Host requests continue to have explicit locale inputs.
- `LocaleController::error_message` displays server-owned public payloads as-is
  and formats network error copy with an application-generated arg-less function.
  Diagnostic text and ServerError classification stay intact. The isolated
  generated-API fixture exercises this function and shared cache installation.
- Six headless Chrome tests pass with both default Intl and strict ICU backends: failed/stale
  catalogs, racing/cancelled loads, cache hits, reactive updates/release,
  untracked RPC snapshots, public error display, and actual browser Request
  headers for buffered calls, middleware replays and an SSE response reader.
  The network is intercepted at `window.fetch`; server negotiation is exercised
  separately by the real HTTP fixture.
- The full generated-API verifier passes, including host recipient and HTTP
  tests, intentional compile failures, wasm execution and release byte audits.
  The standalone fixture is 422,077 bytes raw / 106,741 gzip. Its normal browser
  dependency tree still excludes ICU formatting and Jiff; core is a wasm test
  dependency here, so this measurement does not measure the new controller or
  its RPC integration. Full integration and growth gates remain open above.
- Focused host and wasm `--all-targets -D warnings` Clippy checks pass for core,
  the umbrella crate and locale; the core/umbrella strict-parity wasm check also
  passes. Workspace formatting and diff whitespace checks pass. Full workspace
  gates remain pending.


2026-09-06, corrected `$t` template checkpoint:

- Replaced the directive proposal with `$t` in existing text bindings, attribute
  bindings and interpolation. Calls such as `$t('common.welcome', name)` use a
  literal key and positional values in alphabetical catalog-argument order,
  matching generated Rust functions. The removed directive has no registry
  entry; its spellings produce migration diagnostics.
- Catalog extraction and template compilation enforce static keys, arity and
  element contracts. A generated private macro resolves keys to descriptors
  with dense IDs and build identity. Runtime values are checked against the
  catalog's text/number/date kinds. Exact decimal-string plural inputs retain
  visible fractional digits. Extended static expressions stay scoped to locale
  plans; ordinary expression fallback behavior remains unchanged.
- Direct translated `pp-text` preserves template child elements and safely
  patches text/order/nesting. Attribute and interpolation translations remain
  plain text. Bindings recover after late controller activation; unmount releases
  them. The fixture checks names containing markup as literal text, changing
  variables/counts/languages, conditionals, keyed rows, rich sibling reordering,
  reversed nesting, focus, refs, listeners and teardown.
- Verification: 38 locale unit tests, 134 macro unit tests, all 54 existing
  template browser tests and all six locale browser tests pass. The complete
  generated-API verifier passes host/worker/HTTP tests, normal browser calls,
  default-Intl and strict-parity component tests, and targeted compile failures
  for missing keys, wrong arity, rich attributes, dynamic keys and `pp-t`.
  Focused host and wasm strict Clippy and formatter fixed-point checks pass.
- Reachable release templates retain no audited catalog key/message bytes,
  including inert source HTML for lifted bodies. The whole template fixture is
  1,565,164 bytes raw / 426,107 gzip; the separate leaf fixture is 422,077 raw /
  106,742 gzip. These are whole binaries, not marginal locale or configured-data
  growth measurements. Full workspace gates and remaining integration tasks are
  still required.
- SSR history was checked locally: `feat/rfc-099-ssr-phase2` and its remote ref
  contain the stamper/hydration work (including `6a94e7b0`); this checkout does
  not. The old RFC's PR status cannot establish availability on this branch.

2026-09-06, CLI and catalog-delivery checkpoint:

- The CLI probes Cargo's actual host/browser cfg, features and build-script
  flags before source generation, discovers configured server/worker roots,
  then writes the typed API and fingerprinted browser catalogs. The app uses
  `pocopine::locale::include_translations!()` at crate root. Generated runtime
  initialization APIs and their Rust import aliases are excluded from message
  extraction. Catalog/config edits participate in dev rebuilds.
- `[locale].strict_parity` selects the matching Cargo feature. Configured data
  slicing remains unfinished; enabling the feature still uses full baked ICU
  formatting data.
- Generated HTML embeds metadata for its exact bundle and starts the selected
  catalog before deferred wasm boot. CLDR parent exceptions are resolved from
  the shared data, including script boundaries. Browser boot independently
  negotiates, validates build/config/count/catalog identity, shares the preload,
  and activates before mount. A failed boot exposes an explicit reload action;
  a failed later selection preserves the committed language and can be retried.
- `t::initialize(t::locales())`, followed by
  `pocopine::locale::client::boot(t::catalogs()?).await`, prepares the app.
  `LocaleController::set_locale` loads from the matching manifest and commits
  only after readiness. Script direction is resolved at build time and sent
  as one value per configured language; `lang` and `dir` follow committed state.
  URL rewriting, persistence and router integration remain unchecked above.
- The isolated `examples/locale` app was built and run through the Pocopine CLI.
  An isolated Chrome session verified live name interpolation, plurals, rich
  link text, translated attributes, French and Arabic server greetings/errors,
  outgoing RPC locale snapshots, three catalog downloads across repeated
  switches, immutable catalog response headers, RTL and a 390-pixel viewport.
  Blocking catalog requests produced the failure screen without mounting;
  its reload action recovered once requests were restored. Private source
  catalogs and generated host files returned 404 from the example server.
- Forty locale unit tests and all 145 CLI unit tests pass. All seven core
  browser locale/boot tests and the standalone loader contract pass. Strict
  host lint and core wasm lint pass; the final formatted example builds through
  the CLI. The complete workspace gates are still pending. Including the
  host-only CLI in a wasm lint attempt fails in its existing Tokio/mio graph;
  `.github/workflows/ci.yml` explicitly excludes that CLI and other host crates.
  A disk-full lint attempt was environmental: only this worktree's derived
  debug caches were removed, recovering about 30 GB. Subsequent checks disable
  incremental compilation and debug symbols to keep artifacts bounded.
