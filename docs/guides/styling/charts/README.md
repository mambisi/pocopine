---
title: "Pine Charts"
description: "SVG-first chart primitives built on Pine and pocopine."
---

# Pine Charts

Pine Charts is the SVG-first chart layer for Pine. It follows the same contract
as the rest of Pine: the crate owns behavior, geometry, accessibility metadata,
and stable DOM hooks, while the application owns visual styling.

The primitives layer covers:

- chart rectangles and margins,
- linear and band scales,
- SVG path builders for line and area series,
- single-line, multi-line, scatter, area, grouped bar, stacked bar, pie/donut,
  radial bar, legend, marker, and hover components,
- strict finite-number validation on every data input.

Higher-level components build on those primitives instead of hiding them. That
keeps charts useful for simple dashboards while still giving you a path to
custom marks, axes, interaction, and styling.

## Design Model

Pine Charts is a block system, not a full dashboard framework. The crate makes
hard chart behavior reusable without taking over your application's layout or
product decisions.

Pine owns:

- geometry, scales, SVG paths, and responsive measurement,
- hit testing, hover state, keyboard selection, and event payloads,
- accessibility metadata, stable classes, and `data-*` hooks,
- validation errors for malformed data and unsupported options.

Applications own:

- cards, panels, dashboard grids, and page layout,
- color, typography, spacing, animation timing, and visual emphasis,
- filtering policy, custom tooltips, selection detail panels, and drilldown,
- routing, persistence, analytics, and domain-specific labels.

That boundary keeps the public API low-to-high: use the ready-made line, bar,
area, scatter, pie, radial, and Cartesian components when they fit; drop to
layered SVG blocks when the visualization needs custom marks.

## Styling Contract

Pine Charts ships no theme. Components expose semantic classes such as
`pine-chart-root`, `pine-chart-svg`, `pine-chart-axis`, and `pine-chart-line`,
plus `data-state` and `data-orientation` attributes for component state and
layout direction. You style those selectors with your own CSS.

The crate prefers attributes and CSS variables over inline presentation styles.
SVG geometry attributes such as `x`, `y`, `d`, and `viewBox`, as well as ARIA
attributes, are framework-owned. Color, stroke width, typography, and spacing
remain author-owned.

## Guides

- [Foundation](foundation.md)
- [Components](components.md)
- [Cartesian Composition](cartesian.md)
- [Axes and Grid](axes-grid.md)
- [Legends](legend.md)
- [Responsive Containers](responsive.md)
- [Layered Charts](layered.md)
- [Layers](layers.md)
- [Interaction](interaction.md)
- [Events](events.md)
- [Cookbook](cookbook.md)
- [Examples](examples.md)

## Progression

The guides follow a bottom-up order that matches how the crate is layered:

1. **Foundation** — pure geometry, scale, and path utilities.
2. **Components** — SVG root, plot area, and ready-made line, area, bar, scatter,
   pie, and radial components.
3. **Composition** — axes, grid lines, labels, markers, and legends combined via
   `PineCartesianChart` and `PineLayerChart`.
4. **Interaction** — tooltip, hover/crosshair, selection, and keyboard
   affordances.

Canvas is out of scope. If a future chart type needs canvas for very large
datasets, it will be introduced as a separate rendering backend rather than
replacing the SVG component model.
