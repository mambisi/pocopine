---
title: "Layered Charts"
description: "pine-layer-chart is the composable SVG chart primitive. The root owns state and rendering; child components describe marks that register into named paint-order buckets."
---

# Layered Charts

`pine-layer-chart` is the composable SVG chart primitive. It follows the Pine UI
compound-component pattern: the root owns state and rendering, while child
components register mark definitions.

```poco
<pine-layer-chart label="Metro map">
  <pine-chart-layer name="grid">
    <pine-chart-guide key="top" x1="80" y1="120" x2="820" y2="120"></pine-chart-guide>
  </pine-chart-layer>

  <pine-chart-layer name="series">
    <pine-chart-line key="line-a"
                     label="Line A"
                     color="#19a974"
                     stroke_width="14"
                     pp-bind:points="line_a"></pine-chart-line>
  </pine-chart-layer>

  <pine-chart-layer name="reference-foreground">
    <pine-chart-reference-dot key="hub"
                              label="Hub"
                              x="480"
                              y="300"
                              radius="21"
                              fill="#d9363e"
                              stroke="#ffffff"
                              stroke_width="3"></pine-chart-reference-dot>
  </pine-chart-layer>
</pine-layer-chart>
```

The child tags do not render SVG directly. They register mark definitions into
the nearest `pine-layer-chart`. The root renders one valid SVG tree, preserving
namespace correctness, paint order, responsive sizing, and validation.

## Components

- `pine-layer-chart`: root SVG owner. Accepts chart marks directly or through
  `pine-chart-layer`.
- `pine-chart-layer`: groups child definitions and provides a layer name.
- `pine-chart-guide`: guide line in the `grid` layer.
- `pine-chart-line`: path generated from `Vec<ChartLayerPoint>`.
- `pine-chart-marker`: point marker rendered above series paths.
- `pine-chart-reference-dot`: larger background or foreground reference mark.
- `pine-chart-label`: positioned SVG text with optional rotation.
- `pine-chart-icon`: annotation icon. The only built-in kind is `"plane"`;
  unrecognized kinds are accepted but render no path.

## Root props

| Prop | Type | Default | Description |
|---|---|---|---|
| `label` | `String` | `"Layer chart"` | `aria-label` on the SVG root. |
| `width` | `f64` | `900` | SVG `width` and `viewBox` width in user units. |
| `height` | `f64` | `480` | SVG `height` and `viewBox` height in user units. |
| `empty_message` | `String` | `"No visible data"` | Status text shown when no marks are registered. |
| `animate` | `bool` | `false` | Enables CSS transitions on marks via `data-animate`. |
| `animation_duration` | `f64` | `160` | Transition duration in milliseconds. |
| `animation_easing` | `String` | `"ease"` | CSS easing function for the transition. |

## Layers

The SVG tree has seven paint-order groups rendered in this order:

1. `grid`
2. `reference-background`
3. `series`
4. `markers`
5. `reference-foreground`
6. `annotations`
7. `labels`

Each mark type maps to a fixed group: guides → `grid`, lines → `series`,
markers → `markers`, icons → `annotations`, labels → `labels`. Only
`pine-chart-reference-dot` is layer-selectable.

SVG does not support CSS `z-index` for children inside one `<svg>`. Use the
`layer` prop on `pine-chart-reference-dot` to place a dot in front of or behind
the series path instead of CSS z-index.

`pine-chart-reference-dot` accepts `"reference-background"` / `"background"` and
`"reference-foreground"` / `"foreground"` for its `layer` prop. Omitting `layer`
defaults to `reference-background`. Any other value marks the chart invalid
instead of silently painting in the wrong bucket.

`pine-chart-layer` is an optional grouping element. Nesting marks inside
`<pine-chart-layer name="...">` does not change where non-reference-dot marks
are painted; it provides organizational structure in the host tree and passes a
layer-name context that `pine-chart-reference-dot` can inherit when its own
`layer` prop is empty.

## Data

Line points are plain data:

```rust
use pine_charts::ChartLayerPoint;

let line_a = vec![
    ChartLayerPoint::new(100.0, 120.0),
    ChartLayerPoint::new(220.0, 120.0),
    ChartLayerPoint::new(340.0, 180.0),
];
```

Other marks are authored directly in markup. Use `key` values that
remain stable while the component is mounted; changing a key is treated as a new
mark.
