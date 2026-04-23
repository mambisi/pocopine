use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TabsDemo.poco", style = "tabs.css", role = "panel")]
pub struct TabsDemo {
    pub tab: String,
}

#[handlers]
impl TabsDemo {
    pub fn on_mount(&mut self) {
        self.tab = "account".into();
    }
}
