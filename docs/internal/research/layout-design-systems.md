# Layout & Responsiveness in Major Design Systems — Research for `pine-layout`

> Fact-checked research base for RFC-103 (`pine-layout`). Compiled 2026-06-13.
> Every numeric claim below survived 3-vote adversarial verification (24/25
> claims confirmed). Claims that could **not** be verified from primary sources
> are explicitly marked ⚠️ UNVERIFIED so we don't bake folklore into the API.

## TL;DR

All three systems converge on the same shape:

1. **Viewport-tier responsiveness** — a small set of named width buckets drives
   layout, not a continuum.
2. **A handful of layout primitives** — a container, a vertical stacker, a
   horizontal stacker, plus a column grid.
3. **Canonical multi-pane patterns** that reflow from one column on narrow
   screens to two/three on wide ones (list-detail, supporting pane, feed).

They diverge sharply on **exact numbers**, and the numbers are not directly
comparable: Atlassian uses CSS **px**, Material uses density-independent **dp**,
Apple uses **pt**. A web/WASM library must pick **px** as canonical and treat
dp/pt thresholds as *conceptual tiers*.

```
NARROW ───────────────────────────────────────────────────► WIDE
single column          two columns               three columns
stacked nav            nav rail                  nav drawer / sidebar
bottom bar             list-detail (1 pane)      list-detail (both panes)
```

## Side-by-side: the verified numbers

| Dimension            | Apple HIG                              | Material 3                                   | Atlassian DS (grid-beta)                         |
|----------------------|----------------------------------------|----------------------------------------------|--------------------------------------------------|
| **Size model**       | `compact` / `regular` size classes (H+V) | Window size classes (compact/medium/expanded…) ⚠️ exact dp unverified | 6 px breakpoints `xxs…xl`                         |
| **Breakpoints**      | coarse (2 classes per axis)            | ~600dp, ~840dp tiers ⚠️ UNVERIFIED            | xxs 320–479 · xs 480–767 · s 768–1023 · m 1024–1439 · l 1440–1767 · xl 1768+ |
| **Grid columns**     | n/a (Auto Layout / stacks)             | grid varies by class                          | **2 → 6 → 12** (xxs → xs/s → m/l/xl)             |
| **Gutters**          | system spacing                         | 4dp/8dp grid                                  | **12px** small, **16px** large                   |
| **Outer margins**    | safe-area + layout margins             | per size class                                | **16px** small, **32px** large                   |
| **Spacing base**     | 8pt baseline grid ⚠️ UNVERIFIED         | 4dp / 8dp grid                                | **8px base**, scale `space.0…space.1000`         |
| **Touch target**     | 44pt ⚠️ UNVERIFIED                       | 48dp ⚠️ UNVERIFIED                             | —                                                |

## Per-system detail

### Apple — Human Interface Guidelines

- **Split View** is the canonical adaptive container: a **two- or three-column**
  interface — *primary column + optional supplementary column + secondary
  content pane*. Default split devotes **⅓ to primary, ⅔ to secondary**; a
  half-and-half layout is also available. Apple says to **prefer a split view in
  a `regular` (not `compact`) environment** — it needs horizontal space.
  [HIG: Split Views]
- **SwiftUI `NavigationSplitView`** is the code embodiment: multi-column on
  iPad/macOS/large iPhones-in-landscape, and **auto-collapses to a single-column
  `NavigationStack` in a compact horizontal size class**. Two-column form takes
  sidebar-first/detail-second; three-column form inserts a content column where
  each column's selection drives the next, with an automatic sidebar-toggle.
  [Apple docs: NavigationSplitView]
- **`safeAreaLayoutGuide`** (UIKit) is the containment/inset mechanism: "the
  portion of your view that is unobscured by bars and other content" — not
  covered by navigation bars, tab bars, toolbars, or ancestor views.
  [Apple docs: safeAreaLayoutGuide]

### Material 3 / Android — Adaptive design

- **Three canonical layouts**, each designed to reflow across all window size
  classes: **List-Detail**, **Feed**, **Supporting Pane**.
  [m3.material.io + developer.android.com: canonical-layouts]
  - **List-Detail**: expanded width shows **both** list and detail
    simultaneously; medium/compact show **one** pane (list *or* detail) based on
    interaction.
  - **Supporting Pane**: **50/50** split at medium width; **70/30**
    (main/supporting) at expanded width.
- **`SlidingPaneLayout` reflow rule** (the buildable heuristic): two panes show
  side-by-side **only when available width ≥ the combined minimum widths of both
  panes** (e.g. 200dp list + 400dp detail ⇒ needs ≥600dp). Below that they
  collapse to single-pane, top view filling the width, revealed/dismissed by an
  edge-drag. [developer.android.com: twopane]
- **Adaptive navigation** swaps by width: **bottom bar** (compact) → **navigation
  rail** (medium+) → **navigation drawer** (very large, ~1200dp+).
  [codelab + m3 navigation-drawer guidelines] — *medium confidence; the drawer
  tier and exact dp are partially unverified.*

### Atlassian — Design System (the most web-native, precise numbers)

- **6 breakpoints**, two tiers: small viewports **XXS/XS/S (320–1023px)**, large
  viewports **M/L/XL (1024px+)**. [atlassian.design: grid-beta]
- **12-column grid**: column count steps **2 (xxs) → 6 (xs/s) → 12 (m/l/xl)**;
  content spans **3–12 columns**. Gutters **12px** small / **16px** large; outer
  margins **16px** small / **32px** large. [grid-beta]
- **Spacing**: **8px base**; tokens
  `space.0`(0) `space.025`(2) `space.050`(4) `space.075`(6) `space.100`(8)
  `space.150`(12) `space.200`(16) `space.250`(20) `space.300`(24) `space.400`(32)
  `space.500`(40) `space.600`(48) `space.800`(64) `space.1000`(80). Gaps
  (no 700/900) are intentional. [atlassian.design: spacing]
- **Primitives**: core trio **Box** (container), **Inline** (horizontal),
  **Stack** (vertical); plus **Flex** (CSS flexbox), **Grid** (CSS Grid), and
  **Bleed** (negative whitespace). [atlassian.design: primitives]
- ⚠️ Atlassian also ships a *separate* `Breakpoints` primitive with
  `xxs/xs/sm/md/lg/xl` naming — **distinct** from the grid-beta breakpoints.
  Don't conflate them.

## Container queries vs viewport

Best practice (MDN) combines **both**: viewport media queries drive **page
structure** (app shell, page grid); **container queries** drive **component**
responsiveness so a card/pane adapts to its parent slot regardless of where it's
placed. Container queries are stably supported (Chrome 105+, Firefox 110+,
Safari 16+). → `pine-layout` should let *page-level* primitives (AppShell, page
Grid) key off the viewport and *component-level* primitives (cards, SplitPane)
optionally key off their container.

## Implications for `pine-layout`

1. **Breakpoints — align to Stylekit, don't invent.** `pocopine-stylekit`
   already ships Tailwind's scale: `sm 640 · md 768 · lg 1024 · xl 1280 ·
   2xl 1536`. A component's reactive `breakpoint` state **must** resolve to these
   exact thresholds, or `breakpoint == "md"` and the `md:` utility prefix
   disagree. This single constraint settles the "which breakpoints" open
   question that the research couldn't (Atlassian-6 vs Material-tiers): **reuse
   Stylekit's 5.**
2. **Spacing — reuse Stylekit's spacing scale** (already an 8px/4px-based
   `p-*`/`gap-*` system) rather than adding a parallel `space.*` token set.
3. **Grid — 12 columns** is the cross-system consensus (Atlassian explicit;
   Material/Bootstrap/Tailwind all 12-friendly).
4. **Canonical patterns worth shipping as components**: `AppShell` (adaptive
   nav: bar→rail→drawer/sidebar), `Container` (max-width + safe-area aware),
   `Stack`/`Inline` (1-D fl*), `Grid` (12-col), and a **`SplitPane`/`ListDetail`**
   primitive whose two-pane→one-pane reflow follows the combined-min-width rule.
5. **Reflow heuristic = combined-min-width**, not a hard breakpoint — this is the
   one genuinely portable algorithm from the research (Android `SlidingPaneLayout`).

## Caveats (read before trusting a number)

- Numbers differ in **unit** (px/dp/pt) — not directly comparable.
- The Atlassian grid is explicitly **beta**; its numbers may shift.
- Material's clean "compact/medium/expanded" taxonomy and the **44pt/48dp** touch
  targets and **8pt** baseline grid were **requested but not verified** from
  primary sources here — widely known, but treat as unconfirmed.
- Apple's developer.apple.com pages are JS-rendered; some SwiftUI claims lean on
  an authoritative secondary source (Hacking with Swift) plus Apple initializer
  names rather than verbatim primary fetches.

## Primary sources

- Apple HIG — Split Views: https://developer.apple.com/design/human-interface-guidelines/split-views
- Apple — NavigationSplitView: https://developer.apple.com/documentation/swiftui/navigationsplitview
- Apple — safeAreaLayoutGuide: https://developer.apple.com/documentation/uikit/uiview/safearealayoutguide
- Material/Android — Canonical layouts: https://developer.android.com/develop/adaptive-apps/guides/canonical-layouts
- Android — SlidingPaneLayout (twopane): https://developer.android.com/develop/ui/views/layout/twopane
- Atlassian — Grid (beta): https://atlassian.design/foundations/grid-beta
- Atlassian — Spacing: https://atlassian.design/foundations/spacing
- Atlassian — Primitives: https://atlassian.design/components/primitives/overview
- MDN — Container queries: https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_containment/Container_queries
