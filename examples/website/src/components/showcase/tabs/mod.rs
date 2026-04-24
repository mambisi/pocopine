use pine::{PineTabsContent, PineTabsList, PineTabsRoot, PineTabsTrigger};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "TabsDemo.poco",
    style = "tabs.css",
    role = "panel",
    // RFC 049 — Tabs has two-level strict structure:
    //   Root → [List, Content]
    //   List → [Trigger]
    // Typos like placing a Trigger outside a List, or
    // dropping raw buttons where a Trigger belongs, fail at
    // compile time.
    uses = [PineTabsRoot, PineTabsList, PineTabsTrigger, PineTabsContent]
)]
pub struct TabsDemo {
    pub tab: String,
}

#[handlers]
impl TabsDemo {
    pub fn on_mount(&mut self) {
        self.tab = "account".into();
    }
}
