use crate::legend::series_label_or_default;
use crate::pie::slice_label;
use crate::{ChartAreaSeries, ChartBarSeries, ChartLineSeries, ChartPieSlice, ChartScatterSeries};

trait VisibleSeries {
    fn label(&self) -> &str;
    fn visible(&self) -> bool;
    fn set_visible(&mut self, visible: bool);
}

impl VisibleSeries for ChartLineSeries {
    fn label(&self) -> &str {
        &self.label
    }

    fn visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
}

impl VisibleSeries for ChartAreaSeries {
    fn label(&self) -> &str {
        &self.label
    }

    fn visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
}

impl VisibleSeries for ChartScatterSeries {
    fn label(&self) -> &str {
        &self.label
    }

    fn visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
}

impl VisibleSeries for ChartBarSeries {
    fn label(&self) -> &str {
        &self.label
    }

    fn visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
}

fn series_key(prefix: &str, index: usize, label: &str) -> String {
    format!("{prefix}-{index}-{}", series_label_or_default(label, index))
}

fn set_series_visible<T: VisibleSeries>(
    prefix: &str,
    series: &mut [T],
    key: &str,
    visible: bool,
) -> bool {
    let Some(item) = series
        .iter_mut()
        .enumerate()
        .find(|(index, series)| series_key(prefix, *index, series.label()) == key)
        .map(|(_, series)| series)
    else {
        return false;
    };
    item.set_visible(visible);
    true
}

fn toggle_series_visible<T: VisibleSeries>(prefix: &str, series: &mut [T], key: &str) -> bool {
    let Some(item) = series
        .iter_mut()
        .enumerate()
        .find(|(index, series)| series_key(prefix, *index, series.label()) == key)
        .map(|(_, series)| series)
    else {
        return false;
    };
    item.set_visible(!item.visible());
    true
}

pub fn set_line_series_visible(series: &mut [ChartLineSeries], key: &str, visible: bool) -> bool {
    set_series_visible("line-series", series, key, visible)
}

pub fn toggle_line_series_visibility(series: &mut [ChartLineSeries], key: &str) -> bool {
    toggle_series_visible("line-series", series, key)
}

pub fn set_area_series_visible(series: &mut [ChartAreaSeries], key: &str, visible: bool) -> bool {
    set_series_visible("area-series", series, key, visible)
}

pub fn toggle_area_series_visibility(series: &mut [ChartAreaSeries], key: &str) -> bool {
    toggle_series_visible("area-series", series, key)
}

pub fn set_scatter_series_visible(
    series: &mut [ChartScatterSeries],
    key: &str,
    visible: bool,
) -> bool {
    set_series_visible("scatter-series", series, key, visible)
}

pub fn toggle_scatter_series_visibility(series: &mut [ChartScatterSeries], key: &str) -> bool {
    toggle_series_visible("scatter-series", series, key)
}

pub fn set_bar_series_visible(series: &mut [ChartBarSeries], key: &str, visible: bool) -> bool {
    set_series_visible("bar-series", series, key, visible)
}

pub fn toggle_bar_series_visibility(series: &mut [ChartBarSeries], key: &str) -> bool {
    toggle_series_visible("bar-series", series, key)
}

pub fn set_pie_slice_visible(data: &mut [ChartPieSlice], key: &str, visible: bool) -> bool {
    let Some(slice) = data
        .iter_mut()
        .enumerate()
        .find(|(index, slice)| {
            format!("pie-slice-{index}-{}", slice_label(&slice.label, *index)) == key
        })
        .map(|(_, slice)| slice)
    else {
        return false;
    };
    slice.visible = visible;
    true
}

pub fn toggle_pie_slice_visibility(data: &mut [ChartPieSlice], key: &str) -> bool {
    let Some(slice) = data
        .iter_mut()
        .enumerate()
        .find(|(index, slice)| {
            format!("pie-slice-{index}-{}", slice_label(&slice.label, *index)) == key
        })
        .map(|(_, slice)| slice)
    else {
        return false;
    };
    slice.visible = !slice.visible;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChartBar, ChartPoint};

    #[test]
    fn updates_series_visibility_by_legend_key() {
        let mut series = vec![
            ChartLineSeries::new("Actual", vec![ChartPoint::new(0.0, 1.0)]),
            ChartLineSeries::new("", vec![ChartPoint::new(0.0, 2.0)]),
        ];

        assert!(set_line_series_visible(
            &mut series,
            "line-series-1-Series 2",
            false,
        ));
        assert!(series[0].visible);
        assert!(!series[1].visible);
    }

    #[test]
    fn toggles_bar_visibility_by_legend_key() {
        let mut series = vec![ChartBarSeries::new(
            "Revenue",
            vec![ChartBar::new("Q1", 42.0)],
        )];

        assert!(toggle_bar_series_visibility(
            &mut series,
            "bar-series-0-Revenue",
        ));
        assert!(!series[0].visible);
    }

    #[test]
    fn updates_pie_visibility_by_slice_key() {
        let mut data = vec![ChartPieSlice::new("", 4.0)];

        assert!(set_pie_slice_visible(
            &mut data,
            "pie-slice-0-Slice 1",
            false,
        ));
        assert!(!data[0].visible);
    }
}
