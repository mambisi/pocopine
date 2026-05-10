# Interaction

Chart interaction is hook-first. Pine Charts owns the geometry, hit testing,
keyboard focus, selection bookkeeping, and typed event payloads. Applications
own the visible product behavior: the tooltip body, detail panel, drilldown,
filtering policy, and any route or analytics side effects.

## Ownership Contract

| Pine Charts owns | Application owns |
| --- | --- |
| Pointer-to-SVG coordinate conversion | Card and dashboard layout |
| Plot-bound hit testing | Tooltip markup and visual styling |
| Crosshair, hover marker, and focused/selected mark state | Selection detail panels and drilldown |
| `ChartHover`, `ChartSelection`, and `LegendToggle` payloads | Data filtering and domain-specific labels |
| ARIA labels, `data-*` hooks, and keyboard affordances | Live-region copy when replacing the built-in tooltip |

This keeps the primitives usable by default while still letting an application
compose its own interaction surface around them.

## Hover

Pointer movement over the chart SVG is converted into SVG-space coordinates.
Hover activates only while the pointer is inside the plot rectangle, not while
it is over margins, axes, or tick labels.

Line, scatter, and area charts select the nearest sampled point by SVG distance.
Bar charts select the painted SVG rect under the pointer. Pie and donut charts
select the hovered slice. All of them emit `pp:chart:hover` while hover is
visible and `pp:chart:hover-end` when it clears.

The built-in hover surface exposes:

- `.pine-chart-hover`
- `.pine-chart-crosshair`
- `.pine-chart-hover-marker`
- `.pine-chart-tooltip`
- `.pine-chart-tooltip-series`
- `.pine-chart-tooltip-x`
- `.pine-chart-tooltip-y`
- `data-hover`
- `data-tooltip="default|none"`
- `data-tooltip-x="left|right"`
- `data-tooltip-y="above|below"`
- `data-x`
- `data-y`
- `data-series`
- CSS variables `--pine-chart-tooltip-x` and `--pine-chart-tooltip-y`

Set `tooltip="none"` when the application should render its own tooltip from
events. That suppresses only the built-in HTML tooltip; hover markers,
crosshairs, and data attributes still update. Because Pine Charts does not ship
a stylesheet, application CSS must include the hide rule:

```css
.pine-chart-root[data-tooltip="none"] .pine-chart-tooltip {
  display: none;
}
```

When `tooltip="none"` is used, the application is responsible for any live
region used by the replacement tooltip.

```html
<pine-area-chart
  label="Trend area"
  pp-bind:series="area_series"
  show_markers="true"
  tooltip="none"
  @pp:chart:hover="show_custom_tooltip"
  @pp:chart:hover-end="hide_custom_tooltip"></pine-area-chart>

<div
  class="chart-custom-tooltip"
  role="status"
  aria-live="polite"
  :data-visible="custom_tooltip_visible"
  :data-tooltip-x="custom_tooltip_x"
  :data-tooltip-y="custom_tooltip_y"
  :style="custom_tooltip_visible ? custom_tooltip_style : ''">
  <span pp-text="custom_tooltip_title"></span>
  <strong pp-text="custom_tooltip_value"></strong>
</div>
```

```rust
use pine_charts::ChartHover;
use pocopine::prelude::JsValue;

pub fn show_custom_tooltip(&mut self, event: JsValue) {
    let Some(hover) = ChartHover::from_event_value(event) else {
        return;
    };

    self.custom_tooltip_visible = true;
    self.custom_tooltip_title = if hover.series.is_empty() {
        hover.chart
    } else {
        hover.series
    };
    self.custom_tooltip_value = match hover.kind.as_str() {
        "xy" => format!("{}: {}", hover.x_label, hover.y_label),
        "category" => format!("{}: {}", hover.category, hover.value_label),
        "share" => format!("{} ({})", hover.value_label, hover.percentage_label),
        _ => hover.label,
    };
    self.custom_tooltip_x = hover.tooltip_x;
    self.custom_tooltip_y = hover.tooltip_y;
    self.custom_tooltip_style = hover.tooltip_style;
}

pub fn hide_custom_tooltip(&mut self) {
    self.custom_tooltip_visible = false;
}
```

## Selection And Drilldown

Line markers, area markers, scatter points, bars, and pie/donut slices expose a
small selection contract. The chart root is keyboard focusable. Arrow keys move
the focused item, Enter or Space selects it, and Escape clears selection. Line
and area plot clicks select the current hovered sample. Scatter, bar, and pie
clicks select the clicked point, bar, or slice. Background chart clicks clear
selection when there is no hovered sample.

Selection emits a bubbling `pp:chart:select` event, and clearing emits
`pp:chart:select-end`. Rendered selectable marks expose:

- `data-key`
- `data-focused`
- `data-selected`
- `aria-selected="true|false"`

Line and area selection uses sampled data. Use `show_markers="true"` when the
selected/focused sample should have a visible mark; keyboard selection still
tracks sampled line data even when markers are hidden.

```html
<pine-area-chart
  label="Trend area"
  pp-bind:series="area_series"
  show_markers="true"
  @pp:chart:select="show_selection_detail"
  @pp:chart:select-end="hide_selection_detail"></pine-area-chart>

<aside
  class="chart-selection-detail"
  role="status"
  aria-live="polite"
  :data-visible="selection_visible">
  <span pp-text="selection_title"></span>
  <strong pp-text="selection_value"></strong>
  <span pp-text="selection_meta"></span>
</aside>
```

```rust
use pine_charts::ChartSelection;
use pocopine::prelude::JsValue;

pub fn show_selection_detail(&mut self, event: JsValue) {
    let Some(selection) = ChartSelection::from_event_value(event) else {
        return;
    };

    self.selection_visible = true;
    self.selection_title = if selection.series.is_empty() {
        selection.chart
    } else {
        selection.series
    };
    self.selection_value = match selection.kind.as_str() {
        "xy" => format!("{}: {}", selection.x_label, selection.y_label),
        "category" => format!("{}: {}", selection.category, selection.value_label),
        "share" => format!("{} ({})", selection.value_label, selection.percentage_label),
        _ => selection.label,
    };
    self.selection_meta = format!("Selected {}", selection.key);
}

pub fn hide_selection_detail(&mut self) {
    self.selection_visible = false;
}
```

## Legend Filtering

Interactive legends are opt-in with `interactive="true"`. The legend gives each
item keyboard focus, toggles `data-active`, and emits
`pp:chart:legend-toggle`. The chart data remains application-owned; the event is
the hook for filtering, dimming, routing, or analytics.

```html
<pine-chart-legend
  label="Trend area legend"
  interactive="true"
  pp-bind:items="area_legend"
  @pp:chart:legend-toggle="toggle_area_series"></pine-chart-legend>
```

```rust
use pine_charts::LegendToggle;
use pocopine::prelude::JsValue;

pub fn toggle_area_series(&mut self, event: JsValue) {
    let Some(toggle) = LegendToggle::from_event_value(event) else {
        return;
    };

    if set_area_series_visible(&mut self.area_series, &toggle.key, toggle.active) {
        self.area_legend = area_legend_items(&self.area_series);
    }
}
```

## Animation Hooks

Set `animate="true"` on a chart when CSS transitions should use the chart's
animation variables. Keyframe entry animations should target keyed marks as they
enter the DOM; add/remove updates then animate only the new line, area, bar,
point, or pie segment instead of restarting the whole chart.

Pie/donut slices expose `data-entering="true"` during entry. Area series and
pie/donut slices expose `data-leaving="true"` during exit so CSS or
renderer-owned animation can show removal before the renderer prunes the mark.
Pie/donut enter, exit, and shape changes are rendered by interpolating sector
geometry in component state, so the visible path itself sweeps between sector
angles and radii.

## Styling

```css
.pine-line-chart {
  position: relative;
}

.pine-chart-tooltip {
  left: var(--pine-chart-tooltip-x);
  opacity: 0;
  position: absolute;
  top: var(--pine-chart-tooltip-y);
  transform: translate(10px, calc(-100% - 10px));
  transition: opacity var(--pine-chart-animation-duration, 120ms)
    var(--pine-chart-animation-easing, ease);
  visibility: hidden;
}

.pine-chart-root[data-hover] .pine-chart-tooltip {
  opacity: 1;
  visibility: visible;
}

.pine-chart-root[data-tooltip="none"] .pine-chart-tooltip {
  display: none;
}

.pine-chart-tooltip[data-tooltip-x="left"] {
  transform: translate(calc(-100% - 10px), calc(-100% - 10px));
}

.pine-chart-tooltip[data-tooltip-y="below"] {
  transform: translate(10px, 10px);
}

.pine-chart-tooltip[data-tooltip-x="left"][data-tooltip-y="below"] {
  transform: translate(calc(-100% - 10px), 10px);
}

.pine-chart-bar[data-hovered],
.pine-chart-pie-slice[data-hovered] {
  opacity: 1;
  stroke: currentColor;
}

.pine-chart-marker[data-focused],
.pine-chart-point[data-focused],
.pine-chart-bar[data-focused],
.pine-chart-pie-slice[data-focused] {
  stroke-dasharray: 3 2;
}

.pine-chart-marker[data-selected],
.pine-chart-point[data-selected],
.pine-chart-bar[data-selected],
.pine-chart-pie-slice[data-selected] {
  stroke-width: 3;
}
```
