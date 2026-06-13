//! SVG-first chart primitives for Pine.
//!
//! This crate starts with testable geometry, scale, and path helpers.
//! Pine components are layered on top of these primitives so chart behavior
//! can be checked without a browser and without committing to canvas.

mod animation;
pub mod area;
pub mod bar;
mod cartesian;
pub mod cartesian_chart;
pub mod error;
pub mod events;
pub mod geometry;
pub mod layered;
pub mod legend;
pub mod line;
mod marks;
pub mod path;
pub mod pie;
pub mod radial;
pub mod responsive;
pub mod scale;
pub mod scatter;
pub mod svg;
mod visibility;

pub use area::{
    AreaChartGeometry, AreaChartSeriesRender, ChartAreaSeries, PineAreaChart, area_legend_items,
};
pub use bar::{
    BarChartGeometry, BarChartMode, BarChartOptions, ChartBar, ChartBarSeries, PineBarChart,
    SvgBar, bar_legend_items,
};
pub use cartesian_chart::{
    CartesianAreaSeriesConfig, CartesianAreaSeriesRender, CartesianAxisConfig, CartesianBarRender,
    CartesianBarSeriesConfig, CartesianChartInputs, CartesianChartOptions, CartesianChartRender,
    CartesianGridConfig, CartesianLineSeriesConfig, CartesianLineSeriesRender,
    CartesianMarkerRender, CartesianReferenceDotConfig, CartesianReferenceDotRender,
    CartesianReferenceLabelConfig, CartesianReferenceLabelRender, CartesianReferenceLineConfig,
    CartesianReferenceLineRender, CartesianScatterPointRender, CartesianScatterSeriesConfig,
    PineAreaSeries, PineBarSeries, PineCartesianChart, PineCartesianReferenceDot,
    PineCartesianReferenceLabel, PineCartesianReferenceLine, PineChartGrid, PineLineSeries,
    PineScatterSeries, PineXAxis, PineYAxis, render_cartesian_chart,
};
pub use error::{ChartError, ChartResult};
pub use events::{
    CHART_HOVER_END_EVENT, CHART_HOVER_EVENT, CHART_SELECT_END_EVENT, CHART_SELECT_EVENT,
    ChartHover, ChartHoverEnd, ChartSelection, ChartSelectionEnd, LEGEND_TOGGLE_EVENT,
    LegendToggle,
};
pub use geometry::{ChartMargins, ChartRect, Point};
pub use layered::{
    ChartLayerGuide, ChartLayerIcon, ChartLayerLabel, ChartLayerLine, ChartLayerMarker,
    ChartLayerPoint, ChartLayerReferenceDot, LayerChartInputs, LayerChartRender, PineChartGuide,
    PineChartIcon, PineChartLabel, PineChartLayer, PineChartLine, PineChartMarker,
    PineChartReferenceDot, PineLayerChart, SvgLayerGuide, SvgLayerIcon, SvgLayerLabel,
    SvgLayerLine, SvgLayerMarker, SvgLayerReferenceDot, render_layer_chart,
};
pub use legend::{LegendItem, PineChartLegend};
pub use line::{
    ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions, LineChartSample,
    LineChartSeriesRender, PineLineChart, line_legend_items, nearest_line_sample,
    nearest_line_sample_at,
};
pub use path::{area_path, line_path};
pub use pie::{
    ChartPieSlice, PieChartGeometry, PieChartOptions, PinePieChart, SvgPieSlice, pie_legend_items,
};
pub use radial::{
    ChartRadialBar, PineRadialBarChart, RadialBarChartGeometry, RadialBarChartOptions,
    SvgRadialBar, radial_bar_legend_items,
};
pub use responsive::{PineChartResponsive, ResponsiveChartSize};
pub use scale::{BandScale, LinearScale, Tick};
pub use scatter::{
    ChartScatterSeries, PineScatterChart, ScatterChartGeometry, ScatterChartOptions,
    ScatterChartSample, ScatterChartSeriesRender, nearest_scatter_sample, scatter_legend_items,
};
pub use svg::{SvgAxisLabel, SvgLine, SvgTickLabel};
pub use visibility::{
    set_area_series_visible, set_bar_series_visible, set_line_series_visible,
    set_pie_slice_visible, set_radial_bar_visible, set_scatter_series_visible,
    toggle_area_series_visibility, toggle_bar_series_visibility, toggle_line_series_visibility,
    toggle_pie_slice_visibility, toggle_radial_bar_visibility, toggle_scatter_series_visibility,
};

/// Register every Pine Charts custom-element tag.
pub fn register_all() {
    PineAreaChart::register();
    PineAreaSeries::register();
    PineBarChart::register();
    PineBarSeries::register();
    PineCartesianChart::register();
    PineCartesianReferenceDot::register();
    PineCartesianReferenceLabel::register();
    PineCartesianReferenceLine::register();
    PineChartGuide::register();
    PineChartGrid::register();
    PineChartIcon::register();
    PineChartLabel::register();
    PineChartLayer::register();
    PineChartLegend::register();
    PineChartLine::register();
    PineChartMarker::register();
    PineChartReferenceDot::register();
    PineLayerChart::register();
    PineLineSeries::register();
    PineChartResponsive::register();
    PineLineChart::register();
    PinePieChart::register();
    PineRadialBarChart::register();
    PineScatterChart::register();
    PineScatterSeries::register();
    PineXAxis::register();
    PineYAxis::register();
}
