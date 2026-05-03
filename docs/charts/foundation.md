# Chart Foundation

The foundation layer is plain Rust and has no browser dependency. It exists so
the math behind charts can be tested before any Pine component or SVG node is
mounted.

## Geometry

`ChartMargins` describes the reserved space around a plot. `ChartRect` describes
the usable plot rectangle after margins are removed.

```rust
use pine_charts::{ChartMargins, ChartRect};

let rect = ChartRect::from_outer(
    640.0,
    360.0,
    ChartMargins::new(16.0, 16.0, 32.0, 40.0),
)?;
```

The constructor rejects non-finite and non-positive sizes. Charts should fail
early instead of silently emitting invalid SVG.

## Scales

`LinearScale` maps numeric domains into pixel ranges. It supports reversed
ranges for SVG y axes.

```rust
use pine_charts::LinearScale;

let y = LinearScale::new((0.0, 100.0), (320.0, 0.0))?;
let pixel = y.map(75.0)?;
```

`BandScale` maps indexed categories into evenly-spaced bands. It is the base for
bar charts, grouped categories, and categorical axes.

## Paths

`line_path` and `area_path` build SVG path `d` strings from validated points.
The helpers return errors for empty series or non-finite coordinates.

```rust
use pine_charts::{line_path, Point};

let d = line_path([
    Point::new(0.0, 10.0)?,
    Point::new(20.0, 5.0)?,
])?;
```

The next layer should consume these helpers rather than duplicating SVG path
formatting inside components.
