# Chart Events

Pine Charts emits bubbling `CustomEvent`s for interactions that applications
usually connect to detail panels, filters, or route changes.

## Selection

Line markers, scatter points, bars, and pie/donut slices emit
`pp:chart:select` when selected by pointer or keyboard. The event payload is
`ChartSelection`:

```rust
use pine_charts::ChartSelection;
```

Payload fields:

- `chart`: `"line"`, `"scatter"`, `"bar"`, or `"pie"`
- `key`: stable rendered mark key
- `label`: human-readable mark label
- `series`: series label when present
- `category`: categorical label for bars
- `x` / `y`: numeric coordinates for line and scatter charts
- `value`: numeric value for bars and pie/donut slices
- `percentage`: share percentage for pie/donut slices

Template listeners use the event name directly:

```html
<pine-line-chart
  pp-bind:series="series"
  show_markers="true"
  @pp:chart:select="show_detail"></pine-line-chart>
```

## Hover

Line, scatter, area, bar, and pie/donut charts emit `pp:chart:hover` while the
pointer is over an interactive chart mark. Pointer exit emits
`pp:chart:hover-end`. Hover events are pointer-driven, so custom handlers should
stay cheap and push expensive work outside the pointer path. The hover payload
is `ChartHover`:

```rust
use pine_charts::ChartHover;
use pocopine::prelude::JsValue;
```

Payload fields:

- `chart`: `"line"`, `"scatter"`, `"area"`, `"bar"`, or `"pie"`
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
  `series` is populated for grouped/stacked series, and `value` /
  `value_label` are populated.
- `share`: pie and donut hovers. `label` is the slice label, `value`,
  `value_label`, `percentage`, and `percentage_label` are populated, and
  `series` / `category` are empty.

`aria_label` always matches the rendered mark's accessible label. Prefer
`label` for concise custom UI and `aria_label` when mirroring the chart's
accessible announcement.

The built-in tooltip remains the default. Set `tooltip="none"` on a chart to
hide the built-in tooltip while keeping hover crosshairs, markers, data
attributes, and hover events active. Applications can then render a custom
tooltip block, overlay, or portal from the event payload. Because Pine Charts
does not ship a stylesheet, include the `[data-tooltip="none"]` suppression rule
from [Interaction](interaction.md) when styling the built-in tooltip.

```html
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
    self.tooltip_title = hover.series;
    self.tooltip_value = format!("{}: {}", hover.x_label, hover.y_label);
}
```

When `tooltip="none"` is set, the application owns the replacement tooltip's
accessibility. Use a status/live region such as `role="status"` and
`aria-live="polite"` when the custom tooltip should be announced.

## Legend Toggles

`PineChartLegend` can be made interactive with `interactive="true"`. Interactive
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

The legend intentionally does not hide chart series by itself. Applications use
the event to decide whether toggling should filter data, dim marks, open a
drilldown, or do nothing.

For the common controlled-filtering case, update chart data with the visibility
helpers, then rebuild legend items from that same data:

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
