# Pine Charts

Pine Charts is the SVG-first chart layer for Pine. It follows the same contract
as the rest of Pine: the crate owns behavior, geometry, accessibility metadata,
and stable DOM hooks, while the application owns visual styling.

The first layer is intentionally low level:

- chart rectangles and margins,
- linear and band scales,
- SVG path builders for line and area series,
- single-line, multi-line, scatter, area, grouped bar, stacked bar, pie/donut,
  legend, marker, and hover components,
- strict finite-number validation.

Higher level components build on those primitives instead of hiding them. That
keeps charts useful for simple dashboards while still giving application authors
a path to custom marks, axes, interaction, and styling.

## Styling Contract

Pine Charts does not ship a theme. Components must expose semantic classes such
as `pine-chart-root`, `pine-chart-axis`, and `pine-chart-line`, plus `data-*`
attributes for state and orientation. Applications style those selectors with
their own CSS.

The crate should prefer attributes and CSS variables over inline presentation
styles. Inline SVG geometry such as `x`, `y`, `d`, `viewBox`, and ARIA
attributes is framework-owned; color, stroke width, typography, and spacing
should remain author-owned whenever possible.

## Guides

- [Foundation](foundation.md)
- [Components](components.md)
- [Axes and Grid](axes-grid.md)
- [Legends](legend.md)
- [Interaction](interaction.md)
- [Events](events.md)
- [Examples](examples.md)

## Progression

1. Foundation: pure geometry, scale, and path utilities.
2. Components: SVG root, plot area, and first line/area/bar series components.
3. Composition: axes, grid lines, labels, markers, and legends.
4. Interaction: tooltip, hover/crosshair, selection, and keyboard affordances.
5. Strict mode: clear validation errors for malformed data and unsafe chart
   structures.

Canvas is out of scope for now. If a future chart type needs canvas for very
large datasets, it should be introduced as a separate rendering backend instead
of replacing the SVG component model.
