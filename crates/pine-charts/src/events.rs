#[cfg(target_arch = "wasm32")]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const CHART_SELECT_EVENT: &str = "pp:chart:select";
pub const CHART_SELECT_END_EVENT: &str = "pp:chart:select-end";
pub const CHART_HOVER_EVENT: &str = "pp:chart:hover";
pub const CHART_HOVER_END_EVENT: &str = "pp:chart:hover-end";
pub const LEGEND_TOGGLE_EVENT: &str = "pp:chart:legend-toggle";

const KIND_XY: &str = "xy";
const KIND_CATEGORY: &str = "category";
const KIND_SHARE: &str = "share";

#[cfg(target_arch = "wasm32")]
fn from_event_detail<T: DeserializeOwned>(event: wasm_bindgen::JsValue) -> Option<T> {
    let detail = js_sys::Reflect::get(&event, &wasm_bindgen::JsValue::from_str("detail")).ok()?;
    pocopine::__private::serde_wasm_bindgen::from_value(detail).ok()
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn pair_display_value(first: &str, second: &str, separator: &str, fallback: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (false, false) => format!("{first}{separator}{second}"),
        (false, true) => first.into(),
        (true, false) => second.into(),
        (true, true) => fallback.into(),
    }
}

fn parenthetical_display_value(value: &str, qualifier: &str, fallback: &str) -> String {
    match (value.is_empty(), qualifier.is_empty()) {
        (false, false) => format!("{value} ({qualifier})"),
        (false, true) => value.into(),
        (true, false) => qualifier.into(),
        (true, true) => fallback.into(),
    }
}

fn interaction_display_value(
    kind: &str,
    label: &str,
    category: &str,
    x_label: &str,
    y_label: &str,
    value_label: &str,
    percentage_label: &str,
) -> String {
    match kind {
        KIND_XY => pair_display_value(x_label, y_label, ": ", label),
        KIND_CATEGORY => pair_display_value(category, value_label, ": ", label),
        KIND_SHARE => parenthetical_display_value(value_label, percentage_label, label),
        _ => label.into(),
    }
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

    /// Returns the series label when present, otherwise the chart kind.
    ///
    /// This is useful for custom tooltip/detail headings while keeping the
    /// actual mark label available through `label`.
    pub fn series_or_chart(&self) -> &str {
        non_empty_or(&self.series, &self.chart)
    }

    /// Formats the populated value fields according to `kind`.
    ///
    /// - `xy` becomes `<x_label>: <y_label>`.
    /// - `category` becomes `<category>: <value_label>`.
    /// - `share` becomes `<value_label> (<percentage_label>)`.
    ///
    /// If a payload is incomplete, this falls back to the populated side, then
    /// to `label`.
    pub fn display_value(&self) -> String {
        interaction_display_value(
            &self.kind,
            &self.label,
            &self.category,
            &self.x_label,
            &self.y_label,
            &self.value_label,
            &self.percentage_label,
        )
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

pub(crate) struct ChartSelectionKeys<'a> {
    chart: &'static str,
    focused: &'a mut String,
    selected: &'a mut String,
}

impl<'a> ChartSelectionKeys<'a> {
    pub(crate) fn new(
        chart: &'static str,
        focused: &'a mut String,
        selected: &'a mut String,
    ) -> Self {
        Self {
            chart,
            focused,
            selected,
        }
    }

    pub(crate) fn select(&mut self, key: String, selection: ChartSelection) {
        *self.focused = key.clone();
        *self.selected = key;
        pocopine::emit(CHART_SELECT_EVENT, selection);
    }

    pub(crate) fn clear(&mut self) {
        self.focused.clear();
        self.clear_selected();
    }

    pub(crate) fn reconcile(&mut self, has_focused: bool, has_selected: bool) {
        if !has_focused {
            self.focused.clear();
        }
        if !has_selected {
            self.clear_selected();
        }
    }

    pub(crate) fn clear_selected(&mut self) {
        let key = std::mem::take(self.selected);
        if !key.is_empty() {
            pocopine::emit(
                CHART_SELECT_END_EVENT,
                ChartSelectionEnd::new(self.chart, key),
            );
        }
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

    /// Returns the series label when present, otherwise the chart kind.
    ///
    /// This is useful for custom tooltip/detail headings while keeping the
    /// actual mark label available through `label`.
    pub fn series_or_chart(&self) -> &str {
        non_empty_or(&self.series, &self.chart)
    }

    /// Formats the populated value fields according to `kind`.
    ///
    /// - `xy` becomes `<x_label>: <y_label>`.
    /// - `category` becomes `<category>: <value_label>`.
    /// - `share` becomes `<value_label> (<percentage_label>)`.
    ///
    /// If a payload is incomplete, this falls back to the populated side, then
    /// to `label`.
    pub fn display_value(&self) -> String {
        interaction_display_value(
            &self.kind,
            &self.label,
            &self.category,
            &self.x_label,
            &self.y_label,
            &self.value_label,
            &self.percentage_label,
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_hover_formats_display_values_by_kind() {
        let xy = ChartHover::xy(
            "area",
            "point",
            "x 2, y 49",
            "API: x 2, y 49",
            "API",
            2.0,
            49.0,
            "2",
            "49",
            "right",
            "above",
            "",
        );
        assert_eq!(xy.series_or_chart(), "API");
        assert_eq!(xy.display_value(), "2: 49");

        let category = ChartHover::category(
            "bar",
            "bar-a",
            "Bucket A",
            "Bucket A: 42",
            "Bucket A",
            "Actual",
            42.0,
            "42",
            "right",
            "above",
            "",
        );
        assert_eq!(category.series_or_chart(), "Actual");
        assert_eq!(category.display_value(), "Bucket A: 42");

        let share = ChartHover::share(
            "pie",
            "organic",
            "Organic",
            "Organic: 30 (60%)",
            30.0,
            "30",
            60.0,
            "60%",
            "right",
            "above",
            "",
        );
        assert_eq!(share.series_or_chart(), "pie");
        assert_eq!(share.display_value(), "30 (60%)");
    }

    #[test]
    fn chart_selection_formats_display_values_by_kind() {
        let xy = ChartSelection::xy(
            "line",
            "point",
            "x 1, y 10",
            "x 1, y 10",
            "",
            1.0,
            10.0,
            "1",
            "10",
        );
        assert_eq!(xy.series_or_chart(), "line");
        assert_eq!(xy.display_value(), "1: 10");

        let category =
            ChartSelection::category("bar", "bar-a", "A", "A: 5", "A", "Actual", 5.0, "5");
        assert_eq!(category.series_or_chart(), "Actual");
        assert_eq!(category.display_value(), "A: 5");

        let share = ChartSelection::share(
            "pie",
            "organic",
            "Organic",
            "Organic: 8 (80%)",
            8.0,
            "8",
            80.0,
            "80%",
        );
        assert_eq!(share.series_or_chart(), "pie");
        assert_eq!(share.display_value(), "8 (80%)");
    }

    #[test]
    fn display_value_handles_partial_payloads() {
        let mut hover = ChartHover {
            kind: KIND_CATEGORY.into(),
            label: "Fallback".into(),
            value_label: "42".into(),
            ..ChartHover::default()
        };
        assert_eq!(hover.display_value(), "42");

        hover.value_label.clear();
        assert_eq!(hover.display_value(), "Fallback");
    }
}
