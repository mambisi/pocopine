# Layers

SVG does not support CSS `z-index` for elements inside one `<svg>`. Pine Charts
uses SVG paint order instead: earlier sibling groups paint first, and later
sibling groups paint on top.

Every chart exposes stable `data-layer` hooks on its direct SVG groups. These
hooks document the paint order and give applications a reliable styling target.

## Cartesian Order

Line, area, scatter, and bar charts use this order:

1. `grid`
2. `axes`
3. data marks, usually `series`
4. optional `markers`
5. optional `hover`

Area charts split filled areas and line strokes so filled shapes stay behind
their strokes:

1. `grid`
2. `axes`
3. `area`
4. `series`
5. optional `markers`
6. optional `hover`

Bar charts currently use `grid`, `axes`, then `series`.

## Radial Order

Pie and donut charts use:

1. `series`
2. optional `labels`

The center label layer paints above slices and does not receive pointer events.

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

## Future Custom Layers

Pine Charts does not expose a numeric `z_index` prop yet. That API should wait
for a composable chart root that can accept arbitrary marks and reorder them
without relying on fragile template insertion order. Until then, built-in
components keep an opinionated, stable paint order.
