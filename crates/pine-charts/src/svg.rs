use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgLine {
    pub key: String,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl SvgLine {
    pub fn new(key: String, x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self {
            key,
            x1,
            y1,
            x2,
            y2,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgTickLabel {
    pub key: String,
    pub value: f64,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub line_x1: f64,
    pub line_y1: f64,
    pub line_x2: f64,
    pub line_y2: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgAxisLabel {
    pub x: f64,
    pub y: f64,
    pub transform: String,
}

pub(crate) fn format_tick(value: f64) -> String {
    let rounded = if value.abs() >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.4}")
    };
    rounded
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
