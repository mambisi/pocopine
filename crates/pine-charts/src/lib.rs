//! SVG-first chart primitives for Pine.
//!
//! This crate starts with testable geometry, scale, and path helpers.
//! Pine components are layered on top of these primitives so chart behavior
//! can be checked without a browser and without committing to canvas.

pub mod error;
pub mod geometry;
pub mod line;
pub mod path;
pub mod scale;

pub use error::{ChartError, ChartResult};
pub use geometry::{ChartMargins, ChartRect, Point};
pub use line::{
    ChartPoint, LineChartGeometry, LineChartOptions, PineLineChart, SvgLine, SvgTickLabel,
};
pub use path::{area_path, line_path};
pub use scale::{BandScale, LinearScale, Tick};

/// Register every Pine Charts custom-element tag.
pub fn register_all() {
    PineLineChart::register();
}
