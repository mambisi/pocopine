use serde::{Deserialize, Serialize};

pub const CHART_SELECT_EVENT: &str = "pp:chart:select";
pub const LEGEND_TOGGLE_EVENT: &str = "pp:chart:legend-toggle";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartSelection {
    pub chart: String,
    pub key: String,
    pub label: String,
    pub series: String,
    pub category: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub value: Option<f64>,
    pub percentage: Option<f64>,
}

impl ChartSelection {
    pub fn xy(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        series: impl Into<String>,
        x: f64,
        y: f64,
    ) -> Self {
        Self {
            chart: chart.into(),
            key: key.into(),
            label: label.into(),
            series: series.into(),
            category: String::new(),
            x: Some(x),
            y: Some(y),
            value: None,
            percentage: None,
        }
    }

    pub fn category(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        category: impl Into<String>,
        series: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            chart: chart.into(),
            key: key.into(),
            label: label.into(),
            series: series.into(),
            category: category.into(),
            x: None,
            y: None,
            value: Some(value),
            percentage: None,
        }
    }

    pub fn share(
        chart: impl Into<String>,
        key: impl Into<String>,
        label: impl Into<String>,
        value: f64,
        percentage: f64,
    ) -> Self {
        Self {
            chart: chart.into(),
            key: key.into(),
            label: label.into(),
            series: String::new(),
            category: String::new(),
            x: None,
            y: None,
            value: Some(value),
            percentage: Some(percentage),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LegendToggle {
    pub key: String,
    pub label: String,
    pub series: String,
    pub active: bool,
}
