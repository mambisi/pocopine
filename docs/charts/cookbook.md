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

## Custom Tooltip And Drilldown

Use `tooltip="none"` when the built-in tooltip is too small for the product UI.
The chart still owns hit testing, crosshair placement, hover markers, keyboard
selection, and typed event payloads. The application owns the HTML surface.

```html
<section class="metric-card">
  <pine-chart-responsive aspect_ratio="1.7" min_height="220">
    <pine-area-chart
      label="Render latency"
      pp-bind:series="latency_series"
      x_label="Week"
      y_label="Latency"
      show_markers="true"
      tooltip="none"
      @pp:chart:hover="show_latency_tooltip"
      @pp:chart:hover-end="hide_latency_tooltip"
      @pp:chart:select="show_latency_detail"
      @pp:chart:select-end="hide_latency_detail"></pine-area-chart>
  </pine-chart-responsive>

  <div
    class="metric-tooltip"
    role="status"
    aria-live="polite"
    :data-visible="tooltip_visible"
    :style="tooltip_visible ? tooltip_style : ''">
    <strong pp-text="tooltip_value"></strong>
    <span pp-text="tooltip_meta"></span>
  </div>

  <aside
    class="metric-detail"
    role="status"
    aria-live="polite"
    :data-visible="detail_visible">
    <strong pp-text="detail_value"></strong>
    <span pp-text="detail_meta"></span>
  </aside>
</section>
```

```rust
use pine_charts::{ChartHover, ChartSelection};
use pocopine::prelude::JsValue;

pub fn show_latency_tooltip(&mut self, event: JsValue) {
    let Some(hover) = ChartHover::from_event_value(event) else {
        return;
    };

    self.tooltip_visible = true;
    self.tooltip_value = hover.display_value();
    self.tooltip_meta = hover.aria_label;
    self.tooltip_style = hover.tooltip_style;
}

pub fn hide_latency_tooltip(&mut self) {
    self.tooltip_visible = false;
}

pub fn show_latency_detail(&mut self, event: JsValue) {
    let Some(selection) = ChartSelection::from_event_value(event) else {
        return;
    };

    self.detail_visible = true;
    self.detail_value = selection.display_value();
    self.detail_meta = format!("Selected {}", selection.key);
}

pub fn hide_latency_detail(&mut self) {
    self.detail_visible = false;
}
```

```css
.metric-card {
  position: relative;
}

.pine-chart-root[data-tooltip="none"] .pine-chart-tooltip {
  display: none;
}

.metric-tooltip {
  left: var(--pine-chart-tooltip-x);
  opacity: 0;
  position: absolute;
  top: var(--pine-chart-tooltip-y);
  transform: translate(10px, calc(-100% - 10px));
  visibility: hidden;
}

.metric-tooltip[data-visible="true"] {
  opacity: 1;
  visibility: visible;
}
```

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
