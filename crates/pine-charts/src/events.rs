#[cfg(target_arch = "wasm32")]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const CHART_SELECT_EVENT: &str = "pp:chart:select";
pub const CHART_SELECT_END_EVENT: &str = "pp:chart:select-end";
pub const CHART_HOVER_EVENT: &str = "pp:chart:hover";
pub const CHART_HOVER_END_EVENT: &str = "pp:chart:hover-end";
pub const LEGEND_TOGGLE_EVENT: &str = "pp:chart:legend-toggle";

#[cfg(target_arch = "wasm32")]
fn from_event_detail<T: DeserializeOwned>(event: wasm_bindgen::JsValue) -> Option<T> {
    let detail = js_sys::Reflect::get(&event, &wasm_bindgen::JsValue::from_str("detail")).ok()?;
    pocopine::__private::serde_wasm_bindgen::from_value(detail).ok()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartSelection {
    pub chart: String,
    pub kind: String,
    pub key: String,
    pub label: String,
    pub aria_label: String,
    pub series: String,
    pub category: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub value: Option<f64>,
    pub percentage: Option<f64>,
    pub x_label: String,
    pub y_label: String,
    pub value_label: String,
    pub percentage_label: String,
}

impl ChartSelection {
    #[allow(clippy::too_many_arguments)]
    pub fn xy(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        aria_label: impl Into<String>,
        series: impl Into<String>,
        x: f64,
        y: f64,
        x_label: impl Into<String>,
        y_label: impl Into<String>,
    ) -> Self {
        Self {
            chart: chart.into(),
            kind: "xy".into(),
            key: key.into(),
            label: label.into(),
            aria_label: aria_label.into(),
            series: series.into(),
            category: String::new(),
            x: Some(x),
            y: Some(y),
            value: None,
            percentage: None,
            x_label: x_label.into(),
            y_label: y_label.into(),
            value_label: String::new(),
            percentage_label: String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn category(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        aria_label: impl Into<String>,
        category: impl Into<String>,
        series: impl Into<String>,
        value: f64,
        value_label: impl Into<String>,
    ) -> Self {
        Self {
            chart: chart.into(),
            kind: "category".into(),
            key: key.into(),
            label: label.into(),
            aria_label: aria_label.into(),
            series: series.into(),
            category: category.into(),
            x: None,
            y: None,
            value: Some(value),
            percentage: None,
            x_label: String::new(),
            y_label: String::new(),
            value_label: value_label.into(),
            percentage_label: String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn share(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        aria_label: impl Into<String>,
        value: f64,
        value_label: impl Into<String>,
        percentage: f64,
        percentage_label: impl Into<String>,
    ) -> Self {
        Self {
            chart: chart.into(),
            kind: "share".into(),
            key: key.into(),
            label: label.into(),
            aria_label: aria_label.into(),
            series: String::new(),
            category: String::new(),
            x: None,
            y: None,
            value: Some(value),
            percentage: Some(percentage),
            x_label: String::new(),
            y_label: String::new(),
            value_label: value_label.into(),
            percentage_label: percentage_label.into(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn from_event_value(event: wasm_bindgen::JsValue) -> Option<Self> {
        from_event_detail(event)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_event_value(_: wasm_bindgen::JsValue) -> Option<Self> {
        None
    }
}

/// Payload for `pp:chart:select-end`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartSelectionEnd {
    pub chart: String,
    pub key: String,
}

impl ChartSelectionEnd {
    pub fn new(chart: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            chart: chart.into(),
            key: key.into(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn from_event_value(event: wasm_bindgen::JsValue) -> Option<Self> {
        from_event_detail(event)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_event_value(_: wasm_bindgen::JsValue) -> Option<Self> {
        None
    }
}

/// Payload for `pp:chart:hover`.
///
/// `kind` determines which value fields are populated:
/// - `xy`: line, scatter, and area hovers populate `x`, `y`, `x_label`, and
///   `y_label`.
/// - `category`: bar hovers populate `category`, `value`, and `value_label`.
/// - `share`: pie/donut hovers populate `value`, `value_label`, `percentage`,
///   and `percentage_label`.
///
/// `label` is the concise label suitable for custom UI. `aria_label` mirrors
/// the rendered mark's accessible label.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartHover {
    pub chart: String,
    pub kind: String,
    pub key: String,
    pub label: String,
    pub aria_label: String,
    pub series: String,
    pub category: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub value: Option<f64>,
    pub percentage: Option<f64>,
    pub x_label: String,
    pub y_label: String,
    pub value_label: String,
    pub percentage_label: String,
    pub tooltip_x: String,
    pub tooltip_y: String,
    pub tooltip_style: String,
}

impl ChartHover {
    #[allow(clippy::too_many_arguments)]
    pub fn xy(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        aria_label: impl Into<String>,
        series: impl Into<String>,
        x: f64,
        y: f64,
        x_label: impl Into<String>,
        y_label: impl Into<String>,
        tooltip_x: impl Into<String>,
        tooltip_y: impl Into<String>,
        tooltip_style: impl Into<String>,
    ) -> Self {
        Self {
            chart: chart.into(),
            kind: "xy".into(),
            key: key.into(),
            label: label.into(),
            aria_label: aria_label.into(),
            series: series.into(),
            category: String::new(),
            x: Some(x),
            y: Some(y),
            value: None,
            percentage: None,
            x_label: x_label.into(),
            y_label: y_label.into(),
            value_label: String::new(),
            percentage_label: String::new(),
            tooltip_x: tooltip_x.into(),
            tooltip_y: tooltip_y.into(),
            tooltip_style: tooltip_style.into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn category(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        aria_label: impl Into<String>,
        category: impl Into<String>,
        series: impl Into<String>,
        value: f64,
        value_label: impl Into<String>,
        tooltip_x: impl Into<String>,
        tooltip_y: impl Into<String>,
        tooltip_style: impl Into<String>,
    ) -> Self {
        let category = category.into();
        Self {
            chart: chart.into(),
            kind: "category".into(),
            key: key.into(),
            label: label.into(),
            aria_label: aria_label.into(),
            series: series.into(),
            category,
            x: None,
            y: None,
            value: Some(value),
            percentage: None,
            x_label: String::new(),
            y_label: String::new(),
            value_label: value_label.into(),
            percentage_label: String::new(),
            tooltip_x: tooltip_x.into(),
            tooltip_y: tooltip_y.into(),
            tooltip_style: tooltip_style.into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn share(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        aria_label: impl Into<String>,
        value: f64,
        value_label: impl Into<String>,
        percentage: f64,
        percentage_label: impl Into<String>,
        tooltip_x: impl Into<String>,
        tooltip_y: impl Into<String>,
        tooltip_style: impl Into<String>,
    ) -> Self {
        Self {
            chart: chart.into(),
            kind: "share".into(),
            key: key.into(),
            label: label.into(),
            aria_label: aria_label.into(),
            series: String::new(),
            category: String::new(),
            x: None,
            y: None,
            value: Some(value),
            percentage: Some(percentage),
            x_label: String::new(),
            y_label: String::new(),
            value_label: value_label.into(),
            percentage_label: percentage_label.into(),
            tooltip_x: tooltip_x.into(),
            tooltip_y: tooltip_y.into(),
            tooltip_style: tooltip_style.into(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn from_event_value(event: wasm_bindgen::JsValue) -> Option<Self> {
        from_event_detail(event)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_event_value(_: wasm_bindgen::JsValue) -> Option<Self> {
        None
    }
}

/// Payload for `pp:chart:hover-end`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartHoverEnd {
    pub chart: String,
}

impl ChartHoverEnd {
    pub fn new(chart: impl Into<String>) -> Self {
        Self {
            chart: chart.into(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn from_event_value(event: wasm_bindgen::JsValue) -> Option<Self> {
        from_event_detail(event)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_event_value(_: wasm_bindgen::JsValue) -> Option<Self> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LegendToggle {
    pub key: String,
    pub label: String,
    pub series: String,
    pub active: bool,
}

impl LegendToggle {
    #[cfg(target_arch = "wasm32")]
    pub fn from_event_value(event: wasm_bindgen::JsValue) -> Option<Self> {
        from_event_detail(event)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_event_value(_: wasm_bindgen::JsValue) -> Option<Self> {
        None
    }
}
