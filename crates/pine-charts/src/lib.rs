//! SVG-first chart primitives for Pine.
//!
//! This crate starts with testable geometry, scale, and path helpers.
//! Pine components are layered on top of these primitives so chart behavior
//! can be checked without a browser and without committing to canvas.

pub mod area;
pub mod bar;
mod cartesian;
pub mod error;
pub mod events;
pub mod geometry;
pub mod legend;
pub mod line;
pub mod path;
pub mod scale;
pub mod scatter;
pub mod svg;

pub use area::{
    area_legend_items, AreaChartGeometry, AreaChartSeriesRender, ChartAreaSeries, PineAreaChart,
};
pub use bar::{
    bar_legend_items, BarChartGeometry, BarChartMode, BarChartOptions, ChartBar, ChartBarSeries,
    PineBarChart, SvgBar,
};
pub use error::{ChartError, ChartResult};
pub use events::{ChartSelection, LegendToggle, CHART_SELECT_EVENT, LEGEND_TOGGLE_EVENT};
pub use geometry::{ChartMargins, ChartRect, Point};
pub use legend::{LegendItem, PineChartLegend};
pub use line::{
    line_legend_items, nearest_line_sample, nearest_line_sample_at, ChartLineSeries, ChartPoint,
    LineChartGeometry, LineChartOptions, LineChartSample, LineChartSeriesRender, PineLineChart,
};
pub use path::{area_path, line_path};
pub use scale::{BandScale, LinearScale, Tick};
pub use scatter::{
    nearest_scatter_sample, scatter_legend_items, ChartScatterSeries, PineScatterChart,
    ScatterChartGeometry, ScatterChartOptions, ScatterChartSample, ScatterChartSeriesRender,
};
pub use svg::{SvgAxisLabel, SvgLine, SvgTickLabel};

/// Register every Pine Charts custom-element tag.
pub fn register_all() {
    PineAreaChart::register();
    PineBarChart::register();
    PineChartLegend::register();
    PineLineChart::register();
    PineScatterChart::register();
}
