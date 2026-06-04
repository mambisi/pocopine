---
title: "Chart Foundation"
description: "Pure-Rust geometry, scale, and path helpers that underpin every Pine chart. No browser dependency — testable anywhere Rust runs."
---

# Chart Foundation

The foundation layer is pure Rust with no browser dependency. It covers
geometry, scales, and SVG path generation so the math behind every Pine chart
can be unit-tested before any component or SVG node is mounted.

## Geometry

`ChartMargins` describes the reserved space around a plot area, with named
`top`, `right`, `bottom`, and `left` fields. `ChartRect` describes the usable
plot rectangle after those margins are subtracted from the outer container.

```rust
use pine_charts::{ChartMargins, ChartRect};

let rect = ChartRect::from_outer(
    640.0,
    360.0,
    ChartMargins::new(16.0, 16.0, 32.0, 40.0),
)?;
```

`ChartMargins::new` takes arguments in `(top, right, bottom, left)` order.
`ChartRect::from_outer` requires that the outer dimensions are finite and that
the resulting inner dimensions are positive — it returns a `ChartError` rather
than silently producing invalid SVG.

## Scales

`LinearScale` maps a numeric domain to a pixel range. Passing a reversed range
(`(pixel_max, pixel_min)`) naturally flips the axis, which is the standard
approach for SVG y-axes.

```rust
use pine_charts::LinearScale;

let y = LinearScale::new((0.0, 100.0), (320.0, 0.0))?;
let pixel = y.map(75.0)?;
```

`LinearScale::new` rejects a zero-span domain or range. `map` returns
`ChartResult<f64>`, so non-finite input values are caught at the call site.
`ticks(n)` returns a `Vec<Tick>` with nicely stepped values and their
pre-computed pixel positions.

`BandScale` maps indexed categories into evenly-spaced bands. It accepts
separate `padding_inner` and `padding_outer` ratios and exposes `bandwidth`,
`step`, `position(index)`, and `center(index)` for placing bar rectangles and
axis labels.

```rust
use pine_charts::BandScale;

let scale = BandScale::new(3, (0.0, 300.0), 0.1, 0.2)?;
let x = scale.position(0).unwrap(); // leading edge of first band
let cx = scale.center(0).unwrap();  // center of first band
```

## Paths

`line_path` and `area_path` produce SVG `d` attribute strings from a sequence
of `Point` values. Both return a `ChartError` for an empty series or any
non-finite coordinate.

```rust
use pine_charts::{line_path, Point};

let d = line_path([
    Point::new(0.0, 10.0)?,
    Point::new(20.0, 5.0)?,
])?;
// d == "M0,10 L20,5"
```

`area_path` takes an additional `baseline` argument (the y pixel where the area
closes) and emits a closed path that fills the region between the data line and
the baseline.

```rust
use pine_charts::{area_path, Point};

let d = area_path(
    [Point::new(0.0, 10.0)?, Point::new(20.0, 5.0)?],
    30.0,
)?;
// d == "M0,30 L0,10 L20,5 L20,30Z"
```

Chart components consume these helpers directly rather than duplicating SVG path
logic.

## Cartesian Internals

Line, area, and scatter charts share a single crate-private Cartesian layout
path. It computes plot rectangles, linear scales, tick sets, grid lines, axis
lines, and hover placement from width, height, margins, and domain values.
Bar charts and line/bar combos use a parallel categorical path built on
`BandScale` for the x-axis.

This module is intentionally crate-private. Public components stabilize their
own chart-specific props first; if enough primitives converge on the same
extension point, the shared contract is promoted deliberately rather than
through early leakage.
