# Responsive Containers

`PineChartResponsive` is the Recharts-style sizing layer for Pine Charts. It
owns browser measurement with `ResizeObserver`, then writes concrete `width`
and `height` props into the slotted chart component. The SVG chart still owns
its geometry, viewBox, pointer math, hover state, and keyboard selection; the
container only decides the current pixel box.

## Basic Shape

```html
<pine-chart-responsive aspect_ratio="2" min_height="220">
  <pine-line-chart
    label="Revenue"
    pp-bind:series="series"
    x_label="Week"
    y_label="Revenue"></pine-line-chart>
</pine-chart-responsive>
```

The wrapper defaults to `width="100%"` and `aspect_ratio="2"`. If `height` is
not provided, height is derived from the measured width and aspect ratio. If a
real measured height exists, the wrapper uses that value instead.

```html
<pine-chart-responsive width="100%" height="360px">
  <pine-bar-chart
    label="Distribution"
    pp-bind:series="series"></pine-bar-chart>
</pine-chart-responsive>
```

Use `min_width` and `min_height` when labels, markers, or pie center text need a
floor on narrow screens.

## Styling

The container is intentionally unthemed. Applications usually style the
responsive wrapper as the chart panel and let the chart fill its content box.

```css
.chart-panel {
  box-sizing: border-box;
  padding: 20px;
}

.pine-chart-responsive-frame {
  min-width: 0;
}

.pine-chart-responsive-frame > pine-line-chart,
.chart-panel .pine-line-chart,
.chart-panel .pine-chart-svg {
  display: block;
}

.chart-panel .pine-chart-svg {
  height: auto;
  max-width: 100%;
}
```

Padding and borders belong on the responsive container. The wrapper measures an
inner frame inside that container, so the chart receives the usable drawing
size rather than the outer card size.

## Contract

- `width`: CSS width for the container, default `100%`.
- `height`: optional CSS height. When omitted or `auto`, `aspect_ratio` can
  produce the height.
- `aspect_ratio`: width divided by height, default `2`.
- `min_width` and `min_height`: minimum measured chart dimensions in CSS
  pixels.
- `style`: optional author style appended after generated layout styles.

The slotted child should be a Pine chart component. For custom SVG content, the
container falls back to writing `width` and `height` attributes on the child or
its first SVG descendant.
