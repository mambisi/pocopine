use pine::{PineTreeItem, PineTreeItemToggle, PineTreeRoot};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "TreeDemo.poco",
    style = "tree.css",
    role = "panel",
    // RFC 049 — Tree Root only takes Items; an Item can host a
    // Toggle plus nested Items (and arbitrary author content).
    uses = [
        PineTreeRoot,
        PineTreeItem,
        PineTreeItemToggle,
    ]
)]
pub struct TreeDemo {
    pub value: String,
}

#[handlers]
impl TreeDemo {}
