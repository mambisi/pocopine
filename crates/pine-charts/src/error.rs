use core::fmt;

pub type ChartResult<T> = Result<T, ChartError>;

#[derive(Clone, Debug, PartialEq)]
pub enum ChartError {
    NonFiniteValue { field: &'static str, value: f64 },
    InvalidRange { start: f64, end: f64 },
    InvalidDomain { start: f64, end: f64 },
    InvalidSize { width: f64, height: f64 },
    EmptySeries,
}

impl fmt::Display for ChartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteValue { field, value } => {
                write!(f, "chart value `{field}` must be finite, got {value}")
            }
            Self::InvalidRange { start, end } => {
                write!(
                    f,
                    "chart range must have a non-zero finite span, got {start}..{end}"
                )
            }
            Self::InvalidDomain { start, end } => {
                write!(
                    f,
                    "chart domain must have a non-zero finite span, got {start}..{end}"
                )
            }
            Self::InvalidSize { width, height } => {
                write!(
                    f,
                    "chart size must be positive and finite, got {width}x{height}"
                )
            }
            Self::EmptySeries => write!(f, "chart series must contain at least one point"),
        }
    }
}

impl std::error::Error for ChartError {}

pub(crate) fn finite(field: &'static str, value: f64) -> ChartResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ChartError::NonFiniteValue { field, value })
    }
}
