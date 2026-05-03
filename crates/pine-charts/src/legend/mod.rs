use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::events::{LegendToggle, LEGEND_TOGGLE_EVENT};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LegendItem {
    pub key: String,
    pub label: String,
    pub series: String,
    pub active: bool,
}

impl LegendItem {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            key: label.clone(),
            series: label.clone(),
            label,
            active: true,
        }
    }

    pub fn with_key(key: impl Into<String>, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            key: key.into(),
            series: label.clone(),
            label,
            active: true,
        }
    }

    pub fn with_series(
        key: impl Into<String>,
        label: impl Into<String>,
        series: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            series: series.into(),
            active: true,
        }
    }
}

pub(crate) fn series_label_or_default(label: &str, index: usize) -> String {
    if label.is_empty() {
        format!("Series {}", index + 1)
    } else {
        label.to_owned()
    }
}

pub(crate) fn series_legend_items(
    key_prefix: &str,
    labels: impl IntoIterator<Item = String>,
) -> Vec<LegendItem> {
    labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            LegendItem::with_series(
                format!("{key_prefix}-{index}-{label}"),
                label.clone(),
                label,
            )
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartLegend.poco", role = "panel")]
pub struct PineChartLegend {
    #[prop]
    pub items: Vec<LegendItem>,
    #[prop]
    pub label: String,
    #[prop]
    pub orientation: String,
    #[prop]
    pub interactive: bool,
    pub empty: bool,
}

impl Default for PineChartLegend {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            label: "Chart legend".into(),
            orientation: "horizontal".into(),
            interactive: false,
            empty: true,
        }
    }
}

#[handlers]
impl PineChartLegend {
    fn on_setup(&mut self) {
        self.recompute();
    }

    #[watch(items)]
    fn on_items(&mut self, _: Vec<LegendItem>, _: Option<Vec<LegendItem>>) {
        self.recompute();
    }

    #[watch(orientation)]
    fn on_orientation(&mut self, _: String, _: Option<String>) {
        self.recompute();
    }

    pub fn toggle_item(&mut self, key: String) {
        if !self.interactive {
            return;
        }
        let Some(item) = self.items.iter_mut().find(|item| item.key == key) else {
            return;
        };
        item.active = !item.active;
        let event = LegendToggle {
            key: item.key.clone(),
            label: item.label.clone(),
            series: item.series.clone(),
            active: item.active,
        };
        pocopine::emit(LEGEND_TOGGLE_EVENT, event);
    }
}

impl PineChartLegend {
    fn recompute(&mut self) {
        self.empty = self.items.is_empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legend_item_defaults_series_to_label() {
        let item = LegendItem::new("Organic");

        assert_eq!(item.key, "Organic");
        assert_eq!(item.label, "Organic");
        assert_eq!(item.series, "Organic");
        assert!(item.active);
    }

    #[test]
    fn helper_builds_series_legend_items() {
        let items = series_legend_items(
            "line-series",
            ["Actual".to_owned(), series_label_or_default("", 1)],
        );

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "line-series-0-Actual");
        assert_eq!(items[0].label, "Actual");
        assert_eq!(items[0].series, "Actual");
        assert_eq!(items[1].key, "line-series-1-Series 2");
        assert_eq!(items[1].label, "Series 2");
        assert!(items[1].active);
    }

    #[test]
    fn component_tracks_empty_state() {
        let mut legend = PineChartLegend::default();
        legend.recompute();
        assert!(legend.empty);

        legend.items = vec![LegendItem::new("Organic")];
        legend.recompute();
        assert!(!legend.empty);
    }

    #[test]
    fn component_toggles_items_when_interactive() {
        let mut legend = PineChartLegend {
            interactive: true,
            items: vec![LegendItem::new("Organic")],
            ..Default::default()
        };

        legend.toggle_item("Organic".into());

        assert!(!legend.items[0].active);
    }
}
