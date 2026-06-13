use crate::error::{ChartError, ChartResult, finite};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub value: f64,
    pub position: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearScale {
    domain_start: f64,
    domain_end: f64,
    range_start: f64,
    range_end: f64,
}

impl LinearScale {
    pub fn new(domain: (f64, f64), range: (f64, f64)) -> ChartResult<Self> {
        let domain_start = finite("domain.start", domain.0)?;
        let domain_end = finite("domain.end", domain.1)?;
        let range_start = finite("range.start", range.0)?;
        let range_end = finite("range.end", range.1)?;

        if domain_start == domain_end {
            return Err(ChartError::InvalidDomain {
                start: domain_start,
                end: domain_end,
            });
        }

        if range_start == range_end {
            return Err(ChartError::InvalidRange {
                start: range_start,
                end: range_end,
            });
        }

        Ok(Self {
            domain_start,
            domain_end,
            range_start,
            range_end,
        })
    }

    pub fn domain(self) -> (f64, f64) {
        (self.domain_start, self.domain_end)
    }

    pub fn range(self) -> (f64, f64) {
        (self.range_start, self.range_end)
    }

    pub fn map(self, value: f64) -> ChartResult<f64> {
        let value = finite("value", value)?;
        let t = (value - self.domain_start) / (self.domain_end - self.domain_start);
        Ok(self.range_start + t * (self.range_end - self.range_start))
    }

    pub fn invert(self, value: f64) -> ChartResult<f64> {
        let value = finite("value", value)?;
        let t = (value - self.range_start) / (self.range_end - self.range_start);
        Ok(self.domain_start + t * (self.domain_end - self.domain_start))
    }

    pub fn ticks(self, requested: usize) -> Vec<Tick> {
        let count = requested.max(2);
        let start = self.domain_start;
        let stop = self.domain_end;
        let reverse = stop < start;
        let span = (stop - start).abs();
        let step = nice_step(span / (count.saturating_sub(1) as f64));

        if step == 0.0 {
            return Vec::new();
        }

        let lo = start.min(stop);
        let hi = start.max(stop);
        let mut value = (lo / step).ceil() * step;
        let mut ticks = Vec::new();

        while value <= hi + step * f64::EPSILON {
            let signed_value = if reverse { start - (value - lo) } else { value };

            if let Ok(position) = self.map(signed_value) {
                ticks.push(Tick {
                    value: signed_value,
                    position,
                });
            }

            value += step;
        }

        ticks
    }
}

fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 0.0;
    }

    let power = raw.log10().floor();
    let base = 10.0_f64.powf(power);
    let error = raw / base;
    let factor = if error >= 7.5 {
        10.0
    } else if error >= 3.5 {
        5.0
    } else if error >= 1.5 {
        2.0
    } else {
        1.0
    };

    factor * base
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandScale {
    len: usize,
    range_start: f64,
    range_end: f64,
    padding_inner: f64,
    padding_outer: f64,
}

impl BandScale {
    pub fn new(
        len: usize,
        range: (f64, f64),
        padding_inner: f64,
        padding_outer: f64,
    ) -> ChartResult<Self> {
        let range_start = finite("range.start", range.0)?;
        let range_end = finite("range.end", range.1)?;
        let padding_inner = finite("padding.inner", padding_inner)?;
        let padding_outer = finite("padding.outer", padding_outer)?;

        if range_start == range_end {
            return Err(ChartError::InvalidRange {
                start: range_start,
                end: range_end,
            });
        }

        Ok(Self {
            len,
            range_start,
            range_end,
            padding_inner: padding_inner.clamp(0.0, 1.0),
            padding_outer: padding_outer.max(0.0),
        })
    }

    pub fn bandwidth(self) -> f64 {
        if self.len == 0 {
            return 0.0;
        }

        let denominator =
            self.len as f64 - self.padding_inner + self.padding_outer.mul_add(2.0, 0.0);
        (self.range_end - self.range_start).abs() / denominator * (1.0 - self.padding_inner)
    }

    pub fn step(self) -> f64 {
        if self.len == 0 {
            return 0.0;
        }

        let denominator =
            self.len as f64 - self.padding_inner + self.padding_outer.mul_add(2.0, 0.0);
        (self.range_end - self.range_start) / denominator
    }

    pub fn position(self, index: usize) -> Option<f64> {
        if index >= self.len {
            return None;
        }

        Some(self.range_start + self.step() * (self.padding_outer + index as f64))
    }

    pub fn center(self, index: usize) -> Option<f64> {
        self.position(index)
            .map(|position| position + self.step().signum() * self.bandwidth() * 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_scale_maps_and_inverts() {
        let scale = LinearScale::new((0.0, 100.0), (20.0, 220.0)).unwrap();

        assert_eq!(scale.map(25.0).unwrap(), 70.0);
        assert_eq!(scale.invert(70.0).unwrap(), 25.0);
    }

    #[test]
    fn linear_scale_rejects_flat_domain() {
        let err = LinearScale::new((2.0, 2.0), (0.0, 1.0)).unwrap_err();

        assert_eq!(
            err,
            ChartError::InvalidDomain {
                start: 2.0,
                end: 2.0,
            }
        );
    }

    #[test]
    fn linear_ticks_use_nice_steps() {
        let scale = LinearScale::new((0.0, 95.0), (0.0, 190.0)).unwrap();
        let values: Vec<f64> = scale.ticks(5).into_iter().map(|tick| tick.value).collect();

        assert_eq!(values, vec![0.0, 20.0, 40.0, 60.0, 80.0]);
    }

    #[test]
    fn band_scale_places_bands() {
        let scale = BandScale::new(3, (0.0, 300.0), 0.1, 0.2).unwrap();

        assert_close(scale.step(), 90.9090909090909);
        assert_close(scale.bandwidth(), 81.81818181818181);
        assert_close(scale.position(0).unwrap(), 18.18181818181818);
        assert_eq!(scale.position(3), None);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000000000001,
            "expected {actual} to be close to {expected}",
        );
    }
}
