# Chart Events

Pine Charts emits bubbling `CustomEvent`s for interactions that applications
usually connect to detail panels, filters, or route changes.

## Selection

Line markers, scatter points, and bars emit `pp:chart:select` when selected by
pointer or keyboard. The event payload is `ChartSelection`:

```rust
use pine_charts::ChartSelection;
```

Payload fields:

- `chart`: `"line"`, `"scatter"`, or `"bar"`
- `key`: stable rendered mark key
- `label`: human-readable mark label
- `series`: series label when present
- `category`: categorical label for bars
- `x` / `y`: numeric coordinates for line and scatter charts
- `value`: numeric value for bars and future share charts
- `percentage`: percentage for future share charts

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
```

Payload fields:

- `key`
- `label`
- `series`
- `active`

The legend intentionally does not hide chart series by itself. Applications use
the event to decide whether toggling should filter data, dim marks, open a
drilldown, or do nothing.
