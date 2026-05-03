use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LegendItem {
    pub key: String,
    pub label: String,
    pub series: String,
}

impl LegendItem {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            key: label.clone(),
            series: label.clone(),
            label,
        }
    }

    pub fn with_key(key: impl Into<String>, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            key: key.into(),
            series: label.clone(),
            label,
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
        }
    }
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
    pub empty: bool,
}

impl Default for PineChartLegend {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            label: "Chart legend".into(),
            orientation: "horizontal".into(),
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
}
