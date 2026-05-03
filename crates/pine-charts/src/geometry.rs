use crate::error::{finite, ChartError, ChartResult};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> ChartResult<Self> {
        Ok(Self {
            x: finite("x", x)?,
            y: finite("y", y)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartMargins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl ChartMargins {
    pub const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    pub const fn new(top: f64, right: f64, bottom: f64, left: f64) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    pub fn validate(self) -> ChartResult<Self> {
        let top = finite("margin.top", self.top)?;
        let right = finite("margin.right", self.right)?;
        let bottom = finite("margin.bottom", self.bottom)?;
        let left = finite("margin.left", self.left)?;

        Ok(Self {
            top,
            right,
            bottom,
            left,
        })
    }

    pub fn horizontal(self) -> f64 {
        self.left + self.right
    }

    pub fn vertical(self) -> f64 {
        self.top + self.bottom
    }
}

impl Default for ChartMargins {
    fn default() -> Self {
        Self {
            top: 16.0,
            right: 16.0,
            bottom: 32.0,
            left: 40.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ChartRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> ChartResult<Self> {
        let x = finite("x", x)?;
        let y = finite("y", y)?;
        let width = finite("width", width)?;
        let height = finite("height", height)?;

        if width <= 0.0 || height <= 0.0 {
            return Err(ChartError::InvalidSize { width, height });
        }

        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub fn from_outer(width: f64, height: f64, margins: ChartMargins) -> ChartResult<Self> {
        let width = finite("width", width)?;
        let height = finite("height", height)?;
        let margins = margins.validate()?;
        let inner_width = width - margins.horizontal();
        let inner_height = height - margins.vertical();
        Self::new(margins.left, margins.top, inner_width, inner_height)
    }

    pub fn right(self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_from_outer_subtracts_margins() {
        let rect =
            ChartRect::from_outer(300.0, 200.0, ChartMargins::new(10.0, 20.0, 30.0, 40.0)).unwrap();

        assert_eq!(
            rect,
            ChartRect {
                x: 40.0,
                y: 10.0,
                width: 240.0,
                height: 160.0,
            }
        );
    }

    #[test]
    fn rect_rejects_non_positive_inner_size() {
        let err = ChartRect::from_outer(20.0, 20.0, ChartMargins::new(10.0, 10.0, 5.0, 10.0))
            .unwrap_err();

        assert_eq!(
            err,
            ChartError::InvalidSize {
                width: 0.0,
                height: 5.0,
            }
        );
    }
}
