---
title: "Layers"
description: "SVG does not support CSS z-index for elements inside one <svg>. Pine Charts uses SVG paint order instead: earlier sibling groups paint first, and later…"
---

# Layers

SVG does not support CSS `z-index` for elements inside one `<svg>`. Pine Charts
uses SVG paint order instead: earlier sibling groups paint first, and later
sibling groups paint on top.

Every chart exposes stable `data-layer` hooks on its direct SVG groups. These
hooks document the paint order and give applications a reliable styling target.

## Cartesian Order

Line charts use:

1. `grid`
2. `axes`
3. `series`
4. optional `markers`
5. `hover`

Area charts add a separate `area` layer so filled shapes stay behind their
line strokes:

1. `grid`
2. `axes`
3. `area`
4. `series`
5. optional `markers`
6. `hover`

Scatter charts have no `markers` layer:

1. `grid`
2. `axes`
3. `series`
4. `hover`

Bar charts currently use `grid`, `axes`, then `series`.

Composable Cartesian combo charts use `grid`, `axes`,
`reference-background`, `bars`, `area`, `series`, `points`, optional
`markers`, `reference-foreground`, then `labels`. Background references can sit
behind data marks, area fills stay behind line strokes, scatter points paint
above lines, and foreground references plus labels stay readable.

## Radial Order

Pie and donut charts use:

1. `series`
2. `hover`
3. optional `labels`

The `hover` layer renders the highlighted slice overlay and does not receive
pointer events. The center label (`labels`) paints above both and also blocks
no pointer events.

Radial bar charts use:

1. `tracks`
2. `series`
3. `hover`
4. optional `labels`

## Styling

Use classes for most styling and `data-layer` when the layer role matters:

```css
.pine-chart-svg > g[data-layer="grid"] {
  opacity: 0.7;
}

.pine-chart-svg > g[data-layer="hover"] {
  pointer-events: none;
}
```

Do not use CSS `z-index` to reorder SVG children. It will not produce a real
per-mark layer order inside the chart. If two whole charts need to overlap, put
`z-index` on the outer chart container instead.

## Composable Layers

Use `pine-layer-chart` when an app needs custom composition. It accepts
`pine-chart-layer`, `pine-chart-guide`, `pine-chart-line`,
`pine-chart-marker`, `pine-chart-reference-dot`, `pine-chart-label`, and
`pine-chart-icon` as direct children or nested inside a `pine-chart-layer`,
then renders everything through a fixed SVG paint-order:

1. `grid` — guide lines
2. `reference-background` — reference dots behind data
3. `series` — line paths
4. `markers` — point markers
5. `reference-foreground` — reference dots in front of data
6. `annotations` — icon overlays
7. `labels` — text labels

`pine-chart-layer` sets the paint bucket for its children via its `name` prop.
Accepted names are `grid`, `reference-background`, `series`, `markers`,
`reference-foreground`, `annotations`, and `labels`. `pine-chart-reference-dot` placed without
a `pine-chart-layer` wrapper defaults to `reference-background`; other mark
types are assigned to their natural bucket regardless of wrapping.

Pine Charts does not expose a numeric `z_index` prop. The bucketed order is
portable across browsers and avoids the pretense that CSS z-index works inside
SVG.
