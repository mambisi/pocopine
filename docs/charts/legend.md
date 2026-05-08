# Legends

Legends are separate components, not built into each chart. That keeps layout
author-owned and lets a dashboard place a legend beside several charts, above a
plot, below a dense table, or outside a scroll area.

## Data

Use `LegendItem` directly for custom legends:

```rust
use pine_charts::LegendItem;

let items = vec![LegendItem::new("Organic"), LegendItem::new("Referral")];
```

For bar charts, derive legend items from the same series input:

```rust
use pine_charts::{bar_legend_items, ChartBarSeries};

let series: Vec<ChartBarSeries> = /* application data */;
let legend_items = bar_legend_items(&series);
```

For multi-series line charts, use the matching line helper:

```rust
use pine_charts::{line_legend_items, ChartLineSeries};

let series: Vec<ChartLineSeries> = /* application data */;
let legend_items = line_legend_items(&series);
```

For multi-series area charts, use the area helper:

```rust
use pine_charts::{area_legend_items, ChartAreaSeries};

let series: Vec<ChartAreaSeries> = /* application data */;
let legend_items = area_legend_items(&series);
```

## Component

```html
<pine-chart-legend
  label="Acquisition legend"
  pp-bind:items="legend_items"></pine-chart-legend>
```

Set `interactive="true"` when legend items should be keyboard-focusable toggles:

```html
<pine-chart-legend
  label="Acquisition legend"
  interactive="true"
  pp-bind:items="legend_items"
  @pp:chart:legend-toggle="toggle_series"></pine-chart-legend>
```

The component renders an HTML list with stable styling hooks:

- `.pine-chart-legend`
- `.pine-chart-legend-list`
- `.pine-chart-legend-item`
- `.pine-chart-legend-marker`
- `.pine-chart-legend-label`
- `data-key="<stable legend key>"`
- `data-series="<series label>"`
- `data-active`

Interactive items also expose `aria-pressed`. Toggling emits
`pp:chart:legend-toggle`; the chart does not filter itself.

## Controlled Visibility

Series and pie slices have an explicit `visible` field. Chart renderers skip
items where `visible == false`, and the legend helpers mirror that state through
`LegendItem.active`. Use the toggle event to update the same app-owned data:

```rust
use pine_charts::{
    line_legend_items, set_line_series_visible, ChartLineSeries, LegendItem,
    LegendToggle,
};
use pocopine::prelude::JsValue;

pub struct Metrics {
    series: Vec<ChartLineSeries>,
    legend_items: Vec<LegendItem>,
}

impl Metrics {
    pub fn toggle_series(&mut self, event: JsValue) {
        let Some(event) = LegendToggle::from_event_value(event) else {
            return;
        };
        if set_line_series_visible(&mut self.series, &event.key, event.active) {
            self.legend_items = line_legend_items(&self.series);
        }
    }
}
```

Matching helpers exist for area, scatter, bar, and pie data:
`set_area_series_visible`, `set_scatter_series_visible`,
`set_bar_series_visible`, and `set_pie_slice_visible`.

Markers do not ship with framework colors. Applications map `data-series` to
their palette:

```css
.pine-chart-legend-marker[data-series="Organic"] {
  background: var(--organic-series);
}
```
