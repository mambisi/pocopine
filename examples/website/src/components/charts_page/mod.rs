//! `/charts/:name` — a per-chart reference page, modelled on the
//! component reference (`/components/:name`): a catalogue sidebar of
//! charts, then the focused chart with a Preview/Code card and an API
//! table. The router remounts this on every URL change, so `on_setup`
//! re-derives content from `:name`.

use pine_charts::{
    ChartAreaSeries, ChartBar, ChartBarSeries, ChartLayerPoint, ChartLineSeries, ChartPieSlice,
    ChartPoint, ChartRadialBar, ChartScatterSeries, LegendItem, area_legend_items,
    bar_legend_items, line_legend_items, pie_legend_items, radial_bar_legend_items,
    scatter_legend_items,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::delegate_nav;
use crate::gen_code::charts;

/// Catalogue of charts. `slug` is the route param and the `code(slug)`
/// snippet key; the live preview is keyed on it via `pp-if`.
struct ChartMeta {
    slug: &'static str,
    title: &'static str,
    tag: &'static str,
    blurb: &'static str,
}

const CHARTS: &[ChartMeta] = &[
    ChartMeta {
        slug: "line",
        title: "Line chart",
        tag: "<pine-line-chart>",
        blurb: "A multi-series line chart with point markers and an animated draw-in.",
    },
    ChartMeta {
        slug: "area",
        title: "Area chart",
        tag: "<pine-area-chart>",
        blurb: "Layered area series with optional point markers and a fade-in.",
    },
    ChartMeta {
        slug: "bar",
        title: "Bar chart",
        tag: "<pine-bar-chart>",
        blurb: "Grouped bars with a per-series legend and a grow-in animation.",
    },
    ChartMeta {
        slug: "stacked-bar",
        title: "Stacked bar",
        tag: "<pine-bar-chart>",
        blurb: "The same bar chart with mode=\"stacked\" — part-to-whole over each bucket.",
    },
    ChartMeta {
        slug: "scatter",
        title: "Scatter plot",
        tag: "<pine-scatter-chart>",
        blurb: "Plots individual (x, y) points to reveal correlation and spread across two axes.",
    },
    ChartMeta {
        slug: "pie",
        title: "Pie chart",
        tag: "<pine-pie-chart>",
        blurb: "A categorical share chart with hover emphasis on each slice.",
    },
    ChartMeta {
        slug: "donut",
        title: "Donut chart",
        tag: "<pine-pie-chart>",
        blurb: "A pie with a hole — set inner_radius and an optional center label / value.",
    },
    ChartMeta {
        slug: "radial",
        title: "Radial bars",
        tag: "<pine-radial-bar-chart>",
        blurb: "Concentric rings compare a few metrics against their max — great for KPI readiness.",
    },
    ChartMeta {
        slug: "cartesian",
        title: "Combo (composable)",
        tag: "<pine-cartesian-chart>",
        blurb: "Compose bar, area, line, and scatter series on shared axes, with reference lines, dots, and labels.",
    },
    ChartMeta {
        slug: "layer",
        title: "Layer (custom)",
        tag: "<pine-layer-chart>",
        blurb: "Hand-place guides, polylines, markers, and labels for custom diagrams — here, a metro map.",
    },
];

/// One sidebar catalogue row.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NavRow {
    pub slug: String,
    pub title: String,
    pub href: String,
    pub active: bool,
}

/// One API-table row (Prop / Type / Description).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PropRow {
    pub key: String,
    pub name: String,
    pub ty: String,
    pub desc: String,
}

impl PropRow {
    fn new(name: &str, ty: &str, desc: &str) -> Self {
        PropRow {
            key: name.into(),
            name: name.into(),
            ty: ty.into(),
            desc: desc.into(),
        }
    }
}

#[derive(Serialize, Deserialize, RouteComponent)]
#[component(
    template = "ChartsPage.poco",
    style = "charts_page.css",
    role = "panel"
)]
pub struct ChartsPage {
    /// Route param `:name` — the chart slug.
    #[prop]
    pub name: String,
    /// Visible tab: `"preview"` | `"code"`.
    pub tab: String,
    pub title: String,
    pub tag: String,
    pub blurb: String,
    pub code: String,
    pub not_found: bool,
    pub nav: Vec<NavRow>,
    pub props: Vec<PropRow>,
    // Live preview data — built once; `pp-if` shows the selected chart.
    pub line_series: Vec<ChartLineSeries>,
    pub line_legend: Vec<LegendItem>,
    pub bar_series: Vec<ChartBarSeries>,
    pub bar_legend: Vec<LegendItem>,
    pub area_series: Vec<ChartAreaSeries>,
    pub area_legend: Vec<LegendItem>,
    pub pie_data: Vec<ChartPieSlice>,
    pub pie_legend: Vec<LegendItem>,
    pub scatter_series: Vec<ChartScatterSeries>,
    pub scatter_legend: Vec<LegendItem>,
    pub radial_data: Vec<ChartRadialBar>,
    pub radial_legend: Vec<LegendItem>,
    /// Center readout for the radial chart (the visible average).
    pub radial_center: String,
    // Composable cartesian combo (bar + area + line + scatter + refs).
    pub combo_bar_data: Vec<ChartBar>,
    pub combo_line_data: Vec<ChartBar>,
    pub combo_area_points: Vec<ChartPoint>,
    pub combo_scatter_points: Vec<ChartPoint>,
    pub combo_goal: f64,
    pub combo_latest_x: f64,
    pub combo_latest_y: f64,
    // Layer chart (metro map) polylines.
    pub metro_line_a: Vec<ChartLayerPoint>,
    pub metro_line_b: Vec<ChartLayerPoint>,
    pub metro_line_c: Vec<ChartLayerPoint>,
}

impl Default for ChartsPage {
    fn default() -> Self {
        let line_series = vec![
            ChartLineSeries::new(
                "Actual",
                points(&[14.0, 18.0, 17.0, 24.0, 31.0, 38.0, 44.0]),
            ),
            ChartLineSeries::new(
                "Target",
                points(&[12.0, 15.0, 19.0, 24.0, 30.0, 37.0, 45.0]),
            ),
        ];
        let area_series = vec![
            ChartAreaSeries::new("Organic", points(&[4.0, 7.0, 9.0, 13.0, 18.0, 25.0, 31.0])),
            ChartAreaSeries::new("Referral", points(&[3.0, 4.0, 6.0, 9.0, 11.0, 14.0, 16.0])),
        ];
        let bar_series = vec![
            ChartBarSeries::new("Organic", bars(&[9.0, 11.0, 12.0, 16.0, 19.0, 23.0])),
            ChartBarSeries::new("Referral", bars(&[5.0, 7.0, 5.0, 8.0, 12.0, 15.0])),
        ];
        let pie_data = vec![
            ChartPieSlice::new("Organic", 42.0),
            ChartPieSlice::new("Referral", 24.0),
            ChartPieSlice::new("Paid", 18.0),
            ChartPieSlice::new("Partner", 10.0),
        ];
        let scatter_series = vec![
            ChartScatterSeries::new(
                "Segment A",
                vec![
                    ChartPoint::new(12.0, 42.0),
                    ChartPoint::new(18.0, 49.0),
                    ChartPoint::new(26.0, 57.0),
                    ChartPoint::new(32.0, 61.0),
                    ChartPoint::new(41.0, 68.0),
                ],
            ),
            ChartScatterSeries::new(
                "Segment B",
                vec![
                    ChartPoint::new(10.0, 35.0),
                    ChartPoint::new(16.0, 39.0),
                    ChartPoint::new(22.0, 44.0),
                    ChartPoint::new(31.0, 47.0),
                    ChartPoint::new(38.0, 52.0),
                ],
            ),
        ];
        let radial_data = vec![
            ChartRadialBar::new("Activation", 84.0, 100.0),
            ChartRadialBar::new("Retention", 68.0, 100.0),
            ChartRadialBar::new("Revenue", 92.0, 100.0),
        ];
        // Combo (cartesian) series, derived from the line data.
        let combo_bar_data = bars_from_points(&line_series[0].data);
        let combo_line_data = bars_from_points(&line_series[1].data);
        let combo_area_points: Vec<ChartPoint> = line_series[0]
            .data
            .iter()
            .map(|p| ChartPoint::new(p.x, p.y * 0.82))
            .collect();
        let combo_scatter_points: Vec<ChartPoint> = line_series[0]
            .data
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 3 == 1)
            .map(|(_, p)| ChartPoint::new(p.x, p.y * 1.05))
            .collect();
        let combo_latest = line_series[0]
            .data
            .last()
            .cloned()
            .unwrap_or_else(|| ChartPoint::new(0.0, 0.0));
        Self {
            name: String::new(),
            tab: "preview".into(),
            title: String::new(),
            tag: String::new(),
            blurb: String::new(),
            code: String::new(),
            not_found: false,
            nav: Vec::new(),
            props: Vec::new(),
            line_legend: line_legend_items(&line_series),
            line_series,
            bar_legend: bar_legend_items(&bar_series),
            bar_series,
            area_legend: area_legend_items(&area_series),
            area_series,
            pie_legend: pie_legend_items(&pie_data),
            pie_data,
            scatter_legend: scatter_legend_items(&scatter_series),
            scatter_series,
            radial_legend: radial_bar_legend_items(&radial_data),
            radial_center: radial_average(&radial_data),
            radial_data,
            combo_goal: combo_latest.y,
            combo_latest_x: combo_latest.x,
            combo_latest_y: combo_latest.y,
            combo_bar_data,
            combo_line_data,
            combo_area_points,
            combo_scatter_points,
            metro_line_a: metro_line_a(),
            metro_line_b: metro_line_b(),
            metro_line_c: metro_line_c(),
        }
    }
}

#[handlers]
impl ChartsPage {
    pub fn on_setup(&mut self) {
        // Bare `/charts` (or an unknown slug) defaults to the first chart.
        if self.name.is_empty() {
            self.name = "line".into();
        }
        self.nav = build_nav(&self.name);

        if let Some(m) = CHARTS.iter().find(|c| c.slug == self.name) {
            self.title = m.title.into();
            self.tag = m.tag.into();
            self.blurb = m.blurb.into();
            self.code = charts::code(&self.name).into();
            self.props = props_for(&self.name);
        } else {
            self.not_found = true;
            self.title = "Unknown chart".into();
            self.tag = String::new();
            self.blurb = String::new();
        }
    }

    pub fn show_preview(&mut self) {
        self.tab = "preview".into();
    }

    pub fn show_code(&mut self) {
        self.tab = "code".into();
    }

    /// Delegated sidebar nav (`<a href="/charts/…">` clicks bubble here).
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        delegate_nav(&ev);
    }
}

fn build_nav(active: &str) -> Vec<NavRow> {
    CHARTS
        .iter()
        .map(|c| NavRow {
            slug: c.slug.into(),
            title: c.title.into(),
            href: format!("/charts/{}", c.slug),
            active: c.slug == active,
        })
        .collect()
}

/// Per-chart props for the API table.
fn props_for(slug: &str) -> Vec<PropRow> {
    let mut rows = vec![PropRow::new(
        "label",
        "String",
        "Accessible title for the chart's <svg>.",
    )];
    match slug {
        "line" => {
            rows.push(PropRow::new(
                "series",
                "Vec<ChartLineSeries>",
                "One entry per plotted line.",
            ));
            rows.push(PropRow::new("x_label / y_label", "String", "Axis titles."));
            rows.push(PropRow::new(
                "show_markers",
                "bool",
                "Draw a dot at each data point.",
            ));
        }
        "bar" | "stacked-bar" => {
            rows.push(PropRow::new(
                "series",
                "Vec<ChartBarSeries>",
                "One entry per bar series.",
            ));
            rows.push(PropRow::new(
                "mode",
                "String",
                "\"grouped\" (default) or \"stacked\".",
            ));
            rows.push(PropRow::new("x_label / y_label", "String", "Axis titles."));
        }
        "area" => {
            rows.push(PropRow::new(
                "series",
                "Vec<ChartAreaSeries>",
                "One entry per area series.",
            ));
            rows.push(PropRow::new("x_label / y_label", "String", "Axis titles."));
            rows.push(PropRow::new(
                "show_markers",
                "bool",
                "Draw a dot at each data point.",
            ));
        }
        "scatter" => {
            rows.push(PropRow::new(
                "series",
                "Vec<ChartScatterSeries>",
                "One entry per point cloud.",
            ));
            rows.push(PropRow::new("x_label / y_label", "String", "Axis titles."));
            rows.push(PropRow::new(
                "point_radius",
                "f64",
                "Dot radius in px (default 4).",
            ));
        }
        "pie" | "donut" => {
            rows.push(PropRow::new(
                "data",
                "Vec<ChartPieSlice>",
                "Flat slices (no x-axis).",
            ));
            rows.push(PropRow::new(
                "inner_radius",
                "f64",
                "0..1 ratio; > 0 makes a donut.",
            ));
            rows.push(PropRow::new(
                "center_label / center_value",
                "String",
                "Optional text in the donut hole.",
            ));
        }
        "radial" => {
            rows.push(PropRow::new(
                "data",
                "Vec<ChartRadialBar>",
                "One ring per metric (value, max).",
            ));
            rows.push(PropRow::new(
                "inner_radius",
                "f64",
                "0..1 ratio of the innermost ring.",
            ));
            rows.push(PropRow::new("ring_gap", "f64", "Gap between rings in px."));
            rows.push(PropRow::new(
                "center_label / center_value",
                "String",
                "Optional center readout.",
            ));
        }
        "cartesian" => {
            rows.push(PropRow::new(
                "<pine-bar-series>",
                "data: Vec<ChartBar>",
                "A series child; also line / area / scatter.",
            ));
            rows.push(PropRow::new(
                "<pine-x-axis> / <pine-y-axis>",
                "label",
                "Axis children; pair with <pine-chart-grid>.",
            ));
            rows.push(PropRow::new(
                "<pine-cartesian-reference-line>",
                "y: f64",
                "A reference line; also -dot and -label.",
            ));
        }
        "layer" => {
            rows.push(PropRow::new(
                "<pine-chart-layer>",
                "name",
                "An ordered draw layer (grid, series, markers…).",
            ));
            rows.push(PropRow::new(
                "<pine-chart-line>",
                "points: Vec<ChartLayerPoint>",
                "A polyline in layer coordinates.",
            ));
            rows.push(PropRow::new(
                "<pine-chart-marker>",
                "x / y / radius",
                "A circle; also -guide / -reference-dot / -label.",
            ));
        }
        _ => {}
    }
    rows.push(PropRow::new(
        "animate",
        "bool",
        "Animate the marks in on mount.",
    ));
    rows.push(PropRow::new(
        "animation_duration",
        "f64",
        "Entrance duration in ms.",
    ));
    rows.push(PropRow::new(
        "animation_easing",
        "String",
        "CSS timing function for the entrance.",
    ));
    rows
}

/// Build a series of `(index, y)` points from a slice of y-values.
fn points(ys: &[f64]) -> Vec<ChartPoint> {
    ys.iter()
        .enumerate()
        .map(|(i, &y)| ChartPoint::new(i as f64, y))
        .collect()
}

/// Build `W1..Wn` labelled bars from a slice of values.
fn bars(values: &[f64]) -> Vec<ChartBar> {
    values
        .iter()
        .enumerate()
        .map(|(i, &v)| ChartBar::new(format!("W{}", i + 1), v))
        .collect()
}

/// The visible average of the radial metrics, as a `NN%` center readout.
fn radial_average(data: &[ChartRadialBar]) -> String {
    let visible: Vec<&ChartRadialBar> = data.iter().filter(|b| b.visible).collect();
    if visible.is_empty() {
        return "0%".into();
    }
    let avg = visible.iter().map(|b| b.value / b.max * 100.0).sum::<f64>() / visible.len() as f64;
    format!("{}%", avg.round())
}

/// `W1..Wn` labelled bars from a slice of `(x, y)` points (cartesian
/// bar/line series take `ChartBar` data).
fn bars_from_points(points: &[ChartPoint]) -> Vec<ChartBar> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| ChartBar::new(format!("W{}", i + 1), p.y))
        .collect()
}

fn metro_line_a() -> Vec<ChartLayerPoint> {
    vec![
        ChartLayerPoint::new(100.0, 120.0),
        ChartLayerPoint::new(220.0, 120.0),
        ChartLayerPoint::new(340.0, 120.0),
        ChartLayerPoint::new(420.0, 180.0),
        ChartLayerPoint::new(480.0, 300.0),
        ChartLayerPoint::new(620.0, 360.0),
        ChartLayerPoint::new(780.0, 360.0),
    ]
}

fn metro_line_b() -> Vec<ChartLayerPoint> {
    vec![
        ChartLayerPoint::new(80.0, 240.0),
        ChartLayerPoint::new(220.0, 240.0),
        ChartLayerPoint::new(420.0, 180.0),
        ChartLayerPoint::new(520.0, 180.0),
        ChartLayerPoint::new(700.0, 240.0),
        ChartLayerPoint::new(840.0, 240.0),
    ]
}

fn metro_line_c() -> Vec<ChartLayerPoint> {
    vec![
        ChartLayerPoint::new(140.0, 360.0),
        ChartLayerPoint::new(300.0, 360.0),
        ChartLayerPoint::new(480.0, 300.0),
        ChartLayerPoint::new(520.0, 180.0),
        ChartLayerPoint::new(620.0, 120.0),
        ChartLayerPoint::new(760.0, 120.0),
    ]
}
