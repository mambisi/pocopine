# Layered Charts

`pine-layer-chart` is the composable SVG chart primitive. It follows the Pine UI
compound-component pattern: the root owns state and rendering, while child
components describe marks.

```html
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
- `pine-chart-icon`: annotation icon. `kind="plane"` is built in for now.

## Layers

Layer names map to SVG paint-order buckets:

1. `grid`
2. `reference-background`
3. `series`
4. `markers`
5. `reference-foreground`
6. `annotations`
7. `labels`

SVG does not support CSS `z-index` for children inside one `<svg>`. Use
`pine-chart-layer` or the `layer` prop on `pine-chart-reference-dot` instead of
CSS z-index.

## Data

Line points are intentionally plain data:

```rust
use pine_charts::ChartLayerPoint;

let line_a = vec![
    ChartLayerPoint::new(100.0, 120.0),
    ChartLayerPoint::new(220.0, 120.0),
    ChartLayerPoint::new(340.0, 180.0),
];
```

Other marks can usually be authored directly in markup. Use `key` values that
remain stable while the component is mounted; changing a key is treated as a new
mark.
