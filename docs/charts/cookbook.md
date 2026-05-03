# Cookbook

These snippets show the intended composition style for application dashboards:
chart behavior stays in Pine Charts, while spacing, color, cards, and layout
stay in application CSS.

## Dashboard Card

Use a responsive wrapper for each chart surface, then place legends wherever
the application layout needs them.

```html
<section class="metrics-grid">
  <pine-chart-responsive class="metric-card" aspect_ratio="2" min_height="220">
    <pine-line-chart
      label="Weekly revenue"
      pp-bind:series="revenue_series"
      x_label="Week"
      y_label="Revenue"
      show_markers="true"></pine-line-chart>
  </pine-chart-responsive>

  <pine-chart-legend
    class="metric-legend"
    label="Weekly revenue legend"
    pp-bind:items="revenue_legend"></pine-chart-legend>

  <pine-chart-responsive
    class="metric-card metric-card--radial"
    width="min(420px, 100%)"
    aspect_ratio="1"
    min_height="240">
    <pine-pie-chart
      label="Channel share"
      pp-bind:data="channel_slices"
      inner_radius="0.58"
      pp-bind:center_value="channel_total"
      center_label="Total"></pine-pie-chart>
  </pine-chart-responsive>

  <pine-chart-legend
    class="metric-legend"
    label="Channel share legend"
    pp-bind:items="channel_legend"></pine-chart-legend>
</section>
```

```css
.metrics-grid {
  display: grid;
  gap: 16px;
}

.metric-card {
  box-sizing: border-box;
  border: 1px solid #dde4ec;
  border-radius: 8px;
  padding: 20px;
}

.metric-card--radial {
  margin-inline: auto;
}

.pine-chart-responsive-frame {
  min-width: 0;
}

.pine-chart-responsive-frame > pine-line-chart,
.pine-chart-responsive-frame > pine-pie-chart,
.metric-card .pine-line-chart,
.metric-card .pine-pie-chart {
  display: block;
  height: 100%;
  position: relative;
  width: 100%;
}

.metric-card .pine-chart-svg {
  display: block;
  height: auto;
  max-width: 100%;
  overflow: visible;
}
```

The important rule is that the responsive component owns the chart pixel box.
Application CSS can decorate the panel, but should not force the SVG to
`width: 100%; height: 100%`; doing so can stretch text and circular marks.

## Half Donut Progress

Half donuts use the same `PinePieChart` component as full donuts. The angle
range controls the visible arc:

```html
<pine-chart-responsive
  class="metric-card metric-card--radial"
  width="min(420px, 100%)"
  aspect_ratio="1"
  min_height="240">
  <pine-pie-chart
    label="Goal progress"
    pp-bind:data="progress_slices"
    inner_radius="0.58"
    start_angle="180"
    end_angle="360"
    center_value="74%"
    center_label="Progress"></pine-pie-chart>
</pine-chart-responsive>
```

For half donuts, Pine Charts keeps `center_label` on the chart center line and
places `center_value` above it. That keeps the text attached to the ring rather
than visually centered in the empty lower half of the SVG.
