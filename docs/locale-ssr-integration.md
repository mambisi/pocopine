# Locale integration after RFC-099

RFC-120 section 6 explicitly allows the catalog compiler, browser translations,
routing, server errors and recipient messages to ship before SSR. Translated
server stamping and hydration are a separate, still unimplemented slice.

Inspected on 2026-09-06: this branch's base (`08665567`) has no `pocopine-ssr`,
`host_eval` or hydration runtime. The local `feat/rfc-099-ssr-phase2` branch at
`6a94e7b0818d52bcf50ffaf7f3a9d1260ab47b57` contains those primitives. Its change
from the common base spans 52 files, including structural mounting and macro
changes. The old RFC's PR status does not establish availability on this base.
Integrate that prerequisite separately before claiming translated SSR support.

## Integration points

1. Reconcile the RFC-099 static plan/macro additions with locale's
   `BindingKind::Translate`, `StaticExpr` translation nodes and `TranslationPlan`.
   Retain dense message IDs, argument contracts and release key removal in lifted
   `if`, `match`, `for`, child-component and slot plans.
2. Extend `pocopine-ssr`'s `stamp_with_plan` and `apply_binding` paths, as well as
   its interpolation evaluation. Pass an explicit render context containing
   `Locale`, immutable `ServerCatalogs`, timezone inputs and the build identity
   through recursive stamping. Evaluate each argument using `host_eval`, convert
   it to the same `ArgumentKind` contract, then call `ServerCatalogs::render`.
   The existing host web-sys template stubs are not an SSR evaluator.
3. Stamp text and attributes with the serializer's escaping. For rich `pp-text`,
   associate source children with their numeric placeholders before arranging
   `RenderedPart` values. Preserve elements and their attributes; catalog markup
   never supplies arbitrary HTML or element attributes.
4. Include resolved locale, catalog build identity and a versioned claim record
   in `RenderedPage::state_island`. Bind number/date text spans and rich child
   identities to the exact compiled plan. Emit `lang`, `dir`, canonical and
   `hreflang`/`x-default` links from the same `LocaleRoutes` configuration.
5. Extend `hydrate_root`/`hydrate_subtree` and translation installation to validate
   the identity before claiming. On the first pass, claim the stamped text and
   elements without Intl reformatting or DOM writes. Register dependencies even
   when suppressing that first write. A later argument/locale change uses the
   selected formatter normally. A stale build must enter the existing visible
   reload path, never reinterpret another build's dense IDs.

## Acceptance checks

- Extend RFC-099's `structural_stamp` and `ssr_hydration` harnesses with the same
  exact-decimal CLDR corpus used by locale's host oracle. Exercise branches,
  nested rich placeholders and all lifted structural contexts.
- Assert identical `outerHTML` and zero first-pass mutation records. Verify node
  identity, focused controls, listeners and refs survive claiming and switching.
- Exercise deliberately different server/browser Intl formatting. Default mode
  must retain the server's initial number/date bytes; strict parity must produce
  identical bytes independently using the configured ICU data pack.
- Test explicit locale precedence, query preservation, RTL metadata, parallel
  requests, catalog arrival order and stale document/catalog/build combinations.
- Assert host-only messages and catalog key strings remain absent from browser
  assets and release Wasm after the SSR integration.

The current host formatting, request negotiation and browser tests do not
substitute for these hydration checks.
