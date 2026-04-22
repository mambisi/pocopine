use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TabsDemo.poco", role = "panel")]
pub struct TabsDemo {
    pub tab: String,
}

#[handlers]
impl TabsDemo {
    pub fn on_mount(&mut self) { self.tab = "account".into(); }
}
