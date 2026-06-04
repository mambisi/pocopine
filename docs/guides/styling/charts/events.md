---
title: "Chart Events"
description: "Pine Charts emits bubbling CustomEvents for interactions that applications usually connect to detail panels, filters, or route changes."
---

# Chart Events

Pine Charts emits bubbling `CustomEvent`s for interactions that applications
connect to detail panels, filters, or route changes.

## Selection

Line markers, area markers, scatter points, bars, pie/donut slices, and radial
bars emit `pp:chart:select` when selected by pointer or keyboard. The payload
type is `ChartSelection`:

```rust
use pine_charts::{ChartSelection, ChartSelectionEnd};
```

Payload fields:

- `chart`: `"line"`, `"area"`, `"scatter"`, `"bar"`, `"pie"`, or `"radial"`
- `kind`: `"xy"`, `"category"`, or `"share"`
- `key`: stable rendered mark key
- `label`: human-readable selection label
- `aria_label`: rendered mark accessibility label
- `series`: series label when present
- `category`: categorical label for bars
- `x` / `y`: numeric coordinates for line and scatter charts
- `value`: numeric value for bars, pie/donut slices, and radial bars
- `percentage`: share percentage for pie/donut slices and radial bars
- `x_label` / `y_label`: formatted point coordinates for `xy` selections
- `value_label`: formatted value for `category` and `share` selections
- `percentage_label`: formatted percentage for `share` selections

Template listeners use the event name directly:

```poco
<pine-line-chart
  pp-bind:series="series"
  show_markers="true"
  @pp:chart:select="show_detail"></pine-line-chart>
```

Selection is persistent: the selected mark retains `data-selected` and
`aria-selected="true"` until another mark is selected or selection is cleared.
Charts emit `pp:chart:select-end` with `ChartSelectionEnd { chart, key }` when
selection is cleared by Escape, a background chart click, or a data update that
removes the selected mark. Use `pp:chart:select` for drilldown/detail panels and
`pp:chart:select-end` to close those panels.

## Hover

Line, scatter, area, bar, pie/donut, and radial charts emit `pp:chart:hover`
while the pointer is over an interactive mark. Pointer exit emits
`pp:chart:hover-end` with `ChartHoverEnd { chart }`. Hover events are
pointer-driven, so handlers must stay cheap and push expensive work off the
pointer path. The hover payload type is `ChartHover`:

```rust
use pine_charts::{ChartHover, ChartHoverEnd};
use pocopine::prelude::JsValue;
```

Payload fields:

- `chart`: `"line"`, `"scatter"`, `"area"`, `"bar"`, `"pie"`, or `"radial"`
- `kind`: `"xy"`, `"category"`, or `"share"`
- `key`
- `label`
- `aria_label`
- `series`
- `category`
- `x` / `y`
- `value`
- `percentage`
- `x_label` / `y_label`
- `value_label`
- `percentage_label`
- `tooltip_x` / `tooltip_y`
- `tooltip_style`

`kind` determines which fields are populated:

- `xy`: line, scatter, and area hovers. `label` is the compact point label,
  `series` is the series label when present, and `x`, `y`, `x_label`, and
  `y_label` are populated.
- `category`: bar hovers. `label` and `category` are the category label,
  `series` is populated for grouped/stacked series, and `value` and
  `value_label` are populated.
- `share`: pie, donut, and radial hovers. `label` is the slice or ring label,
  `value`, `value_label`, `percentage`, and `percentage_label` are populated,
  and `series` and `category` are empty.

`aria_label` always matches the rendered mark's accessible label. Prefer
`label` for concise custom UI and `aria_label` when mirroring the chart's
accessible announcement.

`ChartHover` and `ChartSelection` expose formatting helpers for custom UI:

- `series_or_chart()` returns the series label when present, otherwise the chart
  kind.
- `display_value()` formats the populated value fields for the payload kind:
  `<x_label>: <y_label>`, `<category>: <value_label>`, or
  `<value_label> (<percentage_label>)`.

The built-in tooltip is on by default. Set `tooltip="none"` on a chart to hide
it while keeping hover crosshairs, markers, data attributes, and hover events
active. Applications can then render a custom tooltip, overlay, or portal from
the event payload. Because Pine Charts does not ship a stylesheet, include the
`[data-tooltip="none"]` suppression rule from [Interaction](interaction.md) when
styling the built-in tooltip.

```poco
<pine-area-chart
  pp-bind:series="series"
  tooltip="none"
  @pp:chart:hover="show_tooltip"
  @pp:chart:hover-end="hide_tooltip"></pine-area-chart>
```

```rust
use pine_charts::ChartHover;
use pocopine::prelude::JsValue;

pub fn show_tooltip(&mut self, event: JsValue) {
    let Some(hover) = ChartHover::from_event_value(event) else {
        return;
    };
    self.tooltip_title = hover.series_or_chart().into();
    self.tooltip_value = hover.display_value();
}
```

When `tooltip="none"` is set, the application owns the replacement tooltip's
accessibility. Use a live region such as `role="status"` and
`aria-live="polite"` when the custom tooltip should be announced.

## Legend Toggles

`PineChartLegend` becomes interactive with `interactive="true"`. Interactive
legend items toggle their own `active` state, expose `data-active`, and emit
`pp:chart:legend-toggle` with `LegendToggle`:

```rust
use pine_charts::LegendToggle;
use pocopine::prelude::JsValue;
```

Payload fields:

- `key`
- `label`
- `series`
- `active`

The legend does not hide chart series on its own. Applications use the event to
decide whether toggling should filter data, dim marks, open a drilldown, or do
nothing.

For the common controlled-filtering pattern, update chart data with the
visibility helpers, then rebuild legend items from that same data:

```rust
use pine_charts::{line_legend_items, set_line_series_visible, LegendToggle};
use pocopine::prelude::JsValue;

pub fn toggle_series(&mut self, event: JsValue) {
    let Some(event) = LegendToggle::from_event_value(event) else {
        return;
    };
    if set_line_series_visible(&mut self.series, &event.key, event.active) {
        self.legend = line_legend_items(&self.series);
    }
}
```
