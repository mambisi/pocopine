use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id};
use serde::{Deserialize, Serialize};

use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::Point;
use crate::path::line_path;

const DEFAULT_WIDTH: f64 = 900.0;
const DEFAULT_HEIGHT: f64 = 480.0;
const DEFAULT_LINE_WIDTH: f64 = 3.0;
const DEFAULT_MARKER_RADIUS: f64 = 4.0;
const DEFAULT_REFERENCE_RADIUS: f64 = 12.0;
const DEFAULT_ICON_SCALE: f64 = 0.12;

create_context!(ROOT: Handle<PineLayerChart>);
create_context!(LAYER: String);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerPoint {
    pub x: f64,
    pub y: f64,
}

impl ChartLayerPoint {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn validate(self, x_field: &'static str, y_field: &'static str) -> ChartResult<Self> {
        Ok(Self {
            x: finite(x_field, self.x)?,
            y: finite(y_field, self.y)?,
        })
    }
}

impl From<ChartLayerPoint> for Point {
    fn from(point: ChartLayerPoint) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerGuide {
    pub key: String,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerLine {
    pub key: String,
    pub label: String,
    pub color: String,
    pub stroke_width: f64,
    pub points: Vec<ChartLayerPoint>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerMarker {
    pub key: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerReferenceDot {
    pub key: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub layer: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerLabel {
    pub key: String,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub dx: f64,
    pub dy: f64,
    pub angle: f64,
    pub fill: String,
    pub text_anchor: String,
    pub font_weight: String,
}

impl Default for ChartLayerLabel {
    fn default() -> Self {
        Self {
            key: String::new(),
            text: String::new(),
            x: 0.0,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            angle: 0.0,
            fill: "currentColor".into(),
            text_anchor: "middle".into(),
            font_weight: "600".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartLayerIcon {
    pub key: String,
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub fill: String,
}

impl Default for ChartLayerIcon {
    fn default() -> Self {
        Self {
            key: String::new(),
            kind: "plane".into(),
            x: 0.0,
            y: 0.0,
            scale: DEFAULT_ICON_SCALE,
            fill: "currentColor".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLayerGuide {
    pub key: String,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLayerLine {
    pub key: String,
    pub label: String,
    pub d: String,
    pub color: String,
    pub stroke_width: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLayerMarker {
    pub key: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub aria_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLayerReferenceDot {
    pub key: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub layer: String,
    pub aria_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLayerLabel {
    pub key: String,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub fill: String,
    pub text_anchor: String,
    pub font_weight: String,
    pub transform: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLayerIcon {
    pub key: String,
    pub kind: String,
    pub transform: String,
    pub fill: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerChartRender {
    pub view_box: String,
    pub guides: Vec<SvgLayerGuide>,
    pub lines: Vec<SvgLayerLine>,
    pub markers: Vec<SvgLayerMarker>,
    pub reference_dots: Vec<SvgLayerReferenceDot>,
    pub labels: Vec<SvgLayerLabel>,
    pub icons: Vec<SvgLayerIcon>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineLayerChart.poco", role = "panel")]
#[slot(default, accepts = [
    PineChartLayer,
    PineChartGuide,
    PineChartLine,
    PineChartMarker,
    PineChartReferenceDot,
    PineChartLabel,
    PineChartIcon,
])]
pub struct PineLayerChart {
    #[prop]
    pub label: String,
    #[prop]
    pub width: f64,
    #[prop]
    pub height: f64,
    pub state: String,
    pub view_box: String,
    pub guides: Vec<ChartLayerGuide>,
    pub lines: Vec<ChartLayerLine>,
    pub markers: Vec<ChartLayerMarker>,
    pub reference_dots: Vec<ChartLayerReferenceDot>,
    pub labels: Vec<ChartLayerLabel>,
    pub icons: Vec<ChartLayerIcon>,
    pub svg_guides: Vec<SvgLayerGuide>,
    pub svg_lines: Vec<SvgLayerLine>,
    pub svg_markers: Vec<SvgLayerMarker>,
    pub svg_reference_background_dots: Vec<SvgLayerReferenceDot>,
    pub svg_reference_foreground_dots: Vec<SvgLayerReferenceDot>,
    pub svg_labels: Vec<SvgLayerLabel>,
    pub svg_icons: Vec<SvgLayerIcon>,
    pub error: String,
    pub ready: bool,
    pub empty: bool,
    pub invalid: bool,
}

impl Default for PineLayerChart {
    fn default() -> Self {
        Self {
            label: "Layer chart".into(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            state: "empty".into(),
            view_box: format!("0 0 {DEFAULT_WIDTH} {DEFAULT_HEIGHT}"),
            guides: Vec::new(),
            lines: Vec::new(),
            markers: Vec::new(),
            reference_dots: Vec::new(),
            labels: Vec::new(),
            icons: Vec::new(),
            svg_guides: Vec::new(),
            svg_lines: Vec::new(),
            svg_markers: Vec::new(),
            svg_reference_background_dots: Vec::new(),
            svg_reference_foreground_dots: Vec::new(),
            svg_labels: Vec::new(),
            svg_icons: Vec::new(),
            error: String::new(),
            ready: false,
            empty: true,
            invalid: false,
        }
    }
}

#[handlers]
impl PineLayerChart {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
        self.recompute();
    }

    #[watch(width)]
    fn on_width(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(height)]
    fn on_height(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }
}

impl PineLayerChart {
    pub fn upsert_guide(&mut self, guide: ChartLayerGuide) {
        let key = guide.key.clone();
        upsert_by_key(&mut self.guides, &key, guide, |item| &item.key);
        self.recompute();
    }

    pub fn remove_guide(&mut self, key: &str) {
        remove_by_key(&mut self.guides, key, |item| &item.key);
        self.recompute();
    }

    pub fn upsert_line(&mut self, line: ChartLayerLine) {
        let key = line.key.clone();
        upsert_by_key(&mut self.lines, &key, line, |item| &item.key);
        self.recompute();
    }

    pub fn remove_line(&mut self, key: &str) {
        remove_by_key(&mut self.lines, key, |item| &item.key);
        self.recompute();
    }

    pub fn upsert_marker(&mut self, marker: ChartLayerMarker) {
        let key = marker.key.clone();
        upsert_by_key(&mut self.markers, &key, marker, |item| &item.key);
        self.recompute();
    }

    pub fn remove_marker(&mut self, key: &str) {
        remove_by_key(&mut self.markers, key, |item| &item.key);
        self.recompute();
    }

    pub fn upsert_reference_dot(&mut self, dot: ChartLayerReferenceDot) {
        let key = dot.key.clone();
        upsert_by_key(&mut self.reference_dots, &key, dot, |item| &item.key);
        self.recompute();
    }

    pub fn remove_reference_dot(&mut self, key: &str) {
        remove_by_key(&mut self.reference_dots, key, |item| &item.key);
        self.recompute();
    }

    pub fn upsert_label(&mut self, label: ChartLayerLabel) {
        let key = label.key.clone();
        upsert_by_key(&mut self.labels, &key, label, |item| &item.key);
        self.recompute();
    }

    pub fn remove_label(&mut self, key: &str) {
        remove_by_key(&mut self.labels, key, |item| &item.key);
        self.recompute();
    }

    pub fn upsert_icon(&mut self, icon: ChartLayerIcon) {
        let key = icon.key.clone();
        upsert_by_key(&mut self.icons, &key, icon, |item| &item.key);
        self.recompute();
    }

    pub fn remove_icon(&mut self, key: &str) {
        remove_by_key(&mut self.icons, key, |item| &item.key);
        self.recompute();
    }

    fn recompute(&mut self) {
        match render_layer_chart(
            self.width,
            self.height,
            &self.guides,
            &self.lines,
            &self.markers,
            &self.reference_dots,
            &self.labels,
            &self.icons,
        ) {
            Ok(render) => {
                self.view_box = render.view_box;
                self.svg_guides = render.guides;
                self.svg_lines = render.lines;
                self.svg_markers = render.markers;
                let (foreground, background): (Vec<_>, Vec<_>) = render
                    .reference_dots
                    .into_iter()
                    .partition(|dot| dot.layer == "reference-foreground");
                self.svg_reference_background_dots = background;
                self.svg_reference_foreground_dots = foreground;
                self.svg_labels = render.labels;
                self.svg_icons = render.icons;
                self.error.clear();
                self.empty = self.svg_guides.is_empty()
                    && self.svg_lines.is_empty()
                    && self.svg_markers.is_empty()
                    && self.svg_reference_background_dots.is_empty()
                    && self.svg_reference_foreground_dots.is_empty()
                    && self.svg_labels.is_empty()
                    && self.svg_icons.is_empty();
                self.ready = !self.empty;
                self.invalid = false;
                self.state = if self.empty { "empty" } else { "ready" }.into();
            }
            Err(error) => {
                self.svg_guides.clear();
                self.svg_lines.clear();
                self.svg_markers.clear();
                self.svg_reference_background_dots.clear();
                self.svg_reference_foreground_dots.clear();
                self.svg_labels.clear();
                self.svg_icons.clear();
                self.error = error.to_string();
                self.ready = false;
                self.empty = false;
                self.invalid = true;
                self.state = "invalid".into();
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineChartLayer.poco", role = "scope")]
#[slot(default, only = [
    PineChartGuide,
    PineChartLine,
    PineChartMarker,
    PineChartReferenceDot,
    PineChartLabel,
    PineChartIcon,
])]
pub struct PineChartLayer {
    #[prop]
    pub name: String,
}

#[handlers]
impl PineChartLayer {
    fn on_setup(&mut self) {
        LAYER.provide(normalize_layer(&self.name));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartGuide.poco", role = "visual")]
pub struct PineChartGuide {
    #[prop]
    pub key: String,
    #[prop]
    pub x1: f64,
    #[prop]
    pub y1: f64,
    #[prop]
    pub x2: f64,
    #[prop]
    pub y2: f64,
    pub component_key: String,
}

impl Default for PineChartGuide {
    fn default() -> Self {
        Self {
            key: String::new(),
            x1: 0.0,
            y1: 0.0,
            x2: 0.0,
            y2: 0.0,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartGuide {
    fn on_setup(&mut self) {
        self.ensure_key("guide");
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_guide(&self.component_key));
    }

    #[watch(x1)]
    fn on_x1(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y1)]
    fn on_y1(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(x2)]
    fn on_x2(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y2)]
    fn on_y2(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }
}

impl PineChartGuide {
    fn ensure_key(&mut self, prefix: &str) {
        ensure_component_key(&mut self.component_key, prefix, &self.key);
    }

    fn sync(&self) {
        let guide = ChartLayerGuide {
            key: self.component_key.clone(),
            x1: self.x1,
            y1: self.y1,
            x2: self.x2,
            y2: self.y2,
        };
        update_root(|root| root.upsert_guide(guide));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartLine.poco", role = "visual")]
pub struct PineChartLine {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub color: String,
    #[prop]
    pub stroke_width: f64,
    #[prop]
    pub points: Vec<ChartLayerPoint>,
    pub component_key: String,
}

impl Default for PineChartLine {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            color: "currentColor".into(),
            stroke_width: DEFAULT_LINE_WIDTH,
            points: Vec::new(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartLine {
    fn on_setup(&mut self) {
        self.ensure_key("line");
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_line(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(color)]
    fn on_color(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(points)]
    fn on_points(&mut self, _: Vec<ChartLayerPoint>, _: Option<Vec<ChartLayerPoint>>) {
        self.sync();
    }
}

impl PineChartLine {
    fn ensure_key(&mut self, prefix: &str) {
        ensure_component_key(&mut self.component_key, prefix, &self.key);
    }

    fn sync(&self) {
        let line = ChartLayerLine {
            key: self.component_key.clone(),
            label: self.label.clone(),
            color: self.color.clone(),
            stroke_width: self.stroke_width,
            points: self.points.clone(),
        };
        update_root(|root| root.upsert_line(line));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartMarker.poco", role = "visual")]
pub struct PineChartMarker {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub x: f64,
    #[prop]
    pub y: f64,
    #[prop]
    pub radius: f64,
    #[prop]
    pub fill: String,
    #[prop]
    pub stroke: String,
    #[prop]
    pub stroke_width: f64,
    pub component_key: String,
}

impl Default for PineChartMarker {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            x: 0.0,
            y: 0.0,
            radius: DEFAULT_MARKER_RADIUS,
            fill: "currentColor".into(),
            stroke: "#ffffff".into(),
            stroke_width: 2.0,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartMarker {
    fn on_setup(&mut self) {
        self.ensure_key("marker");
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_marker(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(radius)]
    fn on_radius(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke)]
    fn on_stroke(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }
}

impl PineChartMarker {
    fn ensure_key(&mut self, prefix: &str) {
        ensure_component_key(&mut self.component_key, prefix, &self.key);
    }

    fn sync(&self) {
        let marker = ChartLayerMarker {
            key: self.component_key.clone(),
            label: self.label.clone(),
            x: self.x,
            y: self.y,
            radius: self.radius,
            fill: self.fill.clone(),
            stroke: self.stroke.clone(),
            stroke_width: self.stroke_width,
        };
        update_root(|root| root.upsert_marker(marker));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartReferenceDot.poco", role = "visual")]
pub struct PineChartReferenceDot {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub x: f64,
    #[prop]
    pub y: f64,
    #[prop]
    pub radius: f64,
    #[prop]
    pub fill: String,
    #[prop]
    pub stroke: String,
    #[prop]
    pub stroke_width: f64,
    #[prop]
    pub layer: String,
    pub component_key: String,
}

impl Default for PineChartReferenceDot {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            x: 0.0,
            y: 0.0,
            radius: DEFAULT_REFERENCE_RADIUS,
            fill: "currentColor".into(),
            stroke: "none".into(),
            stroke_width: 0.0,
            layer: String::new(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartReferenceDot {
    fn on_setup(&mut self) {
        self.ensure_key("reference-dot");
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_reference_dot(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(radius)]
    fn on_radius(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke)]
    fn on_stroke(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(layer)]
    fn on_layer(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineChartReferenceDot {
    fn ensure_key(&mut self, prefix: &str) {
        ensure_component_key(&mut self.component_key, prefix, &self.key);
    }

    fn sync(&self) {
        let dot = ChartLayerReferenceDot {
            key: self.component_key.clone(),
            label: self.label.clone(),
            x: self.x,
            y: self.y,
            radius: self.radius,
            fill: self.fill.clone(),
            stroke: self.stroke.clone(),
            stroke_width: self.stroke_width,
            layer: layer_or_context(&self.layer, "reference-background"),
        };
        update_root(|root| root.upsert_reference_dot(dot));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartLabel.poco", role = "visual")]
pub struct PineChartLabel {
    #[prop]
    pub key: String,
    #[prop]
    pub text: String,
    #[prop]
    pub x: f64,
    #[prop]
    pub y: f64,
    #[prop]
    pub dx: f64,
    #[prop]
    pub dy: f64,
    #[prop]
    pub angle: f64,
    #[prop]
    pub fill: String,
    #[prop]
    pub text_anchor: String,
    #[prop]
    pub font_weight: String,
    pub component_key: String,
}

impl Default for PineChartLabel {
    fn default() -> Self {
        Self {
            key: String::new(),
            text: String::new(),
            x: 0.0,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            angle: 0.0,
            fill: "currentColor".into(),
            text_anchor: "middle".into(),
            font_weight: "600".into(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartLabel {
    fn on_setup(&mut self) {
        self.ensure_key("label");
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_label(&self.component_key));
    }

    #[watch(text)]
    fn on_text(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(dx)]
    fn on_dx(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(dy)]
    fn on_dy(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(angle)]
    fn on_angle(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(text_anchor)]
    fn on_text_anchor(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(font_weight)]
    fn on_font_weight(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineChartLabel {
    fn ensure_key(&mut self, prefix: &str) {
        ensure_component_key(&mut self.component_key, prefix, &self.key);
    }

    fn sync(&self) {
        let label = ChartLayerLabel {
            key: self.component_key.clone(),
            text: self.text.clone(),
            x: self.x,
            y: self.y,
            dx: self.dx,
            dy: self.dy,
            angle: self.angle,
            fill: self.fill.clone(),
            text_anchor: self.text_anchor.clone(),
            font_weight: self.font_weight.clone(),
        };
        update_root(|root| root.upsert_label(label));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartIcon.poco", role = "visual")]
pub struct PineChartIcon {
    #[prop]
    pub key: String,
    #[prop]
    pub kind: String,
    #[prop]
    pub x: f64,
    #[prop]
    pub y: f64,
    #[prop]
    pub scale: f64,
    #[prop]
    pub fill: String,
    pub component_key: String,
}

impl Default for PineChartIcon {
    fn default() -> Self {
        Self {
            key: String::new(),
            kind: "plane".into(),
            x: 0.0,
            y: 0.0,
            scale: DEFAULT_ICON_SCALE,
            fill: "currentColor".into(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartIcon {
    fn on_setup(&mut self) {
        self.ensure_key("icon");
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_icon(&self.component_key));
    }

    #[watch(kind)]
    fn on_kind(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(scale)]
    fn on_scale(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineChartIcon {
    fn ensure_key(&mut self, prefix: &str) {
        ensure_component_key(&mut self.component_key, prefix, &self.key);
    }

    fn sync(&self) {
        let icon = ChartLayerIcon {
            key: self.component_key.clone(),
            kind: self.kind.clone(),
            x: self.x,
            y: self.y,
            scale: self.scale,
            fill: self.fill.clone(),
        };
        update_root(|root| root.upsert_icon(icon));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_layer_chart(
    width: f64,
    height: f64,
    guides: &[ChartLayerGuide],
    lines: &[ChartLayerLine],
    markers: &[ChartLayerMarker],
    reference_dots: &[ChartLayerReferenceDot],
    labels: &[ChartLayerLabel],
    icons: &[ChartLayerIcon],
) -> ChartResult<LayerChartRender> {
    let width = finite("width", width)?;
    let height = finite("height", height)?;
    if width <= 0.0 || height <= 0.0 {
        return Err(ChartError::InvalidSize { width, height });
    }

    Ok(LayerChartRender {
        view_box: format!("0 0 {width} {height}"),
        guides: render_guides(guides)?,
        lines: render_lines(lines)?,
        markers: render_markers(markers)?,
        reference_dots: render_reference_dots(reference_dots)?,
        labels: render_labels(labels)?,
        icons: render_icons(icons)?,
    })
}

fn render_guides(guides: &[ChartLayerGuide]) -> ChartResult<Vec<SvgLayerGuide>> {
    guides
        .iter()
        .enumerate()
        .map(|(index, guide)| {
            Ok(SvgLayerGuide {
                key: key_or_index("guide", &guide.key, index),
                x1: finite("guide.x1", guide.x1)?,
                y1: finite("guide.y1", guide.y1)?,
                x2: finite("guide.x2", guide.x2)?,
                y2: finite("guide.y2", guide.y2)?,
            })
        })
        .collect()
}

fn render_lines(lines: &[ChartLayerLine]) -> ChartResult<Vec<SvgLayerLine>> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if line.points.is_empty() {
                return Err(ChartError::EmptySeries);
            }
            let points = line
                .points
                .iter()
                .copied()
                .map(|point| point.validate("line.point.x", "line.point.y"))
                .collect::<ChartResult<Vec<_>>>()?;
            Ok(SvgLayerLine {
                key: key_or_index("line", &line.key, index),
                label: label_or_key(&line.label, &line.key, index),
                d: line_path(points.into_iter().map(Point::from))?,
                color: color_or_current(&line.color),
                stroke_width: positive_or_default(
                    "line.stroke_width",
                    line.stroke_width,
                    DEFAULT_LINE_WIDTH,
                )?,
            })
        })
        .collect()
}

fn render_markers(markers: &[ChartLayerMarker]) -> ChartResult<Vec<SvgLayerMarker>> {
    markers
        .iter()
        .enumerate()
        .map(|(index, marker)| {
            let x = finite("marker.x", marker.x)?;
            let y = finite("marker.y", marker.y)?;
            let label = label_or_key(&marker.label, &marker.key, index);
            Ok(SvgLayerMarker {
                key: key_or_index("marker", &marker.key, index),
                label: label.clone(),
                x,
                y,
                radius: positive_or_default("marker.radius", marker.radius, DEFAULT_MARKER_RADIUS)?,
                fill: color_or_current(&marker.fill),
                stroke: color_or(&marker.stroke, "none"),
                stroke_width: non_negative("marker.stroke_width", marker.stroke_width)?,
                aria_label: format!("{label}: x {x}, y {y}"),
            })
        })
        .collect()
}

fn render_reference_dots(
    dots: &[ChartLayerReferenceDot],
) -> ChartResult<Vec<SvgLayerReferenceDot>> {
    dots.iter()
        .enumerate()
        .map(|(index, dot)| {
            let x = finite("reference_dot.x", dot.x)?;
            let y = finite("reference_dot.y", dot.y)?;
            let label = label_or_key(&dot.label, &dot.key, index);
            Ok(SvgLayerReferenceDot {
                key: key_or_index("reference-dot", &dot.key, index),
                label: label.clone(),
                x,
                y,
                radius: positive_or_default(
                    "reference_dot.radius",
                    dot.radius,
                    DEFAULT_REFERENCE_RADIUS,
                )?,
                fill: color_or_current(&dot.fill),
                stroke: color_or(&dot.stroke, "none"),
                stroke_width: non_negative("reference_dot.stroke_width", dot.stroke_width)?,
                layer: reference_layer(&dot.layer).into(),
                aria_label: format!("{label}: x {x}, y {y}"),
            })
        })
        .collect()
}

fn render_labels(labels: &[ChartLayerLabel]) -> ChartResult<Vec<SvgLayerLabel>> {
    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let base_x = finite("label.x", label.x)?;
            let base_y = finite("label.y", label.y)?;
            let dx = finite("label.dx", label.dx)?;
            let dy = finite("label.dy", label.dy)?;
            let angle = finite("label.angle", label.angle)?;
            let x = base_x + dx;
            let y = base_y + dy;
            Ok(SvgLayerLabel {
                key: key_or_index("label", &label.key, index),
                text: label.text.clone(),
                x,
                y,
                fill: color_or(&label.fill, "currentColor"),
                text_anchor: text_anchor(&label.text_anchor).into(),
                font_weight: if label.font_weight.trim().is_empty() {
                    "600".into()
                } else {
                    label.font_weight.trim().into()
                },
                transform: rotation_transform(angle, x, y),
            })
        })
        .collect()
}

fn render_icons(icons: &[ChartLayerIcon]) -> ChartResult<Vec<SvgLayerIcon>> {
    icons
        .iter()
        .enumerate()
        .map(|(index, icon)| {
            let x = finite("icon.x", icon.x)?;
            let y = finite("icon.y", icon.y)?;
            let scale = positive_or_default("icon.scale", icon.scale, DEFAULT_ICON_SCALE)?;
            Ok(SvgLayerIcon {
                key: key_or_index("icon", &icon.key, index),
                kind: icon_kind(&icon.kind).into(),
                transform: format!("translate({x} {y}) scale({scale})"),
                fill: color_or(&icon.fill, "currentColor"),
            })
        })
        .collect()
}

fn update_root(f: impl FnOnce(&mut PineLayerChart)) {
    if let Some(root) = ROOT.inject() {
        root.update(f);
    }
}

fn ensure_component_key(target: &mut String, prefix: &str, authored_key: &str) {
    if target.is_empty() {
        *target = component_key(prefix, authored_key);
    }
}

fn component_key(prefix: &str, authored_key: &str) -> String {
    let authored_key = authored_key.trim();
    if !authored_key.is_empty() {
        return authored_key.into();
    }

    current_scope_id()
        .map(|scope| format!("{prefix}-{}", scope.0))
        .unwrap_or_else(|| prefix.into())
}

fn layer_or_context(layer: &str, fallback: &str) -> String {
    if !layer.trim().is_empty() {
        return layer.trim().into();
    }

    LAYER.inject().unwrap_or_else(|| fallback.into())
}

fn normalize_layer(layer: &str) -> String {
    match layer.trim() {
        "reference-foreground" | "foreground" => "reference-foreground".into(),
        "reference-background" | "background" => "reference-background".into(),
        "annotations" | "annotation" => "annotations".into(),
        "labels" | "label" => "labels".into(),
        "markers" | "marker" => "markers".into(),
        "grid" => "grid".into(),
        "series" => "series".into(),
        _ => layer.trim().into(),
    }
}

fn upsert_by_key<T>(items: &mut Vec<T>, key: &str, item: T, key_of: impl Fn(&T) -> &str) {
    if let Some(existing) = items.iter_mut().find(|existing| key_of(existing) == key) {
        *existing = item;
    } else {
        items.push(item);
    }
}

fn remove_by_key<T>(items: &mut Vec<T>, key: &str, key_of: impl Fn(&T) -> &str) {
    items.retain(|item| key_of(item) != key);
}

fn positive_or_default(field: &'static str, value: f64, default: f64) -> ChartResult<f64> {
    let value = finite(field, value)?;
    if value <= 0.0 {
        Ok(default)
    } else {
        Ok(value)
    }
}

fn non_negative(field: &'static str, value: f64) -> ChartResult<f64> {
    let value = finite(field, value)?;
    Ok(value.max(0.0))
}

fn color_or_current(value: &str) -> String {
    color_or(value, "currentColor")
}

fn color_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.into()
    } else {
        trimmed.into()
    }
}

fn reference_layer(value: &str) -> &'static str {
    match value.trim() {
        "reference-foreground" | "foreground" => "reference-foreground",
        _ => "reference-background",
    }
}

fn text_anchor(value: &str) -> &'static str {
    match value.trim() {
        "start" => "start",
        "end" => "end",
        _ => "middle",
    }
}

fn icon_kind(value: &str) -> &'static str {
    match value.trim() {
        "plane" => "plane",
        _ => "custom",
    }
}

fn rotation_transform(angle: f64, x: f64, y: f64) -> String {
    if angle.abs() <= f64::EPSILON {
        String::new()
    } else {
        format!("rotate({angle} {x} {y})")
    }
}

fn label_or_key(label: &str, key: &str, index: usize) -> String {
    if label.trim().is_empty() {
        key_or_index("item", key, index)
    } else {
        label.trim().into()
    }
}

fn key_or_index(prefix: &str, key: &str, index: usize) -> String {
    if key.trim().is_empty() {
        format!("{prefix}-{index}")
    } else {
        key.trim().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layered_chart_renders_registered_marks_in_svg_form() {
        let render = render_layer_chart(
            300.0,
            180.0,
            &[ChartLayerGuide {
                key: "midline".into(),
                x1: 0.0,
                y1: 90.0,
                x2: 300.0,
                y2: 90.0,
            }],
            &[ChartLayerLine {
                key: "line-a".into(),
                label: "A".into(),
                color: "#1aa300".into(),
                stroke_width: 12.0,
                points: vec![
                    ChartLayerPoint::new(0.0, 140.0),
                    ChartLayerPoint::new(120.0, 30.0),
                ],
            }],
            &[ChartLayerMarker {
                key: "stop".into(),
                label: "Stop".into(),
                x: 120.0,
                y: 30.0,
                radius: 8.0,
                fill: "#1aa300".into(),
                stroke: "#fff".into(),
                stroke_width: 2.0,
            }],
            &[ChartLayerReferenceDot {
                key: "hub".into(),
                label: "Hub".into(),
                x: 120.0,
                y: 30.0,
                radius: 18.0,
                fill: "#ff242e".into(),
                stroke: "#fff".into(),
                stroke_width: 3.0,
                layer: "reference-foreground".into(),
            }],
            &[ChartLayerLabel {
                key: "hub-label".into(),
                text: "Hub".into(),
                x: 120.0,
                y: 30.0,
                dx: 8.0,
                dy: -12.0,
                angle: -65.0,
                fill: "#18212f".into(),
                text_anchor: "start".into(),
                font_weight: "700".into(),
            }],
            &[ChartLayerIcon {
                key: "airport".into(),
                kind: "plane".into(),
                x: 20.0,
                y: 20.0,
                scale: 0.1,
                fill: "#18212f".into(),
            }],
        )
        .unwrap();

        assert_eq!(render.view_box, "0 0 300 180");
        assert_eq!(render.guides[0].key, "midline");
        assert_eq!(render.lines[0].d, "M0,140 L120,30");
        assert_eq!(render.reference_dots[0].layer, "reference-foreground");
        assert_eq!(render.labels[0].transform, "rotate(-65 128 18)");
        assert_eq!(render.icons[0].kind, "plane");
    }

    #[test]
    fn layered_chart_rejects_invalid_size() {
        let error = render_layer_chart(0.0, 100.0, &[], &[], &[], &[], &[], &[]).unwrap_err();
        assert_eq!(
            error,
            ChartError::InvalidSize {
                width: 0.0,
                height: 100.0,
            }
        );
    }
}
