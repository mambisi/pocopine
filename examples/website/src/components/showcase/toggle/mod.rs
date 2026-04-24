use pine::{PineToggle, PineToggleGroupItem, PineToggleGroupRoot};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ToggleDemo.poco",
    style = "toggle.css",
    role = "panel",
    // RFC 049 — Toggle is a leaf primitive; ToggleGroup Root
    // strictly only accepts Items.
    uses = [
        PineToggle,
        PineToggleGroupRoot,
        PineToggleGroupItem,
    ]
)]
pub struct ToggleDemo {
    pub bold: bool,
    pub align: String,
    pub format: Vec<String>,
}

#[handlers]
impl ToggleDemo {
    pub fn on_mount(&mut self) {
        self.align = "left".into();
    }
}
