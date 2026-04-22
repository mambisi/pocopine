# RFC 043 — `pocopine::text` layout engine

| Field | Value |
|---|---|
| **Status** | Implemented (v1) |
| **Author** | pocopine team |
| **Created** | 2026-04-22 |
| **Related** | [RFC 014 — Focus & timing utilities](./rfc-014-focus-utilities.md), [RFC 023 — Pine MVP](./rfc-023-pine-mvp.md), [RFC 033 — Primitive roles](./rfc-033-primitive-roles.md), [RFC 037 — JS bridge](./rfc-037-js-bridge.md) |
| **Upstream** | [`github.com/chenglou/pretext`](https://github.com/chenglou/pretext) (MIT) — the TypeScript library this port is derived from |

## 1. Summary

Add a text layout engine to `pocopine-core` — a Rust port of the
`prepare` / `layout` model from chenglou's
[pretext](https://github.com/chenglou/pretext). The engine returns
line counts, per-line widths, heights, and cursor-addressed per-line
slices *without rendering anything*, so components can drive DOM /
canvas / SVG output themselves.

```rust
use pocopine::text::{prepare, layout_with_lines, CanvasMeasurer, Font, PrepareOptions};

let measurer = CanvasMeasurer;
let font     = Font("16px system-ui".into());
let prepared = prepare("The quick brown fox…", &font, PrepareOptions::default(), &measurer);
let (r, lines) = layout_with_lines(&prepared, 240.0, 20.0);
// r.line_count = N, lines[i].width = px, lines[i].end = cursor
```

Without it: Pine had no way to do measurement-aware truncation,
balanced headlines, autosize inputs, or virtualized text lists. CSS
`-webkit-line-clamp` doesn't report whether it clamped, doesn't give
cursor positions, and can't feed other components. Hand-rolling per
primitive means several copies of the same line-break walk —
pretext solved this once; we port the pure-algorithm half.

## 2. Non-goals

- **Rendering.** The engine measures and slices; putting characters
  on screen is the caller's job. `PineText` (RFC for that primitive
  is implicit — Pine's own docs own the component surface) is the
  first consumer; virtualized lists and autosize inputs will follow.
- **Font metrics without the browser.** Width measurement delegates
  to `canvas.measureText()` via a small JS shim. We are not
  embedding HarfBuzz, rasterizing fonts, or shipping font files
  inside WASM. SSR / headless callers supply their own [`Measurer`].
- **Full pretext parity.** We port the line-break walk and the
  two-phase prepare/layout shape. CJK-specific passes (kinsoku,
  keep-all word break, CJK closing-quote carry), bidi metadata,
  emoji-width correction, URL/numeric run merging, `pre-wrap` hard
  breaks, letter-spacing paint/fit duality, and the rich-inline
  API from pretext's `rich-inline.ts` are **deferred v2+**. v1
  exposes the smallest shape that unlocks measurement-backed
  truncation.
- **A new RFC for `<pine-text>`.** The primitive lives in the Pine
  component library and follows the general Pine conventions
  (RFC 023 / RFC 033). This RFC is about the *engine in core* it
  consumes.

## 3. Layering

- **Engine** in `crates/pocopine-core/src/text/` — zero Pine
  coupling, usable from any WASM consumer.
- **Primitive** in `crates/pine/src/text/` — `<pine-text>`. Owns
  only template + props + lifecycle; calls into
  `pocopine::text::*` for the math.

This matches the rule used by [`focus`](./rfc-014-focus-utilities.md),
[`scroll_lock`](./rfc-021-scroll-lock.md), and
[`animate`](./rfc-038-animation.md): generic engines live in core,
thin components compose them.

## 4. Surface

```rust
pub mod text {
    // ── measurement ────────────────────────────────────────────
    pub trait Measurer {
        /// Width of `text` rendered in `font`, in CSS pixels.
        fn measure(&self, font: &Font, text: &str) -> f64;
    }
    pub struct CanvasMeasurer;   // Canvas 2D `measureText()` impl

    // ── inputs ────────────────────────────────────────────────
    pub struct Font(pub String);         // CSS `font` shorthand

    #[derive(Default)]
    pub enum WhiteSpace { #[default] Normal }    // v1

    #[derive(Default)]
    pub struct PrepareOptions {
        pub white_space:    WhiteSpace,
        pub letter_spacing: f64,          // reserved; v1 ignores
    }

    // ── prepared text (opaque) ────────────────────────────────
    pub struct PreparedText { /* opaque */ }
    impl PreparedText {
        pub fn is_empty(&self) -> bool;
        pub fn normalized(&self) -> &str;
        pub fn font(&self) -> &Font;
    }

    // ── cursors + results ─────────────────────────────────────
    pub struct LayoutCursor { pub segment_index: u32, pub grapheme_index: u32 }
    impl LayoutCursor { pub const fn start() -> Self; }

    pub struct LayoutResult {
        pub line_count:     u32,
        pub height:         f64,
        pub max_line_width: f64,
    }
    pub struct LayoutLine {
        pub text:              String,        // reconstructed, hyphen-inclusive
        pub width:             f64,
        pub start:             LayoutCursor,
        pub end:               LayoutCursor,
        pub soft_hyphen_break: bool,
    }

    // ── entry points ──────────────────────────────────────────
    pub fn prepare<M: Measurer>(text: &str, font: &Font,
                                options: PrepareOptions, m: &M) -> PreparedText;
    pub fn layout(p: &PreparedText, max_width: f64,
                  line_height: f64) -> LayoutResult;
    pub fn layout_with_lines(p: &PreparedText, max_width: f64,
                             line_height: f64) -> (LayoutResult, Vec<LayoutLine>);
}
```

Re-exported at `pocopine::text::*` through the umbrella crate for
ergonomics.

## 5. Semantics

### 5.1 Two-phase model

`prepare()` is the expensive step: it normalizes whitespace, walks
the text into graphemes, classifies each run as one of four
segment kinds (see 5.2), measures every Text segment's total width
plus per-grapheme widths, and caches it all in an opaque
`PreparedText`. `layout()` / `layout_with_lines()` are pure
arithmetic over that cache — cheap enough to re-run on every
resize / every slider tick.

Matches pretext's `prepare` → `layout` split for the same reason:
measurement is the only slow thing, so do it once.

### 5.2 Segment kinds (v1)

| Kind             | Source                 | Width                   | Break opportunity |
|------------------|------------------------|-------------------------|-------------------|
| `Text`           | anything non-special   | from measurer           | at graphemes only if whole segment overflows |
| `Space`          | U+0020 (normalized)    | from measurer           | yes; collapses at line end |
| `ZeroWidthBreak` | U+200B                 | 0                       | yes; zero-width |
| `SoftHyphen`     | U+00AD                 | 0 mid-line / hyphen at break | yes; inserts visible `"-"` |

Whitespace normalization (`WhiteSpace::Normal`) collapses every
`[ \t\n\r\f]+` run to a single U+0020 and trims the edges — the
CSS default.

### 5.3 Line-break walk

Cursor-walks segments left-to-right with one `pending_break`
slot — the most recent position we can retreat to when the next
segment overflows. Three escape routes on overflow, in priority
order:

1. **Overflowing segment is itself a break** (space / ZWSP /
   SHY). End the line before it; consume the break; continue.
2. **Retreat to `pending_break`** if the retreated width fits.
   Emit line up to the stored cursor (with hyphen width folded in
   for SHY); resume at `pending_break.segment_index`.
3. **Oversize word fallback**: single Text segment wider than the
   line at line start → walk its grapheme widths and emit
   per-grapheme lines until it fits. Matches pretext's
   `breakableFitAdvances` fast path.

Trailing spaces collapse at end-of-line (width `0`), matching
`white-space: normal` paint semantics.

### 5.4 Cursors

`LayoutCursor { segment_index, grapheme_index }` addresses any
position inside a `PreparedText`. `grapheme_index == 0` means
aligned with the *start* of `segments[segment_index]`. An
end-cursor of `{ N, 0 }` is "one past segment N-1 inclusive" — the
same off-by-one pretext uses. Cursors are stable across layout
re-runs at different widths.

### 5.5 Soft-hyphen break rendering

When the walker retreats to a mid-word SHY, the emitted line's
`soft_hyphen_break` flag flips to `true` and `layout_with_lines`
appends a visible `"-"` to that line's `.text`. Consumers that
only want the cursor (e.g. custom canvas painters) ignore the
text and look at the flag.

### 5.6 Fit epsilon

`FIT_EPSILON = 0.05`px slack absorbs subpixel canvas-measurement
jitter. Matches pretext's default `lineFitEpsilon`. Without it,
pixel-exact fits flap between "fits" and "overflows" as the DPR
changes.

## 6. Implementation

New module `crates/pocopine-core/src/text/` — seven files,
~600 lines of Rust.

```
text/
├── mod.rs          # re-exports
├── analysis.rs     # normalize_whitespace + segment()
├── measure.rs      # Measurer trait + CanvasMeasurer
├── measure_js.rs   # wasm-bindgen inline_js canvas shim + host stub
├── prepare.rs      # prepare() — runs analysis + measurement
├── line_break.rs   # walk() — the cursor breaker
└── layout.rs       # layout() + layout_with_lines() + tests
```

### 6.1 Dependencies

Adds `unicode-segmentation = "1"` to `pocopine-core/Cargo.toml`.
No new `web-sys` features — everything lives in the inline JS
shim.

### 6.2 JS measurement shim

`measure_js.rs` gates a `#[wasm_bindgen(inline_js = "…")]` extern
on `target_arch = "wasm32"`. The JS side owns:

- One shared `OffscreenCanvas` (falling back to a hidden
  `<canvas>` for Safari ≤16).
- A `Map<string, number>` LRU cache keyed by `font + \x01 + text`,
  capped at **5000 entries**, FIFO-evicting the oldest on
  insert-over-cap. Diverges from pretext's unbounded cache to
  bound memory on long-lived pages with dynamic text.

Host builds (`cfg(not(target_arch = "wasm32"))`) get a
deterministic stub (`text.chars().count() * 8.0`) so unit tests
run on the host without a browser.

### 6.3 Tests

Embedded in `layout.rs` behind `#[cfg(test)]`, using a `Mock`
`Measurer` that returns `10px * char_count`. Coverage:

- Empty input → 0 lines.
- Short text → 1 line.
- Space-wrap at word boundary when next word overflows.
- Whitespace collapsing + trimming.
- U+200B as a break opportunity.
- U+00AD inserts visible `-` at break.
- Oversize word falls back to grapheme-level placement.
- Exact-boundary width (one line when `total == max_width`).

All 8 pass on the host. The shim itself is exercised indirectly
by the Pine demo (manual smoke test).

## 7. Pine primitive (reference)

For discoverability — the first caller of this engine:

```rust
#[component(template = "PineText.poco", role = "text", display = "contents")]
pub struct PineText {
    #[prop] pub lines:       u32,   // 0 = no clamp
    #[prop] pub max_width:   f64,   // 0 = resolve ancestor content-box
    #[prop] pub line_height: f64,   // 0 = 20.0

    pub truncated:  bool,
    pub line_count: u32,
}
```

**Division of labor.** Visual truncation is CSS
`-webkit-line-clamp` — applied as inline style when `lines > 0`,
cleared when it drops to `0`. The browser handles pixel-perfect
clamping (including the ellipsis glyph) and Pine never touches
`textContent`. The engine runs in parallel purely to tell the
author *whether* clamping happened: `data-truncated` and
`data-line-count` come from `layout()`, not from CSS. That
split — browser for chrome, engine for state — keeps `pine-text`
reliable across browser font metrics, letter-spacing, and
hyphenation rules that our v1 engine doesn't model precisely.

`on_ready` reads the computed font + nearest block ancestor's
content-box width, runs `prepare` + `layout`, and writes back
`line_count` / `truncated`. `#[watch(lines)]` and
`#[watch(max_width)]` re-run via `tick::next`
([RFC 014](./rfc-014-focus-utilities.md)) to sidestep the
`on_ready` double-borrow.

## 8. Alternatives considered

- **Bundle pretext JS directly and call it via `wasm-bindgen`.**
  Smaller Rust footprint but keeps the algorithm on the JS side
  forever, locks layout behavior to a JS dependency, and
  complicates unit testing. Pretext is MIT-licensed and well
  specified — porting the ~1000 line-break lines was a few days'
  work and buys us host-side unit tests.
- **Port *all* of pretext, bidi tables included.** Bidi alone is
  a 5KB TS file + 64KB of pre-generated tables. We don't render
  RTL today and the Rust ecosystem has `unicode-bidi` for when we
  do. Not worth the up-front port.
- **Use CSS `-webkit-line-clamp` + `text-overflow: ellipsis`.**
  Doesn't tell us *whether* clamping happened, doesn't give
  cursors, and paints an inaccurate ellipsis at an arbitrary cut
  point. Authors wanting "2 lines, ellipsis only if actually too
  long, state hook on overflow" can't get there from CSS.
- **Embed HarfBuzz (`rustybuzz`) + ship fonts.** Real font
  metrics in pure Rust, and it would let us measure headless.
  Adds ~500KB of tables and the whole font-loading problem for
  a v1 that only runs in the browser. Revisit if/when SSR rendering
  of measured text becomes a requirement.
- **Put the engine inside `crates/pine`.** Rejected per the
  core-owns-engines rule — virtualized lists and autosize inputs
  are legitimate non-Pine consumers, and pine would otherwise
  become a dependency cone for anything touching text.

## 9. Forward plan (not in this RFC)

Ordered by expected demand:

1. **`balance`** prop on `<pine-text>` for headlines — one-axis
   binary search over `max_width` minimizing variance of line
   widths. The engine already returns everything needed.
2. **ResizeObserver auto-reflow** — currently authors bind
   `:max-width` to a reactive state (e.g. from a slider or
   `pp-resize`). An observer hook makes static layouts reflow
   automatically.
3. **`fit-width`** — scale font-size to fit N lines in a box.
   Second binary search, same engine.
4. **Bidi (`unicode-bidi`)** — compute segment levels once in
   `prepare`, include in `LayoutLine` for renderer use.
5. **CJK / kinsoku** — port pretext's `kinsokuStart`/`kinsokuEnd`
   sets + line-start prohibition, plus `icu_segmenter` for
   word-break `keep-all`.
6. **Rich inline** — port `rich-inline.ts` for mixed-font runs
   with atomic inline elements (icons / badges inside text).
7. **Emoji width correction** — probably last. Platform-specific
   rendering quirks that only matter for pixel-perfect
   parity with browser-rendered text.

Each is a separate RFC; the v1 API was deliberately shaped so
every one of them is additive, not breaking.
