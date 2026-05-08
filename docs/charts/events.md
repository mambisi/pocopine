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
